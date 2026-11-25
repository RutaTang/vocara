//! Cmd+Q while a take is armed, on a real Mac (CI's macOS runner has a GUI
//! session): the quit path `main` installs (`quit_request::intercept`, the
//! `terminate:` AppKit's Quit item sends) into the app's own quit guard
//! (`App`'s `QuitRequested` subscription and `quit`, with the real
//! `ServiceQuitHooks`, so the real `RecordingFinalizer::instance`), over a
//! stand-in for an armed take registered with that finalizer as a recording
//! controller registers one.
//!
//! The app does not answer `applicationShouldTerminate:` with
//! `NSTerminateLater`: its `terminate:` never reaches AppKit's at all (see
//! `vocara_services::quit_request`), the guard runs, and the app exits by
//! itself (`iced::exit`) once the recordings are saved. So "deferred" here
//! is: AppKit's own `terminate:` never runs. The test's application
//! delegate answers `applicationShouldTerminate:` (which AppKit's
//! `terminate:` asks before it ends the process) with `NSTerminateCancel`
//! and counts the questions: zero means AppKit's terminate never ran, and
//! the answer keeps the test process alive the one time it is meant to.
//!
//! What it checks, in order:
//! 1. ⌘Q, posted as typed and sent as `-[NSApplication run]` sends it,
//!    leaves the process running, asks the delegate nothing, and reaches the
//!    app as `Message::QuitRequested` through its subscription; nothing is
//!    finalized yet;
//! 2. the app's guard (`update(QuitRequested)`) calls the take's finalizer
//!    and does not finish while the take saves;
//! 3. a second ⌘Q meanwhile reaches neither AppKit nor the app, and the
//!    guard does not run again;
//! 4. the guard finishes (`QuitGuardDone`) only after the take's file
//!    landed, the take is no longer registered, and `QuitGuardDone` exits
//!    the app (`iced::exit`);
//! 5. a ⌘Q after `quit_request::REPEAT_WINDOW` (65 s), as if the guard had
//!    never answered, goes through to AppKit's own `terminate:` (the
//!    original implementation the hook calls), which asks the delegate
//!    (waited out in the run without `device` only: the path is the same in
//!    both, and CI runs both).
//!
//! It shows a window for a little over a minute and takes the keyboard.
//! Off macOS it prints a skip line.

