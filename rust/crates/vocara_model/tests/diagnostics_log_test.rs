//! Port of `client/test/diagnostics_log_test.dart`: ordered on-disk entries,
//! a strict two-file size budget, pre-bind buffering, the write lock, path
//! scrubbing, and the export bundle (log files + version metadata, never
//! user content). The global-hook test lives in `diagnostics_hooks_test.rs`
//! (a panic hook is process-wide). Lock retries are counted, not timed: the
//! waits go through an injected sleep.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use vocara_model::diagnostics::{DiagnosticsLog, DiagnosticsOptions, LockFile, OpenLock};

fn current_file(dir: &Path) -> PathBuf {
    dir.join("diagnostics.log")
}

fn rolled_file(dir: &Path) -> PathBuf {
    dir.join("diagnostics.log.1")
}

fn log_in(dir: &Path) -> DiagnosticsLog {
    DiagnosticsLog::for_testing(DiagnosticsOptions {
        directory: Some(dir.to_path_buf()),
        ..DiagnosticsOptions::default()
    })
}

/// `^\d{4}-\d{2}-\d{2}T[\d:.]+Z \[`.
fn starts_with_utc_timestamp(line: &str) -> bool {
    let b = line.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    if !(digits(0..4) && b.get(4) == Some(&b'-') && digits(5..7) && b.get(7) == Some(&b'-')) {
        return false;
    }
    if !(digits(8..10) && b.get(10) == Some(&b'T')) {
        return false;
    }
    let rest = &line[11..];
    let run = rest
        .bytes()
        .take_while(|c| c.is_ascii_digit() || *c == b':' || *c == b'.')
        .count();
    run > 0 && rest[run..].starts_with("Z [")
}

#[test]
fn log_appends_timestamped_entries_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_in(dir.path());
    log.log("boot", "first message");
    log.log("persistence", "second message");
    log.flush();

    let content = fs::read_to_string(current_file(dir.path())).unwrap();
    assert!(content.contains("[boot] first message"));
    assert!(content.contains("[persistence] second message"));
    assert!(content.find("first message") < content.find("second message"));
    // Every line starts with a UTC ISO-8601 timestamp.
    for line in content.trim().split('\n') {
        assert!(starts_with_utc_timestamp(line), "{line}");
    }
}

#[test]
fn rotates_into_exactly_two_files_at_the_size_cap_newest_last() {
    let dir = tempfile::tempdir().unwrap();
    let log = DiagnosticsLog::for_testing(DiagnosticsOptions {
        directory: Some(dir.path().to_path_buf()),
        max_bytes_per_file: 400,
        ..DiagnosticsOptions::default()
    });
    for i in 0..40 {
        log.log("rotation", &format!("entry {i} {}", "x".repeat(40)));
    }
    log.flush();

    assert!(rolled_file(dir.path()).exists());
    assert!(fs::metadata(current_file(dir.path())).unwrap().len() <= 400);
    assert!(fs::metadata(rolled_file(dir.path())).unwrap().len() <= 400);
    // The budget is two files: nothing rotates beyond .1.
    assert!(!dir.path().join("diagnostics.log.2").exists());
    // The newest entry lives in the current file.
    assert!(fs::read_to_string(current_file(dir.path()))
        .unwrap()
        .contains("entry 39"));
}

#[test]
fn entries_are_still_written_when_the_write_lock_cannot_be_had() {
    // A lock file that would not open (or a volume refusing locks) dropped
    // every entry from then on.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("diagnostics.log.lock")).unwrap();
    let log = log_in(dir.path());
    log.log("boot", "still recorded");
    log.flush();
    assert!(fs::read_to_string(current_file(dir.path()))
        .unwrap()
        .contains("still recorded"));
}

/// A lock file whose lock is always refused with [errno].
struct RefusingLock {
    errno: i32,
    tries: Arc<AtomicUsize>,
}

impl LockFile for RefusingLock {
    fn try_lock_exclusive(&mut self) -> io::Result<()> {
        self.tries.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::from_raw_os_error(self.errno))
    }
}

