//! The app's AppKit hooks on a real Mac, in a GUI session (CI's macOS
//! runner): the menu bar and its edit target (`app_menu`), the delegate's
//! secure restorable state (`restorable_state`) and the intercepted
//! `terminate:` (`quit_request`).
//!
//! AppKit wants the main thread, so this test has no harness (`harness =
//! false` in Cargo.toml): `main` runs the checks on the main thread, prints
//! each, and exits non-zero when one fails. It drives events the way
//! `-[NSApplication run]` does: an event is posted to the queue, dequeued
//! (which makes it `NSApp.currentEvent`) and sent with `sendEvent:`, which
//! tries the key equivalents before the key window. A window that cannot
//! become key fails the test: nothing here is skipped quietly on a Mac.
//!
//! What it checks, in order:
//! 1. the delegate: one without `applicationSupportsSecureRestorableState:`
//!    answers YES once set (and is NSApp's delegate: AppKit's `setDelegate:`
//!    ran); one that answers NO keeps its answer;
//! 2. the menu bar: Cut, Copy, Paste, Select All, Undo and Redo carry ⌘X,
//!    ⌘C, ⌘V, ⌘A, ⌘Z and ⇧⌘Z, share one explicit target, and are enabled
//!    though nothing in the responder chain implements their actions (or,
//!    for Undo and Redo, has anything to undo); Quit is ⌘Q, Minimize ⌘M,
//!    Enter Full Screen ⌃⌘F;
//! 3. a winit-like view, key and first responder: ⌘C, ⌘V, ⌘X, ⌘A and ⌘Z
//!    each reach its `keyDown:` once, as the very event sent; a click on
//!    each of the six items (no key event current) hands it one ⌘-keyDown
//!    of the item's key, with its ANSI key code, and one keyUp without ⌘;
//! 4. an `NSTextField` being edited: ⌘A ⌘C copies its text to the general
//!    pasteboard, ⌘A ⌘V pastes over it, ⌘A ⌘X cuts it;
//! 5. `NSApp.terminate(nil)` returns, and the quit reaches
//!    `QuitRequests`' subscribers.
//!
//! Off macOS there is no AppKit, and `main` prints a skip line.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macos_gui: skipped (AppKit is macOS only)");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::process::exit;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
    use objc2::{
        available, define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly,
        Message,
    };
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
        NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSPasteboard,
        NSPasteboardTypeString, NSResponder, NSTextField, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{
        NSDate, NSDefaultRunLoopMode, NSPoint, NSProcessInfo, NSRange, NSRect, NSSize, NSString,
    };
    use vocara_services::quit_request::QuitRequests;
    use vocara_services::{app_menu, quit_request, restorable_state};

    /// How long a window gets to become key.
    const KEY_TIMEOUT: Duration = Duration::from_secs(15);
    /// How long the whole run may take before the watchdog ends it (a modal
    /// alert nobody answers, say).
    const WATCHDOG: Duration = Duration::from_secs(240);

    /// The step under way, for the watchdog's message.
    static STEP: Mutex<&str> = Mutex::new("start");

    fn step(name: &'static str) {
        *STEP.lock().unwrap_or_else(|e| e.into_inner()) = name;
        println!("macos_gui: {name}");
    }

    /// Ends the run, failed, if it is still going after [`WATCHDOG`].
    fn start_watchdog() {
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            let step = *STEP.lock().unwrap_or_else(|e| e.into_inner());
            eprintln!("macos_gui: FAIL: timed out during: {step}");
            // At once: the main thread is stuck, possibly inside AppKit.
            std::process::abort();
        });
    }

    /// A failure the run cannot go on after.
    fn fatal(what: &str) -> ! {
        eprintln!("macos_gui: FAIL: {what}");
        exit(1);
    }

    /// The checks, each printed as it is made; failures are listed at the
    /// end.
    #[derive(Default)]
    struct Checks {
        failed: Vec<String>,
    }

    impl Checks {
        fn check(&mut self, ok: bool, what: impl Into<String>) {
            let what = what.into();
            if ok {
                println!("  ok: {what}");
            } else {
                eprintln!("  FAIL: {what}");
                self.failed.push(what);
            }
        }
    }

    define_class!(
        /// An application delegate without
        /// `applicationSupportsSecureRestorableState:`, as winit's.
        // SAFETY: NSObject has no subclassing requirements; no `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "VocaraGuiTestPlainDelegate"]
        struct PlainDelegate;

        unsafe impl NSObjectProtocol for PlainDelegate {}

        unsafe impl NSApplicationDelegate for PlainDelegate {}
    );

    define_class!(
        /// An application delegate that answers NO itself.
        // SAFETY: NSObject has no subclassing requirements; no `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "VocaraGuiTestAnsweringDelegate"]
        struct AnsweringDelegate;

        // SAFETY: `-(BOOL)applicationSupportsSecureRestorableState:
        // (NSApplication *)app`.
        impl AnsweringDelegate {
            #[unsafe(method(applicationSupportsSecureRestorableState:))]
            fn supports_secure_restorable_state(&self, _app: &NSApplication) -> bool {
                false
            }
        }

        unsafe impl NSObjectProtocol for AnsweringDelegate {}

        unsafe impl NSApplicationDelegate for AnsweringDelegate {}
    );

    /// A key event a [`KeyView`] received.
    #[derive(Debug, Clone, PartialEq)]
    struct Key {
        chars: String,
        flags: NSEventModifierFlags,
        code: u16,
        time: f64,
    }

    impl Key {
        fn of(event: &NSEvent) -> Self {
            Key {
                chars: event
                    .characters()
                    .map(|chars| chars.to_string())
                    .unwrap_or_default(),
                flags: event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask,
                code: event.keyCode(),
                time: event.timestamp(),
            }
        }
    }

    #[derive(Default)]
    struct Received {
        downs: Vec<Key>,
        ups: Vec<Key>,
    }

    define_class!(
        /// A view like winit's: it takes first responder and handles
        /// `keyDown:` and `keyUp:` itself (here, it records them), and
        /// implements none of `cut:`, `copy:`, `paste:` and `selectAll:`.
        // SAFETY: NSView has no subclassing requirements beyond its
        // designated initializer, which `new` calls; no `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "VocaraGuiTestKeyView"]
        #[ivars = RefCell<Received>]
        struct KeyView;

        // SAFETY: the signatures of NSResponder's `acceptsFirstResponder`,
        // `keyDown:` and `keyUp:`, and of the `_wantsKeyDownForEvent:`
        // winit's view answers (`-(BOOL)_wantsKeyDownForEvent:(NSEvent *)`).
        impl KeyView {
            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }

            #[unsafe(method(keyDown:))]
            fn key_down(&self, event: &NSEvent) {
                self.ivars().borrow_mut().downs.push(Key::of(event));
            }

            #[unsafe(method(keyUp:))]
            fn key_up(&self, event: &NSEvent) {
                self.ivars().borrow_mut().ups.push(Key::of(event));
            }

            // As winit's view does (for Ctrl-Tab and Ctrl-Esc).
            #[unsafe(method(_wantsKeyDownForEvent:))]
            fn wants_key_down_for_event(&self, _event: &NSEvent) -> bool {
                true
            }
        }

        unsafe impl NSObjectProtocol for KeyView {}
    );

    impl KeyView {
        fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(RefCell::new(Received::default()));
            // SAFETY: `initWithFrame:` is NSView's designated initializer,
            // and the ivars are set.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        fn take(&self) -> Received {
            std::mem::take(&mut *self.ivars().borrow_mut())
        }
    }

    impl PlainDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            // SAFETY: `init` is NSObject's designated initializer, and the
            // ivars (none) are set.
            unsafe { msg_send![super(this), init] }
        }
    }

    impl AnsweringDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            // SAFETY: `init` is NSObject's designated initializer, and the
            // ivars (none) are set.
            unsafe { msg_send![super(this), init] }
        }
    }

    fn uptime() -> f64 {
        NSProcessInfo::processInfo().systemUptime()
    }

    /// Dequeues and sends one event, as `-[NSApplication run]` does,
    /// waiting at most `wait` seconds for one.
    fn pump_once(app: &NSApplication, wait: f64) -> Option<Retained<NSEvent>> {
        let until = NSDate::dateWithTimeIntervalSinceNow(wait);
        // SAFETY: an immutable NSString constant Foundation exports.
        let mode = unsafe { NSDefaultRunLoopMode };
        let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            mode,
            true,
        )?;
        app.sendEvent(&event);
        Some(event)
    }

    /// Runs the event loop until `done`, at most `timeout`.
    fn pump_until(app: &NSApplication, timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            pump_once(app, 0.05);
            app.updateWindows();
        }
    }

    /// Whether `next`, dequeued, is the event `sent` was posted as. Not by
    /// timestamp: AppKit keeps a made event's time in its CGEvent, in whole
    /// nanoseconds, so the seconds read back after the queue need not be the
    /// same double (macOS 26 runner: the posted ⌘C was dispatched, then not
    /// recognized). By what the event says instead.
    fn same_event(next: &NSEvent, sent: &NSEvent) -> bool {
        if next.r#type() != sent.r#type() {
            return false;
        }
        match sent.r#type() {
            NSEventType::KeyDown | NSEventType::KeyUp => {
                next.keyCode() == sent.keyCode()
                    && next.characters().map(|c| c.to_string())
                        == sent.characters().map(|c| c.to_string())
                    && (next.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask)
                        == (sent.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask)
            }
            NSEventType::ApplicationDefined => {
                next.subtype() == sent.subtype() && next.data1() == sent.data1()
            }
            _ => close_in_time(next.timestamp(), sent.timestamp()),
        }
    }

    /// Two event times that are the same instant, as AppKit hands them back.
    fn close_in_time(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    /// Sends `event` through the app as its run loop does: posted, dequeued
    /// (so it is `NSApp.currentEvent` while it is handled), then
    /// `sendEvent:`, key equivalents first. `Err` lists what came off the
    /// queue instead, for the failure message.
    fn dispatch(app: &NSApplication, event: &NSEvent) -> Result<(), String> {
        app.postEvent_atStart(event, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let Some(next) = pump_once(app, 0.1) else {
                continue;
            };
            if same_event(&next, event) {
                return Ok(());
            }
            if seen.len() < 20 {
                seen.push(format!(
                    "{:?} at {:.6} (sent at {:.6})",
                    next.r#type(),
                    next.timestamp(),
                    event.timestamp()
                ));
            }
        }
        Err(if seen.is_empty() {
            "nothing came off the queue".to_string()
        } else {
            format!("dequeued instead: {}", seen.join("; "))
        })
    }

    /// A keyDown of `key` with ⌘ for `window` (the ANSI key code given). Its
    /// time is 100 s in the past, so a keyDown the edit target made afresh
    /// (stamped now) cannot pass for it: only the event sent itself is within
    /// a millisecond of it.
    fn command_key(key: &str, code: u16, window: &NSWindow) -> Retained<NSEvent> {
        let chars = NSString::from_str(key);
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::ZERO,
            NSEventModifierFlags::Command,
            uptime() - 100.0,
            window.windowNumber(),
            None,
            &chars,
            &chars,
            false,
            code,
        )
        .unwrap_or_else(|| fatal(&format!("could not make a ⌘{key} keyDown")))
    }

    /// Sends ⌘`key` through the app, failing the run if it never went
    /// through the queue; returns the event sent.
    fn press(app: &NSApplication, key: &str, code: u16, window: &NSWindow) -> Retained<NSEvent> {
        let event = command_key(key, code, window);
        if let Err(seen) = dispatch(app, &event) {
            fatal(&format!(
                "the posted ⌘{key} keyDown never came back from the queue ({seen})"
            ));
        }
        event
    }

    /// Makes `NSApp.currentEvent` an event that is not a key event, as
    /// during a click on a menu item.
    fn no_key_event_current(app: &NSApplication) {
        // Each one tagged (`data1`), so `same_event` finds this one.
        static TAG: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(1);
        let tag = TAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::ZERO,
            NSEventModifierFlags::empty(),
            uptime(),
            0,
            None,
            0,
            tag,
            0,
        )
        .unwrap_or_else(|| fatal("could not make an application-defined event"));
        if let Err(seen) = dispatch(app, &event) {
            fatal(&format!(
                "the posted application-defined event never came back from the queue ({seen})"
            ));
        }
    }

    fn new_window(mtm: MainThreadMarker, title: &str, content: &NSView) -> Retained<NSWindow> {
        let frame = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(420.0, 160.0));
        // SAFETY: a plain titled window; the `Retained` is its only owner,
        // as `setReleasedWhenClosed(false)` below makes sure before it can
        // be closed.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                frame,
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: see above; closing must not release what the `Retained`
        // owns.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        window.setContentView(Some(content));
        window
    }

    fn is_key(app: &NSApplication, window: &NSWindow) -> bool {
        window.isKeyWindow()
            && app
                .keyWindow()
                .is_some_and(|key| std::ptr::eq(&*key, window))
    }

    /// Makes `window` key with `responder` its first responder, or fails
    /// the run: the key checks mean nothing without a key window.
    fn make_key(app: &NSApplication, window: &NSWindow, responder: &NSResponder, what: &str) {
        let mut last_ask = None::<Instant>;
        let became = pump_until(app, KEY_TIMEOUT, || {
            if is_key(app, window) {
                return true;
            }
            if last_ask.is_none_or(|at| at.elapsed() > Duration::from_millis(500)) {
                last_ask = Some(Instant::now());
                // As winit asks, and on macOS 14+ with the newer call too.
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
                if available!(macos = 14.0) {
                    app.activate();
                }
                window.makeKeyAndOrderFront(None);
                window.orderFrontRegardless();
            }
            false
        });
        if !became {
            fatal(&format!(
                "the {what} window never became key in {}s (app active: {}); \
                 the GUI session must let a test app activate and take the keyboard",
                KEY_TIMEOUT.as_secs(),
                app.isActive()
            ));
        }
        if !window.makeFirstResponder(Some(responder)) {
            fatal(&format!("the {what} window refused its first responder"));
        }
        pump_until(app, Duration::from_millis(200), || false);
    }

    /// The first item with `action` in `menu` and its submenus, with the
    /// menu that holds it.
    fn find(menu: &NSMenu, action: Sel) -> Option<(Retained<NSMenu>, Retained<NSMenuItem>)> {
        for index in 0..menu.numberOfItems() {
            let Some(item) = menu.itemAtIndex(index) else {
                continue;
            };
            if item.action() == Some(action) {
                return Some((menu.retain(), item));
            }
            if let Some(found) = item.submenu().and_then(|sub| find(&sub, action)) {
                return Some(found);
            }
        }
        None
    }

    pub fn run() {
        let mtm = MainThreadMarker::new()
            .unwrap_or_else(|| fatal("main is not on the main thread (harness = false?)"));
        start_watchdog();
        let mut checks = Checks::default();
        println!(
            "macos_gui: macOS {}",
            NSProcessInfo::processInfo().operatingSystemVersionString()
        );

        step("setting up the app");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        quit_request::intercept();
        restorable_state::declare_secure();
        // A delegate that answers keeps its answer.
        let answering = AnsweringDelegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*answering)));
        let restorable = sel!(applicationSupportsSecureRestorableState:);
        // SAFETY: `-(BOOL)applicationSupportsSecureRestorableState:
        // (NSApplication *)`, which `AnsweringDelegate` implements.
        let answer: bool =
            unsafe { msg_send![&*answering, applicationSupportsSecureRestorableState: &*app] };
        checks.check(
            !answer,
            "a delegate that answers the restorable-state question keeps its answer (NO)",
        );
        // The delegate the run keeps: one without the method, as winit's.
        let delegate = PlainDelegate::new(mtm);
        let lacked = !delegate.respondsToSelector(restorable);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        app.finishLaunching();
        // After launch, as the app does (winit sets its menu at launch).
        app_menu::install();

        step("checking the delegate");
        checks.check(
            lacked,
            "the test delegate lacks the restorable-state method before it is set",
        );
        let is_delegate = app.delegate().is_some_and(|set| {
            std::ptr::eq(
                std::ptr::from_ref(&*set).cast::<AnyObject>(),
                std::ptr::from_ref(&*delegate).cast::<AnyObject>(),
            )
        });
        checks.check(
            is_delegate,
            "AppKit's setDelegate: ran: NSApp.delegate is the test delegate",
        );
        let responds = delegate.respondsToSelector(restorable);
        checks.check(
            responds,
            "the delegate responds to applicationSupportsSecureRestorableState:",
        );
        if responds {
            // SAFETY: the delegate responds to it (checked), with the
            // method `restorable_state` added: `-(BOOL)…:(NSApplication *)`.
            let answer: bool =
                unsafe { msg_send![&*delegate, applicationSupportsSecureRestorableState: &*app] };
            checks.check(answer, "the delegate answers YES");
        }

        step("checking the menu bar");
        let Some(bar) = app.mainMenu() else {
            fatal("NSApp has no main menu after app_menu::install");
        };
        let mut edit_items = Vec::new();
        for (title, action, key, mask) in [
            ("Cut", c"cut:", "x", NSEventModifierFlags::Command),
            ("Copy", c"copy:", "c", NSEventModifierFlags::Command),
            ("Paste", c"paste:", "v", NSEventModifierFlags::Command),
            (
                "Select All",
                c"selectAll:",
                "a",
                NSEventModifierFlags::Command,
            ),
            ("Undo", c"undo:", "z", NSEventModifierFlags::Command),
            ("Redo", c"redo:", "Z", NSEventModifierFlags::Command),
            ("Quit ", c"terminate:", "q", NSEventModifierFlags::Command),
            (
                "Minimize",
                c"performMiniaturize:",
                "m",
                NSEventModifierFlags::Command,
            ),
            (
                "Enter Full Screen",
                c"toggleFullScreen:",
                "f",
                NSEventModifierFlags::Control | NSEventModifierFlags::Command,
            ),
        ] {
            let name = action.to_str().unwrap_or("?");
            let Some((menu, item)) = find(&bar, Sel::register(action)) else {
                checks.check(false, format!("an item sends {name}"));
                continue;
            };
            let item_title = item.title().to_string();
            let title_ok = if title.ends_with(' ') {
                item_title.starts_with(title)
            } else {
                item_title == title
            };
            checks.check(title_ok, format!("{name} is \"{item_title}\""));
            let item_key = item.keyEquivalent().to_string();
            let item_mask = item.keyEquivalentModifierMask();
            // macOS 26 gives the standard full-screen item the system's own
            // shortcut, fn-F (Globe-F), in every app (Flutter's included):
            // AppKit rewrote the xib's ⌃⌘F on the CI runner.
            let system_full_screen =
                name == "toggleFullScreen:" && item_mask == NSEventModifierFlags::Function;
            checks.check(
                item_key == key && (item_mask == mask || system_full_screen),
                format!("\"{item_title}\" has key \"{item_key}\" with {item_mask:?}, the xib's {mask:?} {key}"),
            );
            if matches!(
                name,
                "cut:" | "copy:" | "paste:" | "selectAll:" | "undo:" | "redo:"
            ) {
                edit_items.push((menu, item));
            }
        }
        if let [(edit, _), ..] = edit_items.as_slice() {
            // Validates every item, as AppKit does before it shows the menu
            // or matches a key equivalent.
            edit.update();
        }
        let targets: Vec<_> = edit_items.iter().map(|(_, item)| item.target()).collect();
        let first = targets.first().cloned().flatten();
        checks.check(
            first.is_some()
                && targets.iter().all(|target| match (target, &first) {
                    (Some(target), Some(first)) => std::ptr::eq(&**target, &**first),
                    _ => false,
                }),
            "Cut, Copy, Paste, Select All, Undo and Redo share one explicit target",
        );
        for (_, item) in &edit_items {
            checks.check(
                item.isEnabled(),
                format!(
                    "\"{}\" is enabled with no responder that implements it",
                    item.title()
                ),
            );
        }

        step("⌘ keys in a winit-like window");
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(420.0, 160.0));
        let view = KeyView::new(mtm, frame);
        let window = new_window(mtm, "Vocara GUI test: keys", &view);
        make_key(&app, &window, &view, "key view");
        for (key, code) in [("c", 8), ("v", 9), ("x", 7), ("a", 0), ("z", 6)] {
            let sent = press(&app, key, code, &window);
            let received = view.take();
            let downs: Vec<_> = received
                .downs
                .iter()
                .filter(|down| {
                    down.chars == key && down.flags.contains(NSEventModifierFlags::Command)
                })
                .collect();
            checks.check(
                downs.len() == 1 && received.downs.len() == 1,
                format!(
                    "⌘{key} reaches the view's keyDown: once (got {:?})",
                    received.downs
                ),
            );
            checks.check(
                downs
                    .first()
                    .is_some_and(|down| close_in_time(down.time, sent.timestamp())),
                format!("⌘{key} reaches the view as the keyDown that was sent"),
            );
        }

        step("Edit items clicked in a winit-like window");
        for (title, key, code) in [
            ("Copy", "c", 8),
            ("Paste", "v", 9),
            ("Cut", "x", 7),
            ("Select All", "a", 0),
            ("Undo", "z", 6),
            ("Redo", "Z", 6),
        ] {
            let Some((menu, item)) = edit_items
                .iter()
                .find(|(_, item)| item.title().to_string() == title)
            else {
                continue;
            };
            no_key_event_current(&app);
            menu.performActionForItemAtIndex(menu.indexOfItem(item));
            pump_until(&app, Duration::from_millis(100), || false);
            let received = view.take();
            checks.check(
                received.downs.len() == 1
                    && received.downs.iter().all(|down| {
                        down.chars == key
                            && down.code == code
                            && down.flags.contains(NSEventModifierFlags::Command)
                    }),
                format!(
                    "clicking {title} hands the view one ⌘{key} keyDown (got {:?})",
                    received.downs
                ),
            );
            checks.check(
                received.ups.len() == 1
                    && received.ups.iter().all(|up| {
                        up.chars == key && !up.flags.contains(NSEventModifierFlags::Command)
                    }),
                format!(
                    "clicking {title} hands the view one {key} keyUp without ⌘ (got {:?})",
                    received.ups
                ),
            );
        }

        step("⌘ keys in a native text field");
        let field = NSTextField::textFieldWithString(&NSString::from_str("hello"), mtm);
        field.setFrame(NSRect::new(
            NSPoint::new(20.0, 70.0),
            NSSize::new(300.0, 24.0),
        ));
        let content = NSView::initWithFrame(mtm.alloc(), frame);
        content.addSubview(&field);
        let field_window = new_window(mtm, "Vocara GUI test: text field", &content);
        make_key(&app, &field_window, &field, "text field");
        checks.check(
            field.currentEditor().is_some(),
            "the text field is being edited (field editor active)",
        );
        let pasteboard = NSPasteboard::generalPasteboard();
        // SAFETY: an immutable NSString constant AppKit exports.
        let string_type = unsafe { NSPasteboardTypeString };
        let pasted = || pasteboard.stringForType(string_type).map(|s| s.to_string());
        // The insertion point at the start, nothing selected: only ⌘A
        // selects the text the next key works on.
        let select_none = || {
            if let Some(editor) = field.currentEditor() {
                editor.setSelectedRange(NSRange::new(0, 0));
            }
        };
        let text = || {
            field.validateEditing();
            field.stringValue().to_string()
        };
        pasteboard.clearContents();
        select_none();
        press(&app, "a", 0, &field_window);
        press(&app, "c", 8, &field_window);
        let copied = pasted();
        checks.check(
            copied.as_deref() == Some("hello"),
            format!("⌘A ⌘C copies the field's text (pasteboard: {copied:?})"),
        );
        pasteboard.clearContents();
        pasteboard.setString_forType(&NSString::from_str("world"), string_type);
        select_none();
        press(&app, "a", 0, &field_window);
        press(&app, "v", 9, &field_window);
        let after_paste = text();
        checks.check(
            after_paste == "world",
            format!("⌘A ⌘V pastes over it (field: {after_paste:?})"),
        );
        pasteboard.clearContents();
        select_none();
        press(&app, "a", 0, &field_window);
        press(&app, "x", 7, &field_window);
        let (after_cut, cut) = (text(), pasted());
        checks.check(
            after_cut.is_empty() && cut.as_deref() == Some("world"),
            format!("⌘A ⌘X cuts it (field: {after_cut:?}, pasteboard: {cut:?})"),
        );

        step("quitting");
        let mut asked = QuitRequests::global().subscribe();
        app.terminate(None);
        pump_until(&app, Duration::from_millis(200), || false);
        checks.check(true, "NSApp.terminate(nil) returned: the process goes on");
        checks.check(
            asked.try_recv().is_ok(),
            "the quit reached quit_request's subscribers",
        );

        window.orderOut(None);
        field_window.orderOut(None);
        if checks.failed.is_empty() {
            println!("macos_gui: all checks passed");
            exit(0);
        }
        eprintln!("macos_gui: {} check(s) failed:", checks.failed.len());
        for failure in &checks.failed {
            eprintln!("  - {failure}");
        }
        exit(1);
    }
}