#[cfg(target_os = "macos")]
mod macos_support;

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macos_quit: skipped (AppKit is macOS only)");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant};

    use iced::advanced::subscription::{into_recipes, EventStream};
    use iced::futures::StreamExt;
    use iced::Task;
    use iced_test::runtime::Action;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject, Sel};
    use objc2::{
        available, define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly,
    };
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
        NSApplicationTerminateReply, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
        NSMenu, NSWindow,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint};
    use vocara_app::app::quit::ServiceQuitHooks;
    use vocara_app::app::{App, AppEnvironment, Boot, Message};
    use vocara_services::app_menu;
    use vocara_services::quit_request::{self, REPEAT_WINDOW};
    use vocara_services::recording_finalizer::{Finalizer, RecordingFinalizer};

    use crate::macos_support::{fatal, key_event, new_window, note, start, step, Checks, Q_KEY};

    /// How long the whole run may take (it waits out the 65 s window).
    const LIMIT: Duration = Duration::from_secs(240);
    /// How long the window gets to become key.
    const KEY_TIMEOUT: Duration = Duration::from_secs(15);
    /// How long the stand-in take's stop and save take.
    const SAVE_TIME: Duration = Duration::from_millis(1500);
    /// How long the app gets to hear a quit, and the guard to call the take.
    const REACH: Duration = Duration::from_secs(5);
    /// How long the guard gets to finish: the finalizer's budgets (5 s per
    /// finalizer, 8 s in all) and then some.
    const GUARD: Duration = Duration::from_secs(20);

    define_class!(
        /// An application delegate that answers `applicationShouldTerminate:`
        /// with `NSTerminateCancel`, counting the questions (see the file's
        /// docs).
        // SAFETY: NSObject has no subclassing requirements; no `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "VocaraQuitTestDelegate"]
        #[ivars = Cell<usize>]
        struct QuitDelegate;

        unsafe impl NSObjectProtocol for QuitDelegate {}

        // SAFETY: `-(NSApplicationTerminateReply)applicationShouldTerminate:
        // (NSApplication *)sender`, the protocol's signature.
        unsafe impl NSApplicationDelegate for QuitDelegate {
            #[unsafe(method(applicationShouldTerminate:))]
            fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
                self.ivars().set(self.ivars().get() + 1);
                NSApplicationTerminateReply::TerminateCancel
            }
        }
    );

    impl QuitDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(Cell::new(0));
            // SAFETY: `init` is NSObject's designated initializer, and the
            // ivars are set.
            unsafe { msg_send![super(this), init] }
        }

        /// How many times AppKit's `terminate:` asked.
        fn asked(&self) -> usize {
            self.ivars().get()
        }
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

    /// Runs the event loop until `rx` has something, at most `timeout`.
    fn pump_recv<T>(app: &NSApplication, rx: &Receiver<T>, timeout: Duration) -> Option<T> {
        let mut got = None;
        pump_until(app, timeout, || {
            if got.is_none() {
                got = rx.try_recv().ok();
            }
            got.is_some()
        });
        got
    }

    /// Whether `next`, dequeued, is the key event `sent` was posted as (by
    /// what it says, not by its time: see macos_gui.rs).
    fn same_key(next: &NSEvent, sent: &NSEvent) -> bool {
        next.r#type() == sent.r#type()
            && next.keyCode() == sent.keyCode()
            && next.characters().map(|c| c.to_string()) == sent.characters().map(|c| c.to_string())
            && (next.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask)
                == (sent.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask)
    }

    /// Posts ⌘Q for `window` and runs the loop until it came off the queue
    /// and was sent (key equivalents first: the menu's Quit), as a typed
    /// ⌘Q is. Fails the run if it never comes back.
    fn command_q(app: &NSApplication, window: &NSWindow) {
        let flags = NSEventModifierFlags::Command;
        let down = key_event(
            NSEventType::KeyDown,
            "q",
            Q_KEY,
            flags,
            window.windowNumber(),
        );
        app.postEvent_atStart(&down, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(next) = pump_once(app, 0.1) {
                if same_key(&next, &down) {
                    return;
                }
            }
        }
        fatal("the posted ⌘Q keyDown never came back from the queue");
    }

    /// The first item with `action` in `menu` and its submenus.
    fn find(menu: &NSMenu, action: Sel) -> Option<(String, String, NSEventModifierFlags)> {
        for index in 0..menu.numberOfItems() {
            let Some(item) = menu.itemAtIndex(index) else {
                continue;
            };
            if item.action() == Some(action) {
                return Some((
                    item.title().to_string(),
                    item.keyEquivalent().to_string(),
                    item.keyEquivalentModifierMask(),
                ));
            }
            if let Some(found) = item.submenu().and_then(|sub| find(&sub, action)) {
                return Some(found);
            }
        }
        None
    }

    /// Makes `window` key, or fails the run.
    fn make_key(app: &NSApplication, window: &NSWindow) {
        let mut last_ask = None::<Instant>;
        let became = pump_until(app, KEY_TIMEOUT, || {
            if window.isKeyWindow() {
                return true;
            }
            if last_ask.is_none_or(|at| at.elapsed() > Duration::from_millis(500)) {
                last_ask = Some(Instant::now());
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
                "the window never became key in {}s (app active: {}); \
                 the GUI session must let a test app activate and take the keyboard",
                KEY_TIMEOUT.as_secs(),
                app.isActive()
            ));
        }
    }

    /// The app's subscriptions, run on a thread of their own with no window
    /// events: what they output comes back on the receiver.
    fn listen(app: &App) -> Receiver<Message> {
        // Started here: `Subscription::run` subscribes as its stream is made.
        let streams: Vec<_> = into_recipes(app.subscription())
            .into_iter()
            .map(|recipe| {
                let no_events: EventStream = iced::futures::stream::pending().boxed();
                recipe.stream(no_events)
            })
            .collect();
        let (sender, messages) = mpsc::channel();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|e| fatal(&format!("no runtime for the subscriptions: {e}")));
            runtime.block_on(async move {
                let mut all = iced::futures::stream::select_all(streams);
                while let Some(message) = all.next().await {
                    if sender.send(message).is_err() {
                        break;
                    }
                }
            });
        });
        messages
    }

    /// Runs `task` to its end on a thread of its own: its outputs, each with
    /// when it came, are sent as they come.
    fn run_task(task: Task<Message>) -> Receiver<(Instant, Message)> {
        let (sender, outputs) = mpsc::channel();
        thread::spawn(move || {
            let Some(mut stream) = iced_test::runtime::task::into_stream(task) else {
                return;
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|e| fatal(&format!("no runtime for the task: {e}")));
            runtime.block_on(async move {
                while let Some(action) = stream.next().await {
                    if let Action::Output(message) = action {
                        let _ = sender.send((Instant::now(), message));
                    }
                }
            });
        });
        outputs
    }

    /// The first action `task` asks the runtime for, if it asks any.
    fn first_action(task: Task<Message>) -> Option<Action<Message>> {
        let mut stream = iced_test::runtime::task::into_stream(task)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| fatal(&format!("no runtime for the task: {e}")));
        runtime.block_on(stream.next())
    }

    /// A stand-in for an armed take: registered with the app's finalizer as
    /// a recording controller registers its take, it "stops and saves" when
    /// finalized (a while, then its file written and renamed into place),
    /// and unregisters itself once saved, as a stopped take does.
    struct Take {
        file: PathBuf,
        called: AtomicBool,
        saving: Mutex<Option<Instant>>,
    }

    impl Take {
        fn arm(file: PathBuf) -> Arc<Take> {
            let take = Arc::new(Take {
                file,
                called: AtomicBool::new(false),
                saving: Mutex::new(None),
            });
            let me: Arc<OnceLock<Finalizer>> = Arc::new(OnceLock::new());
            let finalizer = Finalizer::new({
                let take = Arc::clone(&take);
                let me = Arc::clone(&me);
                move || {
                    take.called.store(true, Ordering::SeqCst);
                    // Safe to call again: a second call waits for the first
                    // save, then finds it done.
                    let mut landed = take.saving.lock().unwrap_or_else(|e| e.into_inner());
                    if landed.is_some() {
                        return;
                    }
                    thread::sleep(SAVE_TIME);
                    let partial = take.file.with_extension("partial");
                    if std::fs::write(&partial, b"RIFF take").is_ok() {
                        let _ = std::fs::rename(&partial, &take.file);
                    }
                    *landed = Some(Instant::now());
                    if let Some(me) = me.get() {
                        RecordingFinalizer::instance().unregister(me);
                    }
                }
            });
            let _ = me.set(finalizer.clone());
            // A take that started work: the quit waits for it.
            RecordingFinalizer::instance().register(&finalizer, true);
            take
        }

        fn called(&self) -> bool {
            self.called.load(Ordering::SeqCst)
        }

        /// When its file landed, if it has.
        fn landed(&self) -> Option<Instant> {
            // Never blocks on a save in flight: `try_lock` fails then.
            self.saving.try_lock().ok().and_then(|landed| *landed)
        }
    }

    pub fn run() {
        let mtm = MainThreadMarker::new()
            .unwrap_or_else(|| fatal("main is not on the main thread (harness = false?)"));
        start("macos_quit", LIMIT);
        let mut checks = Checks::default();
        let dir = tempfile::Builder::new()
            .prefix("vocara-quit-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap_or_else(|e| fatal(&format!("no temporary folder: {e}")));

        step("setting up AppKit as main does: the terminate: hook, then the menu bar");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        quit_request::intercept();
        let delegate = QuitDelegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        app.finishLaunching();
        app_menu::install();
        let Some(bar) = app.mainMenu() else {
            fatal("NSApp has no main menu after app_menu::install");
        };
        let quit = find(&bar, sel!(terminate:));
        checks.check(
            matches!(&quit, Some((_, key, mask)) if key == "q" && *mask == NSEventModifierFlags::Command),
            format!("the menu bar's Quit item sends terminate: on ⌘Q ({quit:?})"),
        );
        let window = new_window(mtm, "Vocara: quit", NSPoint::new(240.0, 240.0));
        make_key(&app, &window);

        step("the app with its real quit hooks, and an armed take");
        let mut boot = Boot::new(AppEnvironment::Dev);
        boot.quit_hooks = Arc::new(ServiceQuitHooks);
        let (mut vocara, _opening) = App::new(boot);
        let heard = listen(&vocara);
        let take = Take::arm(dir.path().join("take.wav"));
        checks.check(
            RecordingFinalizer::instance().has_active(),
            "the armed take is registered with the app's RecordingFinalizer",
        );

        step("⌘Q");
        command_q(&app, &window);
        let first_quit = Instant::now();
        checks.check(
            delegate.asked() == 0,
            format!(
                "⌘Q does not run AppKit's terminate: (applicationShouldTerminate: asked {} time(s)): \
                 the process goes on, its end the app's to decide",
                delegate.asked()
            ),
        );
        let message = pump_recv(&app, &heard, REACH);
        checks.check(
            matches!(message, Some(Message::QuitRequested)),
            format!("⌘Q reaches the app as Message::QuitRequested (got {message:?})"),
        );
        checks.check(
            !take.called(),
            "nothing is finalized before the app's guard runs",
        );

        step("the app's quit guard");
        let guard = run_task(vocara.update(Message::QuitRequested));
        let called = pump_until(&app, REACH, || take.called());
        checks.check(
            called,
            "the guard calls the armed take's finalizer (ServiceQuitHooks → RecordingFinalizer::finalize_all)",
        );
        let early = guard.try_recv().ok();
        checks.check(
            early.is_none() && take.landed().is_none(),
            format!("the guard has not finished while the take saves (got {early:?})"),
        );

        step("⌘Q again while the take saves");
        command_q(&app, &window);
        checks.check(
            delegate.asked() == 0,
            "a second ⌘Q does not run AppKit's terminate: either",
        );
        let again = pump_recv(&app, &heard, Duration::from_millis(500));
        checks.check(
            again.is_none(),
            format!("nor does it ask the app again (got {again:?})"),
        );
        let rerun = iced_test::runtime::task::into_stream(vocara.update(Message::QuitRequested));
        checks.check(rerun.is_none(), "a quit request while closing runs nothing");

        step("the guard's end");
        let done = pump_recv(&app, &guard, GUARD);
        let landed = take.landed();
        match &done {
            Some((at, Message::QuitGuardDone)) => {
                checks.check(
                    landed.is_some_and(|landed| landed <= *at),
                    format!(
                        "the guard finishes only after the take landed \
                         (landed {landed:?}, finished {at:?})"
                    ),
                );
            }
            other => {
                checks.check(
                    false,
                    format!(
                        "the guard finishes with QuitGuardDone within {}s (got {other:?})",
                        GUARD.as_secs()
                    ),
                );
            }
        }
        let saved = std::fs::read(&take.file).ok();
        checks.check(
            saved.as_deref() == Some(&b"RIFF take"[..]),
            format!("the take's file is on disk ({})", take.file.display()),
        );
        checks.check(
            !RecordingFinalizer::instance().has_active(),
            "the saved take is no longer registered",
        );
        let exit = first_action(vocara.update(Message::QuitGuardDone));
        checks.check(
            matches!(exit, Some(Action::Exit)),
            "QuitGuardDone exits the app (iced::exit)",
        );
        checks.check(delegate.asked() == 0, "AppKit's terminate: never ran");

        step(format!(
            "a ⌘Q {}s after the first, as if the guard never answered",
            REPEAT_WINDOW.as_secs()
        ));
        // The quit path is the same with `device` or without, and CI runs
        // both: the minute is waited out once, in the plain run.
        if cfg!(feature = "device") {
            note("waited out in the run without `device` only: not checked here");
            window.orderOut(None);
            drop(dir);
            checks.finish();
        }
        let until = first_quit + REPEAT_WINDOW + Duration::from_millis(500);
        note(format!(
            "waiting {}s for the repeat window to pass",
            until.saturating_duration_since(Instant::now()).as_secs()
        ));
        pump_until(
            &app,
            until.saturating_duration_since(Instant::now()),
            || false,
        );
        command_q(&app, &window);
        checks.check(
            delegate.asked() == 1,
            format!(
                "past the repeat window ⌘Q goes through to AppKit's own terminate:, \
                 which asks the delegate (asked {} time(s); it answered Cancel)",
                delegate.asked()
            ),
        );

        window.orderOut(None);
        drop(dir);
        checks.finish();
    }
}