/// Logs three entries behind a lock refused with [errno]; returns the lock
/// attempts and the waits asked for.
fn log_three(errno: i32) -> (usize, Vec<Duration>) {
    let dir = tempfile::tempdir().unwrap();
    let tries = Arc::new(AtomicUsize::new(0));
    let open: OpenLock = {
        let tries = Arc::clone(&tries);
        Arc::new(move |_: &Path| {
            Ok(Box::new(RefusingLock {
                errno,
                tries: Arc::clone(&tries),
            }) as Box<dyn LockFile>)
        })
    };
    let waits = Arc::new(Mutex::new(Vec::new()));
    let sleep = {
        let waits = Arc::clone(&waits);
        Arc::new(move |d: Duration| waits.lock().unwrap().push(d))
    };
    let log = DiagnosticsLog::for_testing(DiagnosticsOptions {
        directory: Some(dir.path().to_path_buf()),
        open_lock: Some(open),
        sleep: Some(sleep),
        ..DiagnosticsOptions::default()
    });
    for m in ["one", "two", "three"] {
        log.log("boot", m);
    }
    log.flush();
    let content = fs::read_to_string(current_file(dir.path())).unwrap();
    for m in ["one", "two", "three"] {
        assert!(content.contains(&format!("[boot] {m}")));
    }
    let waits = waits.lock().unwrap().clone();
    (tries.load(Ordering::SeqCst), waits)
}

// Retried in full for every entry, it cost each one ~2 s for good: on a
// volume that refuses locks, or behind a peer frozen mid-write.

#[test]
fn on_a_volume_that_refuses_locks_is_no_longer_tried() {
    let (tries, waits) = log_three(45); // ENOTSUP on macOS
    assert_eq!(tries, 1);
    assert!(waits.is_empty());
}

#[test]
fn behind_another_holder_is_waited_for_once_then_tried_once_per_entry() {
    let (tries, waits) = log_three(35); // EAGAIN on macOS
    assert_eq!(tries, 41 + 1 + 1);
    // 40 waits of 50 ms (~2 s) for the first entry, none for the others.
    assert_eq!(waits.len(), 40);
    assert!(waits.iter().all(|d| *d == Duration::from_millis(50)));
}

#[test]
fn entries_logged_before_a_directory_is_bound_are_flushed_after() {
    let dir = tempfile::tempdir().unwrap();
    let log = DiagnosticsLog::for_testing(DiagnosticsOptions::default());
    log.log("early", "buffered before bind");
    log.bind_directory(dir.path());
    log.flush();
    assert!(fs::read_to_string(current_file(dir.path()))
        .unwrap()
        .contains("[early] buffered before bind"));
}

#[test]
fn the_real_lock_is_taken_and_released_around_each_entry() {
    // Two instances (two "processes" sharing a directory) both write; the
    // fcntl lock is per process, so in one process both take it in turn.
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (log_in(dir.path()), log_in(dir.path()));
    for i in 0..10 {
        a.log("a", &format!("{i}"));
        b.log("b", &format!("{i}"));
    }
    a.flush();
    b.flush();
    let content = fs::read_to_string(current_file(dir.path())).unwrap();
    assert_eq!(content.lines().count(), 20);
    assert!(dir.path().join("diagnostics.log.lock").exists());
}

#[test]
fn an_overlong_entry_is_cut_to_4096_utf16_units() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_in(dir.path());
    log.log("big", &"é".repeat(10_000));
    log.flush();
    let content = fs::read_to_string(current_file(dir.path())).unwrap();
    let line = content.strip_suffix('\n').unwrap();
    assert_eq!(line.encode_utf16().count(), 4096);
}

// --- scrubPaths ---

fn scrub(m: &str) -> String {
    DiagnosticsLog::scrub_paths(m)
}

