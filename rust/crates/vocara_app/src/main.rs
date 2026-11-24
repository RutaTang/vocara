//! Vocara (port of `main.dart`): attaches the diagnostics log, then runs the
//! iced app with the library open and the quit hooks.
//!
//! What `main.dart` did and where it went:
//! - the exit guard and the window-close guard → the app's quit guard
//!   (`app::quit`), on the window's close request and on the quits macOS
//!   sends through `NSApplication.terminate:` (Cmd+Q, the app-menu Quit, a
//!   Dock quit, logout), which the Runner's `AppDelegate` held for it →
//!   `vocara_services::quit_request`, installed below;
//! - `DiagnosticsLog.attachGlobalHooks` → [`app::attach_diagnostics`] (the
//!   log bound to the library folder on disk; every uncaught panic is
//!   logged, then the default hook runs);
//! - `MediaKit.ensureInitialized` → nothing: the technique screens' players
//!   are AVFoundation's, which need no global setup;
//! - the Runner's launch-time microphone request (`AppDelegate.swift`) → the
//!   request below;
//! - the frameless window, its title and the prevented close →
//!   `app::window_settings` and `App::title`;
//! - `Persistence.open` with its refusals and failures → the app's boot
//!   (`app::library`), with the fatal windows;
//! - the reveal-after-first-frame channel → iced shows the window once its
//!   first UI is built;
//! - the Runner's menu bar (`MainMenu.xib`) → `vocara_services::app_menu`,
//!   installed when the window opens;
//! - the Runner's `AppDelegate.applicationSupportsSecureRestorableState` →
//!   `vocara_services::restorable_state`, declared below on whatever
//!   delegate winit sets.

use std::sync::Arc;

use vocara_app::app::{self, library, quit, Boot};

fn main() -> iced::Result {
    // Diagnostics first: from here on, every entry is appended to the
    // rotating on-disk log and every uncaught panic is logged (then handed to
    // the default hook), so a field failure leaves a trace the export can
    // bundle.
    app::attach_diagnostics();

    // Ask for the microphone at launch, as the Runner did: raw CoreAudio
    // capture never prompts and is fed silence without the grant, so the
    // prompt (or an existing grant) is settled before any capture starts.
    // The request blocks on the user's answer, so it runs off the UI thread;
    // the mic actions ask again (and report a refusal) when they start.
    std::thread::spawn(|| {
        let _ = vocara_services::mic_permission::request();
    });

    // Cmd+Q, the app-menu Quit, a Dock quit and logout go through
    // NSApplication.terminate:, which never reaches the window's close
    // request: route them to the quit guard too, before AppKit could end the
    // process with a recording or a save still in flight.
    vocara_services::quit_request::intercept();

    // Answer AppKit's launch-time question about secure restorable state,
    // as the Runner's AppDelegate did (macOS 14+ warns otherwise). The
    // delegate is winit's, set as the event loop is created inside
    // `app::run`: hook `setDelegate:` before that.
    vocara_services::restorable_state::declare_secure();

    let boot = Boot::from_env(library::open_library, Arc::new(quit::ServiceQuitHooks));
    app::run(boot)
}
