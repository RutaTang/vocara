//! A library a sandboxed release left in its App Sandbox container, that the
//! launch could not move out (`vocara_services::sandbox_migration`), is the
//! one the whole app opens: the diagnostics log, the library itself, the
//! settings storage and the "Reveal Library Folder" folder all resolve to
//! the container, and nothing is made in Application Support beside it. A
//! test binary of its own, with one test, since it points `HOME` at a
//! temporary folder for the whole process. (The container's layout is
//! macOS's; the launch only looks for it there, but the rooting it leads to
//! is the same everywhere.)

use vocara_app::app::library::{library_folder, open_library, LibraryOpen};
use vocara_app::app::{attach_diagnostics, AppEnvironment};
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::sandbox_migration::{locations, settle_in, Outcome, Patience, Why};

#[test]
fn a_library_that_stays_in_the_container_is_the_one_every_store_opens() {
    let home = tempfile::tempdir().expect("temp home");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    std::env::set_var("VOCARA_FLAVOR", "test");
    let bundle_id = AppEnvironment::Test.bundle_id();

    // What the sandboxed release left in its container, and a stray file
    // (no library) in Application Support, which keeps it there: nothing
    // is merged into that folder.
    let at = locations(home.path(), bundle_id).expect("the two folders");
    std::fs::create_dir_all(at.container.join("settings")).unwrap();
    std::fs::write(at.container.join("settings/app.json"), "{}").unwrap();
    std::fs::create_dir_all(&at.unsandboxed).unwrap();
    std::fs::write(at.unsandboxed.join("stray"), "not a library").unwrap();

    let outcome = settle_in(bundle_id, &at, &Patience::default());
    assert!(
        matches!(outcome, Outcome::StaysInContainer(Why::UnsandboxedNotEmpty)),
        "{outcome:?}"
    );

    attach_diagnostics();
    DiagnosticsLog::instance().log("boot", "launched");
    let opened = open_library(&mut || {});
    assert!(matches!(opened, LibraryOpen::Opened(_)), "{opened:?}");
    DiagnosticsLog::instance().flush();

    assert_eq!(
        library_folder(AppEnvironment::Test).expect("library folder"),
        at.container
    );
    assert_eq!(
        LocalStorageService::new(bundle_id).root_dir().unwrap(),
        at.container
    );
    assert!(
        at.container.join("vocara.db").is_file(),
        "the library opened in the container"
    );
    let log = std::fs::read_to_string(at.container.join("diagnostics.log"))
        .expect("the log is on disk, in the container");
    assert!(
        log.contains("[storage] the library stays in the App Sandbox container"),
        "{log}"
    );
    assert!(log.contains("[boot] launched"), "{log}");
    let beside: Vec<_> = std::fs::read_dir(&at.unsandboxed)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(beside, ["stray"], "nothing made in Application Support");
}