#[test]
fn strips_absolute_user_space_paths_keeping_only_the_extension() {
    assert_eq!(
        scrub(
            "FileSystemException: Cannot open file, path = \
             '/Users/ruta/Music/My Song Title.wav' (OS Error: No such file)"
        ),
        "FileSystemException: Cannot open file, path = '<path>.wav' (OS Error: No such file)"
    );
    assert_eq!(
        scrub("copy failed: /private/var/folders/ab/T/vocara/audio/01ABC/take.wav"),
        "copy failed: <path>.wav"
    );
    assert_eq!(
        scrub("/Volumes/USB/session (final)/mix.aiff gone"),
        "<path>.aiff gone"
    );
    // Prose after one path can hold another path; both are scrubbed.
    assert_eq!(
        scrub("cp /Users/a/x.wav /Users/b/y.wav failed"),
        "cp <path>.wav <path>.wav failed"
    );
}

#[test]
fn a_dot_in_a_directory_or_file_name_does_not_end_the_scrub_early() {
    // The cut used to land on the FIRST `.ext`-looking run, so a dotted
    // directory leaked the rest of the path verbatim.
    assert_eq!(
        scrub("path = '/Users/alice/Music/Vol.2 Live/My Secret Song.wav'"),
        "path = '<path>.wav'"
    );
    assert_eq!(
        scrub("/Users/alice/feat.Ed Sheeran/Perfect Duet.wav (OS Error)"),
        "<path>.wav (OS Error)"
    );
    assert_eq!(
        scrub("at /Users/alice/Adele 2016.12 Remix/Hello"),
        "at <path>"
    );
    assert_eq!(scrub("/Users/a/My.Song.wav gone"), "<path>.wav gone");
    assert_eq!(
        scrub("'/Users/a/Ms.Lauryn Hill.wav' gone"),
        "'<path>.wav' gone"
    );
    // A second path after an extensionless one is still scrubbed on its own.
    assert_eq!(
        scrub("cp /Users/a/dir /Users/b/y.wav failed"),
        "cp <path> <path>.wav failed"
    );
}

#[test]
fn an_apostrophe_inside_a_name_does_not_end_the_scrub_early() {
    // The match stopped at the first `'`, so everything after it in the
    // name was logged verbatim.
    assert_eq!(
        scrub(
            "FileSystemException: Cannot open file, path = \
             '/Users/a/Music/Don't Stop Believin.m4a' (OS Error: x, errno = 2)"
        ),
        "FileSystemException: Cannot open file, path = '<path>.m4a' (OS Error: x, errno = 2)"
    );
    for message in [
        "path = '/Users/a/Guns N' Roses/Sweet Child O' Mine.mp3' (OS Error)",
        "path = '/Users/a/Rock 'n' Roll/Take 2.wav'",
        "copy '/Users/a/x.wav' to '/Users/a/O' Mine.wav', failed",
        // Any ` (` after an apostrophe used to read as the closing quote.
        "path = '/Users/a/The Players' (Club Mix) - Mine.wav' (OS Error: x)",
    ] {
        let out = scrub(message);
        for leak in ["Roses", "Mine", "Roll", "Take 2", "Club Mix"] {
            assert!(!out.contains(leak), "{out}");
        }
    }
    // The closing quote still ends the path, so what follows survives.
    assert_eq!(
        scrub("open '/Users/a/Don't.wav', retrying"),
        "open '<path>.wav', retrying"
    );
    assert_eq!(
        scrub("copy '/Users/a/x.wav' to '/Users/b/y.aiff'"),
        "copy '<path>.wav' to '<path>.aiff'"
    );
}

