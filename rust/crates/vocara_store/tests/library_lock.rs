//! The single-instance guard against a genuinely separate process (port of
//! the library-lock parts of `test/persistence_single_instance_test.dart`).
//!
//! POSIX record locks never conflict within one process, so the "other
//! process" is this test binary re-invoked to run [`lock_helper`] alone,
//! told what to do through the environment (Dart used a python3 helper with
//! the same `fcntl` calls). Every helper call is bounded, so no test can
//! hang on a lock.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use vocara_store::db::library_lock::{LibraryGuard, LibraryLock};

/// The ceiling on every wait for a helper process.
const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

const MARK: &str = "VOCARA_LOCK_HELPER ";
const ENV_MODE: &str = "VOCARA_LOCK_HELPER_MODE";
const ENV_PATH: &str = "VOCARA_LOCK_HELPER_PATH";
const ENV_DOWNGRADE: &str = "VOCARA_LOCK_HELPER_DOWNGRADE_MS";

fn say(word: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "\n{MARK}{word}");
    let _ = out.flush();
}

/// One `fcntl` record lock on [byte], as Python's `fcntl.lockf(fd, mode, 1,
/// byte)` issues it (`F_SETLK`, or `F_SETLKW` when [blocking]).
fn helper_lock(fd: i32, kind: i32, byte: i64, blocking: bool) -> Result<(), i32> {
    // SAFETY: a zeroed flock is valid; the fields the call reads are set.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = kind as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = byte as libc::off_t;
    fl.l_len = 1;
    let cmd = if blocking {
        libc::F_SETLKW
    } else {
        libc::F_SETLK
    };
    // SAFETY: fd is open for the helper's lifetime and fl outlives the call.
    if unsafe { libc::fcntl(fd, cmd, &fl) } == -1 {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
    } else {
        Ok(())
    }
}

/// The helper process. A no-op in a normal test run; when re-invoked by
/// [`OtherProcess`] it holds (or probes) the lock as told, and exits on its
/// own after a minute, so a crashed test can never leave it behind.
#[test]
fn lock_helper() {
    let Ok(mode) = std::env::var(ENV_MODE) else {
        return;
    };
    let path = std::env::var(ENV_PATH).expect("helper path");
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(60));
        std::process::exit(2);
    });
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .expect("helper opens the lock file");
    let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
    match mode.as_str() {
        "probe-sh" | "probe-ex" => {
            let kind = if mode == "probe-sh" {
                libc::F_RDLCK
            } else {
                libc::F_WRLCK
            };
            say(if helper_lock(fd, kind, 0, false).is_ok() {
                "got"
            } else {
                "held"
            });
            std::process::exit(0);
        }
        _ => {
            let (kind, byte) = match mode.as_str() {
                "sh" => (libc::F_RDLCK, 0),
                "ex" => (libc::F_WRLCK, 0),
                _ => (libc::F_WRLCK, 1), // "gate"
            };
            if let Err(errno) = helper_lock(fd, kind, byte, false) {
                say(&format!("refused {errno}"));
                std::process::exit(1);
            }
            say("locked");
            if let Ok(ms) = std::env::var(ENV_DOWNGRADE) {
                thread::sleep(Duration::from_millis(ms.parse().unwrap_or(0)));
                let _ = helper_lock(fd, libc::F_RDLCK, 0, true);
            }
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink);
            std::process::exit(0);
        }
    }
}

/// Starts the helper in [mode] on [path] and returns it with the first
/// word it said (or "timed out"). Its stdout stays drained after that word:
/// closing the pipe would kill the helper the next time it writes.
fn spawn_helper(path: &Path, mode: &str, downgrade_after: Option<Duration>) -> (Child, String) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "lock_helper", "--nocapture", "--test-threads=1"])
        .env(ENV_MODE, mode)
        .env(ENV_PATH, path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(after) = downgrade_after {
        command.env(ENV_DOWNGRADE, after.as_millis().to_string());
    }
    let mut child = command.spawn().expect("spawn the helper process");
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !sent {
                if let Some(i) = line.find(MARK) {
                    let _ = tx.send(line[i + MARK.len()..].trim().to_string());
                    sent = true;
                }
            }
        }
    });
    let first = rx
        .recv_timeout(HELPER_TIMEOUT)
        .unwrap_or_else(|_| "timed out".to_string());
    (child, first)
}

