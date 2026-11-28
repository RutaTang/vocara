//! The installer's tools, run for real on a Mac (CI's macOS runner):
//! `codesign` judging bundles by the pinned requirement, `csreq` compiling
//! it, and `hdiutil` attaching, reading and detaching update images it
//! builds here. This machine has no Developer ID to sign with, so every
//! image here is refused at the signature check — the half an attacker
//! controls — and what is asserted is that nothing is staged and the image
//! never stays attached. (The swap script itself runs against dummy bundles
//! in `install::tests`, with `ditto` here.)

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::self_update::install::SigningIdentity;

fn identity(identifier: &str, team: &str) -> SigningIdentity {
    SigningIdentity {
        identifier: identifier.into(),
        team: team.into(),
    }
}

/// Builds a minimal ad-hoc signed `.app` under `dir`, declaring `version`.
/// Ad hoc means no certificate chain at all, which is what a forged update
/// looks like to everything but a requirement that names the anchor.
fn ad_hoc_bundle(dir: &Path, version: &str) -> PathBuf {
    let app = dir.join("Fake.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(app.join("Contents/MacOS/Fake"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>Fake</string>\
             <key>CFBundleIdentifier</key><string>com.rutatang.vocara</string>\
             <key>CFBundlePackageType</key><string>APPL</string>\
             <key>CFBundleShortVersionString</key><string>{version}</string>\
             </dict></plist>\n"
        ),
    )
    .unwrap();
    let out = Command::new("/usr/bin/codesign")
        .args(["--force", "-s", "-"])
        .arg(&app)
        .output()
        .unwrap();
    assert!(out.status.success(), "ad-hoc signing failed: {out:?}");
    app
}

/// csreq is the compiler codesign hands `-R` text to: the requirement
/// parses, rather than only being a string spelled right.
#[test]
fn the_requirement_compiles() {
    let arg = requirement_arg(&identity("com.rutatang.vocara", "TP7LCYW3NY")).unwrap();
    let out = Command::new("/usr/bin/csreq")
        .args(["-r", &arg, "-b", "/dev/null"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the requirement did not compile: {out:?}"
    );
}

/// The gate's reason to exist: `codesign --verify --deep --strict` alone
/// exits 0 on a bundle with no certificate chain whatsoever, because it
/// checks the candidate against the candidate's own designated requirement.
/// Only the pinned `-R` requirement rejects it. And an ad-hoc signature has
/// no team for an update to be anchored to.
#[test]
fn an_ad_hoc_bundle_passes_bare_verify_but_fails_the_pinned_requirement() {
    let dir = tempfile::tempdir().unwrap();
    let app = ad_hoc_bundle(dir.path(), "9.8.7");

    let bare = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app)
        .status()
        .unwrap();
    assert!(bare.success(), "the bare check is supposed to accept this");

    let err = verify(&app, &identity("com.rutatang.vocara", "TP7LCYW3NY")).unwrap_err();
    assert!(err.contains("signature is invalid"), "{err}");
    let err = signing_identity(&app).unwrap_err();
    assert!(err.contains("no Team Identifier"), "{err}");
    // The version gate reads the bundle's own plist.
    assert_eq!(bundle_short_version(&app).unwrap(), "9.8.7");
}

/// The test binary runs from no `.app`: it cannot replace itself, and says
/// so first.
#[test]
fn a_test_binary_cannot_update_itself() {
    assert_eq!(installed_bundle(), None);
    assert_eq!(running_bundle_version(), None);
    assert_eq!(
        self_install_blocker().as_deref(),
        Some("Vocara is not running from an installed app")
    );
    let err = MacInstaller
        .install(&InstallRequest {
            dmg: Path::new("/nonexistent.dmg"),
            expected: "9.9.9",
            running: "0.3.0",
            updates_dir: Path::new("/nonexistent"),
            clear_to_swap: &|| true,
        })
        .unwrap_err();
    assert!(err.contains("not running from an installed app"), "{err}");
}

// ------------------------------------------------------- disk images

