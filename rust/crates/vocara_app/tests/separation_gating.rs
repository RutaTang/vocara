//! Port of `client/test/separation_gating_test.dart`: which separation engine
//! each environment is wired to, and the prod engine's dark behaviour.
//!
//! Dart checked the factory's result by type (`isA<...>`). The trait objects
//! here have no type to test, so each engine is told apart by what only it
//! does: the Null engine refuses every run as unsupported, while the
//! Python-sidecar prototype registers a run that a cancel then aborts (no
//! process is ever spawned: the run is cancelled before it starts).

use std::path::Path;
use std::sync::Arc;

use vocara_app::services::create_source_separation_service;
use vocara_model::config::environment::AppEnvironment;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::source_separation_service::{
    NullSourceSeparationService, SeparationError, SourceSeparationService,
};

fn storage(dir: &tempfile::TempDir) -> Arc<LocalStorageService> {
    Arc::new(LocalStorageService::with_root(dir.path()))
}

#[test]
fn prod_is_wired_to_the_null_engine() {
    // The dead feature stays dark.
    let dir = tempfile::tempdir().unwrap();
    let svc = create_source_separation_service(AppEnvironment::Prod, storage(&dir));
    assert!(!svc.is_available());
    assert!(matches!(
        svc.separate(Path::new("/no/such/song.wav"), None),
        Err(SeparationError::Unsupported(_))
    ));
}

#[test]
fn dev_and_test_keep_the_python_sidecar_prototype() {
    for env in [AppEnvironment::Dev, AppEnvironment::Test] {
        let dir = tempfile::tempdir().unwrap();
        let svc = create_source_separation_service(env, storage(&dir));
        let ticket = svc.begin();
        svc.cancel();
        let outcome = svc.separate_with(ticket, Path::new("/no/such/song.wav"), None);
        assert!(
            matches!(outcome, Err(SeparationError::Cancelled)),
            "{env:?} should keep the prototype engine, got {outcome:?}"
        );
    }
}

// The availability probe runs in the detail view model's constructor on
// every practice open: in prod it must resolve false without spawning any
// process.
#[test]
fn the_null_engine_reports_unavailable() {
    assert!(!NullSourceSeparationService.is_available());
}

#[test]
fn the_null_engine_refuses_to_separate() {
    // The guard behind the hidden UI.
    assert!(matches!(
        NullSourceSeparationService.separate(Path::new("/no/such/song.wav"), None),
        Err(SeparationError::Unsupported(_))
    ));
}

#[test]
fn the_null_engines_cancel_is_a_safe_no_op() {
    NullSourceSeparationService.cancel();
}
