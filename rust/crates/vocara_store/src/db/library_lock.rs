//! The library's single-instance guard (port of
//! `lib/data/db/library_lock.dart`).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use vocara_model::diagnostics::DiagnosticsLog;

/// What the library's single-instance guard lets this process do: only an
/// EXCLUSIVE holder may reclaim anything (boot recovery, the boot sweeps,
/// Repair library's GC). See [`LibraryLock`].
pub trait LibraryGuard: Send + Sync {
    /// Takes the library exclusively, or returns false when another Vocara
    /// process holds it (shared or exclusive). Never waits. Every true must
    /// be paired with a [`LibraryGuard::release_exclusive`].
    fn try_exclusive(&self) -> bool;

    /// Ends one [`LibraryGuard::try_exclusive`] that returned true. The last
    /// one in this process hands the library back to shared, so it stays
    /// guarded.
    fn release_exclusive(&self);
}

/// A guard that answers [`LibraryGuard::try_exclusive`] with a fixed value,
/// always, and holds no lock: for stores no other process can share (in
/// memory) and for tests that stand in for another process (Dart's
/// `LibraryGuard.fixed`).
#[derive(Debug, Clone, Copy)]
pub struct FixedGuard {
    exclusive: bool,
}

impl FixedGuard {
    /// A guard that always answers [exclusive].
    pub fn new(exclusive: bool) -> Self {
        FixedGuard { exclusive }
    }
}

impl LibraryGuard for FixedGuard {
    fn try_exclusive(&self) -> bool {
        self.exclusive
    }

    fn release_exclusive(&self) {}
}

/// The single-instance guard for one library directory: an advisory lock
/// on `<dir>/vocara.lock`.
///
/// Boot recovery and every destructive sweep assume that nothing else is
/// writing the library: a staged WAV with no row is a crashed take, a pool
/// file with no row is a crash leftover, the staging trees hold nothing in
/// flight. A second Vocara process on the same library (`open -n`, or two
/// builds that share a bundle id) breaks all of that while the other one is
/// recording or saving: a boot adopts its half-written take as
/// "(recovered)" and deletes the file, and the staging sweep trashes what it
/// has not saved yet. The migrator makes the same assumption.
///
/// So EVERY running instance holds the lock SHARED for its whole life, and
/// reclaiming needs it EXCLUSIVE, which no process can get while any other
/// one holds it at all:
///
///  * at boot, [`LibraryLock::for_library`] takes it shared and at once tries
///    to upgrade it, both while holding a separate LAUNCH GATE (byte 1 of
///    the file, held only for those two non-blocking calls): so no other
///    launch can take shared between this one's shared and its upgrade.
///    Without the gate two launches racing for a library nobody held — at
///    the same moment, or both waiting on an owner's boot that died — each
///    refused the other's upgrade, leaving the library unowned and a pending
///    upgrade refused by both. A launch that upgrades is the boot's owner
///    ([`LibraryGuard::try_exclusive`] hands it that claim): it runs recovery
///    and the sweeps, then hands the lock back to shared. One that can't
///    take shared at all (an owner is mid boot) waits — however long that
///    boot takes, telling the app once the wait runs long, but NEVER going
///    on without a hold: an instance with no hold is one the owner's sweeps
///    cannot see, and they trashed the take it was recording;
///  * Repair library tries the same upgrade and reclaims nothing without it.
///
/// An instance that started second keeps its shared hold, so it still
/// guards the library after the first one quits: a third launch then cannot
/// own it either.
///
/// The lock is a POSIX record lock (`fcntl` `F_SETLK`, the primitive Dart's
/// `RandomAccessFile.lock` uses, so a Flutter build and this one exclude
/// each other) on byte 0 of the file, the launch gate one on byte 1 (locks
/// on different bytes never conflict): a conversion between shared and
/// exclusive on the same descriptor is atomic, and a refused upgrade leaves
/// the shared hold in place. Record locks belong to the PROCESS: two
/// requests from this process never conflict, and closing ANY descriptor
/// this process holds on the file drops them all. So the file is opened once
/// per library (read and write, which a shared and an exclusive lock each
/// need), the guard is kept in a process-wide registry and never closed, a
/// later open of the same library in this process reuses it, and exclusive
/// holds inside this process are counted. The kernel releases everything
/// when the process exits, however it exits.
#[derive(Debug)]
pub struct LibraryLock {
    path: PathBuf,
    /// The open lock file, or None when it could not be opened at all.
    file: Option<File>,
    /// Every lock call runs under this mutex: two in flight could convert
    /// each other's lock (a shared retry landing on an exclusive hold
    /// downgrades it).
    state: Mutex<LockState>,
}

