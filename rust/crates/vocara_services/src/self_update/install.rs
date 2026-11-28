//! Applying a downloaded update: verify the new bundle, then swap it in and
//! relaunch (a port of Clew's `src/macos/install.rs`). This file holds the
//! decisions, platform-neutral and tested everywhere; [`macos`] runs the
//! tools they are made of (`codesign`, `spctl`, `hdiutil`, `ditto`, the
//! swap helper) on a Mac.
//!
//! The trust anchor is the running app itself. The gate that establishes
//! PROVENANCE is the signature check ([`requirement_arg`]): the `vocara.app`
//! inside the downloaded image must satisfy a code requirement pinning
//! `anchor apple generic`, a Developer ID certificate chain (the
//! intermediate's and the leaf's Apple OIDs), the SAME signing identifier
//! and the SAME Team Identifier as the app currently running, which the user
//! already trusted enough to install. It is offline and never skipped. No
//! key or team id is hard-coded — an update is accepted only when it was
//! signed by whoever signed the copy already running, as the same app (so a
//! dev or test flavor signed by the same team cannot replace the release
//! build, nor the reverse), with a certificate chain that really terminates
//! at an Apple root. That is why the bundle id and the Team ID must never
//! change (`docs/release-runbook.md`): an install would refuse every update
//! signed otherwise.
//!
//! Provenance is not freshness, so the verified bundle's own version is
//! checked too (`super::check_candidate_version`): it must be exactly the
//! release the user was offered and newer than the running one.
//!
//! A second gate assesses Apple notarization ([`notarization_gate`]). It
//! adds Apple's malware scan of this build and post-hoc revocation, and it
//! is REQUIRED — but only because the release workflow guarantees every
//! signed release is notarized. [`REQUIRE_NOTARIZATION`] states that
//! coupling; neither side may move alone.
//!
//! The one gate has to be spelled out, because the obvious spellings check
//! nothing. `codesign --verify` with no `-R` verifies a bundle only for
//! internal integrity and against its OWN designated requirement, which a
//! self-signed forgery satisfies by construction, and the `TeamIdentifier=`
//! line `codesign -d` prints is a field the candidate declares about itself
//! rather than one read from a certificate. Nothing in the download path
//! sets `com.apple.quarantine` either, so the swapped bundle is never
//! assessed by Gatekeeper on relaunch: whatever `-R` does not check, nothing
//! downstream will.
//!
//! Verification proves something about the bundle on the read-only image,
//! but what gets installed is the copy made of it, so that copy lands
//! somewhere only this user can reach (`super::files::create_private_dir`),
//! is verified again there, and the script that installs it never touches
//! the filesystem before Vocara is gone ([`swap_script`]).
//!
//! The swap itself cannot happen in-process (a running bundle is not
//! replaced cleanly from inside it), so a small detached shell helper waits
//! for this process to exit, swaps the bundle, and relaunches. Everything
//! that could make that swap fail after Vocara has already quit — a
//! read-only disk image, App Translocation, a folder this user cannot write
//! — is checked BEFORE the download ([`replaceable`]), and a swap that fails
//! anyway puts the previous version back, relaunches it, and leaves the
//! reason where the relaunched Vocara reports it.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
pub mod macos;

/// The app's folder on the update image: release.yml's "Create and sign
/// DMG" stages it as `vocara.app` beside an Applications link.
pub const APP_ON_IMAGE: &str = "vocara.app";

/// Whether an update must carry Apple's notarization ticket, as opposed to
/// merely being assessed for one.
///
/// True, and true only because the release workflow guarantees it: every
/// release is signed with the Developer ID and then notarized and stapled,
/// and release.yml's "A signed DMG is a notarized DMG" step asserts the very
/// predicate `macos::assess_notarized` evaluates, so a DMG that would fail
/// here fails the release first. If the workflow ever published a signed
/// but un-notarized image again, this would have to go back to `false` in
/// the same change — or every such release would be a permanent "Update
/// failed" with a Try again that can never succeed, while the same file
/// installed by hand through Gatekeeper's override.
///
/// What it adds over the signature check: Apple's malware scan of this exact
/// build, and Apple's ability to revoke a ticket after the fact. Provenance
/// is NOT what it buys — that comes from the pinned requirement, which reads
/// the team out of the leaf certificate of a chain terminating at an Apple
/// root.
pub const REQUIRE_NOTARIZATION: bool = true;

