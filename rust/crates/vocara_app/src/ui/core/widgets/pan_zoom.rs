//! What a wheel event was to Flutter on macOS: its embedder
//! (`FlutterViewController.mm`, Flutter 3.38.4) delivered a scroll as one
//! of three things, by the gesture phases AppKit gave it, and the app's
//! run loop hands those phases along with each `WheelScrolled`
//! (`iced_winit::gesture`, in the vendored iced_winit):
//!
//! - **A pan-zoom gesture** (`PointerPanZoomStartEvent`, `…UpdateEvent`,
//!   `…EndEvent`): a scroll with gesture phases, a trackpad's two fingers
//!   or a Magic Mouse's. Flutter hit-tested it at its start and sent the
//!   rest of it to what that hit, as it did a pointer's press: a drag
//!   recognizer that took the trackpad (every one by default; the app's
//!   own controls left it out with `kPointerDragDevices`) took it as a
//!   drag, its moves the pan's deltas; a `Listener` saw its updates.
//! - **A scroll signal** (`PointerScrollEvent`): a scroll with neither
//!   phase nor momentum phase (a mouse wheel, or a precise device that
//!   sends no phases). Only `onPointerSignal` handlers saw it (a scroll
//!   view's `pointerScroll`, the video's volume button), where the pointer
//!   was.
//! - **Nothing**: the momentum macOS sends once the fingers lift. Flutter
//!   ran its own ballistic simulation where a drag ended in a fling
//!   ([`scroll_fling`](super::scroll_fling)).
//!
//! iced hands every one of them to the widgets as `WheelScrolled`; a
//! widget asks [`current`] which one it is handling. A wheel event the run
//! loop knows nothing more of (off macOS, or one whose `NSEvent` it could
//! not read back) is a scroll signal, as a mouse wheel's is.

use std::cell::Cell;

use iced::time::Instant;
use iced::{mouse, Vector};
use iced_winit::gesture::{self, Pointer};

/// `kTouchSlop`: a trackpad's hit slop (`computeHitSlop` for
/// `PointerDeviceKind.trackpad`, macOS setting no touch slop of its own),
/// which a trackpad's drag must travel to be a fling, or to win a gesture
/// arena another drag recognizer is in.
pub const TOUCH_SLOP: f64 = 18.0;

/// A line of a scroll signal that is not precise, in pixels
/// (`pixelsPerLine`: the embedder's 40, as Chromium scrolls).
pub const PIXELS_PER_LINE: f32 = 40.0;

/// Flutter's `PointerScrollEvent.scrollDelta` for a scroll signal's
/// `delta` (iced's sign: positive moves the content right and down) with
/// Shift held as `shift`: its negation, a line 40 px, and on macOS the
/// axes AppKit swaps under Shift put back, as the embedder put them.
pub fn scroll_delta(delta: mouse::ScrollDelta, shift: bool) -> Vector {
    let (x, y, per_line) = match delta {
        mouse::ScrollDelta::Lines { x, y } => (x, y, PIXELS_PER_LINE),
        mouse::ScrollDelta::Pixels { x, y } => (x, y, 1.0),
    };
    let (x, y) = if cfg!(target_os = "macos") && shift {
        (y, x)
    } else {
        (x, y)
    };
    Vector::new(-x * per_line, -y * per_line)
}

thread_local! {
    /// The handling time tests set (see [`event_time`]).
    static EVENT_TIME: Cell<Option<Instant>> = const { Cell::new(None) };
    /// How many vertical scroll views the widget being updated lies in.
    static SCROLL_VIEWS: Cell<u32> = const { Cell::new(0) };
}

/// When a pointer event is handled: iced's events carry no time stamp, so
/// now (a test sets it, [`testing::set_event_time`]).
pub fn event_time() -> Instant {
    EVENT_TIME.with(Cell::get).unwrap_or_else(Instant::now)
}

/// Runs `f`, a vertical scroll view's update of its content: a drag
/// recognizer in the content then shares a trackpad gesture's arena with
/// the view's own ([`in_scroll_view`]).
pub fn within_scroll_view<R>(f: impl FnOnce() -> R) -> R {
    struct Leave;

    impl Drop for Leave {
        fn drop(&mut self) {
            SCROLL_VIEWS.with(|n| n.set(n.get().saturating_sub(1)));
        }
    }

    SCROLL_VIEWS.with(|n| n.set(n.get() + 1));
    let _leave = Leave;
    f()
}

/// Whether the widget being updated lies in a vertical scroll view, whose
/// drag recognizer a trackpad gesture starting on it would also reach.
pub fn in_scroll_view() -> bool {
    SCROLL_VIEWS.with(Cell::get) > 0
}

