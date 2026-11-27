//! Where an environment keeps its data: the application support directory
//! of its bundle id, `~/Library/Application Support/<bundle id>` on macOS
//! (`vocara_services::local_storage_service::application_support_dir`, and
//! the diagnostics log's own resolver).
//!
//! One exception, for the whole process: a library that a sandboxed release
//! (every release up to 0.3.0) left in its App Sandbox container, and that
//! this launch could not move out of it
//! (`vocara_services::sandbox_migration`). The launch then roots the app at
//! the container instead — the library, the settings, the asset folders and
//! the diagnostics log alike — so the user opens their library where it is,
//! never an empty one beside it. Every resolver of the directory asks
//! [`rooted_at`] first.

use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

/// The bundle ids rooted somewhere other than their default directory, and
/// where: one entry per environment a process settles (the app settles one).
static ROOTS: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

/// Roots every store of [bundle_id] at [dir] for the rest of this process.
/// Called once at startup, before anything resolves the directory; a later
/// call for the same bundle id replaces the first.
pub fn root_at(bundle_id: &str, dir: PathBuf) {
    let mut roots = ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    roots.retain(|(id, _)| id != bundle_id);
    roots.push((bundle_id.to_string(), dir));
}

/// The directory [`root_at`] rooted [bundle_id] at: `None` when nothing did,
/// so the default directory applies.
///
/// An error when that directory is gone: it was moved after this launch
/// settled on it (a second Vocara, starting at the same moment, carried the
/// library out of the container), and creating it again would open an empty
/// library there while the user's is elsewhere. Quitting and opening Vocara
/// again finds it. Any other failure to look hands the directory back, so
/// the open that follows fails with the system's own reason (a refusal to
/// read the container, say) instead of this one.
pub fn rooted_at(bundle_id: &str) -> Option<io::Result<PathBuf>> {
    let dir = {
        let roots = ROOTS.lock().unwrap_or_else(|e| e.into_inner());
        roots
            .iter()
            .find(|(id, _)| id == bundle_id)
            .map(|(_, dir)| dir.clone())?
    };
    match std::fs::metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the library moved while Vocara was starting; quit Vocara and open it again",
        ))),
        _ => Some(Ok(dir)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_id_nothing_rooted_takes_its_default_directory() {
        assert!(rooted_at("test.app-support.never-rooted").is_none());
    }

    #[test]
    fn a_rooted_bundle_id_resolves_to_its_root_while_that_exists() {
        let tmp = tempfile::tempdir().unwrap();
        root_at("test.app-support.rooted", tmp.path().to_path_buf());
        assert_eq!(
            rooted_at("test.app-support.rooted").unwrap().unwrap(),
            tmp.path()
        );
        // The root is the bundle id's alone.
        assert!(rooted_at("test.app-support.other").is_none());
    }

    #[test]
    fn a_root_that_moved_away_is_an_error_never_a_directory_to_create_again() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("moved");
        root_at("test.app-support.moved", gone.clone());
        let err = rooted_at("test.app-support.moved").unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!gone.exists());
    }

    #[test]
    fn rooting_again_replaces_the_first_root() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        root_at("test.app-support.again", first.path().to_path_buf());
        root_at("test.app-support.again", second.path().to_path_buf());
        assert_eq!(
            rooted_at("test.app-support.again").unwrap().unwrap(),
            second.path()
        );
    }
}
