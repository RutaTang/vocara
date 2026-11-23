//! The quit guard's seam: what must happen before the process goes (port of
//! `main.dart`'s `_QuitGuard` and its exit listener).
//!
//! Two things run on every quit, in this order:
//! 1. kill any in-flight separation sidecar (`SeparationQuitGuard.instance
//!    .killAll()`, a synchronous SIGKILL): it is a child process that macOS
//!    reparents rather than kills on app exit, so without this it would keep
//!    burning CPU/RAM for minutes. Its orphaned `.tmp` output is swept on the
//!    next launch.
//! 2. finalize (save) every in-flight recording (`RecordingFinalizer.instance
//!    .finalizeAll()`), instead of losing it as a 0-length take.
//!
//! The app runs with [`ServiceQuitHooks`], which calls the two services;
//! tests use [`NoQuitHooks`] or their own counting hooks.

use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::source_separation_service::SeparationQuitGuard;

/// What the quit guard calls before the app exits. Called from the UI thread
/// (`kill_separation_sidecars`) and then a blocking worker thread
/// (`finalize_recordings`).
pub trait QuitHooks: Send + Sync {
    /// Kills every in-flight separation sidecar, synchronously.
    fn kill_separation_sidecars(&self);

    /// Finalizes every in-flight recording; returns when they are saved.
    fn finalize_recordings(&self);
}

/// Hooks with nothing to do (no services yet, or a test).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoQuitHooks;

impl QuitHooks for NoQuitHooks {
    fn kill_separation_sidecars(&self) {}
    fn finalize_recordings(&self) {}
}

/// The real hooks: the process-wide separation guard and recording finalizer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ServiceQuitHooks;

impl QuitHooks for ServiceQuitHooks {
    fn kill_separation_sidecars(&self) {
        SeparationQuitGuard::instance().kill_all();
    }

    fn finalize_recordings(&self) {
        RecordingFinalizer::instance().finalize_all();
    }
}
