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
//!   drag, its moves the pan's deltas; a `Listener` saw its updates. The
//!   drag recognizers its start reached shared its gesture arena
//!   ([`enter`]): alone, one won as the start closed the arena; with
//!   others, the first moved past its slop along its axis won.
//! - **A scroll signal** (`PointerScrollEvent`): a scroll with neither
//!   phase nor momentum phase (a mouse wheel, or a precise device that
//!   sends no phases). Only `onPointerSignal` handlers saw it (a scroll
//!   view's `pointerScroll`, the video's volume button), where the pointer
//!   was.
//! - **Nothing**: the momentum macOS sends once the fingers lift. Flutter
//!   ran its own ballistic simulation where a drag ended in a fling
//!   ([`scroll_fling`](super::scroll_fling)).
//! - **An inertia cancel** (`PointerScrollInertiaCancelEvent`): not a
//!   scroll but a finger touching the trackpad less than 50 ms after
//!   macOS's momentum last moved (`touchesBeganWithEvent:`), once per
//!   momentum, a new swipe's start ending the chance. Its hit test, where
//!   the pointer was, reached every scrollable under it, and each stopped
//!   whatever moved it (`Scrollable._receivedPointerSignal`'s
//!   `pointerScroll(0)`, a shown scrollbar's track `jumpTo(pixels)`): a
//!   fling, a page or a caret's run stopped where its last frame left it;
//!   no `pointerSignalResolver`, so nested views all stopped. The run loop
//!   reads the touches winit does not (`iced_winit::gesture::Touch`) and
//!   hands each cancel over as a `WheelScrolled` of no delta, hit tested
//!   where its touch was.
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
    /// The arena of the pan-zoom gesture last entered.
    static ARENA: Cell<Arena> = const {
        Cell::new(Arena {
            key: None,
            entrants: 0,
            winner: None,
        })
    };
}

/// When a pointer event is handled: iced's events carry no time stamp, so
/// now (a test sets it, [`testing::set_event_time`]).
pub fn event_time() -> Instant {
    EVENT_TIME.with(Cell::get).unwrap_or_else(Instant::now)
}

/// A pan-zoom gesture's arena (`GestureArenaManager`'s for its pointer):
/// the drag recognizers its start reached, in the order it reached them,
/// and the one that won.
#[derive(Debug, Clone, Copy)]
struct Arena {
    /// The gesture and when it started: each embedder (each test's
    /// trackpad) counts gestures afresh, so the time tells them apart.
    key: Option<(u64, Instant)>,
    entrants: u32,
    winner: Option<u32>,
}

/// A drag recognizer's place in a pan-zoom gesture's arena (see
/// [`enter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    key: (u64, Instant),
    index: u32,
}

/// Enters the arena of `gesture`, which started at `time` (a
/// [`Scroll::Start`]'s): a drag recognizer that takes the trackpad and
/// that the start reached (`addAllowedPointerPanZoom`). Recognizers enter
/// in the order Flutter's hit test reached them, the innermost first, the
/// order the pointer router then handed each of them the gesture's
/// events in: a widget with content enters once its content has handled
/// the start, and hands it each update before it looks at the update.
pub fn enter(gesture: u64, time: Instant) -> Entry {
    let key = (gesture, time);
    ARENA.with(|cell| {
        let mut arena = cell.get();
        if arena.key != Some(key) {
            arena = Arena {
                key: Some(key),
                entrants: 0,
                winner: None,
            };
        }
        let index = arena.entrants;
        arena.entrants += 1;
        cell.set(arena);
        Entry { key, index }
    })
}

impl Entry {
    /// Where it stands once its gesture's start has been dispatched:
    /// `Some(true)` won (alone in the arena, it won as the start closed
    /// the arena: `_resolveByDefault`), `Some(false)` another won, `None`
    /// still racing.
    pub fn standing(self) -> Option<bool> {
        ARENA.with(|cell| {
            let mut arena = cell.get();
            if arena.key != Some(self.key) {
                // A later gesture's arena: this one is over.
                return Some(false);
            }
            if arena.winner.is_none() && arena.entrants == 1 {
                arena.winner = Some(self.index);
                cell.set(arena);
            }
            arena.winner.map(|winner| winner == self.index)
        })
    }

