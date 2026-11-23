//! Port of the Persistence cases of
//! `client/test/persistence_single_instance_test.dart`.
//!
//! The single-instance guard: every running Vocara holds
//! `<support>/vocara.lock` SHARED for its whole life, and only an instance
//! that can upgrade it to EXCLUSIVE (no other process has the library open)
//! runs boot recovery, the boot sweeps, or Repair library's reclaim. In a
//! library another process has open, a staged WAV with no row is that
//! process's take still recording, and a pool file with no row is its import
//! still landing.
//!
//! POSIX record locks never conflict within one process, so the "other
//! process" is this test binary re-invoked to run [`lock_helper`] alone (Dart
//! used a python3 helper with the same `fcntl` calls). Every helper call is
//! bounded, so no test can hang on a lock. The cases that drive the lock
//! alone (a launch waiting out an owner's boot, the launch gate, the idle
//! claim) are ported with the lock in `vocara_store/tests/library_lock.rs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use vocara_services::persistence::{OpenError, Persistence};
use vocara_services::waveform_service::SCRATCH_DIR_NAME;
use vocara_store::db::library_lock::{FixedGuard, LibraryGuard, LibraryLock};
use vocara_store::db::safe_migrator::SafeMigrator;
use vocara_store::db::AssetStore;
use vocara_store::repositories::SongPracticeRepository;

use crate::support::*;

/// The ceiling on every wait for a helper process.
const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

const MARK: &str = "VOCARA_LOCK_HELPER ";
const ENV_MODE: &str = "VOCARA_LOCK_HELPER_MODE";
const ENV_PATH: &str = "VOCARA_LOCK_HELPER_PATH";

fn say(word: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "\n{MARK}{word}");
    let _ = out.flush();
}

/// One `fcntl` record lock on byte [byte], as Python's `fcntl.lockf(fd,
/// mode | LOCK_NB, 1, byte)` issues it.
fn helper_lock(fd: i32, kind: impl Into<i64>, byte: i64) -> bool {
    // SAFETY: a zeroed flock is valid; the fields the call reads are set.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = kind.into() as _; // `c_short` on macOS, `c_int` on Linux
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = byte as libc::off_t;
    fl.l_len = 1;
    // SAFETY: fd is open for the helper's lifetime and fl outlives the call.
    unsafe { libc::fcntl(fd, libc::F_SETLK, &fl) != -1 }
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
            say(if helper_lock(fd, kind, 0) {
                "got"
            } else {
                "held"
            });
            std::process::exit(0);
        }
        _ => {
            let kind = if mode == "sh" {
                libc::F_RDLCK
            } else {
                libc::F_WRLCK
            };
            if !helper_lock(fd, kind, 0) {
                say("refused");
                std::process::exit(1);
            }
            say("locked");
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink);
            std::process::exit(0);
        }
    }
}