/// How long the swap helper waits for Vocara to exit, in 0.1 s ticks: three
/// minutes. The app quits through its quit guard, which finalizes the
/// recordings first and may itself take up to `quit_request::REPEAT_WINDOW`
/// (65 s); past the wait the helper gives up WITHOUT swapping — moving the
/// bundle out from under a Vocara that is still running, then opening a
/// second copy, is worse than no update.
pub const EXIT_WAIT_TICKS: u32 = 1800;

/// The installed `vocara.app` the executable `exe` runs from
/// (`…/vocara.app/Contents/MacOS/vocara` → `…/vocara.app`), or `None` when
/// it runs from no `.app` (a `cargo run` build): there is nothing to swap
/// then.
pub fn installed_bundle_of(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    bundle
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        .then(|| bundle.to_path_buf())
}

/// Whether the swap helper will be able to move `bundle` aside and put the
/// new version in its place: not a copy App Translocation made, and a
/// folder and a bundle this account may write. Asked before the download,
/// and again before the install: the reasons all used to surface only after
/// the app had quit for the swap, when nobody was left to see them.
pub fn replaceable(bundle: &Path) -> Result<(), String> {
    // Gatekeeper runs a quarantined app that was never moved from a
    // randomized read-only mirror; there is no installed copy to replace.
    if bundle
        .as_os_str()
        .as_bytes()
        .windows(18)
        .any(|w| w == b"/AppTranslocation/")
    {
        return Err(
            "Vocara is running from a temporary copy that macOS made. Move Vocara to \
                    the Applications folder and open it from there"
                .to_string(),
        );
    }
    let parent = bundle
        .parent()
        .ok_or("the app bundle has no parent folder")?;
    // `access(W_OK)` answers for this user, ACLs and read-only mounts
    // included (a mounted disk image reports EROFS). The parent is where the
    // backup and the new copy are made; the bundle itself is renamed and,
    // once the new version is in, deleted, which needs write access inside
    // it too.
    for (path, what) in [
        (parent, "the folder Vocara is installed in"),
        (bundle, "Vocara's app"),
    ] {
        if let Err(e) = writable(path) {
            return Err(match e.raw_os_error() {
                Some(libc::EROFS) => "Vocara is running from a read-only volume, probably its \
                                      disk image. Copy Vocara to the Applications folder and \
                                      open it from there"
                    .to_string(),
                _ => format!(
                    "this account cannot modify {what} ({}): {e}",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

/// `access(path, W_OK)` for the current user.
fn writable(path: &Path) -> std::io::Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("the path holds a NUL byte"))?;
    // SAFETY: `c` is a valid NUL-terminated string for the whole call.
    if unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Who signed a bundle, as `codesign -d` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningIdentity {
    /// The code-signing identifier (the bundle id, for an app).
    pub identifier: String,
    /// The Team Identifier.
    pub team: String,
}

/// `Identifier=` and `TeamIdentifier=` from `codesign -d --verbose=4`'s
/// output. Both are interpolated into a requirement expression later, so
/// anything but their plain character sets is refused rather than escaped;
/// an ad-hoc signature (`TeamIdentifier=not set`) has no team to anchor an
/// update to.
pub fn parse_signing_identity(text: &str) -> Result<SigningIdentity, String> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(name))
            .map(|s| s.trim().to_string())
    };
    let team = field("TeamIdentifier=")
        .filter(|s| !s.is_empty() && s != "not set")
        .ok_or("the app has no Team Identifier")?;
    if !plain_team(&team) {
        return Err(format!("`{team}` is not a usable Team Identifier"));
    }
    let identifier = field("Identifier=")
        .filter(|s| !s.is_empty())
        .ok_or("the app has no signing identifier")?;
    if !plain_identifier(&identifier) {
        return Err(format!("`{identifier}` is not a usable signing identifier"));
    }
    Ok(SigningIdentity { identifier, team })
}

fn plain_team(team: &str) -> bool {
    !team.is_empty() && team.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn plain_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// The `-R` argument an update's signature must satisfy:
///
/// - `anchor apple generic`: a certificate chain that really terminates at
///   an Apple root;
/// - `certificate 1[field.1.2.840.113635.100.6.2.6]`: the intermediate is
///   Apple's Developer ID CA, and
///   `certificate leaf[field.1.2.840.113635.100.6.1.13]`: the leaf is a
///   Developer ID Application certificate — together the standard
///   Developer ID designated requirement, so a Mac App Store, development or
///   enterprise certificate issued to the same team does not qualify;
/// - `identifier`: the same signing identifier as the running app
///   (`com.rutatang.vocara`), so another app of the same team, or another
///   Vocara flavor, cannot pose as an update;
/// - `certificate leaf[subject.OU]`: the team, read from the leaf
///   certificate instead of the self-declared CodeDirectory field.
///
/// The leading `=` makes codesign read this as requirement source text;
/// without it the string is taken as a path to a compiled requirement. Both
/// interpolated values were held to plain character sets when read
/// ([`parse_signing_identity`]) and are checked again here, where a quote
/// would reshape the expression being trusted.
pub fn requirement_arg(identity: &SigningIdentity) -> Result<String, String> {
    let SigningIdentity { identifier, team } = identity;
    if !plain_team(team) {
        return Err(format!("`{team}` is not a usable Team Identifier"));
    }
    if !plain_identifier(identifier) {
        return Err(format!("`{identifier}` is not a usable signing identifier"));
    }
    Ok(format!(
        "=anchor apple generic and identifier \"{identifier}\" \
         and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team}\""
    ))
}

/// Applies [`REQUIRE_NOTARIZATION`] (as `required`) to Gatekeeper's verdict
/// on the image. `Ok(None)`: assessed clean. `Ok(Some(reason))`: no ticket,
/// admitted by policy, and the reason handed back so it is logged rather
/// than passing silently. `Err`: no ticket where policy demands one.
pub fn notarization_gate(
    assessed: Result<(), String>,
    required: bool,
) -> Result<Option<String>, String> {
    match assessed {
        Ok(()) => Ok(None),
        Err(e) => {
            let reason = format!("the update is not notarized by Apple: {e}");
            if required {
                Err(reason)
            } else {
                Ok(Some(reason))
            }
        }
    }
}

/// The `vocara.app` at the top of the mounted image `mount`: the folder
/// itself, never a link to one — a link is verified at its target and then
/// copied from wherever it points by then.
pub fn app_on_image(mount: &Path) -> Result<PathBuf, String> {
    let app = mount.join(APP_ON_IMAGE);
    match std::fs::symlink_metadata(&app) {
        Ok(meta) if meta.is_dir() => Ok(app),
        Ok(_) => Err("the update image's vocara.app is not an app folder".to_string()),
        Err(_) => Err("the update image has no vocara.app".to_string()),
    }
}

// ------------------------------------------------------------- hdiutil

/// Where the volume in `hdiutil attach -plist`'s output is mounted: the
/// first system entity with a `mount-point` (and a `/dev/` entry).
///
/// Read as the property list it is — what `hdiutil` tells programs to read
/// — rather than as the columns of its text output: a volume's name is the
/// image's to choose, and a crafted one holds the tabs and newlines those
/// columns are cut at, where the plist keeps it inside its own `<string>`.
pub fn parse_attach_plist(xml: &str) -> Option<PathBuf> {
    let Plist::Dict(root) = parse_plist(xml)? else {
        return None;
    };
    let Plist::Array(entities) = plist_field(&root, "system-entities")? else {
        return None;
    };
    entities.iter().find_map(|entity| {
        let Plist::Dict(fields) = entity else {
            return None;
        };
        plist_string(fields, "dev-entry").filter(|d| d.starts_with("/dev/"))?;
        plist_string(fields, "mount-point")
            .map(Path::new)
            .filter(|m| m.is_absolute())
            .map(Path::to_path_buf)
    })
}

/// In `hdiutil info -plist`'s output, the disk the image file `image`
/// (canonical) is attached as: `Some(None)` when no attached image is that
/// file, `None` when the document is not what `hdiutil info` prints.
///
/// An image is listed under the path it was attached by (`image-path`),
/// matched as it is against every entry first — the installer attaches by
/// the canonical path — and only then once resolved, and only an entry by
/// the image's own file name is resolved (resolving another image's path
/// touches its file system, which for one on a network share can take
/// long). Its disk is the first whole disk (`/dev/diskN`) among its
/// entities — the image's own, listed before the APFS container `hdiutil`
/// synthesizes for it — else any of its nodes, which detaches it all the
/// same.
///
/// Found by the file, never by a device node or a mount path: disk numbers
/// are handed out again, and once someone else ejects the update image the
/// next disk attached takes its number; a mount path names whatever is
/// mounted there now. Detached by either, another disk would be ejected in
/// the update image's place.
pub fn parse_info_plist(xml: &str, image: &Path) -> Option<Option<PathBuf>> {
    let Plist::Dict(root) = parse_plist(xml)? else {
        return None;
    };
    let images = match plist_field(&root, "images") {
        Some(Plist::Array(images)) => images.as_slice(),
        Some(_) => return None,
        None => &[],
    };
    let listed = || {
        images.iter().filter_map(|entry| match entry {
            Plist::Dict(fields) => Some((Path::new(plist_string(fields, "image-path")?), fields)),
            _ => None,
        })
    };
    let ours = listed()
        .find(|(path, _)| *path == image)
        .or_else(|| {
            listed().find(|(path, _)| {
                path.file_name() == image.file_name()
                    && std::fs::canonicalize(path).is_ok_and(|p| p == image)
            })
        })
        .map(|(_, fields)| fields);
    let Some(fields) = ours else {
        return Some(None);
    };
    let Plist::Array(entities) = plist_field(fields, "system-entities")? else {
        return None;
    };
    let nodes: Vec<&str> = entities
        .iter()
        .filter_map(|entity| match entity {
            Plist::Dict(fields) => plist_string(fields, "dev-entry"),
            _ => None,
        })
        .filter(|node| node.starts_with("/dev/disk"))
        .collect();
    let whole = |node: &str| {
        let number = &node["/dev/disk".len()..];
        !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
    };
    let disk = nodes
        .iter()
        .copied()
        .find(|node| whole(node))
        .or(nodes.first().copied())?;
    Some(Some(PathBuf::from(disk)))
}

/// The value of `key` in a plist dictionary's entries.
fn plist_field<'a>(entries: &'a [(String, Plist)], key: &str) -> Option<&'a Plist> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, value)| value)
}

/// The string value of `key` in a plist dictionary's entries.
fn plist_string<'a>(entries: &'a [(String, Plist)], key: &str) -> Option<&'a str> {
    match plist_field(entries, key)? {
        Plist::String(s) => Some(s),
        _ => None,
    }
}

