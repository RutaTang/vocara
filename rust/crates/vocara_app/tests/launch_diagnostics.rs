//! `main`'s first step (`DiagnosticsLog.attachGlobalHooks` in `main.dart`):
//! the diagnostics log is written to disk, in the library folder, and an
//! uncaught panic lands in it. A test binary of its own, with one test, since
//! it points `HOME` at a temporary folder for the whole process.

use vocara_app::app::{attach_diagnostics, library::library_folder, AppEnvironment};
use vocara_model::diagnostics::DiagnosticsLog;

#[test]
fn the_launch_diagnostics_land_in_the_library_folder() {
    let home = tempfile::tempdir().expect("temp home");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    std::env::set_var("VOCARA_FLAVOR", "test");

    attach_diagnostics();
    let log = DiagnosticsLog::instance();
    log.log("boot", "launched");
    let panicked = std::panic::catch_unwind(|| panic!("boom at launch"));
    assert!(panicked.is_err());
    log.flush();

    let folder = library_folder(AppEnvironment::Test).expect("library folder");
    assert!(folder.starts_with(home.path()), "{folder:?}");
    let written = std::fs::read_to_string(folder.join("diagnostics.log"))
        .expect("the log is on disk, in the library folder");
    assert!(written.contains("[boot] launched"), "{written}");
    assert!(written.contains("[panic] boom at launch"), "{written}");
}
