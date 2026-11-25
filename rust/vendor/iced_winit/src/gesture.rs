//! vocara: the gesture a wheel event belongs to, which iced's
//! `mouse::Event::WheelScrolled` cannot carry. Not part of upstream
//! iced_winit (see `rust/vendor/README.md`).
//!
//! winit 0.30 reports a scroll's phase with macOS's momentum phase merged
//! into it, and iced drops even that. Flutter's macOS embedder told them
//! apart (`FlutterViewController.mm`, Flutter 3.38.4:
//! `dispatchGestureEvent:` and `dispatchMouseEvent:phase:`):
//!
//! - a scroll with a gesture phase (a trackpad's two fingers, a Magic
//!   Mouse's) became a pan-zoom gesture: `kPanZoomStart` on `Began` or
//!   `MayBegin`, `kPanZoomUpdate` on `Changed` (its pan the sum of the
//!   `scrollingDelta`s), `kPanZoomEnd` on `Ended` or `Cancelled`; a start
//!   while one is under way, or an end with none, was dropped;
//! - a scroll with neither phase nor momentum phase (a mouse wheel, or a
//!   precise device that sends no phases) became a scroll signal, its
//!   delta `scrollingDelta` (times 40 when not precise);
//! - the momentum macOS sends after the fingers lift reached no widget at
//!   all ("the framework will generate scroll momentum"), nor did a
//!   `Stationary` phase.
//!
//! The run loop reads, while it converts a `MouseWheel`, the event AppKit
//! is dispatching (`NSApp.currentEvent`: winit handles the scroll inside
//! the view's `scrollWheel:` and iced converts it before it returns),
//! checks it is the one winit made the event of, runs the embedder's
//! state machine over it ([`Embedder`]), and hands each wheel event to the
//! interface on its own with its [`Wheel`] set ([`update_each`]): a widget
//! reads it with [`current`] while it handles that `WheelScrolled`. Off
//! macOS, and for an event whose `NSEvent` cannot be read back, there is
//! none; the app takes such a wheel event as a scroll signal.

use crate::core::event;
use crate::core::mouse;
use crate::core::time::Instant;
use crate::core::{Event, Vector};
use crate::runtime::user_interface;

use std::cell::Cell;

/// A phase of a scroll gesture, as AppKit reports it (`NSEventPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// `NSEventPhaseNone`.
    #[default]
    None,
    /// `NSEventPhaseBegan`.
    Began,
    /// `NSEventPhaseStationary` (or a value AppKit does not document).
    Stationary,
    /// `NSEventPhaseChanged`.
    Changed,
    /// `NSEventPhaseEnded`.
    Ended,
    /// `NSEventPhaseCancelled`.
    Cancelled,
    /// `NSEventPhaseMayBegin`.
    MayBegin,
}

/// What AppKit said of a scroll event beyond what winit passes on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Platform {
    /// `NSEvent.phase`: the fingers' phase.
    pub phase: Phase,
    /// `NSEvent.momentumPhase`: the momentum's phase after they lifted.
    pub momentum_phase: Phase,
    /// `hasPreciseScrollingDeltas`: the deltas are points, not lines.
    pub precise: bool,
    /// `scrollingDeltaX` and `scrollingDeltaY`: points (lines when not
    /// precise), positive toward the right and the bottom (the content
    /// moving with the fingers, in macOS's natural direction).
    pub delta: Vector,
    /// When it happened (`NSEvent.timestamp`, on this process's clock):
    /// Flutter's `PointerEvent.timeStamp`, which its velocity trackers
    /// sample.
    pub time: Instant,
}

/// What Flutter's embedder sent the framework for a scroll event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    /// `kHover` with a scroll signal: a `PointerScrollEvent`.
    Signal,
    /// `kPanZoomStart`: a `PointerPanZoomStartEvent`.
    PanZoomStart,
    /// `kPanZoomUpdate`: a `PointerPanZoomUpdateEvent`, its `panDelta`
    /// the event's [`Platform::delta`].
    PanZoomUpdate,
    /// `kPanZoomEnd`: a `PointerPanZoomEndEvent`.
    PanZoomEnd,
    /// Nothing: momentum, a `Stationary` phase, or a start or an end the
    /// embedder dropped.
    None,
}