#[derive(Debug, Default)]
struct LockState {
    /// Whether this process holds the lock at least shared right now.
    shared: bool,
    /// try_exclusive calls in this process that returned true and are not
    /// released yet. The lock is exclusive while this is above zero.
    exclusive_holds: u32,
    /// The exclusive hold for_library took from nothing, not yet handed to a
    /// try_exclusive (the boot's): it counts in exclusive_holds already.
    boot_claim: bool,
    /// Set when the platform refused the lock for a reason that says nothing
    /// about another process (no lock file, a filesystem without record
    /// locks). The guard then stands down entirely: every try_exclusive is
    /// granted, which is how the app behaved before the guard existed,
    /// rather than never running crash recovery on such a volume again.
    unsupported: bool,
}

/// What one lock call asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Shared,
    Exclusive,
    Unlock,
}

/// Errno values a non-blocking lock attempt fails with when another process
/// holds a conflicting lock: EACCES, and EAGAIN (11 on Linux, 35 on macOS).
/// Plus EINTR, belt and braces: an interrupted call is a retry, not a
/// filesystem without locks (which turns the guard off). The same numeric
/// set as Dart's.
const HELD_ELSEWHERE: [i32; 4] = [13, 11, 35, 4];

/// The byte the library lock covers, and the launch gate's.
const LIBRARY_BYTE: i64 = 0;
const GATE_BYTE: i64 = 1;

