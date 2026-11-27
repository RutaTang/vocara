//! The move out of the App Sandbox container, on any platform: each case
//! lays out the macOS folders under a temporary home folder
//! ([`Locations::under_home`]) and checks what [`migrate`] decides, that the
//! library ends up whole in exactly one place, and that nothing else is
//! touched.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, Stdio};

use super::*;
use crate::local_storage_service::{application_support_dir, LocalStorageService};
use crate::persistence::library_dir;

const ID: &str = "com.rutatang.vocara";

/// A patience that never keeps a test waiting long.
fn quick() -> Patience {
    Patience {
        wait: Duration::from_millis(200),
        poll: Duration::from_millis(10),
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// What a sandboxed release leaves in its folder: the library (database,
/// WAL, lock, node id, pool, recordings, technique media), the settings,
/// the caches and the log.
fn seed_library(dir: &Path) {
    write(
        &dir.join("vocara.db"),
        b"SQLite format 3\0 the user's library",
    );
    write(&dir.join("vocara.db-wal"), b"the last writes");
    write(&dir.join("vocara.lock"), b"");
    write(&dir.join("device_node"), b"01HZX5P5XQ0V9W4D3J6K8M2N7R");
    write(&dir.join("assets/ab/cd/abcd1234.wav"), b"RIFF a take");
    write(&dir.join("audio/imports/song.m4a"), b"a backing track");
    write(&dir.join("technique_media/clip.m4a"), b"an example");
    write(&dir.join("settings/app.json"), br#"{"volume":1.0}"#);
    write(&dir.join("stems_cache/x.vocal.wav"), b"stems");
    write(
        &dir.join("diagnostics.log"),
        b"2026-01-01T00:00:00.000Z [boot] launched\n",
    );
    fs::create_dir_all(dir.join("assets/.staging")).unwrap();
}

/// The container's other contents, which are the system's and stay put.
fn seed_container_siblings(home: &Path, bundle_id: &str) {
    let data = home.join("Library/Containers").join(bundle_id).join("Data");
    write(
        &data.join(format!("Library/Preferences/{bundle_id}.plist")),
        b"panels",
    );
    write(&data.join("tmp/scratch"), b"temporary");
}

/// Every file (with its bytes) and folder under [dir], by relative path;
/// empty when [dir] does not exist.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

fn container_data(home: &Path, bundle_id: &str) -> PathBuf {
    home.join("Library/Containers").join(bundle_id).join("Data")
}

#[test]
fn locations_put_a_bundle_id_s_folders_where_macos_does() {
    let at = Locations::under_home(Path::new("/Users/ruta"), "com.rutatang.vocara.dev");
    assert_eq!(
        at.container,
        Path::new(
            "/Users/ruta/Library/Containers/com.rutatang.vocara.dev/Data/Library/\
             Application Support/com.rutatang.vocara.dev"
        )
    );
    assert_eq!(
        at.unsandboxed,
        Path::new("/Users/ruta/Library/Application Support/com.rutatang.vocara.dev")
    );
}

#[test]
fn a_fresh_install_has_nothing_to_move_and_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    assert!(matches!(migrate(&at, &quick()), Outcome::NothingToMove));
    assert!(snapshot(home.path()).is_empty());
}

#[test]
fn a_sandboxed_library_moves_whole_into_application_support() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    seed_container_siblings(home.path(), ID);
    let library = snapshot(&at.container);
    let data = container_data(home.path(), ID);
    let siblings: BTreeMap<_, _> = snapshot(&data)
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with("Library/Application Support"))
        .collect();

    assert!(matches!(migrate(&at, &quick()), Outcome::Moved));

    assert_eq!(snapshot(&at.unsandboxed), library);
    assert!(!at.container.exists(), "moved, not copied");
    let left: BTreeMap<_, _> = snapshot(&data)
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with("Library/Application Support"))
        .collect();
    assert_eq!(left, siblings, "the rest of the container stays as it was");
}

#[test]
fn a_container_holding_files_but_no_library_still_moves() {
    // Recordings and settings with no database yet are the user's too.
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    write(&at.container.join("settings/app.json"), b"{}");
    write(&at.container.join("audio/take.wav"), b"RIFF");
    let before = snapshot(&at.container);
    assert!(matches!(migrate(&at, &quick()), Outcome::Moved));
    assert_eq!(snapshot(&at.unsandboxed), before);
}

#[test]
fn a_second_launch_after_the_move_finds_nothing_to_move() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    assert!(matches!(migrate(&at, &quick()), Outcome::Moved));
    let moved = snapshot(home.path());
    for _ in 0..2 {
        assert!(matches!(migrate(&at, &quick()), Outcome::NothingToMove));
        assert_eq!(snapshot(home.path()), moved);
    }
}