/// A wheel event's gesture: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wheel {
    /// What AppKit said of it.
    pub platform: Platform,
    /// What the embedder made of it.
    pub pointer: Pointer,
    /// The pan-zoom gesture it belongs to, counted from 1 (a new one at
    /// each start the embedder sends); an event after it (its momentum, a
    /// signal) carries the last one's, 0 before any.
    pub gesture: u64,
}

thread_local! {
    static CURRENT: Cell<Option<Wheel>> = const { Cell::new(None) };
}

/// The wheel event being handled now: the gesture of the `WheelScrolled`
/// a widget is handling, if the platform told it.
pub fn current() -> Option<Wheel> {
    CURRENT.with(Cell::get)
}

/// Runs `f` with [`current`] answering `wheel` (on this thread), as the
/// run loop does around each wheel event; a test feeds its own.
pub fn with<R>(wheel: Option<Wheel>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Wheel>);

    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|current| current.set(self.0));
        }
    }

    let _restore = Restore(CURRENT.with(|current| current.replace(wheel)));
    f()
}

/// The embedder's state across scroll events (`FlutterViewController`'s
/// `_mouseState`, its scroll fields): whether a pan-zoom has started.
#[derive(Debug, Clone, Default)]
pub struct Embedder {
    started: bool,
    gesture: u64,
}

impl Embedder {
    /// `dispatchGestureEvent:` for a scroll AppKit reported as `platform`.
    pub fn dispatch(&mut self, platform: Platform) -> Wheel {
        let pointer = match platform.phase {
            Phase::Began | Phase::MayBegin => {
                if self.started {
                    // "Already started on a previous gesture type".
                    Pointer::None
                } else {
                    self.started = true;
                    self.gesture += 1;
                    Pointer::PanZoomStart
                }
            }
            Phase::Changed => Pointer::PanZoomUpdate,
            Phase::Ended | Phase::Cancelled => {
                if self.started {
                    self.started = false;
                    Pointer::PanZoomEnd
                } else {
                    // "we have no active gesture to cancel".
                    Pointer::None
                }
            }
            Phase::None if platform.momentum_phase == Phase::None => Pointer::Signal,
            // "Skip momentum update events, the framework will generate
            // scroll momentum."
            Phase::None | Phase::Stationary => Pointer::None,
        };
        Wheel {
            platform,
            pointer,
            gesture: self.gesture,
        }
    }

    /// The gesture of `event`, if it is a `MouseWheel` whose `NSEvent` is
    /// the one AppKit is dispatching; `backing_scale` is its window's
    /// (`backingScaleFactor`, winit's `scale_factor`), which winit scaled
    /// a precise delta by.
    pub fn wheel(
        &mut self,
        event: &winit::event::WindowEvent,
        backing_scale: f64,
    ) -> Option<Wheel> {
        let winit::event::WindowEvent::MouseWheel { delta, .. } = event else {
            return None;
        };
        platform(delta, backing_scale).map(|platform| self.dispatch(platform))
    }
}

