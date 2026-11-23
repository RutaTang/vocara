//! Port of the `attachGlobalHooks` test of
//! `client/test/diagnostics_log_test.dart`, in a binary of its own: the
//! panic hook it installs is process-wide.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use vocara_model::diagnostics::{DiagnosticsLog, DiagnosticsOptions};

#[test]
fn attach_global_hooks_logs_uncaught_panics_and_forwards_to_the_previous_hook() {
    let dir = tempfile::tempdir().unwrap();
    let log = DiagnosticsLog::for_testing(DiagnosticsOptions::default());
    let forwards = Arc::new(AtomicUsize::new(0));
    {
        let forwards = Arc::clone(&forwards);
        std::panic::set_hook(Box::new(move |_| {
            forwards.fetch_add(1, Ordering::SeqCst);
        }));
    }
    log.attach_global_hooks(Some(dir.path()));
    // A second attach rebinds but never chains a second hook.
    log.attach_global_hooks(Some(dir.path()));

    let joined = thread::spawn(|| panic!("boom widget")).join();
    assert!(joined.is_err());
    let joined = thread::spawn(|| {
        let v: Vec<u8> = Vec::new();
        v[3]
    })
    .join();
    assert!(joined.is_err());

    // The previous hook still ran, once per panic.
    assert_eq!(forwards.load(Ordering::SeqCst), 2);

    log.flush();
    let content = fs::read_to_string(dir.path().join("diagnostics.log")).unwrap();
    assert!(content.contains("[panic] boom widget"), "{content}");
    assert!(content.contains("index out of bounds"), "{content}");
    let _ = std::panic::take_hook();
}