/// A value in an XML property list, as far as `hdiutil`'s output is read:
/// dictionaries, arrays and strings, and any other value as just `Other`.
#[derive(Debug)]
enum Plist {
    Dict(Vec<(String, Plist)>),
    Array(Vec<Plist>),
    String(String),
    Other,
}

/// Parses an XML property list, strictly: `None` for anything else. The
/// document is cut at every `<`, which leaves one tag and the text after it
/// in each piece — XML escapes every `<` in text.
fn parse_plist(xml: &str) -> Option<Plist> {
    let mut pieces = xml.split('<').skip(1).map(|piece| piece.split_once('>'));
    // The prolog, then `<plist version="1.0">` around a single value.
    let value = loop {
        let (tag, _) = pieces.next()??;
        if tag.starts_with('?') || tag.starts_with("!DOCTYPE") {
            continue;
        }
        if tag != "plist" && !tag.starts_with("plist ") {
            return None;
        }
        let (tag, text) = pieces.next()??;
        break plist_value(tag, text, &mut pieces)?;
    };
    let (close, _) = pieces.next()??;
    (close == "/plist" && pieces.next().is_none()).then_some(value)
}

/// The value that opens with `tag` (`text` follows it), reading on through
/// `pieces` to its end. See [`parse_plist`].
fn plist_value<'a>(
    tag: &str,
    text: &str,
    pieces: &mut impl Iterator<Item = Option<(&'a str, &'a str)>>,
) -> Option<Plist> {
    match tag {
        "dict" => {
            let mut entries = Vec::new();
            loop {
                match pieces.next()?? {
                    ("/dict", _) => return Some(Plist::Dict(entries)),
                    ("key", key) => {
                        let key = xml_unescape(key)?;
                        let ("/key", _) = pieces.next()?? else {
                            return None;
                        };
                        let (tag, text) = pieces.next()??;
                        entries.push((key, plist_value(tag, text, pieces)?));
                    }
                    _ => return None,
                }
            }
        }
        "array" => {
            let mut items = Vec::new();
            loop {
                match pieces.next()?? {
                    ("/array", _) => return Some(Plist::Array(items)),
                    (tag, text) => items.push(plist_value(tag, text, pieces)?),
                }
            }
        }
        "string" => {
            let ("/string", _) = pieces.next()?? else {
                return None;
            };
            Some(Plist::String(xml_unescape(text)?))
        }
        "dict/" => Some(Plist::Dict(Vec::new())),
        "array/" => Some(Plist::Array(Vec::new())),
        "string/" => Some(Plist::String(String::new())),
        "true/" | "false/" => Some(Plist::Other),
        "integer" | "real" | "date" | "data" => {
            let (close, _) = pieces.next()??;
            (close.strip_prefix('/') == Some(tag)).then_some(Plist::Other)
        }
        _ => None,
    }
}