#[test]
fn an_empty_or_finder_only_application_support_folder_gives_way() {
    for litter in [&[][..], &[FINDER_VIEW_SETTINGS][..]] {
        let home = tempfile::tempdir().unwrap();
        let at = Locations::under_home(home.path(), ID);
        seed_library(&at.container);
        fs::create_dir_all(&at.unsandboxed).unwrap();
        for name in litter {
            write(&at.unsandboxed.join(name), b"view settings");
        }
        let library = snapshot(&at.container);
        assert!(matches!(migrate(&at, &quick()), Outcome::Moved));
        assert_eq!(snapshot(&at.unsandboxed), library, "exactly the library");
        assert!(!at.container.exists());
    }
}

#[test]
fn a_container_holding_nothing_of_the_users_is_left_alone() {
    for litter in [&[][..], &[FINDER_VIEW_SETTINGS][..]] {
        let home = tempfile::tempdir().unwrap();
        let at = Locations::under_home(home.path(), ID);
        fs::create_dir_all(&at.container).unwrap();
        for name in litter {
            write(&at.container.join(name), b"view settings");
        }
        let before = snapshot(home.path());
        assert!(matches!(migrate(&at, &quick()), Outcome::NothingToMove));
        assert_eq!(snapshot(home.path()), before);
    }
}

#[test]
fn application_support_s_library_opens_when_the_container_holds_nothing() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.unsandboxed);
    assert!(matches!(migrate(&at, &quick()), Outcome::NothingToMove));
    fs::create_dir_all(&at.container).unwrap();
    let before = snapshot(home.path());
    assert!(matches!(migrate(&at, &quick()), Outcome::NothingToMove));
    assert_eq!(snapshot(home.path()), before);
}

#[test]
fn when_both_hold_a_library_application_support_s_opens_and_neither_is_touched() {
    // A dev build of the same flavor ran unsandboxed beside a sandboxed
    // release: nothing is merged, nothing overwritten.
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    write(
        &at.unsandboxed.join("vocara.db"),
        b"SQLite format 3\0 the dev library",
    );
    let before = snapshot(home.path());
    for _ in 0..2 {
        assert!(matches!(migrate(&at, &quick()), Outcome::BothHold));
        assert_eq!(snapshot(home.path()), before);
    }
}

#[test]
fn an_interrupted_schema_migration_is_a_library_too() {
    // Its database stranded in the backup, or only the marker left: still
    // the library Application Support opens, never one to move over.
    for entry in [SafeMigrator::BAK_NAME, SafeMigrator::MARKER_NAME] {
        let home = tempfile::tempdir().unwrap();
        let at = Locations::under_home(home.path(), ID);
        seed_library(&at.container);
        write(&at.unsandboxed.join(entry), b"mid-migration");
        let before = snapshot(home.path());
        assert!(matches!(migrate(&at, &quick()), Outcome::BothHold));
        assert_eq!(snapshot(home.path()), before);
    }
}

#[test]
fn files_but_no_library_in_application_support_keep_the_library_in_the_container() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    write(
        &at.unsandboxed.join("updates/stage/partial"),
        b"not the user's library",
    );
    let before = snapshot(home.path());
    assert!(matches!(
        migrate(&at, &quick()),
        Outcome::StaysInContainer(Why::UnsandboxedNotEmpty)
    ));
    assert_eq!(snapshot(home.path()), before);
}

#[test]
fn a_container_that_cannot_be_read_keeps_the_app_there_never_an_empty_library() {
    // A container the system will not let this build look into (a file
    // in the way stands in for the refusal: the look fails, not with
    // "missing").
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    write(
        &home.path().join("Library/Containers").join(ID),
        b"in the way",
    );
    let outcome = migrate(&at, &quick());
    assert!(
        matches!(
            outcome,
            Outcome::StaysInContainer(Why::ContainerUnreadable(_))
        ),
        "{outcome:?}"
    );
    assert!(!at.unsandboxed.exists(), "no empty library made beside it");
}

#[test]
fn an_unreadable_container_beside_application_support_s_library_is_only_logged() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.unsandboxed);
    write(
        &home.path().join("Library/Containers").join(ID),
        b"in the way",
    );
    let outcome = migrate(&at, &quick());
    assert!(
        matches!(outcome, Outcome::ContainerUnchecked(_)),
        "{outcome:?}"
    );
}

