//! What the app's macOS GUI tests share (`macos_file_panels.rs`,
//! `macos_quit.rs`): the step log, the checks, the watchdog, a window and
//! the key events, as `vocara_services`' `tests/macos_gui.rs` has them.
//! Each test uses part of it.
#![allow(dead_code)]

use std::process::exit;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSProcessInfo, NSRect, NSSize, NSString};

/// The test's name, the prefix of every line it prints.
static NAME: OnceLock<&'static str> = OnceLock::new();

/// The step under way, for the watchdog's message.
static STEP: Mutex<String> = Mutex::new(String::new());

fn name() -> &'static str {
    NAME.get().copied().unwrap_or("macos test")
}

/// Names the run and starts its watchdog: past `limit`, the run ends,
/// failed, naming the step it was stuck in (a panel or an alert nobody
/// answers, say).
pub fn start(test: &'static str, limit: Duration) {
    let _ = NAME.set(test);
    println!(
        "{test}: macOS {}",
        NSProcessInfo::processInfo().operatingSystemVersionString()
    );
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        let step = STEP.lock().unwrap_or_else(|e| e.into_inner()).clone();
        eprintln!(
            "{test}: FAIL: timed out after {}s during: {step}",
            limit.as_secs()
        );
        // At once: the main thread is stuck, possibly inside AppKit.
        std::process::abort();
    });
}

/// Starts a step: printed, and named by the watchdog if it never ends.
pub fn step(what: impl Into<String>) {
    let what = what.into();
    println!("{}: {what}", name());
    *STEP.lock().unwrap_or_else(|e| e.into_inner()) = what;
}

/// Something worth knowing that is not a check.
pub fn note(what: impl AsRef<str>) {
    println!("  note: {}", what.as_ref());
}

/// A failure the run cannot go on after.
pub fn fatal(what: &str) -> ! {
    eprintln!("{}: FAIL: {what}", name());
    exit(1);
}

/// The checks, each printed as it is made; failures are listed at the end.
#[derive(Debug, Default)]
pub struct Checks {
    failed: Vec<String>,
}

impl Checks {
    /// Records a check; returns `ok`.
    pub fn check(&mut self, ok: bool, what: impl Into<String>) -> bool {
        let what = what.into();
        if ok {
            println!("  ok: {what}");
        } else {
            eprintln!("  FAIL: {what}");
            self.failed.push(what);
        }
        ok
    }

    /// Ends the run: exit 0 when every check passed, else lists the
    /// failures and exits 1.
    pub fn finish(&self) -> ! {
        if self.failed.is_empty() {
            println!("{}: all checks passed", name());
            exit(0);
        }
        eprintln!("{}: {} check(s) failed:", name(), self.failed.len());
        for failure in &self.failed {
            eprintln!("  - {failure}");
        }
        exit(1);
    }
}

/// The system's uptime, the clock event timestamps are on.
pub fn uptime() -> f64 {
    NSProcessInfo::processInfo().systemUptime()
}

/// A plain titled window (as the app's, it can become key and main), with
/// an empty view; not shown yet. The `Retained` is its only owner.
pub fn new_window(mtm: MainThreadMarker, title: &str, origin: NSPoint) -> Retained<NSWindow> {
    let frame = NSRect::new(origin, NSSize::new(640.0, 480.0));
    // SAFETY: a plain titled window; the `Retained` is its only owner, as
    // `setReleasedWhenClosed(false)` below makes sure before it can be
    // closed.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: see above; closing must not release what the `Retained` owns.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    let content = NSView::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 480.0)),
    );
    window.setContentView(Some(&content));
    window
}

/// A key event of `ty` for the window numbered `window`: `chars` typed
/// with `flags` held, the ANSI key code `code`, stamped now.
pub fn key_event(
    ty: NSEventType,
    chars: &str,
    code: u16,
    flags: NSEventModifierFlags,
    window: isize,
) -> Retained<NSEvent> {
    let text = NSString::from_str(chars);
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        ty,
        NSPoint::ZERO,
        flags,
        uptime(),
        window,
        None,
        &text,
        &text,
        false,
        code,
    )
    .unwrap_or_else(|| fatal(&format!("could not make a {ty:?} event for {chars:?}")))
}

/// `kVK_Escape`.
pub const ESCAPE_KEY: u16 = 53;
/// `kVK_ANSI_Q`.
pub const Q_KEY: u16 = 12;