/// Starts the helper in [mode] on [path] and returns it with the first word
/// it said (or "timed out"). Its stdout stays drained after that word:
/// closing the pipe would kill the helper the next time it writes.
fn spawn_helper(path: &Path, mode: &str) -> (Child, String) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "single_instance::lock_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_MODE, mode)
        .env(ENV_PATH, path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the helper process");
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
/// fcntl lock on the lock file's library byte until released.
struct OtherProcess {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl OtherProcess {
    /// Holds the lock on [path] shared (a running instance).
    fn hold_shared(path: &Path) -> Self {
        let (mut child, first) = spawn_helper(path, "sh");
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
        let (mut child, out) = spawn_helper(path, if shared { "probe-sh" } else { "probe-ex" });
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

/// Mono 16-bit PCM: the shape the recording engine streams takes in.
fn mono_flat(samples: usize) -> Vec<u8> {
    vocara_services::audio_dsp::write_wav_mono(&vec![0.25f64; samples], 48000)
}

fn fixed(exclusive: bool) -> Option<Arc<dyn LibraryGuard>> {
    Some(Arc::new(FixedGuard::new(exclusive)))
}

fn boot_guarded(root: &Path, guard: Option<Arc<dyn LibraryGuard>>) -> Persistence {
    boot_with(root, fake_analyze(), guard).unwrap()
}

/// Creates the library with one practice and one pooled asset (so the
/// orphan-pool sweep has live hashes to judge against), then closes it,
/// without touching the real lock file (so this process holds nothing on it
/// until a test opens the library for real).
fn create_practice_and_close(root: &Path) -> String {
    let p = boot_guarded(root, fixed(true));
    let id = create_practice(&p).id;
    let src = root.join("backing.wav");
    std::fs::write(&src, [9u8, 8, 7]).unwrap();
    p.assets()
        .import_file(&text(&src), AssetStore::DEFAULT_KIND)
        .unwrap();
    p.close().unwrap();
    id
}

/// What another process's in-flight work looks like on disk: a take still
/// streaming into staging, an import staged but not saved, a file already
/// placed in the pool whose asset row has not committed yet, a copy still
/// being written into the pool's own staging, and a waveform analysis's
/// intermediate (a take's stop-and-save analyzes it).
struct InFlight {
    take: PathBuf,
    import: PathBuf,
    pooled: PathBuf,
    part: PathBuf,
    wave: PathBuf,
}

fn plant_in_flight_work(root: &Path, practice_id: &str) -> InFlight {
    let take = root.join("audio").join(practice_id).join("take.wav");
    write(&take, &mono_flat(4800));
    let import = root.join("technique_media").join("T").join("ex.mov");
    write(&import, &[1, 2, 3]);
    let hash = "ab".repeat(32);
    let pooled = root
        .join("assets")
        .join("ab")
        .join("ab")
        .join(format!("{hash}.wav"));
    write(&pooled, &[4, 5, 6]);
    let part = root.join("assets").join(".staging").join("copy.part");
    write(&part, &[7, 8]);
    let wave = root.join(SCRATCH_DIR_NAME).join("analysis.wave");
    write(&wave, &[1]);
    InFlight {
        take,
        import,
        pooled,
        part,
        wave,
    }
}

fn expect_untouched(p: &Persistence, practice_id: &str, work: &InFlight) {
    assert!(work.take.exists(), "take still recording");
    assert!(work.import.exists(), "import not saved yet");
    assert!(work.pooled.exists(), "import still landing");
    assert!(work.part.exists(), "copy still running");
    assert!(work.wave.exists(), "analysis in flight");
    let practice = p.song_repo().get_practice(practice_id).unwrap();
    assert!(
        practice.vocal_tracks.is_empty(),
        "the live take was adopted as \"(recovered)\""
    );
}

#[test]
fn a_boot_that_cannot_have_the_library_to_itself_recovers_and_sweeps_nothing() {
    // And its Repair library reclaims nothing.
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let work = plant_in_flight_work(root.path(), &practice_id);

    let p = boot_guarded(root.path(), fixed(false));

    assert!(!p.owns_library());
    expect_untouched(&p, &practice_id, &work);
    let report = p.repair_library().unwrap();
    assert_eq!(report.assets_swept, 0);
    expect_untouched(&p, &practice_id, &work);
}

#[test]
fn with_another_running_instance_holding_the_lock_shared_boot_leaves_its_work_alone() {
    // Its in-flight take, imports and pool files.
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let work = plant_in_flight_work(root.path(), &practice_id);
    let _other = OtherProcess::hold_shared(&lock_path(root.path()));

    let p = boot(root.path());

    assert!(!p.owns_library());
    expect_untouched(&p, &practice_id, &work);
}

#[test]
fn an_instance_that_started_second_keeps_guarding_the_library_after_the_first_one_quits() {
    // A new launch cannot own it while it runs.
    let root = tempfile::tempdir().unwrap();
    create_practice_and_close(root.path());
    let path = lock_path(root.path());
    let mut first = OtherProcess::hold_shared(&path);

    let second = boot(root.path());
    assert!(!second.owns_library());

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
fn an_owner_hands_the_lock_back_to_shared_once_its_boot_is_done() {
    let root = tempfile::tempdir().unwrap();
    let path = lock_path(root.path());
    let p = boot(root.path());
    assert!(p.owns_library());

    // Another launch can now open alongside it...
    assert!(OtherProcess::could_take(&path, true));
    // ...but never own the library while this instance runs.
    assert!(!OtherProcess::could_take(&path, false));
}

#[test]
fn repair_in_the_boot_owner_reclaims_nothing_once_another_instance_has_opened_the_library() {
    // And reclaims again once it is alone.
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let owner = boot(root.path());
    assert!(owner.owns_library());

    // A second instance opens the library and starts an import.
    let mut other = OtherProcess::hold_shared(&lock_path(root.path()));
    let work = plant_in_flight_work(root.path(), &practice_id);

    let held = owner.repair_library().unwrap();
    assert_eq!(held.assets_swept, 0);
    assert!(work.pooled.exists(), "import landing");
    assert!(work.part.exists(), "copy still running");

    other.release();
    let alone = owner.repair_library().unwrap();
    assert!(alone.assets_swept > 0);
    assert!(!work.pooled.exists(), "a real orphan now");
}

#[test]
fn one_process_opening_its_own_library_twice_owns_it_both_times_and_recovery_still_runs() {
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let take = root
        .path()
        .join("audio")
        .join(&practice_id)
        .join("take.wav");
    write(&take, &mono_flat(4800));

    let first = boot(root.path());
    assert!(first.owns_library());
    let practice = first.song_repo().get_practice(&practice_id).unwrap();
    assert_eq!(practice.vocal_tracks[0].display_name, "Take 1 (recovered)");

    first.close().unwrap();
    let second = boot(root.path());
    assert!(second.owns_library());
    // Both stores live in this process, which still has the library to
    // itself: its Repair library may reclaim.
    let guard = LibraryLock::for_library(root.path());
    assert!(guard.try_exclusive());
    guard.release_exclusive();
}

#[test]
fn a_second_instance_saving_while_the_first_one_writes_waits_its_turn() {
    // Instead of failing the save. Two running instances share the library
    // (each holds the lock shared). Each writes through its own SQLite
    // connection, and with no busy wait a write that met the other's open
    // transaction failed at once with "database is locked": the save
    // reported failed and its edit reverted.
    let root = tempfile::tempdir().unwrap();
    create_practice_and_close(root.path());
    let first = boot_guarded(root.path(), fixed(true));
    let second = boot_guarded(root.path(), fixed(false));

    let (writing_tx, writing) = mpsc::channel();
    thread::scope(|scope| {
        let held = scope.spawn(|| {
            first.db().transaction(|| -> vocara_store::Result<()> {
                first.db().execute(
                    "INSERT INTO sync_state (\"key\", value) VALUES ('probe', 'x')",
                    [],
                )?;
                writing_tx.send(()).unwrap();
                thread::sleep(Duration::from_millis(300));
                Ok(())
            })
        });
        writing.recv().unwrap();
        let saved = second.song_repo().create_practice("Second");
        held.join().unwrap().unwrap();
        assert!(saved.is_ok(), "{saved:?}");
    });
}

#[test]
fn a_non_owner_refuses_a_library_whose_owner_died_mid_migration() {
    // Its live file may be missing (moved to the backup, the migrated copy
    // not yet in): opened as it stood, that became a new EMPTY library, and
    // the next owner's recovery took it for the migrated one and dropped the
    // backup — the data left only in an orphaned copy.
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let db_path = root.path().join("vocara.db");
    let bak = root.path().join(SafeMigrator::BAK_NAME);
    std::fs::copy(&db_path, root.path().join(SafeMigrator::COPY_NAME)).unwrap();
    std::fs::rename(&db_path, &bak).unwrap();
    std::fs::write(
        root.path().join(SafeMigrator::MARKER_NAME),
        r#"{"state":"swapping","to":99}"#,
    )
    .unwrap();

    assert!(matches!(
        boot_with(root.path(), fake_analyze(), fixed(false)),
        Err(OpenError::LibraryInUse)
    ));
    assert!(!db_path.exists(), "nothing created");

    // The owner's recovery promotes the migrated copy: the data is there.
    let owner = boot_guarded(root.path(), fixed(true));
    assert!(owner.song_repo().get_practice(&practice_id).is_ok());
}

#[test]
fn a_library_that_needs_an_upgrade_is_not_migrated_under_another_process() {
    let root = tempfile::tempdir().unwrap();
    create_practice_and_close(root.path());
    let db_path = root.path().join("vocara.db");
    Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "DROP TABLE sync_state; DROP TABLE sync_quarantine; DROP TABLE asset_sync; \
             PRAGMA user_version = 1;",
        )
        .unwrap();

    assert!(matches!(
        boot_with(root.path(), fake_analyze(), fixed(false)),
        Err(OpenError::LibraryInUse)
    ));
    let version: i64 = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    assert!(
        !root.path().join(SafeMigrator::BAK_NAME).exists(),
        "no swap ran"
    );
}