/// Held by each test here that attaches a disk image: across processes too
/// (two test runs at once share the machine's disks), as a shared `flock`
/// on a file every run sees and none writes.
fn disks() -> std::fs::File {
    let file = std::fs::File::open("/usr/bin/hdiutil").expect("hdiutil is where it always is");
    file.lock_shared().expect("the disk images' lock");
    file
}

/// How often a step of `hdiutil`'s that failed for want of the service,
/// not for what a test is about, is tried in all.
const TRIES: u32 = 5;

/// The pause after try `attempt` failed: half a second, doubled each time.
fn pause(attempt: u32) {
    std::thread::sleep(std::time::Duration::from_millis(500) * 2u32.pow(attempt - 1));
}

/// Whether `hdiutil` failed, as `error` says, the way it does now and then
/// on a busy machine.
fn transient(error: &str) -> bool {
    [
        "temporarily unavailable",
        "busy",
        "timed out",
        "no mountable file systems",
    ]
    .iter()
    .any(|how| error.contains(how))
}

/// `f`, run again while it fails the way `hdiutil` does on a busy machine.
fn past_busy<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut attempt = 1;
    loop {
        match f() {
            Err(e) if attempt < TRIES && transient(&e) => {
                pause(attempt);
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// A disk image `dmg` of the folder `src`, its volume named `volume`, on
/// file system `fs` (`hdiutil`'s default when `None`). Its canonical path.
fn create_image(src: &Path, dmg: &Path, volume: &str, fs: Option<&str>) -> PathBuf {
    let created = (1..=TRIES).any(|attempt| {
        let mut create = Command::new("/usr/bin/hdiutil");
        create.args(["create", "-quiet", "-ov", "-volname", volume]);
        if let Some(fs) = fs {
            create.args(["-fs", fs]);
        }
        create.arg("-srcfolder").arg(src).arg(dmg);
        let ok = create.status().is_ok_and(|s| s.success());
        if !ok && attempt < TRIES {
            pause(attempt);
        }
        ok
    });
    assert!(created, "hdiutil create failed ({fs:?})");
    dmg.canonicalize().unwrap()
}

/// [`attached_disk`], asked again while `hdiutil info` fails.
fn disk_of(image: &Path) -> Option<PathBuf> {
    for attempt in 1..=TRIES {
        match attached_disk(image) {
            Ok(disk) => return disk,
            Err(_) if attempt < TRIES => pause(attempt),
            Err(e) => panic!("hdiutil info: {e}"),
        }
    }
    unreachable!()
}

/// Whether the image attached from the file `image` is still attached once
/// `hdiutil info` had ten seconds to stop listing it, detaching it by force
/// if so.
fn left_attached(image: &Path) -> bool {
    let since = std::time::Instant::now();
    let disk = loop {
        match disk_of(image) {
            None => return false,
            Some(disk) if since.elapsed() >= std::time::Duration::from_secs(10) => break disk,
            Some(_) => std::thread::sleep(std::time::Duration::from_millis(250)),
        }
    };
    let _ = Command::new("/usr/bin/hdiutil")
        .args(["detach", "-force", "-quiet"])
        .arg(&disk)
        .status();
    true
}

/// Detaches the image attached from the file it holds, if it still is,
/// when dropped — the cleanup a failing assertion must not skip.
struct Detached(PathBuf);

impl Drop for Detached {
    fn drop(&mut self) {
        if let Some(disk) = disk_of(&self.0) {
            let _ = Command::new("/usr/bin/hdiutil")
                .args(["detach", "-force", "-quiet"])
                .arg(&disk)
                .status();
        }
    }
}

/// The image half of an install, run for real on images `hdiutil` builds
/// here, on both file systems an update image can have (release.yml makes
/// HFS+; APFS is `hdiutil`'s default): the `vocara.app` on it must pass the
/// pinned signature check before anything is copied, and on every outcome
/// the image is detached again and staging is left empty.
#[test]
fn an_image_is_staged_only_past_the_signature_check_and_always_detached() {
    let _disks = disks();
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let staged_nothing = || std::fs::read_dir(&staging).unwrap().next().is_none();
    let signer = identity("com.rutatang.vocara", "TP7LCYW3NY");
    fn request(dmg: &Path) -> InstallRequest<'_> {
        InstallRequest {
            dmg,
            expected: "9.8.7",
            running: "0.3.0",
            // Not the staging half's: it stages where it is told.
            updates_dir: Path::new("/nonexistent"),
            clear_to_swap: &|| true,
        }
    }
    for (n, fs) in [Some("HFS+"), None].into_iter().enumerate() {
        // Unique per run: a leftover volume of the same name would make
        // `hdiutil` mount this one elsewhere.
        let volume = format!("VocaraStageTest{}-{n}", std::process::id());
        let image = |name: &str, app: bool| -> Detached {
            let src = dir.path().join(format!("{name}-{n}-src"));
            std::fs::create_dir_all(&src).unwrap();
            if app {
                std::fs::rename(ad_hoc_bundle(&src, "9.8.7"), src.join("vocara.app")).unwrap();
            } else {
                std::fs::write(src.join("README.txt"), "no app here\n").unwrap();
            }
            Detached(create_image(
                &src,
                &dir.path().join(format!("{name}-{n}.dmg")),
                &volume,
                fs,
            ))
        };

        let forged = image("forged", true);
        let err =
            past_busy(|| stage_from_image(&request(&forged.0), &signer, &staging)).unwrap_err();
        assert!(err.contains("signature is invalid"), "{fs:?}: {err}");
        assert!(
            !left_attached(&forged.0),
            "{fs:?}: the refused image stayed attached"
        );
        assert!(staged_nothing(), "{fs:?}: a refused bundle was staged");

        let empty = image("empty", false);
        let err =
            past_busy(|| stage_from_image(&request(&empty.0), &signer, &staging)).unwrap_err();
        assert!(err.contains("has no vocara.app"), "{fs:?}: {err}");
        assert!(
            !left_attached(&empty.0),
            "{fs:?}: the image stayed attached"
        );
        assert!(staged_nothing(), "{fs:?}");
    }

    let junk = dir.path().join("junk.dmg");
    std::fs::write(&junk, b"not a disk image").unwrap();
    let err = stage_from_image(&request(&junk), &signer, &staging).unwrap_err();
    assert!(err.contains("could not open the update image"), "{err}");
    assert!(staged_nothing());
}

/// `hdiutil attach` succeeded, but its answer could not be read (the strict
/// parser refuses any token it does not expect): the image is detached
/// again rather than left attached for the rest of the session.
#[test]
fn an_image_whose_attach_answer_cannot_be_read_is_detached_again() {
    let _disks = disks();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.txt"), "a\n").unwrap();
    for (n, fs) in [Some("HFS+"), None].into_iter().enumerate() {
        let volume = format!("VocaraUnreadTest{}-{n}", std::process::id());
        let dmg = create_image(
            &dir.path().join("src"),
            &dir.path().join(format!("{volume}.dmg")),
            &volume,
            fs,
        );
        let _cleanup = Detached(dmg.clone());
        let err = past_busy(|| attach_image_reading(&dmg, |_| None)).unwrap_err();
        assert!(err.contains("could not find the update volume"), "{err}");
        assert!(!left_attached(&dmg), "{fs:?}: the image stayed attached");
    }
}

/// An image attached the way an update's is, its volume read from the real
/// `hdiutil attach -plist` answer, is detached by its file.
#[test]
fn a_real_attach_answer_names_the_volume_and_the_image_detaches() {
    let _disks = disks();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.txt"), "a\n").unwrap();
    let volume = format!("Vocara Attach Test {}", std::process::id());
    let dmg = create_image(
        &dir.path().join("src"),
        &dir.path().join("attach.dmg"),
        &volume,
        Some("HFS+"),
    );
    let _cleanup = Detached(dmg.clone());
    let attached = past_busy(|| attach_image(&dmg)).unwrap();
    assert_eq!(attached.image, dmg);
    assert!(
        attached.mount_point.join("a.txt").exists(),
        "{:?}",
        attached.mount_point
    );
    assert!(disk_of(&dmg).is_some());
    detach_image(&dmg);
    assert!(!left_attached(&dmg), "the image stayed attached");
}
