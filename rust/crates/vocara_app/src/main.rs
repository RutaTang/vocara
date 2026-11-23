//! Vocara (port of `main.dart`): installs the diagnostics panic hook, then
//! runs the iced app with the library open and the quit hooks.
//!
//! What `main.dart` did and where it went:
//! - the exit guard and the window-close guard → the app's quit guard
//!   (`app::quit`), on the window's close request;
//! - `DiagnosticsLog.attachGlobalHooks` → the panic hook below (every
//!   uncaught panic is logged, then the default hook runs);
//! - `MediaKit.ensureInitialized` → nothing: the technique screens' players
//!   are AVFoundation's, which need no global setup;
//! - the Runner's launch-time microphone request (`AppDelegate.swift`) → the
//!   request below;
//! - the frameless window, its title and the prevented close →
//!   `app::window_settings` and `App::title`;
//! - `Persistence.open` with its refusals and failures → the app's boot
//!   (`app::library`), with the fatal windows;
//! - the reveal-after-first-frame channel → iced shows the window once its
//!   first UI is built.

use std::sync::Arc;

use vocara_app::app::{self, library, quit, Boot};
use vocara_model::diagnostics::DiagnosticsLog;

fn main() -> iced::Result {
    // Diagnostics first: from here on, every uncaught panic is appended to
    // the diagnostics log (then handed to the default hook), so a field
    // failure leaves a trace the export can bundle.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        DiagnosticsLog::instance().log("panic", &info.to_string());
        default_hook(info);
    }));

    // Ask for the microphone at launch, as the Runner did: raw CoreAudio
    // capture never prompts and is fed silence without the grant, so the
    // prompt (or an existing grant) is settled before any capture starts.
    // The request blocks on the user's answer, so it runs off the UI thread;
    // the mic actions ask again (and report a refusal) when they start.
    std::thread::spawn(|| {
        let _ = vocara_services::mic_permission::request();
    });

    let boot = Boot::from_env(library::open_library, Arc::new(quit::ServiceQuitHooks));
    app::run(boot)
}