/// What AppKit says of the scroll event it is dispatching, if winit made
/// `delta` of it (`-[WinitView scrollWheel:]`).
#[cfg(target_os = "macos")]
pub fn platform(
    delta: &winit::event::MouseScrollDelta,
    backing_scale: f64,
) -> Option<Platform> {
    use crate::core::time::Duration;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEventPhase, NSEventType};
    use objc2_foundation::NSProcessInfo;
    use winit::dpi::LogicalPosition;
    use winit::event::MouseScrollDelta;

    fn phase(phase: NSEventPhase) -> Phase {
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

    let mtm = MainThreadMarker::new()?;
    let event = NSApplication::sharedApplication(mtm).currentEvent()?;
    if event.r#type() != NSEventType::ScrollWheel {
        return None;
    }
    let precise = event.hasPreciseScrollingDeltas();
    let (x, y) = (event.scrollingDeltaX(), event.scrollingDeltaY());
    // The delta as winit made it of this event: another event's is not
    // this one's.
    let same = match *delta {
        MouseScrollDelta::LineDelta(dx, dy) => {
            !precise && dx == x as f32 && dy == y as f32
        }
        MouseScrollDelta::PixelDelta(position) => {
            precise
                && position
                    == LogicalPosition::new(x, y)
                        .to_physical::<f64>(backing_scale)
        }
    };
    if !same {
        return None;
    }
    // Its time is seconds since the system started, as the uptime is.
    let now = Instant::now();
    let ago = NSProcessInfo::processInfo().systemUptime() - event.timestamp();
    let time = Duration::try_from_secs_f64(ago.max(0.0))
        .ok()
        .and_then(|ago| now.checked_sub(ago))
        .unwrap_or(now);
    Some(Platform {
        phase: phase(event.phase()),
        momentum_phase: phase(event.momentumPhase()),
        precise,
        delta: Vector::new(x as f32, y as f32),
        time,
    })
}

/// What AppKit says of the scroll event it is dispatching: nothing, off
/// macOS.
#[cfg(not(target_os = "macos"))]
pub fn platform(
    _delta: &winit::event::MouseScrollDelta,
    _backing_scale: f64,
) -> Option<Platform> {
    None
}

/// Hands `events` to `update` in order, each wheel event in a call of its
/// own with its gesture (the next of `wheels`, one per wheel event) as
/// [`current`], and merges what the calls return as one call over them
/// all would have: any outdated, else the last cursor, the soonest redraw,
/// the first input method enabled, any layout change. The run loop's
/// dispatch, public so a test harness dispatches as it does.
pub fn update_each(
    events: &[Event],
    wheels: &[Option<Wheel>],
    mut update: impl FnMut(&[Event]) -> (user_interface::State, Vec<event::Status>),
) -> (user_interface::State, Vec<event::Status>) {
    let is_wheel = |event: &Event| {
        matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }))
    };
    if !events.iter().any(is_wheel) {
        return update(events);
    }
    let mut wheels = wheels.iter().copied();
    let mut merged: Option<user_interface::State> = None;
    let mut statuses = Vec::with_capacity(events.len());
    let mut rest = events;
    while !rest.is_empty() {
        let (state, run) = if is_wheel(&rest[0]) {
            let wheel = wheels.next().flatten();
            let (state, run) = with(wheel, || update(&rest[..1]));
            rest = &rest[1..];
            (state, run)
        } else {
            let end = rest.iter().position(is_wheel).unwrap_or(rest.len());
            let (state, run) = update(&rest[..end]);
            rest = &rest[end..];
            (state, run)
        };
        statuses.extend(run);
        merged = Some(match merged {
            None => state,
            Some(before) => merge(before, state),
        });
    }
    (
        merged.unwrap_or(user_interface::State::Outdated),
        statuses,
    )
}

/// Two updates' states as one: see [`update_each`].
fn merge(
    before: user_interface::State,
    after: user_interface::State,
) -> user_interface::State {
    use user_interface::State;

    match (before, after) {
        (
            State::Updated {
                redraw_request: earlier_redraw,
                input_method: mut earlier_input,
                has_layout_changed: earlier_layout,
                ..
            },
            State::Updated {
                mouse_interaction,
                redraw_request,
                input_method,
                has_layout_changed,
            },
        ) => {
            earlier_input.merge(&input_method);
            State::Updated {
                mouse_interaction,
                redraw_request: earlier_redraw.min(redraw_request),
                input_method: earlier_input,
                has_layout_changed: earlier_layout || has_layout_changed,
            }
        }
        _ => State::Outdated,
    }
}