#[test]
fn an_application_support_folder_that_cannot_be_read_keeps_the_library_in_the_container() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    write(
        &home.path().join("Library/Application Support"),
        b"in the way",
    );
    let before = snapshot(&at.container);
    let outcome = migrate(&at, &quick());
    assert!(
        matches!(
            outcome,
            Outcome::StaysInContainer(Why::UnsandboxedUnreadable(_))
        ),
        "{outcome:?}"
    );
    assert_eq!(snapshot(&at.container), before);
}

#[test]
fn a_move_that_fails_keeps_the_library_whole_in_the_container() {
    // Onto another volume, or refused (macOS's App Data protection).
    for code in [libc::EXDEV, libc::EPERM] {
        let home = tempfile::tempdir().unwrap();
        let at = Locations::under_home(home.path(), ID);
        seed_library(&at.container);
        let before = snapshot(&at.container);
        let outcome = migrate_with(&at, &quick(), &|_: &Path, _: &Path| {
            Err(io::Error::from_raw_os_error(code))
        });
        match outcome {
            Outcome::StaysInContainer(Why::MoveFailed(e)) => {
                assert_eq!(e.raw_os_error(), Some(code))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(snapshot(&at.container), before);
        assert!(!at.unsandboxed.exists());
    }
}

#[test]
fn a_crash_at_any_step_of_the_move_leaves_one_whole_library_the_next_launch_opens() {
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    let library = snapshot(&at.container);
    let crash = |after_rename: bool| {
        catch_unwind(AssertUnwindSafe(|| {
            migrate_with(&at, &quick(), &|from: &Path, to: &Path| {
                if after_rename {
                    fs::rename(from, to)?;
                }
                panic!("the power went");
            })
        }))
        .unwrap_err();
    };

    // Before the rename: still wholly in the container, and the next
    // launch moves it.
    fs::create_dir_all(&at.unsandboxed).unwrap();
    crash(false);
    assert_eq!(snapshot(&at.container), library);
    assert!(
        !at.unsandboxed.exists(),
        "only the empty folder in the way went"
    );
    assert!(matches!(migrate(&at, &quick()), Outcome::Moved));
    assert_eq!(snapshot(&at.unsandboxed), library);

    // After it: wholly in Application Support, where the next launch opens
    // it.
    fs::rename(&at.unsandboxed, &at.container).unwrap();
    crash(true);
    assert_eq!(snapshot(&at.unsandboxed), library);
    assert!(!at.container.exists());
    assert!(matches!(migrate(&at, &quick()), Outcome::NothingToMove));
    assert_eq!(snapshot(&at.unsandboxed), library);
}

#[test]
fn a_library_another_launch_moved_first_is_found_where_it_went() {
    // Two launches at once: the other one's rename lands between this one's
    // look and its own rename.
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    let library = snapshot(&at.container);
    let outcome = migrate_with(&at, &quick(), &|from: &Path, to: &Path| {
        fs::rename(from, to)?;
        fs::rename(from, to)
    });
    assert!(matches!(outcome, Outcome::NothingToMove), "{outcome:?}");
    assert_eq!(snapshot(&at.unsandboxed), library);
    assert!(!at.container.exists());
}

/// Set in the child process [`a_library_another_vocara_has_open_stays_in_the_container_until_it_quits`]
/// starts: the library folder that process holds as a running Vocara does.
const HOLDER: &str = "VOCARA_TEST_HOLD_LIBRARY";
const HELD: &str = "the other Vocara holds the library";

#[test]
fn a_library_another_vocara_has_open_stays_in_the_container_until_it_quits() {
    if let Some(dir) = std::env::var_os(HOLDER) {
        // The other Vocara (an older, sandboxed one still open): it holds
        // the library as every running instance does, until the test
        // closes its stdin. Record locks belong to the process, so it has to
        // be another one. It says so straight to the handle (the test
        // harness captures `println!`), on a line of its own (the harness has
        // printed "test <name> ... " without one).
        let _lock = LibraryLock::for_library(Path::new(&dir));
        let mut out = io::stdout();
        writeln!(out, "\n{HELD}").unwrap();
        out.flush().unwrap();
        let _ = io::stdin().read_line(&mut String::new());
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), ID);
    seed_library(&at.container);
    let mut other = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sandbox_migration::tests::\
             a_library_another_vocara_has_open_stays_in_the_container_until_it_quits",
            "--test-threads=1",
        ])
        .env(HOLDER, &at.container)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Read on a thread (which also keeps the pipe drained), so a child that
    // never holds the library fails the test instead of hanging it.
    let out = BufReader::new(other.stdout.take().unwrap());
    let (held, holding) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            if line.trim() == HELD {
                let _ = held.send(());
            }
        }
    });
    if holding.recv_timeout(Duration::from_secs(60)).is_err() {
        let _ = other.kill();
        panic!("the other Vocara never held the library");
    }
    let before = snapshot(home.path());

    let patience = Patience {
        wait: Duration::from_millis(300),
        poll: Duration::from_millis(20),
    };
    let started = Instant::now();
    let outcome = migrate(&at, &patience);
    assert!(
        matches!(outcome, Outcome::StaysInContainer(Why::InUse)),
        "{outcome:?}"
    );
    assert!(started.elapsed() >= patience.wait, "it waited for it first");
    assert_eq!(snapshot(home.path()), before, "nothing moved from under it");

    drop(other.stdin.take());
    assert!(other.wait().unwrap().success());
    let library = snapshot(&at.container);
    assert!(matches!(migrate(&at, &patience), Outcome::Moved));
    assert_eq!(snapshot(&at.unsandboxed), library);
}

