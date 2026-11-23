//! File-durability primitives (port of `lib/data/db/durable_fs.dart`).
//!
//! The migration atomic-swap and the atomic asset import depend on TRUE
//! durability: a referenced file's bytes AND the directory entry pointing at
//! it must be on permanent storage before the DB row that references it
//! commits, otherwise a power loss can leave a committed row pointing at a
//! file that was never actually flushed (a dangling reference / lost take).
//!
//! On macOS a plain `fsync` does NOT flush the drive's write cache — only
//! `fcntl(fd, F_FULLFSYNC)` does — and a directory entry is only durable
//! after the directory fd itself is fsync'd. Elsewhere these degrade to a
//! plain `fsync`.
//!
//! The calls block (a F_FULLFSYNC can take whole seconds for a large take on
//! a slow disk): the store is synchronous, and the app runs it off the UI
//! thread (Dart ran the native flush through `Isolate.run` for the same
//! reason).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// The durability operations the migrator and the asset store rely on.
/// Every method has the real implementation as its default; tests override
/// one to record the order of calls (Dart's `extends DurableFs`).
pub trait DurableFs: Send + Sync {
    /// Flush a file's data all the way to permanent storage (drive cache
    /// included on macOS via `F_FULLFSYNC`). See [`fsync_file`].
    fn fsync_file(&self, path: &Path) -> io::Result<()> {
        fsync_file(path)
    }

    /// Flush a directory entry so a rename into it survives power loss. See
    /// [`fsync_dir`].
    fn fsync_dir(&self, path: &Path) {
        fsync_dir(path)
    }

    /// Atomically replace [dest] with [src] (same filesystem). Rename is
    /// atomic on POSIX; the caller is responsible for both paths being on one
    /// volume (a cross-volume rename fails).
    fn atomic_rename(&self, src: &Path, dest: &Path) -> io::Result<()> {
        std::fs::rename(src, dest)
    }
}

/// The real file system (Dart's `const DurableFs()`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDurableFs;

impl DurableFs for SystemDurableFs {}

/// Flush a file's data all the way to permanent storage: `F_FULLFSYNC` on
/// macOS (falling back to `fsync` when the filesystem refuses it), `fsync`
/// elsewhere.
///
/// If the native flush cannot run at all (the file cannot be opened for
/// writing), falls back like Dart did: open the file for appending —
/// creating it when it does not exist — and `fsync` that. Only that fallback
/// can fail.
pub fn fsync_file(path: &Path) -> io::Result<()> {
    // O_WRONLY: F_FULLFSYNC wants a writable fd for a regular file.
    if cfg!(target_os = "macos") {
        if let Ok(file) = OpenOptions::new().write(true).open(path) {
            if native_flush(&file, true) {
                return Ok(());
            }
        }
    }
    // Fallback (and the path off macOS): an fsync of the file. Not a
    // drive-cache flush on macOS.
    let file = OpenOptions::new().append(true).create(true).open(path)?;
    plain_fsync(&file)
}

/// Flush a directory entry so a rename into it survives power loss. A
/// directory fd can only be opened read-only; its entries get the same
/// `F_FULLFSYNC` as [`fsync_file`] on macOS, because a plain `fsync` there
/// only hands them to the drive, whose write cache can still lose them —
/// and the entry must be on permanent storage before the row referencing it
/// commits. A filesystem that refuses F_FULLFSYNC falls back to a plain
/// `fsync`, and so does every other Unix. Best-effort: never fails (and a
/// no-op where directories cannot be opened, as on Windows).
pub fn fsync_dir(path: &Path) {
    if cfg!(unix) {
        if let Ok(dir) = File::open(path) {
            native_flush(&dir, cfg!(target_os = "macos"));
        }
    }
}

/// Flushes [file] (F_FULLFSYNC when [full_fsync], else a plain fsync).
/// Returns true only if the flush succeeded; any failure returns false so
/// the caller can fall back — durability is best-effort, never a crash.
fn native_flush(file: &File, full_fsync: bool) -> bool {
    #[cfg(target_os = "macos")]
    if full_fsync {
        use std::os::fd::AsRawFd;
        // SAFETY: `file` owns an open descriptor for the whole call, and
        // F_FULLFSYNC takes no argument beyond the descriptor.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
            return true;
        }
        // F_FULLFSYNC refused (unsupported by the filesystem for this file
        // or directory): fall back to fsync (still flushes the file/dir
        // metadata through the OS).
    }
    #[cfg(not(target_os = "macos"))]
    let _ = full_fsync;
    plain_fsync(file).is_ok()
}

/// A plain `fsync(2)` (Dart's `RandomAccessFile.flush`), which on macOS,
/// unlike Rust's `File::sync_all`, is NOT an F_FULLFSYNC.
pub(crate) fn plain_fsync(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: `file` owns an open descriptor for the whole call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsync_file_flushes_an_existing_file_without_altering_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..4096).map(|i| (i & 0xFF) as u8).collect();
        let f = dir.path().join("take.bin");
        std::fs::write(&f, &bytes).unwrap();

        SystemDurableFs.fsync_file(&f).unwrap();

        assert_eq!(std::fs::read(&f).unwrap(), bytes);
    }

    #[test]
    fn fsync_dir_completes_on_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("entry.bin"), [1, 2, 3]).unwrap();
        SystemDurableFs.fsync_dir(dir.path());
        assert!(dir.path().join("entry.bin").exists());
    }

    #[test]
    fn concurrent_fsyncs_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..8u8)
            .map(|i| {
                let p = dir.path().join(format!("f{i}.bin"));
                std::fs::write(&p, vec![i; 1024]).unwrap();
                p
            })
            .collect();
        std::thread::scope(|s| {
            for p in &paths {
                s.spawn(move || SystemDurableFs.fsync_file(p).unwrap());
            }
            s.spawn(|| SystemDurableFs.fsync_dir(dir.path()));
        });
        for (i, p) in paths.iter().enumerate() {
            assert_eq!(std::fs::read(p).unwrap()[0], i as u8);
        }
    }

    #[test]
    fn fsync_dir_never_fails_and_atomic_rename_replaces() {
        let dir = tempfile::tempdir().unwrap();
        SystemDurableFs.fsync_dir(&dir.path().join("missing"));
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"new").unwrap();
        std::fs::write(&b, b"old").unwrap();
        SystemDurableFs.atomic_rename(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"new");
    }
}