fn wait_bounded(child: &mut Child) {
    let deadline = Instant::now() + HELPER_TIMEOUT;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Another Vocara process, as far as the lock can tell: a helper holding an
/// fcntl lock on the lock file's library byte (or the launch gate's) until
/// [`OtherProcess::release`]d.
struct OtherProcess {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl OtherProcess {
    /// Holds the lock on [path] shared (a running instance) or exclusive (an
    /// owner mid-boot). With [downgrade_after], an exclusive hold turns
    /// shared after that long, like an owner finishing its boot. With
    /// [gate], it holds the launch gate instead.
    fn hold(path: &Path, shared: bool, downgrade_after: Option<Duration>, gate: bool) -> Self {
        let mode = if gate {
            "gate"
        } else if shared {
            "sh"
        } else {
            "ex"
        };
        let (mut child, first) = spawn_helper(path, mode, downgrade_after);
        let stdin = child.stdin.take();
        let mut other = OtherProcess { child, stdin };
        if first != "locked" {
            other.release();
            panic!("the helper process could not take the lock: {first}");
        }
        other
    }

    /// Ends the helper and waits (bounded) for it to exit. Idempotent.
    fn release(&mut self) {
        self.stdin.take();
        wait_bounded(&mut self.child);
    }

    /// Whether a freshly launched process could take the lock on [path] in
    /// that mode right now (a non-blocking attempt; the helper exits at
    /// once).
    fn could_take(path: &Path, shared: bool) -> bool {
        let (mut child, out) =
            spawn_helper(path, if shared { "probe-sh" } else { "probe-ex" }, None);
        drop(child.stdin.take());
        wait_bounded(&mut child);
        match out.as_str() {
            "got" => true,
            "held" => false,
            other => panic!("the lock probe did not answer: {other}"),
        }
    }
}

impl Drop for OtherProcess {
    fn drop(&mut self) {
        self.release();
    }
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(LibraryLock::FILE_NAME)
}

#[test]
fn a_launch_during_an_owners_boot_waits_for_it_then_opens_shared_as_a_non_owner() {
    let root = tempfile::tempdir().unwrap();
    let _owner = OtherProcess::hold(
        &lock_path(root.path()),
        false,
        Some(Duration::from_millis(400)),
        false,
    );

    let started = Instant::now();
    let lock = LibraryLock::for_library_with(
        root.path(),
        Duration::from_secs(15),
        Duration::from_millis(20),
        None,
    );
    assert!(lock.holds_lock(), "shared, once the boot was done");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(!lock.try_exclusive(), "the owner runs");
    assert!(lock.holds_lock(), "a refused upgrade keeps shared");
}

#[test]
fn a_launch_keeps_waiting_out_an_owners_long_boot_tells_the_app_once_and_opens_holding_the_lock() {
    // It used to give up after a deadline and run with no hold at all: the
    // owner's boot sweeps, cleared before it started, then trashed the take
    // it was recording.
    let root = tempfile::tempdir().unwrap();
    let path = lock_path(root.path());
    let mut owner = OtherProcess::hold(&path, false, None, false);

    let told = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    {
        let told = told.clone();
        let dir = root.path().to_path_buf();
        thread::spawn(move || {
            let mut count = || {
                told.fetch_add(1, Ordering::SeqCst);
            };
            let lock = LibraryLock::for_library_with(
                &dir,
                Duration::from_millis(200),
                Duration::from_millis(20),
                Some(&mut count),
            );
            let _ = tx.send(lock);
        });
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(800)).is_err(),
        "went on without a hold"
    );
    assert_eq!(told.load(Ordering::SeqCst), 1, "the app is told once");

    owner.release();
    let lock = rx.recv_timeout(HELPER_TIMEOUT).expect("the launch opens");
    assert!(lock.holds_lock());
    assert_eq!(told.load(Ordering::SeqCst), 1);
    // The owner went from exclusive straight to nothing (its boot died): the
    // waiter claimed the library outright, not shared-then-upgrade, which
    // two waiters taking shared at that moment refused each other.
    assert!(
        !OtherProcess::could_take(&path, true),
        "the waiter took shared, leaving the library to be raced for"
    );
    assert!(lock.try_exclusive(), "alone now");
    lock.release_exclusive();
    assert!(!OtherProcess::could_take(&path, false));
}

#[test]
fn a_launch_decides_only_inside_the_launch_gate() {
    // Its shared hold and its upgrade happen while it holds the gate, so no
    // other launch can take shared in between: two launches racing for a
    // library nobody held refused each other's upgrade, leaving it unowned.
    let root = tempfile::tempdir().unwrap();
    let mut other = OtherProcess::hold(&lock_path(root.path()), false, None, true);
    let (tx, rx) = mpsc::channel();
    {
        let dir = root.path().to_path_buf();
        thread::spawn(move || {
            let lock = LibraryLock::for_library_with(
                &dir,
                LibraryLock::DEFAULT_WAIT,
                Duration::from_millis(20),
                None,
            );
            let _ = tx.send(lock);
        });
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "decided while another launch held the gate"
    );
    other.release();
    let lock = rx.recv_timeout(HELPER_TIMEOUT).expect("the launch opens");
    assert!(lock.try_exclusive(), "alone: the owner");
    lock.release_exclusive();
}

#[test]
fn a_launch_on_an_idle_library_takes_it_exclusive_from_nothing() {
    // Shared first, then an upgrade, let two launches at the same moment
    // refuse each other: neither owned the library.
    let root = tempfile::tempdir().unwrap();
    let path = lock_path(root.path());
    let lock = LibraryLock::for_library(root.path());
    assert!(
        !OtherProcess::could_take(&path, true),
        "a second launch must not get in before this boot is done"
    );
    assert!(lock.try_exclusive(), "the boot's claim");
    lock.release_exclusive();
    assert!(
        OtherProcess::could_take(&path, true),
        "handed back to shared once the boot is done"
    );
    assert!(!OtherProcess::could_take(&path, false));
}

#[test]
fn an_owner_hands_the_lock_back_to_shared_once_its_boot_is_done() {
    let root = tempfile::tempdir().unwrap();
    let path = lock_path(root.path());
    let lock = LibraryLock::for_library(root.path());
    // The boot: the owner's claim, then its hand-back.
    assert!(lock.try_exclusive());
    lock.release_exclusive();
    assert!(lock.holds_lock());
    // Another launch can now open alongside it...
    assert!(OtherProcess::could_take(&path, true));
    // ...but never own the library while this instance runs.
    assert!(!OtherProcess::could_take(&path, false));
}

#[test]
fn with_another_running_instance_holding_the_lock_shared_a_launch_is_no_owner() {
    let root = tempfile::tempdir().unwrap();
    let _other = OtherProcess::hold(&lock_path(root.path()), true, None, false);
    let lock = LibraryLock::for_library(root.path());
    assert!(lock.holds_lock(), "shared alongside the other instance");
    assert!(!lock.try_exclusive(), "the boot may reclaim nothing");
    assert!(lock.holds_lock());
}

#[test]
fn an_instance_that_started_second_keeps_guarding_the_library_after_the_first_one_quits() {
    let root = tempfile::tempdir().unwrap();
    let path = lock_path(root.path());
    let mut first = OtherProcess::hold(&path, true, None, false);

    let second = LibraryLock::for_library(root.path());
    assert!(!second.try_exclusive(), "not the owner");

    first.release();
    assert!(
        !OtherProcess::could_take(&path, false),
        "this instance is still running, but a relaunch could take the library \
         exclusively and run its boot recovery and sweeps under it"
    );
    // A relaunch still opens alongside it, as a non-owner.
    assert!(OtherProcess::could_take(&path, true));
}

#[test]
fn repair_in_the_boot_owner_reclaims_nothing_once_another_instance_has_opened_the_library() {
    let root = tempfile::tempdir().unwrap();
    let path = lock_path(root.path());
    let owner = LibraryLock::for_library(root.path());
    assert!(owner.try_exclusive(), "the boot's claim");
    owner.release_exclusive();

    // A second instance opens the library.
    let mut other = OtherProcess::hold(&path, true, None, false);
    assert!(!owner.try_exclusive(), "repair must reclaim nothing");
    assert!(owner.holds_lock());

    other.release();
    assert!(owner.try_exclusive(), "alone again: repair may reclaim");
    // Nested holds in this process are counted.
    assert!(owner.try_exclusive());
    owner.release_exclusive();
    assert!(!OtherProcess::could_take(&path, true), "still exclusive");
    owner.release_exclusive();
    assert!(OtherProcess::could_take(&path, true), "back to shared");
}

#[test]
fn one_process_opening_its_own_library_twice_owns_it_both_times() {
    let root = tempfile::tempdir().unwrap();
    let first = LibraryLock::for_library(root.path());
    assert!(first.try_exclusive());
    first.release_exclusive();

    // Both stores live in this process, which still has the library to
    // itself: one guard, and its Repair library may reclaim.
    let second = LibraryLock::for_library(root.path());
    assert!(Arc::ptr_eq(&first, &second));
    assert!(second.try_exclusive());
    second.release_exclusive();
}

#[test]
fn concurrent_opens_of_one_library_share_one_guard() {
    let root = tempfile::tempdir().unwrap();
    let locks: Vec<Arc<LibraryLock>> = thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| s.spawn(|| LibraryLock::for_library(root.path())))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for lock in &locks[1..] {
        assert!(Arc::ptr_eq(&locks[0], lock));
    }
    assert!(locks[0].try_exclusive(), "the single boot claim");
    locks[0].release_exclusive();
}

#[test]
fn a_deleted_lock_file_gets_a_fresh_guard() {
    let root = tempfile::tempdir().unwrap();
    let first = LibraryLock::for_library(root.path());
    std::fs::remove_file(lock_path(root.path())).unwrap();
    let second = LibraryLock::for_library(root.path());
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(lock_path(root.path()).exists());
    assert!(second.try_exclusive(), "a new file, claimed from nothing");
}

#[test]
fn a_library_whose_lock_file_cannot_be_opened_stands_the_guard_down() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("no-such-directory");
    let lock = LibraryLock::for_library(&missing);
    assert!(!lock.holds_lock());
    // Every claim is granted: how the app behaved before the guard existed,
    // rather than never running crash recovery on such a volume again.
    assert!(lock.try_exclusive());
    assert!(lock.try_exclusive());
    lock.release_exclusive();
}
