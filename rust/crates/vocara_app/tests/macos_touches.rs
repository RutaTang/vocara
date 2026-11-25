//! The trackpad touches the app's run loop reads (`iced_winit::gesture`,
//! in the vendored iced_winit), on a real Mac in a GUI session (CI's macOS
//! runner).
//!
//! winit reads no touches. The run loop sets each window's view up as
//! Flutter's macOS embedder set its own (`setAllowedTouchTypes:
//! NSTouchTypeMaskIndirect`, `setWantsRestingTouches:YES`) and gives the
//! view's class a `touchesBeganWithEvent:` that records each touch
//! (`gesture::accept_touches`); the embedder's rule then makes a touch
//! within 50 ms of macOS's momentum's last change an inertia cancel, once
//! (`Embedder::touched`). This test runs winit's own event loop and
//! window, sets the window up as the run loop does (twice: the second
//! call must change nothing), and checks:
//!
//! 1. the view is the window's content view the run loop names
//!    (`gesture::view_of`), takes indirect and resting touches, and its
//!    class (winit's `WinitView`) has a `touchesBeganWithEvent:` of its
//!    own, not `NSView`'s;
//! 2. macOS's momentum, as scroll events made from CGEvents
//!    (`kCGScrollWheelEventMomentumPhase` set) and handed to the view's
//!    `scrollWheel:` as AppKit hands them (posted, dequeued so that each is
//!    `NSApp.currentEvent`, from the main queue outside winit's handler, as
//!    `macos_scroll_phases` does), reaches the embedder the run loop runs;
//! 3. a touch sent to the view as AppKit sends one
//!    (`[view touchesBeganWithEvent:]`) is recorded once, with the view,
//!    its time on the clock the scroll events' are read on, and its place
//!    in the view, and the embedder makes it an inertia cancel 20 ms after
//!    the momentum's change, hit tested there; a second touch at once is
//!    none (one per momentum), nor is one 60 ms after a change, nor one
//!    after two fingers landed (a pan-zoom start); an event without a touch
//!    records nothing.
//!
//! AppKit makes no touch event a program can: a touch here is a real
//! event (made from a CGEvent) whose class is switched to a runtime
//! subclass of its own that answers `allTouches`, `timestamp` and
//! `locationInWindow` (as key-value observing switches an object's class),
//! which is all the method reads.
//!
//! AppKit wants the main thread, so the test has no harness (`harness =
//! false` in Cargo.toml): `main` runs it and exits non-zero when a check
//! fails. Off macOS there is no AppKit, and `main` prints a skip line.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macos_touches: skipped (AppKit is macOS only)");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::process::exit;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use dispatch2::DispatchQueue;
    use iced_winit::core::Point;
    use iced_winit::gesture::{self, Embedder, Pointer, Touch, Wheel};
    use iced_winit::winit;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyObject, ClassBuilder, Method, Sel};
    use objc2::{sel, ClassType, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSTouchTypeMask, NSView};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGMomentumScrollPhase, CGScrollEventUnit,
        CGScrollPhase,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSObject, NSPoint, NSSet};
    use winit::application::ApplicationHandler;
    use winit::dpi::{LogicalPosition, LogicalSize};
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId};

    /// The test window's title, which finds its `NSWindow`.
    const TITLE: &str = "vocara touches";

    /// How long the whole run may take before the watchdog ends it.
    const WATCHDOG: Duration = Duration::from_secs(120);

    /// What the touch event answers (see the module docs).
    #[derive(Debug, Clone, Copy)]
    struct Answers {
        /// `allTouches` holds a touch (or is empty).
        touches: bool,
        /// `timestamp`.
        timestamp: f64,
        /// `locationInWindow`.
        location: (f64, f64),
    }

    thread_local! {
        static ANSWERS: Cell<Answers> = const {
            Cell::new(Answers {
                touches: true,
                timestamp: 0.0,
                location: (0.0, 0.0),
            })
        };
        /// The embedder the run loop would run the window's scrolls and
        /// touches through.
        static EMBEDDER: RefCell<Embedder> = RefCell::new(Embedder::default());
        /// What the embedder made of each scroll winit handed over.
        static RECEIVED: RefCell<Vec<Option<Wheel>>> = const { RefCell::new(Vec::new()) };
        /// The window's view as the run loop names it, and its scale.
        static WINDOW: Cell<Option<(usize, f64)>> = const { Cell::new(None) };
        static FAILED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// The checks have run (on the main queue).
    static DONE: AtomicBool = AtomicBool::new(false);

    fn check(ok: bool, what: impl Into<String>) {
        let what = what.into();
        if ok {
            println!("  ok: {what}");
        } else {
            eprintln!("  FAIL: {what}");
            FAILED.with(|failed| failed.borrow_mut().push(what));
        }
    }

    extern "C-unwind" fn all_touches(_this: &AnyObject, _cmd: Sel) -> *mut AnyObject {
        let set: Retained<NSSet<NSObject>> = if ANSWERS.with(Cell::get).touches {
            NSSet::from_retained_slice(&[NSObject::new()])
        } else {
            NSSet::from_slice(&[])
        };
        Retained::autorelease_return(set).cast()
    }

    extern "C-unwind" fn timestamp(_this: &AnyObject, _cmd: Sel) -> f64 {
        ANSWERS.with(Cell::get).timestamp
    }

    extern "C-unwind" fn location_in_window(_this: &AnyObject, _cmd: Sel) -> NSPoint {
        let (x, y) = ANSWERS.with(Cell::get).location;
        NSPoint::new(x, y)
    }

    /// A scroll event as a trackpad's: `scroll_phase` and `momentum_phase`
    /// (CGEvent's), `dy` points down.
    fn scroll_event(scroll_phase: u32, momentum_phase: u32, dy: i32) -> Option<Retained<NSEvent>> {
        let cg = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 1, dy, 0, 0)?;
        let cg: &CGEvent = &cg;
        let field = |field, value: u32| {
            CGEvent::set_integer_value_field(Some(cg), field, i64::from(value));
        };
        field(CGEventField::ScrollWheelEventScrollPhase, scroll_phase);
        field(CGEventField::ScrollWheelEventMomentumPhase, momentum_phase);
        CGEvent::set_flags(Some(cg), CGEventFlags::empty());
        NSEvent::eventWithCGEvent(cg)
    }

    /// A touch event (see the module docs): a real event whose class is a
    /// subclass of its own answering what [`ANSWERS`] says.
    fn touch_event() -> Option<Retained<NSEvent>> {
        let event = scroll_event(0, 0, 0)?;
        let mut builder = ClassBuilder::new(c"VocaraTestTouchEvent", event.class())?;
        // SAFETY: the functions take and return what the selectors' types
        // say (`NSSet *`, `NSTimeInterval`, `NSPoint`).
        unsafe {
            builder.add_method(
                sel!(allTouches),
                all_touches as extern "C-unwind" fn(_, _) -> _,
            );
            builder.add_method(
                sel!(timestamp),
                timestamp as extern "C-unwind" fn(_, _) -> _,
            );
            builder.add_method(
                sel!(locationInWindow),
                location_in_window as extern "C-unwind" fn(_, _) -> _,
            );
        }
        let class = builder.register();
        // SAFETY: a subclass of the event's own class adding no instance
        // variables, whose methods answer as the overridden ones do.
        unsafe {
            let _ = AnyObject::set_class(&event, class);
        }
        Some(event)
    }

    /// The window's view, winit's (`WinitView`).
    fn view(app: &NSApplication) -> Option<Retained<NSView>> {
        app.windows()
            .to_vec()
            .into_iter()
            .find(|window| window.title().to_string() == TITLE)
            .and_then(|window| window.contentView())
    }

    /// Hands `event` to `view` as AppKit hands a scroll (posted, dequeued
    /// as `NSApp.currentEvent`, then `scrollWheel:`): what the embedder
    /// made of it, and the event.
    fn scroll(
        app: &NSApplication,
        view: &NSView,
        event: Option<Retained<NSEvent>>,
        what: &str,
    ) -> Option<(Wheel, Retained<NSEvent>)> {
        let Some(event) = event else {
            check(false, format!("{what}: AppKit made the scroll event"));
            return None;
        };
        app.postEvent_atStart(&event, true);
        let until = NSDate::dateWithTimeIntervalSinceNow(2.0);
        // SAFETY: an immutable NSString constant Foundation exports.
        let mode = unsafe { NSDefaultRunLoopMode };
        let Some(next) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::ScrollWheel,
            Some(&until),
            mode,
            true,
        ) else {
            check(
                false,
                format!("{what}: the posted scroll came off the queue"),
            );
            return None;
        };
        let before = RECEIVED.with(|received| received.borrow().len());
        view.scrollWheel(&next);
        let wheel = RECEIVED.with(|received| received.borrow().get(before).copied().flatten());
        check(
            wheel.is_some(),
            format!("{what}: winit handed it over and the run loop read it"),
        );
        wheel.map(|wheel| (wheel, next))
    }

    /// Sends `event` to `view` as AppKit sends a touch, answering
    /// `answers`: the touches recorded.
    fn touch(view: &NSView, event: &NSEvent, answers: Answers) -> Vec<Touch> {
        let _ = gesture::take_touches();
        ANSWERS.with(|cell| cell.set(answers));
        view.touchesBeganWithEvent(event);
        gesture::take_touches()
    }

    /// The inertia cancel the embedder makes of `touch`, if any.
    fn touched(touch: &Touch, scale: f64) -> Option<Wheel> {
        EMBEDDER.with(|embedder| {
            embedder
                .borrow_mut()
                .touched(touch, scale, scale as f32)
                .map(|(event, wheel)| {
                    check(
                        event == gesture::inertia_cancel_event(),
                        "the cancel is a wheel event of no delta",
                    );
                    wheel
                })
        })
    }

    fn checks() {
        let Some(mtm) = MainThreadMarker::new() else {
            check(false, "the checks run on the main thread");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(view) = view(&app) else {
            check(false, "the winit window's view is found by its title");
            return;
        };
        let Some((named, scale)) = WINDOW.with(Cell::get) else {
            check(false, "the window was made and named");
            return;
        };

        println!("macos_touches: the view");
        let address = Retained::as_ptr(&view) as usize;
        check(
            named == address,
            "gesture::view_of names the window's content view",
        );
        check(
            view.allowedTouchTypes() == NSTouchTypeMask::Indirect,
            format!(
                "it allows indirect touches only ({:?})",
                view.allowedTouchTypes()
            ),
        );
        check(view.wantsRestingTouches(), "it wants resting touches");
        let class = view.class();
        let selector = sel!(touchesBeganWithEvent:);
        let own = class
            .instance_methods()
            .iter()
            .any(|method| method.name() == selector);
        check(
            own,
            format!(
                "{} has a touchesBeganWithEvent: of its own",
                class.name().to_string_lossy()
            ),
        );
        let ours = class.instance_method(selector).map(Method::implementation);
        let inherited = NSView::class()
            .instance_method(selector)
            .map(Method::implementation);
        check(
            matches!((ours, inherited), (Some(ours), Some(inherited)) if !std::ptr::fn_addr_eq(ours, inherited)),
            "it is not NSView's",
        );

        println!("macos_touches: macOS's momentum");
        let m_begin = CGMomentumScrollPhase::Begin.0;
        let m_continue = CGMomentumScrollPhase::Continue.0;
        let Some((began, _)) = scroll(
            &app,
            &view,
            scroll_event(0, m_begin, -8),
            "the momentum's start",
        ) else {
            return;
        };
        check(
            began.pointer == Pointer::None,
            "the momentum reaches no widget",
        );
        let Some((changed, changed_event)) = scroll(
            &app,
            &view,
            scroll_event(0, m_continue, -5),
            "the momentum's change",
        ) else {
            return;
        };
        check(
            changed.pointer == Pointer::None,
            "its change reaches no widget",
        );

        println!("macos_touches: a touch 20 ms on");
        let Some(event) = touch_event() else {
            check(false, "a touch event was made");
            return;
        };
        let location = (40.5, 30.25);
        let answers = Answers {
            touches: true,
            timestamp: changed_event.timestamp() + 0.020,
            location,
        };
        let touches = touch(&view, &event, answers);
        check(
            touches.len() == 1,
            format!("recorded once ({} touches)", touches.len()),
        );
        let Some(first) = touches.first().copied() else {
            return;
        };
        check(first.view == address, "on the view it went to");
        let at = view.convertPoint_fromView(NSPoint::new(location.0, location.1), None);
        check(
            first.position == LogicalPosition::new(at.x, at.y),
            format!("at its place in the view ({:?}, {at:?})", first.position),
        );
        let after = first.time.saturating_duration_since(changed.platform.time);
        check(
            (Duration::from_millis(19)..=Duration::from_millis(21)).contains(&after),
            format!("20 ms after the change on the scrolls' clock ({after:?})"),
        );
        let cancel = touched(&first, scale);
        check(
            cancel.is_some_and(|wheel| wheel.pointer == Pointer::ScrollInertiaCancel),
            format!("an inertia cancel ({cancel:?})"),
        );
        let expected = Point::new(at.x as f32, at.y as f32);
        check(
            cancel.is_some_and(|wheel| {
                wheel.position.is_some_and(|position| {
                    (position.x - expected.x).abs() < 1e-3 && (position.y - expected.y).abs() < 1e-3
                })
            }),
            "hit tested where the touch was",
        );

        println!("macos_touches: another touch at once");
        let touches = touch(
            &view,
            &event,
            Answers {
                timestamp: answers.timestamp + 0.001,
                ..answers
            },
        );
        check(touches.len() == 1, "recorded");
        check(
            touches.first().is_some_and(|t| touched(t, scale).is_none()),
            "no second cancel for the same momentum",
        );

        println!("macos_touches: a touch 60 ms after a change");
        if let Some((_, changed_event)) = scroll(
            &app,
            &view,
            scroll_event(0, m_continue, -3),
            "another change",
        ) {
            let touches = touch(
                &view,
                &event,
                Answers {
                    timestamp: changed_event.timestamp() + 0.060,
                    ..answers
                },
            );
            check(
                touches.len() == 1 && touched(&touches[0], scale).is_none(),
                "recorded, and no cancel past 50 ms",
            );
        }

        println!("macos_touches: a touch after two fingers landed");
        let may_begin = CGScrollPhase::MayBegin.0;
        let ended = CGScrollPhase::Ended.0;
        if let Some((_, changed_event)) = scroll(
            &app,
            &view,
            scroll_event(0, m_continue, -2),
            "a change before the fingers land",
        ) {
            let start = scroll(
                &app,
                &view,
                scroll_event(may_begin, 0, 0),
                "two fingers landing",
            );
            check(
                start.is_some_and(|(wheel, _)| wheel.pointer == Pointer::PanZoomStart),
                "a pan-zoom start",
            );
            let touches = touch(
                &view,
                &event,
                Answers {
                    timestamp: changed_event.timestamp() + 0.010,
                    ..answers
                },
            );
            check(
                touches.len() == 1 && touched(&touches[0], scale).is_none(),
                "recorded, and no cancel after a pan-zoom start",
            );
            let _ = scroll(
                &app,
                &view,
                scroll_event(ended, 0, 0),
                "the fingers lifting",
            );
        }

        println!("macos_touches: an event without a touch");
        let touches = touch(
            &view,
            &event,
            Answers {
                touches: false,
                ..answers
            },
        );
        check(touches.is_empty(), "records nothing");
    }

    #[derive(Default)]
    struct Test {
        window: Option<Window>,
        scheduled: bool,
    }

    impl ApplicationHandler for Test {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attributes = Window::default_attributes()
                .with_title(TITLE)
                .with_inner_size(LogicalSize::new(320.0, 240.0));
            match event_loop.create_window(attributes) {
                Ok(window) => {
                    // As the run loop sets a window up; once per class, so
                    // the second call changes nothing.
                    gesture::accept_touches(&window);
                    gesture::accept_touches(&window);
                    let named = gesture::view_of(&window);
                    check(named.is_some(), "the run loop names the window's view");
                    if let Some(named) = named {
                        WINDOW.with(|cell| cell.set(Some((named, window.scale_factor()))));
                    }
                    self.window = Some(window);
                }
                Err(error) => {
                    check(false, format!("winit made a window ({error})"));
                    DONE.store(true, Ordering::SeqCst);
                }
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            if !matches!(event, WindowEvent::MouseWheel { .. }) {
                return;
            }
            let scale = self.window.as_ref().map_or(1.0, Window::scale_factor);
            // As the run loop reads it, converting the event.
            let wheel = EMBEDDER.with(|embedder| embedder.borrow_mut().wheel(&event, scale));
            RECEIVED.with(|received| received.borrow_mut().push(wheel));
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if DONE.load(Ordering::SeqCst) {
                event_loop.exit();
                return;
            }
            if self.window.is_some() && !self.scheduled {
                self.scheduled = true;
                // Outside winit's handler, as AppKit dispatches an event.
                DispatchQueue::main().exec_async(|| {
                    autoreleasepool(|_| checks());
                    DONE.store(true, Ordering::SeqCst);
                });
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(50),
            ));
        }
    }

    pub fn run() {
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            eprintln!("macos_touches: FAIL: timed out");
            std::process::abort();
        });
        println!("macos_touches: start");
        let event_loop = match EventLoop::new() {
            Ok(event_loop) => event_loop,
            Err(error) => {
                eprintln!("macos_touches: FAIL: no event loop ({error})");
                exit(1);
            }
        };
        let mut test = Test::default();
        if let Err(error) = event_loop.run_app(&mut test) {
            check(false, format!("the event loop ran ({error})"));
        }
        let failed = FAILED.with(|failed| failed.borrow().clone());
        if failed.is_empty() {
            println!("macos_touches: all checks passed");
        } else {
            eprintln!("macos_touches: {} checks failed:", failed.len());
            for what in &failed {
                eprintln!("  - {what}");
            }
            exit(1);
        }
    }
}