/// What a `WheelScrolled` was to Flutter (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scroll {
    /// A `PointerScrollEvent`: the event's own delta is the scroll's.
    Signal,
    /// A `PointerPanZoomStartEvent` of `gesture`, at `time`.
    Start { gesture: u64, time: Instant },
    /// A `PointerPanZoomUpdateEvent`: `delta` is its `panDelta` (logical
    /// pixels, toward the right and the bottom as the fingers go with
    /// macOS's natural scrolling).
    Update {
        gesture: u64,
        delta: Vector,
        time: Instant,
    },
    /// A `PointerPanZoomEndEvent`.
    End { gesture: u64 },
    /// No pointer event at all: `gesture`'s momentum (or a phase the
    /// embedder dropped).
    Nothing { gesture: u64 },
}

impl Scroll {
    /// The pan-zoom gesture it is part of (its momentum included); none for
    /// a scroll signal.
    pub fn gesture(&self) -> Option<u64> {
        match *self {
            Scroll::Signal => None,
            Scroll::Start { gesture, .. }
            | Scroll::Update { gesture, .. }
            | Scroll::End { gesture }
            | Scroll::Nothing { gesture } => Some(gesture),
        }
    }
}

/// What the `WheelScrolled` being handled was to Flutter.
pub fn current() -> Scroll {
    let Some(wheel) = gesture::current() else {
        return Scroll::Signal;
    };
    let gesture = wheel.gesture;
    match wheel.pointer {
        Pointer::Signal => Scroll::Signal,
        Pointer::PanZoomStart => Scroll::Start {
            gesture,
            time: wheel.platform.time,
        },
        Pointer::PanZoomUpdate => Scroll::Update {
            gesture,
            delta: wheel.platform.delta,
            time: wheel.platform.time,
        },
        Pointer::PanZoomEnd => Scroll::End { gesture },
        Pointer::None => Scroll::Nothing { gesture },
    }
}

/// A widget's hold on the pan-zoom gestures that started where it takes
/// hits: Flutter routed a gesture to what its start hit, to its end, and
/// what its momentum would have reached reached nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held(Option<u64>);

impl Held {
    /// Takes `gesture`, which started here.
    pub fn take(&mut self, gesture: u64) {
        self.0 = Some(gesture);
    }

    /// Whether `gesture` is the one it took (still under way, or ended:
    /// its momentum is still its own).
    pub fn holds(&self, gesture: u64) -> bool {
        self.0 == Some(gesture)
    }

    /// Whether the scroll belongs to a gesture it holds.
    pub fn owns(&self, scroll: &Scroll) -> bool {
        scroll.gesture().is_some_and(|gesture| self.holds(gesture))
    }
}

/// Synthetic wheel events with the gestures the run loop would send along,
/// for tests.
#[cfg(test)]
pub mod testing {
    use iced::time::Instant;
    use iced::{mouse, Event, Vector};
    use iced_winit::gesture::{Embedder, Phase, Platform, Wheel};

    /// Pointer events are handled at `at` (or at the time they are, with
    /// `None`) on this thread.
    pub fn set_event_time(at: Option<Instant>) {
        super::EVENT_TIME.with(|t| t.set(at));
    }

    /// A wheel event and its gesture, as the run loop hands them over.
    pub type Sent = (Event, Option<Wheel>);

    /// Sends `sent` through `ui` one event at a time, each wheel event with
    /// its gesture, as the run loop does.
    pub fn simulate<M>(ui: &mut iced_test::Simulator<'_, M>, sent: impl IntoIterator<Item = Sent>) {
        for (event, wheel) in sent {
            iced_winit::gesture::with(wheel, || {
                let _ = ui.simulate([event]);
            });
        }
    }

    /// A two-finger swipe of `steps` (each a move of `(dx, dy)` points)
    /// from its start to its end, 8 ms apart from `t0`.
    pub fn swipe(steps: &[(f32, f32)], t0: Instant) -> Vec<Sent> {
        let mut pad = Trackpad::new();
        let at = |i: usize| t0 + std::time::Duration::from_millis(8 * i as u64);
        let mut sent = vec![pad.began(at(0))];
        for (i, &(dx, dy)) in steps.iter().enumerate() {
            sent.push(pad.changed(dx, dy, at(i + 1)));
        }
        sent.push(pad.ended(at(steps.len() + 1)));
        sent
    }

    /// A trackpad (and a mouse wheel) through Flutter's embedder: each
    /// event as AppKit would report it.
    #[derive(Debug, Default)]
    pub struct Trackpad {
        embedder: Embedder,
    }

    impl Trackpad {
        pub fn new() -> Self {
            Self::default()
        }