#[test]
fn each_flavor_moves_its_own_folder_only() {
    let home = tempfile::tempdir().unwrap();
    let ids = [
        AppEnvironment::Prod,
        AppEnvironment::Dev,
        AppEnvironment::Test,
    ]
    .map(AppEnvironment::bundle_id);
    for id in ids {
        seed_library(&Locations::under_home(home.path(), id).container);
    }
    let prod = Locations::under_home(home.path(), ids[0]);
    let others: Vec<_> = ids[1..]
        .iter()
        .map(|id| Locations::under_home(home.path(), id))
        .collect();
    let before: Vec<_> = others.iter().map(|at| snapshot(&at.container)).collect();
    assert!(matches!(migrate(&prod, &quick()), Outcome::Moved));
    for (at, before) in others.iter().zip(before) {
        assert_eq!(snapshot(&at.container), before);
        assert!(!at.unsandboxed.exists());
    }
    for at in &others {
        assert!(matches!(migrate(at, &quick()), Outcome::Moved));
    }
    for id in ids {
        let at = Locations::under_home(home.path(), id);
        assert!(at.unsandboxed.join("vocara.db").is_file(), "{id}");
        assert!(!at.container.exists(), "{id}");
    }
}

#[test]
fn a_library_that_stays_roots_every_store_at_the_container() {
    let bundle_id = "test.sandbox-migration.stays";
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), bundle_id);
    seed_library(&at.container);
    write(&at.unsandboxed.join("stray"), b"not a library");
    assert!(matches!(
        settle_in(bundle_id, &at, &quick()),
        Outcome::StaysInContainer(Why::UnsandboxedNotEmpty)
    ));
    assert_eq!(application_support_dir(bundle_id).unwrap(), at.container);
    assert_eq!(library_dir(bundle_id).unwrap(), at.container);
    assert_eq!(
        LocalStorageService::new(bundle_id).root_dir().unwrap(),
        at.container
    );
}

#[test]
fn a_library_that_moved_roots_nothing() {
    let bundle_id = "test.sandbox-migration.moved";
    let home = tempfile::tempdir().unwrap();
    let at = Locations::under_home(home.path(), bundle_id);
    seed_library(&at.container);
    assert!(matches!(
        settle_in(bundle_id, &at, &quick()),
        Outcome::Moved
    ));
    assert!(app_support::rooted_at(bundle_id).is_none());
}

#[test]
fn every_entry_names_its_folders_in_words_the_log_keeps() {
    let e = || io::Error::from_raw_os_error(libc::EPERM);
    let outcomes = [
        Outcome::Moved,
        Outcome::BothHold,
        Outcome::ContainerUnchecked(e()),
        Outcome::StaysInContainer(Why::InUse),
        Outcome::StaysInContainer(Why::ContainerUnreadable(e())),
        Outcome::StaysInContainer(Why::UnsandboxedUnreadable(e())),
        Outcome::StaysInContainer(Why::UnsandboxedNotEmpty),
        Outcome::StaysInContainer(Why::MoveFailed(e())),
    ];
    assert!(Outcome::NothingToMove.describe(ID).is_none());
    for outcome in &outcomes {
        let entry = outcome.describe(ID).unwrap();
        assert_eq!(DiagnosticsLog::scrub_paths(&entry), entry);
        assert!(
            entry.contains("Library/Containers/com.rutatang.vocara"),
            "{entry}"
        );
    }
    // What the release's launch check looks for.
    assert!(outcomes[0]
        .describe(ID)
        .unwrap()
        .starts_with("moved the library out of the App Sandbox container"));
    assert!(outcomes[3]
        .describe(ID)
        .unwrap()
        .starts_with("the library stays in the App Sandbox container"));
}
