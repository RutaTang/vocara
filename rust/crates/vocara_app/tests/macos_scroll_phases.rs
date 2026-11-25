//! The scroll phases AppKit gives a wheel event, as the app's run loop
//! reads them (`iced_winit::gesture`, in the vendored iced_winit), on a
//! real Mac in a GUI session (CI's macOS runner).
//!
//! winit hands its handler a `MouseWheel` with the momentum phase merged
//! into the touch phase; the run loop reads, while it converts that event,
//! the scroll event AppKit is dispatching (`NSApp.currentEvent`), and runs
//! Flutter's embedder over it (`Embedder::wheel`). This test runs winit's
//! own event loop and window and feeds the window's view scroll events made
//! from CGEvents (`CGEventCreateScrollWheelEvent2`, with
//! `kCGScrollWheelEventScrollPhase` and `kCGScrollWheelEventMomentumPhase`
//! set): each is posted to the app's queue and dequeued, which makes it
//! `NSApp.currentEvent`, then handed to the view's `scrollWheel:` as
//! AppKit hands it, from the main queue, outside winit's handler, so winit
//! calls its handler then and there. The handler reads the gesture as the
//! run loop does, and the test checks, for each event, that winit called
//! it at once, that the reading found the event, its phases and deltas,
//! and what Flutter's embedder made of it: a trackpad's swipe a pan-zoom
//! gesture (a second start dropped), its momentum nothing, a wheel or a
//! precise device without phases a scroll signal.
//!
//! AppKit wants the main thread, so the test has no harness (`harness =
//! false` in Cargo.toml): `main` runs it and exits non-zero when a check
//! fails. Off macOS there is no AppKit, and `main` prints a skip line.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macos_scroll_phases: skipped (AppKit is macOS only)");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::process::exit;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use dispatch2::DispatchQueue;
    use iced_winit::gesture::{Embedder, Phase, Pointer, Wheel};
    use iced_winit::winit;
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventPhase, NSEventType, NSView};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGMomentumScrollPhase, CGScrollEventUnit,
        CGScrollPhase,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::{MouseScrollDelta, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId};

    /// The test window's title, which finds its `NSWindow`.
    const TITLE: &str = "vocara scroll phases";

    /// How long the whole run may take before the watchdog ends it.
    const WATCHDOG: Duration = Duration::from_secs(120);

    /// A scroll the test sends, and what the run loop must read of it.
    #[derive(Debug, Clone, Copy)]
    struct Sent {
        what: &'static str,
        /// `kCGScrollWheelEventScrollPhase` (a `CGScrollPhase`, 0 for none).
        scroll_phase: u32,
        /// `kCGScrollWheelEventMomentumPhase` (a `CGMomentumScrollPhase`).
        momentum_phase: u32,
        /// Pixel units (`hasPreciseScrollingDeltas`), or lines.
        precise: bool,
        /// The vertical wheel's delta.
        dy: i32,
        phase: Phase,
        momentum: Phase,
        pointer: Pointer,
        gesture: u64,
    }

    const fn sent(
        what: &'static str,
        scroll_phase: u32,
        momentum_phase: u32,
        precise: bool,
        dy: i32,
        (phase, momentum): (Phase, Phase),
        (pointer, gesture): (Pointer, u64),
    ) -> Sent {
        Sent {
            what,
            scroll_phase,
            momentum_phase,
            precise,
            dy,
            phase,
            momentum,
            pointer,
            gesture,
        }
    }

    /// A trackpad's swipe from the touch to the end of its momentum, then
    /// a mouse wheel's notch and a precise scroll without phases.
    fn script() -> [Sent; 10] {
        let began = CGScrollPhase::Began.0;
        let changed = CGScrollPhase::Changed.0;
        let ended = CGScrollPhase::Ended.0;
        let may_begin = CGScrollPhase::MayBegin.0;
        let m_begin = CGMomentumScrollPhase::Begin.0;
        let m_continue = CGMomentumScrollPhase::Continue.0;
        let m_end = CGMomentumScrollPhase::End.0;
        let none = Phase::None;
        [
            sent(
                "the touch",
                may_begin,
                0,
                true,
                0,
                (Phase::MayBegin, none),
                (Pointer::PanZoomStart, 1),
            ),
            sent(
                "the swipe's start",
                began,
                0,
                true,
                0,
                (Phase::Began, none),
                (Pointer::None, 1),
            ),
            sent(
                "a move",
                changed,
                0,
                true,
                -6,
                (Phase::Changed, none),
                (Pointer::PanZoomUpdate, 1),
            ),
            sent(
                "another move",
                changed,
                0,
                true,
                -4,
                (Phase::Changed, none),
                (Pointer::PanZoomUpdate, 1),
            ),
            sent(
                "the lift",
                ended,
                0,
                true,
                0,
                (Phase::Ended, none),
                (Pointer::PanZoomEnd, 1),
            ),
            sent(
                "the momentum's start",
                0,
                m_begin,
                true,
                -8,
                (none, Phase::Began),
                (Pointer::None, 1),
            ),
            sent(
                "the momentum",
                0,
                m_continue,
                true,
                -5,
                (none, Phase::Changed),
                (Pointer::None, 1),
            ),
            sent(
                "the momentum's end",
                0,
                m_end,
                true,
                0,
                (none, Phase::Ended),
                (Pointer::None, 1),
            ),
            sent(
                "a wheel's notch",
                0,
                0,
                false,
                1,
                (none, none),
                (Pointer::Signal, 1),
            ),
            sent(
                "a precise scroll without phases",
                0,
                0,
                true,
                3,
                (none, none),
                (Pointer::Signal, 1),
            ),
        ]
    }

    /// What winit handed its handler for one scroll, and what the run
    /// loop's reading made of it then.
    #[derive(Debug, Clone, Copy)]
    struct Received {
        delta: MouseScrollDelta,
        wheel: Option<Wheel>,
    }

    thread_local! {
        static RECEIVED: RefCell<Vec<Received>> = const { RefCell::new(Vec::new()) };
        static FAILED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// The script has run (on the main queue).
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

    /// `NSEventPhase` as the run loop reports it.
    fn phase_of(phase: NSEventPhase) -> Phase {
        if phase == NSEventPhase::None {
            Phase::None
        } else if phase == NSEventPhase::Began {
            Phase::Began
        } else if phase == NSEventPhase::Changed {
            Phase::Changed
        } else if phase == NSEventPhase::Ended {
            Phase::Ended
        } else if phase == NSEventPhase::Cancelled {
            Phase::Cancelled
        } else if phase == NSEventPhase::MayBegin {
            Phase::MayBegin
        } else {
            Phase::Stationary
        }
    }

    /// The scroll event `sent` describes, made as a trackpad's or a wheel's
    /// would be (no modifier flags).
    fn make(sent: &Sent) -> Option<Retained<NSEvent>> {
        let units = if sent.precise {
            CGScrollEventUnit::Pixel
        } else {
            CGScrollEventUnit::Line
        };
        let cg = CGEvent::new_scroll_wheel_event2(None, units, 1, sent.dy, 0, 0)?;
        let cg: &CGEvent = &cg;
        let field = |field, value: u32| {
            CGEvent::set_integer_value_field(Some(cg), field, i64::from(value));
        };
        field(CGEventField::ScrollWheelEventScrollPhase, sent.scroll_phase);
        field(
            CGEventField::ScrollWheelEventMomentumPhase,
            sent.momentum_phase,
        );
        CGEvent::set_flags(Some(cg), CGEventFlags::empty());
        NSEvent::eventWithCGEvent(cg)
    }

    /// The window's view, winit's (`WinitView`).
    fn view(app: &NSApplication) -> Option<Retained<NSView>> {
        app.windows()
            .to_vec()
            .into_iter()
            .find(|window| window.title().to_string() == TITLE)
            .and_then(|window| window.contentView())
    }

    /// Sends the script, each event made `NSApp.currentEvent` and handed to
    /// the view, outside winit's handler (this runs on the main queue).
    fn run_script() {
        let Some(mtm) = MainThreadMarker::new() else {
            check(false, "the script runs on the main thread");
            DONE.store(true, Ordering::SeqCst);
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(view) = view(&app) else {
            check(false, "the winit window's view is found by its title");
            DONE.store(true, Ordering::SeqCst);
            return;
        };
        for (i, sent) in script().iter().enumerate() {
            println!("macos_scroll_phases: {}", sent.what);
            let Some(event) = make(sent) else {
                check(
                    false,
                    format!("{}: AppKit made the scroll event", sent.what),
                );
                break;
            };
            // The test's own premise: AppKit reads the CGEvent's fields as
            // a trackpad's or a wheel's.
            let made = (
                phase_of(event.phase()),
                phase_of(event.momentumPhase()),
                event.hasPreciseScrollingDeltas(),
            );
            check(
                made == (sent.phase, sent.momentum, sent.precise),
                format!(
                    "{}: the event made reads {made:?} (phase, momentum, precise)",
                    sent.what
                ),
            );
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
                    format!("{}: the posted scroll came off the queue", sent.what),
                );
                break;
            };
            let current = app.currentEvent();
            let same = |event: &NSEvent| {
                event.r#type() == NSEventType::ScrollWheel
                    && phase_of(event.phase()) == sent.phase
                    && phase_of(event.momentumPhase()) == sent.momentum
                    && event.scrollingDeltaY() == next.scrollingDeltaY()
            };
            check(
                same(&next) && current.is_some_and(|current| same(&current)),
                format!("{}: dequeued, it is NSApp.currentEvent", sent.what),
            );
            let before = RECEIVED.with(|received| received.borrow().len());
            view.scrollWheel(&next);
            let after = RECEIVED.with(|received| received.borrow().len());
            check(
                after == before + 1,
                format!(
                    "{}: winit handed it over within scrollWheel: ({after} after {before})",
                    sent.what
                ),
            );
            let Some(received) = RECEIVED.with(|received| received.borrow().get(i).copied()) else {
                break;
            };
            let Some(wheel) = received.wheel else {
                check(
                    false,
                    format!(
                        "{}: the run loop read its NSEvent ({:?})",
                        sent.what, received.delta
                    ),
                );
                continue;
            };
            check(
                (
                    wheel.platform.phase,
                    wheel.platform.momentum_phase,
                    wheel.platform.precise,
                ) == (sent.phase, sent.momentum, sent.precise),
                format!("{}: the phases read are {:?}", sent.what, wheel.platform),
            );
            check(
                wheel.platform.delta.y == next.scrollingDeltaY() as f32
                    && wheel.platform.delta.x == next.scrollingDeltaX() as f32,
                format!(
                    "{}: the deltas read are the event's ({:?})",
                    sent.what, wheel.platform.delta
                ),
            );
            check(
                (wheel.pointer, wheel.gesture) == (sent.pointer, sent.gesture),
                format!(
                    "{}: Flutter's embedder made it {:?} of gesture {}",
                    sent.what, wheel.pointer, wheel.gesture
                ),
            );
        }
        DONE.store(true, Ordering::SeqCst);
    }

    #[derive(Default)]
    struct Test {
        window: Option<Window>,
        embedder: Embedder,
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
                Ok(window) => self.window = Some(window),
                Err(error) => {
                    check(false, format!("winit made a window ({error})"));
                    DONE.store(true, Ordering::SeqCst);
                }
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            let WindowEvent::MouseWheel { delta, .. } = event else {
                return;
            };
            let scale = self.window.as_ref().map_or(1.0, Window::scale_factor);
            // As the run loop reads it, converting the event.
            let wheel = self.embedder.wheel(&event, scale);
            RECEIVED.with(|received| received.borrow_mut().push(Received { delta, wheel }));
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if DONE.load(Ordering::SeqCst) {
                event_loop.exit();
                return;
            }
            if self.window.is_some() && !self.scheduled {
                self.scheduled = true;
                // Outside winit's handler, as AppKit dispatches an event.
                DispatchQueue::main().exec_async(run_script);
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(50),
            ));
        }
    }

    pub fn run() {
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            eprintln!("macos_scroll_phases: FAIL: timed out");
            std::process::abort();
        });
        println!("macos_scroll_phases: start");
        let event_loop = match EventLoop::new() {
            Ok(event_loop) => event_loop,
            Err(error) => {
                eprintln!("macos_scroll_phases: FAIL: no event loop ({error})");
                exit(1);
            }
        };
        let mut test = Test::default();
        if let Err(error) = event_loop.run_app(&mut test) {
            check(false, format!("the event loop ran ({error})"));
        }
        let received = RECEIVED.with(|received| received.borrow().len());
        check(
            received == script().len(),
            format!("winit handed over {received} of {} scrolls", script().len()),
        );
        let failed = FAILED.with(|failed| failed.borrow().clone());
        if failed.is_empty() {
            println!("macos_scroll_phases: all checks passed");
        } else {
            eprintln!("macos_scroll_phases: {} checks failed:", failed.len());
            for what in &failed {
                eprintln!("  - {what}");
            }
            exit(1);
        }
    }
}