        /// A precise scroll of `(dx, dy)` points with these phases at `at`.
        pub fn send(
            &mut self,
            phase: Phase,
            momentum_phase: Phase,
            (dx, dy): (f32, f32),
            at: Instant,
        ) -> Sent {
            let wheel = self.embedder.dispatch(Platform {
                phase,
                momentum_phase,
                precise: true,
                delta: Vector::new(dx, dy),
                time: at,
            });
            (
                Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: dx, y: dy },
                }),
                Some(wheel),
            )
        }

        /// Two fingers land and start to move (`Began`).
        pub fn began(&mut self, at: Instant) -> Sent {
            self.send(Phase::Began, Phase::None, (0.0, 0.0), at)
        }

        /// They move by `(dx, dy)` (`Changed`).
        pub fn changed(&mut self, dx: f32, dy: f32, at: Instant) -> Sent {
            self.send(Phase::Changed, Phase::None, (dx, dy), at)
        }

        /// They lift (`Ended`).
        pub fn ended(&mut self, at: Instant) -> Sent {
            self.send(Phase::Ended, Phase::None, (0.0, 0.0), at)
        }

        /// The momentum after they lifted.
        pub fn momentum(&mut self, phase: Phase, dx: f32, dy: f32, at: Instant) -> Sent {
            self.send(Phase::None, phase, (dx, dy), at)
        }

        /// A mouse wheel turned `(x, y)` lines (no phases, not precise).
        pub fn lines(&mut self, x: f32, y: f32, at: Instant) -> Sent {
            let wheel = self.embedder.dispatch(Platform {
                phase: Phase::None,
                momentum_phase: Phase::None,
                precise: false,
                delta: Vector::new(x, y),
                time: at,
            });
            (
                Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x, y },
                }),
                Some(wheel),
            )
        }

        /// A precise device without phases scrolled `(x, y)` points.
        pub fn pixels(&mut self, x: f32, y: f32, at: Instant) -> Sent {
            self.send(Phase::None, Phase::None, (x, y), at)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Trackpad;
    use super::*;
    use iced_winit::gesture::Phase;

    fn read((_, wheel): super::testing::Sent) -> Scroll {
        gesture::with(wheel, current)
    }

    #[test]
    fn a_trackpad_swipe_is_a_pan_zoom_and_its_momentum_nothing() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        assert_eq!(
            read(pad.began(t)),
            Scroll::Start {
                gesture: 1,
                time: t
            }
        );
        assert_eq!(
            read(pad.changed(0.0, 4.0, t)),
            Scroll::Update {
                gesture: 1,
                delta: Vector::new(0.0, 4.0),
                time: t
            }
        );
        assert_eq!(read(pad.ended(t)), Scroll::End { gesture: 1 });
        for phase in [Phase::Began, Phase::Changed, Phase::Ended] {
            assert_eq!(
                read(pad.momentum(phase, 0.0, 2.0, t)),
                Scroll::Nothing { gesture: 1 }
            );
        }
        // A second swipe is a new gesture.
        assert_eq!(
            read(pad.began(t)),
            Scroll::Start {
                gesture: 2,
                time: t
            }
        );
    }

    #[test]
    fn a_wheel_without_phases_is_a_signal_precise_or_not() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        assert_eq!(read(pad.lines(0.0, 1.0, t)), Scroll::Signal);
        assert_eq!(read(pad.pixels(0.0, 3.0, t)), Scroll::Signal);
        // Nothing known of it (off macOS): a signal.
        assert_eq!(gesture::with(None, current), Scroll::Signal);
    }

    #[test]
    fn the_embedder_drops_a_second_start_and_an_end_without_one() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        // A touch (`MayBegin`) then the swipe's `Began`: one start.
        assert_eq!(
            read(pad.send(Phase::MayBegin, Phase::None, (0.0, 0.0), t)),
            Scroll::Start {
                gesture: 1,
                time: t
            }
        );
        assert_eq!(read(pad.began(t)), Scroll::Nothing { gesture: 1 });
        assert_eq!(read(pad.ended(t)), Scroll::End { gesture: 1 });
        // A cancel with none under way, and a stationary phase: nothing.
        assert_eq!(
            read(pad.send(Phase::Cancelled, Phase::None, (0.0, 0.0), t)),
            Scroll::Nothing { gesture: 1 }
        );
        assert_eq!(
            read(pad.send(Phase::Stationary, Phase::None, (0.0, 0.0), t)),
            Scroll::Nothing { gesture: 1 }
        );
    }

    #[test]
    fn a_widget_holds_the_gesture_that_started_on_it_and_its_momentum() {
        let mut held = Held::default();
        held.take(3);
        assert!(held.owns(&Scroll::Nothing { gesture: 3 }));
        assert!(held.owns(&Scroll::End { gesture: 3 }));
        assert!(!held.owns(&Scroll::Nothing { gesture: 2 }));
        assert!(!held.owns(&Scroll::Signal));
    }
}
