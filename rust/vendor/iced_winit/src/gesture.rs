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
//!   `Stationary` phase; each of its `Changed` events stamped the time
//!   (`last_scroll_momentum_changed_time`, from the first `Changed` on: a
//!   workaround for `touchesBegan:` in low power mode), which a pan-zoom
//!   start cleared;
//! - a finger touching the trackpad (`touchesBeganWithEvent:` with a touch
//!   in `allTouches`; the view allowed indirect touches and wanted resting
//!   ones, `viewDidLoad`) less than 50 ms after that stamp
//!   (`kTrackpadTouchInertiaCancelWindowMs`) became a
//!   `kFlutterPointerSignalKindScrollInertiaCancel` signal where the
//!   pointer was (`locationInWindow`), and cleared the stamp: one cancel
//!   per momentum. The framework handed it to every scrollable its hit
//!   test reached, which stopped what was moving it
//!   (`PointerScrollInertiaCancelEvent`).
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
//!
//! winit reads no touches, so each window's view is set up as Flutter's
//! was and its class given a `touchesBeganWithEvent:` that records each
//! touch ([`accept_touches`], [`Touch`]) and calls through to what the
//! class had. Before each event it handles, the run loop takes the touches
//! recorded since ([`take_touches`]) and runs the embedder's rule over
//! each ([`Embedder::touched`]): an inertia cancel reaches the interface
//! as a `WheelScrolled` of no delta whose gesture is
//! [`Pointer::ScrollInertiaCancel`], hit tested where the touch was
//! ([`cursor`]). Off macOS there are no touches.

use crate::core::event;
use crate::core::mouse;
use crate::core::time::{Duration, Instant};
use crate::core::{Event, Point, Vector};
use crate::runtime::user_interface;

use std::cell::Cell;

/// `kTrackpadTouchInertiaCancelWindowMs` (in seconds, despite its name):
/// how soon after the momentum's last change a touch cancels it ("to
/// account for delays in event propagation observed in macOS Ventura").
pub const INERTIA_CANCEL_WINDOW: Duration = Duration::from_millis(50);

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
    /// `kFlutterPointerSignalKindScrollInertiaCancel`: a
    /// `PointerScrollInertiaCancelEvent`, which a touch on the trackpad
    /// sent while macOS's momentum ran (see the module docs). Not a
    /// scroll: the run loop makes its wheel event, of no delta.
    ScrollInertiaCancel,
    /// Nothing: momentum, a `Stationary` phase, or a start or an end the
    /// embedder dropped.
    None,
}

/// A wheel event's gesture: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wheel {
    /// What AppKit said of it; for an inertia cancel, the touch's time
    /// alone (no phases, no delta).
    pub platform: Platform,
    /// What the embedder made of it.
    pub pointer: Pointer,
    /// The pan-zoom gesture it belongs to, counted from 1 (a new one at
    /// each start the embedder sends); an event after it (its momentum, a
    /// signal, an inertia cancel) carries the last one's, 0 before any.
    pub gesture: u64,
    /// Where an inertia cancel's touch was (`locationInWindow`), in the
    /// interface's coordinates: the run loop hands the widgets that as the
    /// cursor for its event ([`cursor`]). None for a scroll, which moved
    /// the cursor itself (winit's `scrollWheel:` reports its location
    /// first), and for a cancel a test sends where its pointer is.
    pub position: Option<Point>,
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

/// The cursor the widgets get with the event being handled: an inertia
/// cancel's where its touch was ([`Wheel::position`]), any other's
/// `cursor`, the window's. The run loop's, public so a test harness
/// dispatches as it does.
pub fn cursor(cursor: mouse::Cursor) -> mouse::Cursor {
    current()
        .and_then(|wheel| wheel.position)
        .map_or(cursor, mouse::Cursor::Available)
}

/// The wheel event an inertia cancel reaches the widgets as: no delta.
pub fn inertia_cancel_event() -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Pixels { x: 0.0, y: 0.0 },
    })
}

/// The embedder's state across scroll events (`FlutterViewController`'s
/// `_mouseState`, its scroll fields): whether a pan-zoom has started, and
/// when the momentum last changed.
#[derive(Debug, Clone, Default)]
pub struct Embedder {
    started: bool,
    gesture: u64,
    /// `last_scroll_momentum_changed_time`, while set (Flutter's 0 is
    /// none).
    momentum_changed: Option<Instant>,
}