/// `text` with its XML escapes decoded: the five named entities and numeric
/// character references. `None` for any other `&`.
fn xml_unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let (entity, after) = rest[at + 1..].split_once(';')?;
        out.push(match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = match entity.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => entity.strip_prefix('#')?.parse().ok()?,
                };
                char::from_u32(code)?
            }
        });
        rest = after;
    }
    out.push_str(rest);
    Some(out)
}

// ---------------------------------------------------------- the swap

/// Relaunches the app after the swap (or the old one after a failed swap).
pub const OPENER: &str = "/usr/bin/open";

/// Copies the staged app into place, keeping everything a bundle holds
/// (resource forks, extended attributes, the code signature's files).
pub const COPIER: &str = "/usr/bin/ditto";

/// Everything the swap helper needs, fixed when it is started.
#[derive(Debug, Clone, Copy)]
pub struct SwapPlan<'a> {
    /// The Vocara process to wait for.
    pub pid: u32,
    /// The verified copy to install.
    pub staged_app: &'a Path,
    /// Its private directory, removed on every outcome.
    pub staging: &'a Path,
    /// The installed bundle to replace.
    pub target: &'a Path,
    /// Appended with one line per outcome, and each failing step's own
    /// error output.
    pub log: &'a Path,
    /// Written with the reason when the swap fails, for the relaunched
    /// Vocara to report (`files::take_install_failure`).
    pub failed_marker: &'a Path,
    /// What relaunches the app: [`OPENER`], or a recorder in tests.
    pub opener: &'a Path,
    /// What copies the staged app into place: [`COPIER`], or a stand-in
    /// where there is no `ditto` (the tests off macOS).
    pub copier: &'a Path,
    /// How long to wait for `pid` to exit, in 0.1 s ticks.
    pub wait_ticks: u32,
}