#[test]
fn a_double_quote_inside_a_name_does_not_end_the_scrub_early() {
    // The match stopped at the first `"`, so the rest of the name was
    // logged verbatim: 'path = <path>" Version).mp3'.
    assert_eq!(
        scrub("path = '/Users/a/Music/Blue Monday (12\" Version).mp3' (OS Error: x, errno = 2)"),
        "path = '<path>.mp3' (OS Error: x, errno = 2)"
    );
    let live = scrub("path = '/Users/a/Song \"Live at Wembley\".m4a'");
    assert!(!live.contains("Wembley"), "{live}");
    // A quote right before the extension's dot is still inside the name.
    assert_eq!(
        scrub("path = '/Users/a/Song \"Live\".m4a' (OS Error: x)"),
        "path = '<path>.m4a' (OS Error: x)"
    );
    // A double-quoted path still ends at its closing quote.
    assert_eq!(
        scrub("open \"/Users/a/x.wav\" failed"),
        "open \"<path>.wav\" failed"
    );
    assert_eq!(
        scrub("copy \"/Users/a/x.wav\" to \"/Users/b/y.aiff\""),
        "copy \"<path>.wav\" to \"<path>.aiff\""
    );
}

#[test]
fn a_dotted_number_in_the_prose_after_a_path_is_not_taken_for_its_extension() {
    // Taking the LAST extension-looking run in the final component cut
    // the message at '12.3s', losing the file type and the context.
    assert_eq!(
        scrub("decode failed for /Users/alice/Music/take.wav after 12.3s"),
        "decode failed for <path>.wav after 12.3s"
    );
    assert_eq!(
        scrub("stage /Users/alice/Music/My Song.m4a: rate 22.05 kHz unsupported"),
        "stage <path>.m4a: rate 22.05 kHz unsupported"
    );
    // But only over numbers: a known type INSIDE a name whose own
    // extension is unlisted must not leak the rest of that name.
    for message in [
        "import failed: /Users/alice/Music/Sonata Op.27.Part II (Jane Doe).wma",
        "decode failed: /Users/alice/Movies/Lesson.mov with Jane Doe.avi",
    ] {
        let out = scrub(message);
        assert!(!out.contains("Jane Doe"), "{out}");
    }
}

#[test]
fn leaves_package_uris_and_non_path_text_untouched() {
    let stack_frame = "#0  AssetStore._importFile (package:vocara/data/db/asset_store.dart:61:5)";
    assert_eq!(scrub(stack_frame), stack_frame);
    assert_eq!(scrub("ratio 44100/48000"), "ratio 44100/48000");
    assert_eq!(scrub("dart:async boom"), "dart:async boom");
}

#[test]
fn log_entries_carry_no_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_in(dir.path());
    log.log(
        "mic",
        "discard failed: FileSystemException: Deletion failed, path = \
         '/Users/ruta/Library/Containers/app/take.wav'",
    );
    log.flush();
    let content = fs::read_to_string(current_file(dir.path())).unwrap();
    assert!(!content.contains("/Users"));
    assert!(content.contains("<path>.wav"));
}

#[test]
fn the_scrubber_matches_dart_on_every_golden_message() {
    // golden/diagnostics_scrub.txt: Dart's own scrubPaths on these messages
    // (golden/diagnostics_scrub.gen.dart).
    let golden = include_str!("golden/diagnostics_scrub.txt");
    let mut checked = 0;
    for line in golden.lines().filter(|l| !l.is_empty()) {
        let j: Value = serde_json::from_str(line).unwrap();
        let input = j["in"].as_str().unwrap();
        assert_eq!(scrub(input), j["out"].as_str().unwrap(), "{input:?}");
        checked += 1;
    }
    assert!(checked > 55);
}

// Lines an older build wrote stay on disk until rotation, with whatever its
// scrubber missed, and the export copied them as they were.
#[test]
fn export_archive_scrubs_what_is_already_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        current_file(dir.path()),
        "2026-10-01T00:00:00Z [old] cannot open '/Users/jane/Music/Secret.wav'\n",
    )
    .unwrap();
    let log = log_in(dir.path());
    let path = log.export_archive().unwrap();
    let bundle = fs::read_to_string(&path).unwrap();
    DiagnosticsLog::discard_archive(&path);
    assert!(!bundle.contains("jane"));
    assert!(!bundle.contains("Secret"));
    assert!(bundle.contains("cannot open '<path>.wav'"));
}