/// This process's guards by canonical lock-file path. A slot is registered
/// before the open starts, so two opens racing on one library share one
/// guard (the second waits on the slot). Never closed (see the type doc):
/// a guard replaced because its file was deleted is kept in `retired`.
#[derive(Default)]
struct Registry {
    slots: HashMap<PathBuf, Arc<Mutex<Option<Arc<LibraryLock>>>>>,
    retired: Vec<Arc<LibraryLock>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Dart's `p.canonicalize` on POSIX: absolute and lexically normalized
/// (`.` and `..` resolved without touching the file system; symlinks are
/// left alone).
fn canonicalize(dir: &Path) -> PathBuf {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl LibraryLock {
    /// The lock file's name inside the library directory.
    pub const FILE_NAME: &'static str = "vocara.lock";

    /// How long a launch waits on an owner's boot before
    /// [`LibraryLock::for_library`] tells the app (which then shows a
    /// waiting window; the wait goes on).
    pub const DEFAULT_WAIT: Duration = Duration::from_secs(2);

    /// How often a wait for the shared hold retries.
    pub const DEFAULT_POLL: Duration = Duration::from_millis(100);

    /// This process's guard for [dir] with the default wait and poll (see
    /// [`LibraryLock::for_library_with`]).
    pub fn for_library(dir: &Path) -> Arc<LibraryLock> {
        Self::for_library_with(dir, Self::DEFAULT_WAIT, Self::DEFAULT_POLL, None)
    }

    /// This process's guard for [dir]: holding the lock exclusive when this
    /// launch won it from nothing (the boot's claim, see the type doc), else
    /// shared — taken only once an owner's boot hands it back.
    /// [on_still_waiting] is called once if that takes longer than [wait];
    /// the wait goes on. Blocks the calling thread while it waits.
    pub fn for_library_with(
        dir: &Path,
        wait: Duration,
        poll: Duration,
        on_still_waiting: Option<&mut dyn FnMut()>,
    ) -> Arc<LibraryLock> {
        let path = canonicalize(dir).join(Self::FILE_NAME);
        let slot = registry()
            .lock()
            .slots
            .entry(path.clone())
            .or_default()
            .clone();
        let mut slot = slot.lock();
        if let Some(known) = slot.as_ref() {
            // A guard for a lock file that has since been deleted (a test
            // tearing its directory down and making a new one at the same
            // path) guards nothing: open the new file.
            if known.file.is_none() || path.exists() {
                return known.clone();
            }
            registry().lock().retired.push(known.clone());
        }
        let created = Arc::new(Self::open(path, wait, poll, on_still_waiting));
        *slot = Some(created.clone());
        created
    }

    fn open(
        path: PathBuf,
        wait: Duration,
        poll: Duration,
        mut on_still_waiting: Option<&mut dyn FnMut()>,
    ) -> LibraryLock {
        // Append mode opens for reading AND writing (creating the file),
        // which the shared and the exclusive lock each require.
        let file = match OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
        {
            Ok(file) => Some(file),
            Err(e) => {
                log_unsupported(&e);
                None
            }
        };
        let lock = LibraryLock {
            path,
            state: Mutex::new(LockState {
                unsupported: file.is_none(),
                ..LockState::default()
            }),
            file,
        };
        if lock.file.is_none() {
            return lock;
        }
        let slow = Instant::now() + wait;
        let mut told = false;
        loop {
            {
                let mut s = lock.state.lock();
                // Shared, then the upgrade, both inside the launch gate (see
                // the type doc). Refused shared means an owner is mid boot:
                // let the gate go and wait for it.
                if lock.try_gate(&mut s) {
                    let decided = if lock.try_shared(&mut s) {
                        lock.try_boot_claim(&mut s);
                        true
                    } else {
                        false
                    };
                    lock.release_gate();
                    if decided {
                        drop(s);
                        return lock;
                    }
                }
                if s.unsupported {
                    drop(s);
                    return lock;
                }
            }
            if !told && Instant::now() >= slow {
                told = true;
                DiagnosticsLog::instance().log(
                    "persistence",
                    &format!(
                        "library lock still held exclusively by another process after \
                         {} ms; still waiting for it",
                        wait.as_millis()
                    ),
                );
                if let Some(callback) = on_still_waiting.as_mut() {
                    callback();
                }
            }
            std::thread::sleep(poll);
        }
    }

    /// The lock file this guard holds.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this process holds the lock (shared or exclusive) right now.
    pub fn holds_lock(&self) -> bool {
        let s = self.state.lock();
        s.shared || s.exclusive_holds > 0
    }

    /// The upgrade of a fresh shared hold that makes this launch the boot's
    /// owner, never waiting: refused while any other process holds the lock
    /// at all. Inside the launch gate only.
    fn try_boot_claim(&self, s: &mut LockState) -> bool {
        if !self.lock(s, LockMode::Exclusive, LIBRARY_BYTE) {
            return false;
        }
        s.exclusive_holds = 1;
        s.boot_claim = true;
        true
    }

    /// The launch gate, never waiting.
    fn try_gate(&self, s: &mut LockState) -> bool {
        if s.unsupported {
            return false;
        }
        self.lock(s, LockMode::Exclusive, GATE_BYTE)
    }

    /// Lets the launch gate go. Nothing to let go of is fine: the kernel
    /// drops it with the process anyway.
    fn release_gate(&self) {
        if let Some(file) = &self.file {
            let _ = record_lock(file, LockMode::Unlock, GATE_BYTE);
        }
    }

    /// One non-blocking attempt at the shared hold.
    fn try_shared(&self, s: &mut LockState) -> bool {
        if s.unsupported {
            return false;
        }
        if s.shared || s.exclusive_holds > 0 {
            return true;
        }
        if self.lock(s, LockMode::Shared, LIBRARY_BYTE) {
            s.shared = true;
        }
        s.shared
    }

    /// One non-blocking lock call on [byte]. Returns false when another
    /// process holds a conflicting lock; any other refusal marks the guard
    /// unsupported.
    fn lock(&self, s: &mut LockState, mode: LockMode, byte: i64) -> bool {
        let Some(file) = &self.file else {
            s.unsupported = true;
            return false;
        };
        match record_lock(file, mode, byte) {
            Ok(()) => true,
            Err(e) => {
                if e.raw_os_error()
                    .is_some_and(|c| HELD_ELSEWHERE.contains(&c))
                {
                    return false;
                }
                log_unsupported(&e);
                s.unsupported = true;
                false
            }
        }
    }
}

impl LibraryGuard for LibraryLock {
    fn try_exclusive(&self) -> bool {
        let mut s = self.state.lock();
        if s.unsupported {
            return true;
        }
        if s.boot_claim {
            // The hold for_library took from nothing: this is the boot's.
            s.boot_claim = false;
            return true;
        }
        if s.exclusive_holds > 0 {
            s.exclusive_holds += 1;
            return true;
        }
        // Converts this process's shared hold in place when there is one;
        // never waits. Refused while any other process holds the lock, and
        // the shared hold survives the refusal.
        if !self.lock(&mut s, LockMode::Exclusive, LIBRARY_BYTE) {
            return s.unsupported;
        }
        s.exclusive_holds = 1;
        true
    }

    fn release_exclusive(&self) {
        let mut s = self.state.lock();
        if s.unsupported || s.exclusive_holds == 0 {
            return;
        }
        s.exclusive_holds -= 1;
        if s.exclusive_holds > 0 {
            return;
        }
        // Back to shared, never to nothing: the library stays guarded. A
        // downgrade cannot conflict with anything.
        s.shared = self.lock(&mut s, LockMode::Shared, LIBRARY_BYTE);
    }
}

fn log_unsupported(e: &io::Error) {
    let code = e
        .raw_os_error()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "null".to_string());
    DiagnosticsLog::instance().log(
        "persistence",
        &format!("library lock unavailable ({code}); proceeding without the single-instance guard"),
    );
}

/// One non-blocking POSIX record lock call (`F_SETLK`) on the single byte
/// [byte] of [file], exactly as Dart's `RandomAccessFile.lock(mode, byte,
/// byte + 1)` / `unlock` issue it.
#[cfg(unix)]
fn record_lock(file: &File, mode: LockMode, byte: i64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: `flock` is a plain C struct of integers, for which all-zero is
    // a valid value; every field the call reads is set below.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = match mode {
        LockMode::Shared => libc::F_RDLCK,
        LockMode::Exclusive => libc::F_WRLCK,
        LockMode::Unlock => libc::F_UNLCK,
    } as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = byte as libc::off_t;
    fl.l_len = 1;
    // SAFETY: `file` owns an open descriptor for the whole call, and `fl` is
    // a valid, initialized `flock` that outlives it.
    let r = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &fl) };
    if r == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// No record locks off Unix: the guard stands down (see
/// `LockState::unsupported`).
#[cfg(not(unix))]
fn record_lock(_file: &File, _mode: LockMode, _byte: i64) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_guards_answer_what_they_were_given_and_hold_nothing() {
        assert!(FixedGuard::new(true).try_exclusive());
        assert!(!FixedGuard::new(false).try_exclusive());
        FixedGuard::new(true).release_exclusive();
    }

    #[test]
    fn canonicalize_is_lexical_and_absolute() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(canonicalize(Path::new("a/./b/../c")), cwd.join("a/c"));
        assert_eq!(canonicalize(Path::new("/x/y/..")), PathBuf::from("/x"));
        assert_eq!(canonicalize(Path::new("/..")), PathBuf::from("/"));
    }
}