/// The helper's body, handed to `bash -c` as one argument: wait for Vocara
/// to exit (or give up without touching anything), move the installed app
/// aside, copy the verified one in, and relaunch — the NEW version on
/// success, the RESTORED old one on any failure, so a failed update never
/// leaves the user with no Vocara and no word of why. Each outcome is
/// logged, and a failure's reason is also left in the marker for the
/// relaunched Vocara to show.
///
/// It is an argument, never a file: bash runs whatever is at a file's path
/// when it opens it, seconds later, after Vocara has exited, and `-c`
/// delivers the exact bytes built here, atomically at `exec`. Every tool is
/// named by absolute path and `PATH` is pinned, so nothing on the user's
/// `PATH` stands in for `mv` or `rm` in a script that deletes trees. Every
/// path is single-quoted as the bytes it is ([`quote`]: a name with quotes,
/// `$`, backticks or a non-UTF-8 byte is still exactly that name). The error
/// output of every step that can fail the swap reaches the log, so a failed
/// update's log says WHY (a full disk, a permission, a locked file): the
/// helper has no terminal, and Vocara has already quit.
///
/// A failed copy's half is removed before the backup goes back, and what
/// `rm` could not remove of it (a locked file) is moved aside first: `mv`
/// onto a directory that still exists moves INTO it, which put the backup
/// inside the half copy and reported the previous version restored. If the
/// half cannot even be moved aside, the swap says the previous version was
/// not restored, and where it is.
pub fn swap_script(plan: &SwapPlan<'_>) -> OsString {
    let mut script = b"set -u\nPATH=/usr/bin:/bin:/usr/sbin:/sbin\nexport PATH\n".to_vec();
    for (name, path) in [
        ("TARGET", plan.target),
        ("STAGED", plan.staged_app),
        ("STAGING", plan.staging),
        ("LOG", plan.log),
        ("FAILED", plan.failed_marker),
        ("OPENER", plan.opener),
        ("COPIER", plan.copier),
    ] {
        script.extend_from_slice(name.as_bytes());
        script.push(b'=');
        script.extend(quote(path.as_os_str().as_bytes()));
        script.push(b'\n');
    }
    script.extend_from_slice(
        format!(
            "log() {{ printf '%s %s\\n' \"$(/bin/date '+%Y-%m-%dT%H:%M:%S')\" \"$1\" >> \"$LOG\" 2>/dev/null; }}\n\
             fail() {{ log \"update failed: $1\"; printf '%s\\n' \"$1\" > \"$FAILED\" 2>/dev/null; }}\n\
             relaunch() {{ \"$OPENER\" \"$TARGET\" 2>>\"$LOG\" || log \"could not open $TARGET\"; }}\n\
             i=0\n\
             while kill -0 {pid} 2>/dev/null; do\n\
             \ti=$((i + 1))\n\
             \tif [ \"$i\" -ge {ticks} ]; then\n\
             \t\tfail \"Vocara did not quit, so the update was not applied\"\n\
             \t\t/bin/rm -rf \"$STAGING\"\n\
             \t\texit 1\n\
             \tfi\n\
             \t/bin/sleep 0.1\n\
             done\n\
             BACKUP=\"$TARGET.bak-$$\"\n\
             /bin/rm -rf \"$BACKUP\"\n\
             if [ -e \"$TARGET\" ] && ! /bin/mv \"$TARGET\" \"$BACKUP\" 2>>\"$LOG\"; then\n\
             \tfail \"could not move the installed app aside; nothing was changed\"\n\
             \t/bin/rm -rf \"$STAGING\"\n\
             \trelaunch\n\
             \texit 1\n\
             fi\n\
             if \"$COPIER\" \"$STAGED\" \"$TARGET\" 2>>\"$LOG\"; then\n\
             \t/bin/rm -rf \"$BACKUP\"\n\
             \tlog \"installed $TARGET\"\n\
             else\n\
             \t/bin/rm -rf \"$TARGET\" 2>>\"$LOG\"\n\
             \tif [ -e \"$TARGET\" ] || [ -L \"$TARGET\" ]; then\n\
             \t\tHALF=\"$TARGET.incomplete-$$\"\n\
             \t\t/bin/rm -rf \"$HALF\"\n\
             \t\tif ! /bin/mv \"$TARGET\" \"$HALF\" 2>>\"$LOG\"; then\n\
             \t\t\tfail \"could not install the update, nor restore the previous version (it is at $BACKUP)\"\n\
             \t\t\t/bin/rm -rf \"$STAGING\"\n\
             \t\t\texit 1\n\
             \t\tfi\n\
             \t\tlog \"left the incomplete copy at $HALF\"\n\
             \tfi\n\
             \tif [ -e \"$BACKUP\" ] && ! /bin/mv \"$BACKUP\" \"$TARGET\" 2>>\"$LOG\"; then\n\
             \t\tfail \"could not install the update, nor restore the previous version (it is at $BACKUP)\"\n\
             \t\t/bin/rm -rf \"$STAGING\"\n\
             \t\texit 1\n\
             \tfi\n\
             \tfail \"could not copy the new version into place; the previous version was restored\"\n\
             \t/bin/rm -rf \"$STAGING\"\n\
             \trelaunch\n\
             \texit 1\n\
             fi\n\
             /bin/rm -rf \"$STAGING\"\n\
             relaunch\n",
            pid = plan.pid,
            ticks = plan.wait_ticks,
        )
        .as_bytes(),
    );
    OsString::from_vec(script)
}

/// `word` single-quoted for a POSIX shell: a shell reads it back as exactly
/// `word`, whatever it holds (a `'` inside is closed, escaped and reopened).
pub fn quote(word: &[u8]) -> Vec<u8> {
    let mut quoted = Vec::with_capacity(word.len() + 2);
    quoted.push(b'\'');
    for &b in word {
        if b == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(b);
        }
    }
    quoted.push(b'\'');
    quoted
}

#[cfg(test)]
mod tests;