impl Embedder {
    /// `dispatchGestureEvent:` for a scroll AppKit reported as `platform`.
    pub fn dispatch(&mut self, platform: Platform) -> Wheel {
        let pointer = match platform.phase {
            Phase::Began | Phase::MayBegin => {
                // "Ensure scroll inertia cancel event is not sent
                // afterwards", a dropped start too.
                self.momentum_changed = None;
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
            Phase::None | Phase::Stationary => {
                // From its first change on ("touchesBegan: is called
                // unexpectedly while in low power mode within the interval
                // between momentum start and the first momentum change").
                if platform.momentum_phase == Phase::Changed {
                    self.momentum_changed = Some(platform.time);
                }
                Pointer::None
            }
        };
        Wheel {
            platform,
            pointer,
            gesture: self.gesture,
            position: None,
        }
    }

    /// `touchesBeganWithEvent:` for a touch on the trackpad at `time`: the
    /// inertia cancel the embedder sent if the momentum last changed less
    /// than [`INERTIA_CANCEL_WINDOW`] before (or after: a touch stamped
    /// earlier counts too), after which none until it changes again. Its
    /// [`Wheel::position`] is none (where the pointer is).
    pub fn touch(&mut self, time: Instant) -> Option<Wheel> {
        let changed = self.momentum_changed?;
        if time.saturating_duration_since(changed) >= INERTIA_CANCEL_WINDOW {
            return None;
        }
        // "Ensure no further scroll inertia cancel event will be sent."
        self.momentum_changed = None;
        Some(Wheel {
            platform: Platform {
                phase: Phase::None,
                momentum_phase: Phase::None,
                precise: true,
                delta: Vector::ZERO,
                time,
            },
            pointer: Pointer::ScrollInertiaCancel,
            gesture: self.gesture,
            position: None,
        })
    }

    /// [`Embedder::touch`] for `touch`, which AppKit reported to this
    /// embedder's window: the event the widgets get and its gesture, hit
    /// tested where the touch was. `backing_scale` is the window's
    /// (winit's `scale_factor`), `scale_factor` the interface's (iced's
    /// window state's), as a cursor position is converted.
    pub fn touched(
        &mut self,
        touch: &Touch,
        backing_scale: f64,
        scale_factor: f32,
    ) -> Option<(Event, Wheel)> {
        let wheel = self.touch(touch.time)?;
        let position = crate::conversion::cursor_position(
            touch.position.to_physical(backing_scale),
            scale_factor,
        );
        Some((
            inertia_cancel_event(),
            Wheel {
                position: Some(position),
                ..wheel
            },
        ))
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
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEventPhase, NSEventType};
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
    Some(Platform {
        phase: phase(event.phase()),
        momentum_phase: phase(event.momentumPhase()),
        precise,
        delta: Vector::new(x as f32, y as f32),
        time: event_time(event.timestamp()),
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

/// An `NSEvent.timestamp` on this process's clock: it is seconds since the
/// system started, as the uptime is.
#[cfg(target_os = "macos")]
fn event_time(timestamp: f64) -> Instant {
    use objc2_foundation::NSProcessInfo;

    let now = Instant::now();
    let ago = NSProcessInfo::processInfo().systemUptime() - timestamp;
    Duration::try_from_secs_f64(ago.max(0.0))
        .ok()
        .and_then(|ago| now.checked_sub(ago))
        .unwrap_or(now)
}

/// A finger landing on the trackpad, as AppKit reported it to a window's
/// view (`touchesBeganWithEvent:` with a touch in `allTouches`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    /// The view it went to, by address: a window's, as [`view_of`] gives
    /// it.
    pub view: usize,
    /// When (`NSEvent.timestamp`, on this process's clock).
    pub time: Instant,
    /// Where the pointer was (`locationInWindow`, in the view: points
    /// from its top left, winit's view being flipped).
    pub position: winit::dpi::LogicalPosition<f64>,
}

/// The touches AppKit reported since the last call, in order, on this
/// thread (the main one). The run loop takes them before each event it
/// handles, so a touch comes before what AppKit dispatched after it.
pub fn take_touches() -> Vec<Touch> {
    #[cfg(target_os = "macos")]
    {
        touches::take()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// The address of `window`'s view (winit's `WinitView`, its content
/// view), which [`Touch::view`] names; none off macOS.
pub fn view_of(window: &winit::window::Window) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::AppKit(handle) => {
                Some(handle.ns_view.as_ptr() as usize)
            }
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}

/// Has `window`'s view report trackpad touches as Flutter's view did
/// (`viewDidLoad`: `setAllowedTouchTypes:NSTouchTypeMaskIndirect`,
/// `setWantsRestingTouches:YES`), and gives its class a
/// `touchesBeganWithEvent:` that records each touch for
/// [`take_touches`], then calls through to the one the class had (its
/// own, or the one it inherited: `NSResponder`'s, which hands the event up
/// the responder chain). Once per class; nothing off macOS. The run loop
/// calls it on each window it makes.
pub fn accept_touches(window: &winit::window::Window) {
    #[cfg(target_os = "macos")]
    touches::accept(window);
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// The touches' side of AppKit: the view's set-up, the method added to its
/// class, and what it records.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod touches {
    use super::{Touch, event_time};

    use std::cell::RefCell;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::ptr;

    use objc2::MainThreadMarker;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Method, Sel};
    use objc2::{ffi, sel};
    use objc2_app_kit::{NSEvent, NSTouchTypeMask, NSView};
    use winit::dpi::LogicalPosition;

    /// `touchesBeganWithEvent:`'s implementation, as the runtime calls it.
    type TouchesBegan =
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut NSEvent);

    thread_local! {
        /// The touches recorded since they were last taken.
        static TOUCHES: RefCell<Vec<Touch>> =
            const { RefCell::new(Vec::new()) };
        /// The classes given the method, each with the implementation it
        /// had, which the method calls through to.
        static INSTALLED: RefCell<Vec<(&'static AnyClass, Option<Imp>)>> =
            const { RefCell::new(Vec::new()) };
    }

    pub(super) fn take() -> Vec<Touch> {
        TOUCHES.with(|touches| std::mem::take(&mut *touches.borrow_mut()))
    }

    pub(super) fn accept(window: &winit::window::Window) {
        let Some(view) = super::view_of(window) else {
            return;
        };
        if MainThreadMarker::new().is_none() {
            return;
        }
        // SAFETY: winit's AppKit handle is its window's view, which lives
        // as long as the window; this is the main thread.
        let view: &NSView = unsafe { &*(view as *const NSView) };
        view.setAllowedTouchTypes(NSTouchTypeMask::Indirect);
        view.setWantsRestingTouches(true);
        install(view.class());
    }

    /// Gives `class` the method, unless it has it already, its own or
    /// inherited (calling through to itself, it would never end).
    fn install(class: &'static AnyClass) {
        let selector = sel!(touchesBeganWithEvent:);
        // `void (id self, SEL _cmd, NSEvent *event)`.
        let types = c"v@:@";
        // SAFETY: the runtime calls an `Imp` with the arguments its types
        // say, which `touches_began` takes.
        let ours =
            unsafe { std::mem::transmute::<TouchesBegan, Imp>(touches_began) };
        let current =
            class.instance_method(selector).map(Method::implementation);
        if current.is_some_and(|current| ptr::fn_addr_eq(current, ours)) {
            return;
        }
        let target = ptr::from_ref(class).cast_mut();
        // SAFETY: a registered class, changed on the main thread; the
        // types are the method's.
        let added = unsafe {
            ffi::class_addMethod(target, selector, ours, types.as_ptr())
        };
        let previous = if added.as_bool() {
            // The one it inherited.
            current
        } else {
            // Its own: replaced, and called through.
            // SAFETY: as above.
            unsafe {
                ffi::class_replaceMethod(target, selector, ours, types.as_ptr())
            }
        };
        INSTALLED
            .with(|installed| installed.borrow_mut().push((class, previous)));
    }

    /// What the method had before on `class`, or the nearest class above it
    /// that was given it.
    fn previous(mut class: &'static AnyClass) -> Option<Imp> {
        loop {
            let found = INSTALLED.with(|installed| {
                installed
                    .borrow()
                    .iter()
                    .find(|(c, _)| ptr::eq(*c, class))
                    .map(|(_, previous)| *previous)
            });
            if let Some(previous) = found {
                return previous;
            }
            class = class.superclass()?;
        }
    }

    /// `touchesBeganWithEvent:` on a view of a class given it.
    unsafe extern "C-unwind" fn touches_began(
        this: *mut AnyObject,
        cmd: Sel,
        event: *mut NSEvent,
    ) {
        // SAFETY: AppKit sends it, on the main thread, to a view, with the
        // event that has its touches.
        let (view, touch_event) =
            unsafe { (this.cast::<NSView>().as_ref(), event.as_ref()) };
        let Some(view) = view else {
            return;
        };
        if let Some(touch_event) = touch_event {
            // Nothing of the recording may unwind into AppKit.
            let _ =
                catch_unwind(AssertUnwindSafe(|| record(view, touch_event)));
        }
        if let Some(previous) = previous(view.class()) {
            // SAFETY: the implementation the class had for this selector,
            // called as the runtime would have.
            unsafe {
                let previous =
                    std::mem::transmute::<Imp, TouchesBegan>(previous);
                previous(this, cmd, event);
            }
        }
    }

    /// Records the touch `event` reports to `view`, if it has one
    /// (`event.allTouches.anyObject`: the embedder sent nothing without).
    fn record(view: &NSView, event: &NSEvent) {
        if event.allTouches().anyObject().is_none() {
            return;
        }
        let at = view.convertPoint_fromView(event.locationInWindow(), None);
        let touch = Touch {
            view: ptr::from_ref(view) as usize,
            time: event_time(event.timestamp()),
            position: LogicalPosition::new(at.x, at.y),
        };
        TOUCHES.with(|touches| touches.borrow_mut().push(touch));
    }
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