#[test]
fn export_archive_bundles_both_log_files_with_version_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let log = DiagnosticsLog::for_testing(DiagnosticsOptions {
        directory: Some(dir.path().to_path_buf()),
        max_bytes_per_file: 400,
        ..DiagnosticsOptions::default()
    });
    for i in 0..40 {
        log.log("area", &format!("entry {i} {}", "x".repeat(40)));
    }

    let path = log.export_archive().unwrap();
    let content = fs::read_to_string(&path).unwrap();
    fs::remove_dir_all(path.parent().unwrap()).unwrap();

    assert!(content.contains("Vocara diagnostics bundle"));
    assert!(content.contains("app version:"));
    assert!(content.contains("schema user_version: no database file"));
    // Both rotation generations are included, rolled (older) first.
    assert!(content.contains("===== diagnostics.log.1 ====="));
    assert!(content.contains("===== diagnostics.log ====="));
    assert!(content.contains("entry 39"));
    assert!(
        content.find("===== diagnostics.log.1 =====") < content.find("===== diagnostics.log =====")
    );
}

#[test]
fn export_archive_reads_the_schema_version_and_the_unbound_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let mut header = vec![0u8; 100];
    header[..16].copy_from_slice(b"SQLite format 3\0");
    header[60..64].copy_from_slice(&2i32.to_be_bytes());
    fs::write(dir.path().join("vocara.db"), &header).unwrap();
    let log = log_in(dir.path());
    let path = log.export_archive().unwrap();
    let content = fs::read_to_string(&path).unwrap();
    DiagnosticsLog::discard_archive(&path);
    assert!(content.contains("schema user_version: 2\n"), "{content}");

    fs::write(dir.path().join("vocara.db"), b"not a database").unwrap();
    let path = log.export_archive().unwrap();
    let content = fs::read_to_string(&path).unwrap();
    DiagnosticsLog::discard_archive(&path);
    assert!(content.contains("schema user_version: unreadable (SqliteException)"));

    let unbound = DiagnosticsLog::for_testing(DiagnosticsOptions {
        temp_root: Some(dir.path().to_path_buf()),
        ..DiagnosticsOptions::default()
    });
    unbound.log("early", "held in memory");
    let path = unbound.export_archive().unwrap();
    let content = fs::read_to_string(&path).unwrap();
    DiagnosticsLog::discard_archive(&path);
    assert!(content.contains("===== buffered (no log directory bound) =====\n"));
    assert!(content.contains("[early] held in memory\n"));
}

#[test]
fn a_bundle_whose_write_fails_leaves_no_temporary_folder_behind() {
    // A full disk: the write fails, the caller gets no path to discard, and
    // the folder (with whatever part of the bundle landed) stayed in tmp.
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("tmp");
    fs::create_dir(&temp).unwrap();
    let log = DiagnosticsLog::for_testing(DiagnosticsOptions {
        directory: Some(dir.path().to_path_buf()),
        temp_root: Some(temp.clone()),
        write_bundle: Some(Arc::new(|path: &Path, _: &str| {
            fs::write(path, "partial")?;
            Err(io::Error::other("No space left on device"))
        })),
        ..DiagnosticsOptions::default()
    });
    assert!(log.export_archive().is_err());
    assert_eq!(fs::read_dir(&temp).unwrap().count(), 0);
}

#[test]
fn discard_archive_removes_the_bundle_and_its_own_temp_folder_never_a_folder_that_holds_anything_else(
) {
    let dir = tempfile::tempdir().unwrap();
    let log = log_in(dir.path());
    let path = log.export_archive().unwrap();
    DiagnosticsLog::discard_archive(&path);
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());

    // A path somewhere else: only that file goes, its neighbours stay.
    let other = dir.path().join("bundle.txt");
    fs::write(&other, "x").unwrap();
    let neighbour = dir.path().join("keep.txt");
    fs::write(&neighbour, "y").unwrap();
    DiagnosticsLog::discard_archive(&other);
    assert!(!other.exists());
    assert!(neighbour.exists());
    assert!(dir.path().exists());
}
