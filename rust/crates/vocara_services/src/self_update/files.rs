//! Where an update's files live, and how they go (Clew's
//! `src/backend/updater.rs`): `<data root>/updates`, the app's own data
//! directory, never the shared temp directory.
//!
//! `std::env::temp_dir()` is `$TMPDIR`, or with it unset the
//! world-traversable `/tmp`, where another local user can pre-plant a
//! predictable path — as a symlink the download is then written through, or
//! as the staging directory they own and swap the verified bundle out of
//! during the seconds the helper waits for Vocara to exit, installing a
//! bundle the signature check never saw. So both halves of an update write
//! only into directories of their own here ([`create_private_dir`]:
//! exclusive, 0700), and with no data directory an update fails rather than
//! falling back to the temp directory.
//!
//! Every attempt has its own directory, so none can see, truncate or install
//! another's bytes; each route that ends one takes its directory with it
//! ([`discard_download`]), and what a Vocara that stopped existing
//! mid-download left behind is swept at the next launch
//! ([`sweep_stale_downloads`]).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The directory under the data root the updates live in.
pub const UPDATES_DIR: &str = "updates";

/// Names the private per-attempt directory a download streams into. Also
/// the marker [`discard_download`] checks before deleting a tree.
pub const DOWNLOAD_PREFIX: &str = "dl";

/// Names the private per-install directory the verified bundle is staged in
/// (the installer's half).
pub const STAGING_PREFIX: &str = "stage";

/// The file a failed swap leaves in the updates directory for the relaunched
/// Vocara to report (see `install::swap_script`).
pub const INSTALL_FAILED_MARKER: &str = "last-update-failed";

/// The swap helper's own log, next to that marker.
pub const INSTALL_LOG: &str = "update.log";

/// How long a download directory must have sat untouched before
/// [`sweep_stale_downloads`] removes it: well beyond any real download, and
/// beyond any plausible stall in one, so one another live Vocara is
/// streaming into can never be old enough to qualify.
pub const STALE_DOWNLOAD_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Most entries one sweep looks at, so a directory that somehow filled up
/// cannot turn a launch into a long walk.
const MAX_SWEPT_ENTRIES: usize = 512;

/// A private directory under `parent`, named `<prefix>-<pid>-<n>` and created
/// exclusively. Both halves of an update use it: the download streams into
/// one and the installer stages the verified bundle into another.
///
/// `create_dir`, not `create_dir_all`: an existing entry is a failure stepped
/// over rather than a directory silently adopted, so Vocara never writes an
/// update into something another process made. The counter keeps two
/// attempts of one process apart. Mode 0700 on the leaf, so what lands inside
/// is unreachable to other accounts whatever the modes above it.
pub fn create_private_dir(parent: &Path, prefix: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(parent).map_err(|e| format!("could not prepare the update: {e}"))?;
    let pid = std::process::id();
    for _ in 0..64 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!("{prefix}-{pid}-{n}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not prepare the update: {e}")),
        }
    }
    Err("could not create a private directory for the update".to_string())
}

/// Creates the directory of ONE download attempt under `updates` and returns
/// where its image goes (`Vocara-<version>.dmg` in it). Per attempt, not per
/// version: two attempts at the same release (one cancelled and still
/// finishing, the next under way) must not share a file, or the second's
/// `truncate` would cut into the first's bytes under an installer that could
/// pick them up at any moment.
pub fn create_download_dest(updates: &Path, version: &str) -> Result<PathBuf, String> {
    // The version names a file: only its plain characters (it is a
    // dotted-numeric version already, which this keeps whole).
    let version: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .collect();
    Ok(create_private_dir(updates, DOWNLOAD_PREFIX)?.join(format!("Vocara-{version}.dmg")))
}

