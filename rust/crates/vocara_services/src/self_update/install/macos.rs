//! The installer on a Mac: the tools the decisions in [`super`] are made
//! of — `codesign`, `spctl`, `PlistBuddy`, `hdiutil`, `ditto`, and the
//! detached `bash` that swaps the bundle (Clew's `src/macos/install.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    app_on_image, installed_bundle_of, notarization_gate, parse_attach_plist, parse_info_plist,
    parse_signing_identity, replaceable, requirement_arg, swap_script, SigningIdentity, SwapPlan,
    COPIER, EXIT_WAIT_TICKS, OPENER, REQUIRE_NOTARIZATION,
};
use crate::self_update::files::{self, INSTALL_FAILED_MARKER, INSTALL_LOG, STAGING_PREFIX};
use crate::self_update::{check_candidate_version, InstallRequest, Installer};
use vocara_model::diagnostics::DiagnosticsLog;

/// The installer of an installed, Developer-ID-signed Vocara.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacInstaller;

impl Installer for MacInstaller {
    fn blocker(&self) -> Option<String> {
        self_install_blocker()
    }

    fn install(&self, request: &InstallRequest<'_>) -> Result<(), String> {
        let result = install_dmg(request);
        if let Err(e) = &result {
            DiagnosticsLog::instance().log("update", &format!("not installed: {e}"));
        }
        result
    }
}

/// The installed `vocara.app` this process runs from, or `None` when it runs
/// from no `.app` (a `cargo run` build).
pub fn installed_bundle() -> Option<PathBuf> {
    installed_bundle_of(&std::env::current_exe().ok()?)
}

/// The `CFBundleShortVersionString` of the app bundle Vocara runs from, or
/// `None` when it runs from none (a bare binary's "main bundle" is its
/// directory, with no version of its own).
pub fn running_bundle_version() -> Option<String> {
    use objc2_foundation::{ns_string, NSBundle, NSString};
    installed_bundle()?;
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(ns_string!("CFBundleShortVersionString"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|version| version.to_string())
}

/// Why this copy of Vocara cannot replace itself, or `None` when it can: an
/// installed `.app` ([`installed_bundle`]) this account may replace
/// ([`replaceable`]), signed by a team an update can be anchored to (an
/// ad-hoc-signed build has none). Asked BEFORE the download, so these are a
/// line on screen and the release page instead of reasons surfacing after
/// Vocara quit for a swap that cannot happen; [`install_dmg`] re-checks all
/// of it rather than trusting this answer, the two being a download apart.
pub fn self_install_blocker() -> Option<String> {
    let Some(bundle) = installed_bundle() else {
        return Some("Vocara is not running from an installed app".to_string());
    };
    if let Err(e) = replaceable(&bundle) {
        return Some(e);
    }
    if let Err(e) = signing_identity(&bundle) {
        return Some(format!("this copy of Vocara cannot update itself ({e})"));
    }
    None
}

/// Runs a command: its standard output on success, else its trimmed error
/// output (or standard output, when that is empty).
fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let msg = if err.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        err.into_owned()
    };
    Err(msg.trim().to_string())
}

/// The signing identity of `bundle`, from `codesign -d` (which prints its
/// details to standard error).
fn signing_identity(bundle: &Path) -> Result<SigningIdentity, String> {
    let out = Command::new("/usr/bin/codesign")
        .args(["-d", "--verbose=4"])
        .arg(bundle)
        .output()
        .map_err(|e| e.to_string())?;
    parse_signing_identity(&format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Gatekeeper's assessment of the downloaded image, as if the user had
/// double-clicked it: a valid signature over the whole image and a stapled
/// Apple notarization ticket. The release staples the ticket onto the DMG,
/// so this needs no network. The same command release.yml's "A signed DMG
/// is a notarized DMG" step runs on every release.
fn assess_notarized(dmg: &Path) -> Result<(), String> {
    run(Command::new("/usr/sbin/spctl")
        .args([
            "--assess",
            "--type",
            "open",
            "--context",
            "context:primary-signature",
        ])
        .arg(dmg))
    .map(|_| ())
}

/// Verifies that `app` is an intact Vocara from the same signer as this
/// one: structural integrity and the pinned requirement ([`requirement_arg`];
/// `-R` is the whole point — without it codesign checks the bundle only
/// against its own designated requirement, which a self-signed forgery
/// satisfies). Then what the bundle declares must agree with what the
/// requirement just matched: a disagreement means a hand-assembled
/// signature, not something Apple's tools produced.
fn verify(app: &Path, identity: &SigningIdentity) -> Result<(), String> {
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "-R"])
        .arg(requirement_arg(identity)?)
        .arg(app))
    .map_err(|e| format!("the update's signature is invalid: {e}"))?;
    let declared = signing_identity(app)?;
    if declared != *identity {
        return Err(format!(
            "the update is signed as {} by team {} (this app is {} by {}), so Vocara won't \
             install it",
            declared.identifier, declared.team, identity.identifier, identity.team
        ));
    }
    Ok(())
}