    /// It moved past its slop along its axis (`resolve(accepted)`): it
    /// wins, unless another already has. Whether it won.
    pub fn accept(self) -> bool {
        ARENA.with(|cell| {
            let mut arena = cell.get();
            if arena.key != Some(self.key) {
                return false;
            }
            match arena.winner {
                Some(winner) => winner == self.index,
                None => {
                    arena.winner = Some(self.index);
                    cell.set(arena);
                    true
                }
            }
        })
    }
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
    /// A `PointerScrollInertiaCancelEvent` (see the module docs): a
    /// scrollable it reaches stops what moves it; nothing scrolls.
    InertiaCancel,
}

impl Scroll {
    /// The pan-zoom gesture it is part of (its momentum included); none for
    /// a scroll signal or an inertia cancel.
    pub fn gesture(&self) -> Option<u64> {
        match *self {
            Scroll::Signal | Scroll::InertiaCancel => None,
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
        Pointer::ScrollInertiaCancel => Scroll::InertiaCancel,
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
    use iced::{mouse, Event, Point, Vector};
    use iced_winit::gesture::{self, Embedder, Phase, Platform, Wheel};

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

        /// A finger lands on it at `at`: the inertia cancel the embedder
        /// sends, if it sends one, as the run loop hands it over, hit
        /// tested where the pointer is.
        pub fn touch(&mut self, at: Instant) -> Option<Sent> {
            let wheel = self.embedder.touch(at)?;
            Some((gesture::inertia_cancel_event(), Some(wheel)))
        }

        /// [`Trackpad::touch`] with the pointer at `position` in the
        /// window, wherever the harness's is (AppKit's `locationInWindow`
        /// for the touch, which the run loop hit tests the cancel at).
        pub fn touch_at(&mut self, at: Instant, position: Point) -> Option<Sent> {
            let (event, wheel) = self.touch(at)?;
            Some((
                event,
                wheel.map(|wheel| Wheel {
                    position: Some(position),
                    ..wheel
                }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Trackpad;
    use super::*;
    use iced::Point;
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
    fn an_arena_goes_to_its_sole_entrant_or_to_the_first_to_accept() {
        let t = Instant::now();
        // Alone: won once the start has been dispatched.
        let alone = enter(1, t);
        assert_eq!(alone.standing(), Some(true));
        // Two: undecided until one accepts, the other then lost.
        let inner = enter(2, t);
        let outer = enter(2, t);
        assert_eq!(inner.standing(), None);
        assert_eq!(outer.standing(), None);
        assert!(outer.accept());
        assert!(!inner.accept());
        assert_eq!(inner.standing(), Some(false));
        assert_eq!(outer.standing(), Some(true));
        // Another embedder's gesture 2, started later: an arena of its
        // own, the last one over.
        let later = enter(2, t + std::time::Duration::from_millis(1));
        assert_eq!(later.standing(), Some(true));
        assert_eq!(outer.standing(), Some(false));
    }

    /// A swipe's start, moves and lift, then macOS's momentum: its start,
    /// a change at `changed`, and its end 5 ms later.
    fn flung(pad: &mut Trackpad, t: Instant, changed: Instant) {
        let _ = pad.began(t);
        let _ = pad.changed(0.0, -20.0, t);
        let _ = pad.ended(t);
        let _ = pad.momentum(Phase::Began, 0.0, -10.0, t);
        let _ = pad.momentum(Phase::Changed, 0.0, -8.0, changed);
        let _ = pad.momentum(Phase::Ended, 0.0, 0.0, changed + 5 * MS);
    }

    const MS: std::time::Duration = std::time::Duration::from_millis(1);

    fn touch(pad: &mut Trackpad, at: Instant) -> Option<Scroll> {
        pad.touch(at).map(read)
    }

    #[test]
    fn a_touch_within_50_ms_of_the_momentums_last_change_cancels_it_once() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        // No momentum yet: a touch is nothing.
        assert_eq!(touch(&mut pad, t), None);
        let changed = t + 100 * MS;
        flung(&mut pad, t, changed);
        // 49 ms after its last change (its end changes nothing): a
        // cancel, a wheel event of no delta, of the last gesture.
        let (event, wheel) = pad.touch(changed + 49 * MS).expect("a cancel");
        assert_eq!(event, gesture::inertia_cancel_event());
        let wheel = wheel.expect("its gesture");
        assert_eq!(wheel.pointer, Pointer::ScrollInertiaCancel);
        assert_eq!(wheel.gesture, 1);
        assert_eq!(wheel.platform.delta, Vector::ZERO);
        assert_eq!(wheel.position, None);
        assert_eq!(gesture::with(Some(wheel), current), Scroll::InertiaCancel);
        assert_eq!(Scroll::InertiaCancel.gesture(), None);
        // Once: the next touch, however soon, is nothing.
        assert_eq!(touch(&mut pad, changed + 50 * MS), None);
        // A new change opens the window again; at 50 ms it is shut.
        let again = changed + 200 * MS;
        let _ = pad.momentum(Phase::Changed, 0.0, -1.0, again);
        assert_eq!(touch(&mut pad, again + 50 * MS), None);
        assert_eq!(
            touch(&mut pad, again + 10 * MS),
            Some(Scroll::InertiaCancel)
        );
    }

    #[test]
    fn a_touch_stamped_before_the_momentums_change_cancels_it_too() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        flung(&mut pad, t, t + 100 * MS);
        assert_eq!(touch(&mut pad, t + 60 * MS), Some(Scroll::InertiaCancel));
    }

    #[test]
    fn only_a_change_of_the_momentum_opens_the_window() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        let _ = pad.began(t);
        let _ = pad.ended(t);
        // Its start alone (the low power mode workaround), and its end.
        let _ = pad.momentum(Phase::Began, 0.0, -10.0, t + 5 * MS);
        assert_eq!(touch(&mut pad, t + 10 * MS), None);
        let _ = pad.momentum(Phase::Ended, 0.0, 0.0, t + 15 * MS);
        assert_eq!(touch(&mut pad, t + 20 * MS), None);
        // A wheel's signal or a swipe's own moves neither.
        let _ = pad.lines(0.0, 1.0, t + 25 * MS);
        assert_eq!(touch(&mut pad, t + 30 * MS), None);
    }

    #[test]
    fn a_new_swipes_start_ends_the_chance_to_cancel() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        let changed = t + 100 * MS;
        flung(&mut pad, t, changed);
        // Two fingers land (`MayBegin`): a pan-zoom start, which clears
        // the momentum's time.
        let _ = pad.send(Phase::MayBegin, Phase::None, (0.0, 0.0), changed + 5 * MS);
        assert_eq!(touch(&mut pad, changed + 10 * MS), None);
        // A start the embedder drops (one under way) clears it too.
        let _ = pad.momentum(Phase::Changed, 0.0, -2.0, changed + 20 * MS);
        let _ = pad.began(changed + 25 * MS);
        assert_eq!(touch(&mut pad, changed + 30 * MS), None);
    }

    #[test]
    fn a_cancel_is_hit_tested_where_its_touch_was() {
        let t = Instant::now();
        let mut pad = Trackpad::new();
        flung(&mut pad, t, t);
        let at = Point::new(12.0, 34.0);
        let (_, wheel) = pad.touch_at(t, at).expect("a cancel");
        let elsewhere = mouse::Cursor::Available(Point::new(90.0, 90.0));
        assert_eq!(
            gesture::with(wheel, || gesture::cursor(elsewhere)),
            mouse::Cursor::Available(at)
        );
        // A scroll's is the window's.
        let (_, wheel) = pad.pixels(0.0, 1.0, t);
        assert_eq!(
            gesture::with(wheel, || gesture::cursor(elsewhere)),
            elsewhere
        );
    }

    #[test]
    fn the_run_loop_places_a_touch_in_the_interfaces_coordinates() {
        let t = Instant::now();
        let mut embedder = gesture::Embedder::default();
        let _ = embedder.dispatch(gesture::Platform {
            phase: Phase::None,
            momentum_phase: Phase::Changed,
            precise: true,
            delta: Vector::new(0.0, -3.0),
            time: t,
        });
        // A point 30 × 40 into a Retina window's view, under an interface
        // scaled 1.25: 24 × 32 of the interface's.
        let touch = gesture::Touch {
            view: 1,
            time: t + 20 * MS,
            position: iced_winit::winit::dpi::LogicalPosition::new(30.0, 40.0),
        };
        let (event, wheel) = embedder.touched(&touch, 2.0, 2.5).expect("a cancel");
        assert_eq!(event, gesture::inertia_cancel_event());
        assert_eq!(wheel.position, Some(Point::new(24.0, 32.0)));
        assert_eq!(wheel.platform.time, touch.time);
        assert!(embedder.touched(&touch, 2.0, 2.5).is_none(), "once");
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