/// Drops a download's private directory once its bytes are of no further
/// use. Best effort, and only for a directory [`create_download_dest`] made:
/// this deletes a tree.
pub fn discard_download(dmg: &Path) {
    let Some(dir) = dmg.parent() else { return };
    let ours = dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_download_dir_name);
    if ours {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// `dl-<pid>-<n>`, exactly as [`create_private_dir`] spells it. Matched in
/// full rather than by the `dl-` prefix alone, because this is the gate in
/// front of a recursive delete, and both callers — the discard and the sweep
/// — share it, so the two can never disagree about which trees are Vocara's.
pub fn is_download_dir_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(DOWNLOAD_PREFIX)
        .and_then(|r| r.strip_prefix('-'))
    else {
        return false;
    };
    let mut parts = rest.split('-');
    let (Some(pid), Some(n), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(pid) && digits(n)
}

/// Removes download directories left behind by Vocara runs that no longer
/// exist: a crash, a log-out, a quit mid-download — everything the discard
/// on the routes that end a download cannot reach. Run once per launch.
///
/// Deliberately narrow, because it deletes trees: only entries directly
/// under `updates`; only real directories (not a symlink planted under one
/// of these names); only names of the exact `dl-<pid>-<n>` shape; and only
/// ones nothing has touched for [`STALE_DOWNLOAD_AGE`], which keeps a
/// download another live Vocara is streaming into safe.
///
/// `stage-<pid>-<n>` is deliberately NOT swept, however old it looks: the
/// swap helper works out of it after its Vocara is gone, and it prunes its
/// own directory. Blocking: off the UI thread.
pub fn sweep_stale_downloads(updates: &Path) {
    let Ok(entries) = std::fs::read_dir(updates) else {
        return;
    };
    for entry in entries.take(MAX_SWEPT_ENTRIES).flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(is_download_dir_name) {
            continue;
        }
        // `file_type` does not follow symlinks: a link named `dl-1-0` is not
        // a directory here, and stays where it is.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry.path();
        if last_touched(&path).is_none_or(|age| age < STALE_DOWNLOAD_AGE) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// How long ago anything in `dir` — the directory or a file directly inside
/// it — was last written; `None` when that cannot be told (read as "leave it
/// alone"). The entries have to be looked at: a directory's own mtime stops
/// moving once the file inside it exists, so a download under way would look
/// untouched the whole way through.
fn last_touched(dir: &Path) -> Option<Duration> {
    let mut newest = std::fs::metadata(dir).ok()?.modified().ok()?;
    for entry in std::fs::read_dir(dir)
        .ok()?
        .take(MAX_SWEPT_ENTRIES)
        .flatten()
    {
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            newest = newest.max(modified);
        }
    }
    SystemTime::now().duration_since(newest).ok()
}

/// The reason the previous update's swap failed, if it did — read once and
/// removed, so it is reported exactly once. Capped, and opened without
/// following a symlink.
pub fn take_install_failure(updates: &Path) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = updates.join(INSTALL_FAILED_MARKER);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .ok()?;
    let mut text = String::new();
    let read = file.take(4096).read_to_string(&mut text);
    let _ = std::fs::remove_file(&path);
    read.ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Every update directory — a download's and an install's alike — is
    /// its own, private to this user, and an entry that already exists is
    /// stepped over rather than adopted.
    #[test]
    fn each_update_directory_is_fresh_private_and_never_adopted() {
        let parent = tempfile::tempdir().unwrap();
        let first = create_private_dir(parent.path(), STAGING_PREFIX).unwrap();
        assert!(first.starts_with(parent.path()));
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // Pre-plant the next name in the sequence, with something in it.
        let name = first.file_name().unwrap().to_str().unwrap();
        let n: u64 = name.rsplit('-').next().unwrap().parse().unwrap();
        let planted = parent
            .path()
            .join(format!("stage-{}-{}", std::process::id(), n + 1));
        std::fs::create_dir(&planted).unwrap();
        std::fs::write(planted.join("marker"), "x").unwrap();

        let second = create_private_dir(parent.path(), STAGING_PREFIX).unwrap();
        assert_ne!(second, first, "two attempts must not share a directory");
        assert_ne!(second, planted, "an existing directory is not adopted");
        assert_eq!(std::fs::read_dir(&second).unwrap().count(), 0);
    }

    #[test]
    fn two_download_attempts_never_share_a_file() {
        let updates = tempfile::tempdir().unwrap();
        let a = create_download_dest(updates.path(), "9.9.9").unwrap();
        let b = create_download_dest(updates.path(), "9.9.9").unwrap();
        assert_ne!(a.parent(), b.parent());
        assert!(a.ends_with("Vocara-9.9.9.dmg"));
        std::fs::write(&a, b"half an image").unwrap();
        assert!(!b.exists());
        // A version is a file name's part, not a path.
        let odd = create_download_dest(updates.path(), "../../9.9.9/x").unwrap();
        assert!(odd.ends_with("Vocara-....9.9.9x.dmg"), "{odd:?}");
        assert!(odd.starts_with(updates.path()));
    }

    /// The download's directory goes when its bytes stop being useful — but
    /// only a directory the download made: this deletes a TREE.
    #[test]
    fn only_a_download_directory_is_ever_discarded() {
        let root = tempfile::tempdir().unwrap();
        let dmg = create_download_dest(root.path(), "9.9.9").unwrap();
        std::fs::write(&dmg, b"image").unwrap();
        discard_download(&dmg);
        assert!(!dmg.parent().unwrap().exists());

        let elsewhere = root.path().join("Documents");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let stray = elsewhere.join("Vocara-9.9.9.dmg");
        std::fs::write(&stray, b"image").unwrap();
        discard_download(&stray);
        assert!(stray.exists());
        for name in ["dl-1-2", "dl-12-0"] {
            assert!(is_download_dir_name(name), "{name}");
        }
        for name in [
            "dl-",
            "dl-1",
            "dl-a-1",
            "dl-1-2-3",
            "stage-1-2",
            "xdl-1-2",
            "dl-1-",
        ] {
            assert!(!is_download_dir_name(name), "{name}");
        }
    }

    fn age(path: &Path, by: Duration) {
        let file = std::fs::File::open(path).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    /// A Vocara that died mid-download left its image; the next launch's
    /// sweep removes it — and nothing a live Vocara is still writing, no
    /// staging directory, nothing not named as Vocara's.
    #[test]
    fn only_a_download_no_live_vocara_is_writing_is_swept() {
        let updates = tempfile::tempdir().unwrap();
        let updates = updates.path();
        let old = STALE_DOWNLOAD_AGE * 2;

        let stale = create_private_dir(updates, DOWNLOAD_PREFIX).unwrap();
        let stale_dmg = stale.join("Vocara-9.9.9.dmg");
        std::fs::write(&stale_dmg, b"image").unwrap();
        age(&stale_dmg, old);
        age(&stale, old);

        let live = create_private_dir(updates, DOWNLOAD_PREFIX).unwrap();
        std::fs::write(live.join("Vocara-9.9.9.dmg"), b"image").unwrap();
        age(&live, old);

        let staging = create_private_dir(updates, STAGING_PREFIX).unwrap();
        std::fs::write(staging.join("marker"), b"x").unwrap();
        age(&staging, old);
        let odd = updates.join("dl-notapid");
        std::fs::create_dir(&odd).unwrap();
        age(&odd, old);
        let link = updates.join("dl-1-999999");
        std::os::unix::fs::symlink(&staging, &link).unwrap();

        sweep_stale_downloads(updates);

        assert!(!stale.exists(), "an abandoned download must not survive");
        assert!(live.exists(), "one still being written stays");
        assert!(staging.exists(), "the swap helper works out of staging");
        assert!(odd.exists() && staging.join("marker").exists());
        assert!(std::fs::symlink_metadata(&link).is_ok(), "a link stays");
    }

    /// A failed swap's reason is reported by the relaunched Vocara once.
    #[test]
    fn a_failed_install_is_reported_once_after_relaunch() {
        let updates = tempfile::tempdir().unwrap();
        assert_eq!(take_install_failure(updates.path()), None);
        std::fs::write(
            updates.path().join(INSTALL_FAILED_MARKER),
            "could not copy the new version into place\n",
        )
        .unwrap();
        assert_eq!(
            take_install_failure(updates.path()).as_deref(),
            Some("could not copy the new version into place")
        );
        assert_eq!(take_install_failure(updates.path()), None);
        // A marker that is a link is not followed.
        let elsewhere = updates.path().join("elsewhere");
        std::fs::write(&elsewhere, "planted").unwrap();
        std::os::unix::fs::symlink(&elsewhere, updates.path().join(INSTALL_FAILED_MARKER)).unwrap();
        assert_eq!(take_install_failure(updates.path()), None);
    }
}