/// The `CFBundleShortVersionString` an app bundle declares, as written.
fn bundle_short_version(app: &Path) -> Result<String, String> {
    run(Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString"])
        .arg(app.join("Contents/Info.plist")))
    .map(|out| out.trim().to_string())
    .map_err(|e| format!("the update does not state its version: {e}"))
}

/// Verifies the downloaded image's app, stages it, then — once
/// [`InstallRequest::clear_to_swap`] lets it — hands off to a detached
/// helper that swaps it in and relaunches once this process exits. Returns
/// as soon as the helper runs; the app then quits. Blocking.
pub fn install_dmg(request: &InstallRequest<'_>) -> Result<(), String> {
    let target = installed_bundle()
        .ok_or("Vocara is not running from an installed app, so it can't update itself")?;
    // Re-checked, not taken from the pre-download answer: an install that
    // cannot complete must fail HERE, while Vocara still runs to say so.
    replaceable(&target)?;
    // The identity every signature check below is made against: read again
    // rather than taken on an earlier answer's word. An ad-hoc-signed install
    // fails here (no team), and has never been offered "Update now".
    let identity = signing_identity(&target)
        .map_err(|e| format!("cannot read this app's signing identity: {e}"))?;
    // Assessed, then judged by policy: under the shipped policy this REFUSES
    // an un-notarized image; the admitting branch stays so that turning
    // `REQUIRE_NOTARIZATION` off cannot make such an install silent.
    if let Some(reason) = notarization_gate(assess_notarized(request.dmg), REQUIRE_NOTARIZATION)? {
        DiagnosticsLog::instance().log(
            "update",
            &format!("{reason}: installing it anyway, its signature anchored to this app's team"),
        );
    }
    // Made before the image is touched, so every later failure has one
    // directory to clean up and nothing untrusted is written until the
    // signature check passed.
    let staging = files::create_private_dir(request.updates_dir, STAGING_PREFIX)?;
    let launched = stage_from_image(request, &identity, &staging).and_then(|staged_app| {
        // The last moment the update can step back: once the helper runs,
        // its wait for Vocara to quit has begun. A take or an import started
        // while this verified and staged is waited for here, and a cancel
        // meanwhile starts nothing.
        if !(request.clear_to_swap)() {
            return Err("the update was cancelled before the swap".to_string());
        }
        spawn_swap_helper(&SwapPlan {
            pid: std::process::id(),
            staged_app: &staged_app,
            staging: &staging,
            target: &target,
            log: &request.updates_dir.join(INSTALL_LOG),
            failed_marker: &request.updates_dir.join(INSTALL_FAILED_MARKER),
            opener: Path::new(OPENER),
            copier: Path::new(COPIER),
            wait_ticks: EXIT_WAIT_TICKS,
        })
    });
    if launched.is_err() {
        // Only the helper prunes the staging directory, so if it never
        // starts nothing else will (and a staged copy is a whole app).
        let _ = std::fs::remove_dir_all(&staging);
    }
    launched
}

/// Mounts the image read-only, verifies the `vocara.app` on it, checks its
/// version, and copies it into `staging` (verified again there) so the
/// image can be detached before the swap. Returns the staged copy.
pub(super) fn stage_from_image(
    request: &InstallRequest<'_>,
    identity: &SigningIdentity,
    staging: &Path,
) -> Result<PathBuf, String> {
    let image = attach_image(request.dmg)?;
    // Everything between mount and detach goes through this closure, so the
    // image is always detached, an early error included.
    let staged = (|| -> Result<PathBuf, String> {
        let src_app = app_on_image(&image.mount_point)?;
        verify(&src_app, identity)?;
        // After the signature check: the Info.plist is sealed by the
        // signature just checked, so the version read from it is the
        // signer's.
        check_candidate_version(
            &bundle_short_version(&src_app)?,
            request.expected,
            request.running,
        )?;
        let staged_app = staging.join(super::APP_ON_IMAGE);
        run(Command::new(COPIER).arg(&src_app).arg(&staged_app))
            .map_err(|e| format!("could not stage the update: {e}"))?;
        // What gets installed is the copy, so the copy is what is checked:
        // it sits in this user's private folder, where nothing changes it
        // between this check and the swap.
        verify(&staged_app, identity)?;
        Ok(staged_app)
    })();
    detach_image(&image.image);
    staged
}

/// The update image, attached.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Attached {
    /// The image file, canonical: what it was attached by, which is what
    /// `hdiutil info` lists it under, and so what it is detached by.
    pub(super) image: PathBuf,
    /// Where its volume is mounted.
    pub(super) mount_point: PathBuf,
}

/// Attaches `dmg` read-only — no Finder window, no auto-open — and finds
/// its volume in what `hdiutil` says about it ([`parse_attach_plist`]).
pub(super) fn attach_image(dmg: &Path) -> Result<Attached, String> {
    attach_image_reading(dmg, parse_attach_plist)
}

/// [`attach_image`], with what reads `hdiutil attach`'s answer given (a
/// test's fails). The image is attached by its canonical path, because that
/// is what `hdiutil info` then lists it under; and once `hdiutil attach` has
/// run, any failure detaches the image again, by that path.
pub(super) fn attach_image_reading(
    dmg: &Path,
    read: impl FnOnce(&str) -> Option<PathBuf>,
) -> Result<Attached, String> {
    let image =
        std::fs::canonicalize(dmg).map_err(|e| format!("could not open the update image: {e}"))?;
    let attached = run(Command::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-plist",
            "-nobrowse",
            "-readonly",
            "-noverify",
            "-noautoopen",
        ])
        .arg(&image))
    .map_err(|e| format!("could not open the update image: {e}"))
    .and_then(|plist| read(&plist).ok_or_else(|| "could not find the update volume".to_string()));
    match attached {
        Ok(mount_point) => Ok(Attached { image, mount_point }),
        Err(e) => {
            // Usually nothing is attached, and the lookup says so.
            detach_image(&image);
            Err(e)
        }
    }
}

/// How often a detach is tried before it is forced, how often in all, and
/// the pause after the first failure (it grows with each): 8.4 s of pauses
/// in all, five of the tries forced.
const DETACH_PLAIN: u32 = 3;
const DETACH_ATTEMPTS: u32 = 8;
const DETACH_PAUSE: std::time::Duration = std::time::Duration::from_millis(300);

/// Detaches the image attached from the file `image` (canonical), if it
/// still is. A volume just mounted is often busy for a moment — Spotlight
/// and fseventsd look at every new one — and a lone `hdiutil detach` then
/// fails ("Resource busy"), leaving the image attached for the rest of the
/// session. So it is tried again after a pause, then forced (nothing of
/// Vocara's is open on it any more), off the UI thread. The image is found
/// by its file each time ([`attached_disk`]), never by a name that can come
/// to mean another disk ([`parse_info_plist`]); one `hdiutil info` no longer
/// lists is detached already.
pub(super) fn detach_image(image: &Path) {
    for attempt in 1..=DETACH_ATTEMPTS {
        match attached_disk(image) {
            Ok(None) => return,
            Ok(Some(disk)) => {
                let mut detach = Command::new("/usr/bin/hdiutil");
                detach.arg("detach");
                if attempt > DETACH_PLAIN {
                    detach.arg("-force");
                }
                if run(detach.arg("-quiet").arg(&disk)).is_ok() {
                    return;
                }
            }
            // `hdiutil info` failed: looked at again after the pause.
            Err(_) => {}
        }
        if attempt < DETACH_ATTEMPTS {
            std::thread::sleep(DETACH_PAUSE * attempt);
        }
    }
}

/// The disk the image file `image` (canonical) is attached as now, as
/// `hdiutil info` lists it; `None` when it is not attached.
pub(super) fn attached_disk(image: &Path) -> Result<Option<PathBuf>, String> {
    let plist = run(Command::new("/usr/bin/hdiutil").args(["info", "-plist"]))?;
    parse_info_plist(&plist, image).ok_or_else(|| "could not read what hdiutil info said".into())
}

/// Starts the detached helper that performs the swap once Vocara exits
/// ([`swap_script`], handed to bash through argv). `std::process::Command`
/// does not kill its child when dropped, so the helper outlives Vocara.
fn spawn_swap_helper(plan: &SwapPlan<'_>) -> Result<(), String> {
    Command::new("/bin/bash")
        .arg("-c")
        .arg(swap_script(plan))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the updater helper: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
