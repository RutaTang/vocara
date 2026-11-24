//! Flutter's pan and double-tap recognizers, as the studio widgets wire them
//! (`GestureDetector(onPanStart, onPanUpdate, onPanEnd, onDoubleTap)`), for a
//! mouse: the thresholds and the arena outcome the Dart widgets got.
//!
//! - A press becomes a PAN once the pointer has travelled more than
//!   [`PRECISE_PAN_SLOP`] (`kPrecisePointerPanSlop`, a mouse's pan slop). The
//!   pan then wins the arena, so the press is no longer a tap. With
//!   `DragStartBehavior.start` (GestureDetector's default) the movement up to
//!   that point is swallowed: the first update reports only what moves after.
//! - A press released before that is a TAP. A tap whose press lands within
//!   [`DOUBLE_TAP_TIMEOUT`] of the previous tap's release, within
//!   [`DOUBLE_TAP_SLOP`] of its press and at least [`DOUBLE_TAP_MIN_TIME`]
//!   after it, completes a DOUBLE TAP on release.
//!
//! Only the primary (left) button: Flutter's pan and double-tap recognizers
//! take `kPrimaryButton` only.
//!
//! [`AnyTap`] is a modal barrier's tap (`ModalBarrier`'s
//! `_AnyTapGestureRecognizer`, which dismisses a dialog or popup route): a
//! press of ANY button that comes back up within [`TOUCH_SLOP`]
//! (`kTouchSlop`) of where it went down, with no other button pressed
//! meanwhile, is a tap — on its release, not its press.

use std::time::{Duration, Instant};

use iced::{Point, Vector};

/// `kPrecisePointerPanSlop`: how far a mouse press travels before it pans.
pub const PRECISE_PAN_SLOP: f32 = 2.0;

/// `kDoubleTapTimeout`.
pub const DOUBLE_TAP_TIMEOUT: Duration = Duration::from_millis(300);

/// `kDoubleTapMinTime`.
pub const DOUBLE_TAP_MIN_TIME: Duration = Duration::from_millis(40);

/// `kDoubleTapSlop`: how far the second tap may land from the first.
pub const DOUBLE_TAP_SLOP: f32 = 100.0;

/// `kTouchSlop`: how far a tap may travel from where it went down and still
/// be a tap (the pre-accept slop of `PrimaryPointerGestureRecognizer`, which
/// a barrier's tap recognizer is). Flutter's desktop gesture settings set no
/// touch slop of their own, so a mouse gets this one too.
pub const TOUCH_SLOP: f32 = 18.0;

/// What a pointer event completed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    PanStart,
    /// The movement since the last pan event.
    PanUpdate(Vector),
    PanEnd,
    DoubleTap,
}

#[derive(Debug, Clone, Copy)]
struct Down {
    at: Point,
    time: Instant,
    last: Point,
    travelled: f32,
    panning: bool,
    second_tap: bool,
}

#[derive(Debug, Clone, Copy)]
struct FirstTap {
    at: Point,
    down: Instant,
    up: Instant,
}

/// One widget's pan + double-tap recognizer state.
#[derive(Debug, Default, Clone)]
pub struct PanDoubleTap {
    down: Option<Down>,
    first_tap: Option<FirstTap>,
}

impl PanDoubleTap {
    /// The primary button went down at `at`.
    pub fn press(&mut self, at: Point, now: Instant) {
        let second_tap = match self.first_tap {
            Some(first)
                if now.duration_since(first.up) <= DOUBLE_TAP_TIMEOUT
                    && now.duration_since(first.down) >= DOUBLE_TAP_MIN_TIME
                    && first.at.distance(at) <= DOUBLE_TAP_SLOP =>
            {
                true
            }
            _ => {
                // Timed out, too far or too soon: this press starts afresh.
                self.first_tap = None;
                false
            }
        };
        self.down = Some(Down {
            at,
            time: now,
            last: at,
            travelled: 0.0,
            panning: false,
            second_tap,
        });
    }

    /// The pointer moved to `to`.
    pub fn moved(&mut self, to: Point) -> Option<Gesture> {
        let down = self.down.as_mut()?;
        if down.panning {
            let delta = to - down.last;
            down.last = to;
            return Some(Gesture::PanUpdate(delta));
        }
        down.travelled += down.last.distance(to);
        down.last = to;
        if down.travelled > PRECISE_PAN_SLOP {
            // The pan wins the arena: no tap, and no double tap either.
            down.panning = true;
            self.first_tap = None;
            return Some(Gesture::PanStart);
        }
        None
    }

    /// The primary button went up.
    pub fn release(&mut self, now: Instant) -> Option<Gesture> {
        let down = self.down.take()?;
        if down.panning {
            return Some(Gesture::PanEnd);
        }
        if down.second_tap {
            self.first_tap = None;
            return Some(Gesture::DoubleTap);
        }
        self.first_tap = Some(FirstTap {
            at: down.at,
            down: down.time,
            up: now,
        });
        None
    }
}

/// A modal barrier's tap (see the module docs), fed the pointer events the
/// barrier sees: [`press`](Self::press) for a button going down on it,
/// [`moved`](Self::moved), and [`release`](Self::release), which says
/// whether that release completed a tap.
///
/// A second button pressed while one is down changes the pointer's buttons,
/// which rejects the tap (`BaseTapGestureRecognizer.handlePrimaryPointer`);
/// the press is over once every button is back up.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct AnyTap {
    /// Where the tap in progress went down; `None` once it is rejected.
    down: Option<Point>,
    /// How many buttons are down since the first went down on the barrier.
    held: u8,
}

impl AnyTap {
    /// A button went down at `at`: the first starts a tap there, another
    /// rejects the one in progress.
    pub fn press(&mut self, at: Point) {
        self.down = (self.held == 0).then_some(at);
        self.held = self.held.saturating_add(1);
    }

    /// The pointer moved to `to`: past the slop, it is no tap.
    pub fn moved(&mut self, to: Point) {
        if self.down.is_some_and(|at| at.distance(to) > TOUCH_SLOP) {
            self.down = None;
        }
    }

    /// A button went up: whether that completed a tap (the last button up,
    /// the tap not rejected).
    pub fn release(&mut self) -> bool {
        if self.held == 0 {
            // A press the barrier did not see.
            return false;
        }
        self.held -= 1;
        if self.held > 0 {
            self.down = None;
            return false;
        }
        self.down.take().is_some()
    }

    /// Whether a press that went down on the barrier is still down.
    pub fn is_pressed(&self) -> bool {
        self.held > 0
    }

    /// The pointer is gone (a lost touch): no tap.
    pub fn cancel(&mut self) {
        *self = Self::default();
    }
}

/// [`AnyTap`] fed an event: a press (any button, or a touch) where
/// `on_barrier` says the barrier took it, every move and release. Whether
/// the event completed a tap, and whether the barrier took the event (a
/// press it starts or joins, a release that completes a tap), which its
/// caller then captures.
pub fn any_tap_event(
    tap: &mut AnyTap,
    event: &iced::Event,
    on_barrier: impl Fn(Point) -> bool,
    cursor: Option<Point>,
) -> (bool, bool) {
    use iced::{mouse, touch, Event};
    match event {
        Event::Mouse(mouse::Event::ButtonPressed(_)) => {
            // A second button joins the press wherever the pointer is: it is
            // the same pointer, still the barrier's.
            match cursor {
                Some(at) if tap.is_pressed() || on_barrier(at) => {
                    tap.press(at);
                    (false, true)
                }
                None if tap.is_pressed() => {
                    tap.press(Point::ORIGIN);
                    (false, true)
                }
                _ => (false, false),
            }
        }
        Event::Touch(touch::Event::FingerPressed { position, .. })
            if tap.is_pressed() || on_barrier(*position) =>
        {
            tap.press(*position);
            (false, true)
        }
        Event::Mouse(mouse::Event::CursorMoved { position })
        | Event::Touch(touch::Event::FingerMoved { position, .. }) => {
            tap.moved(*position);
            (false, false)
        }
        Event::Mouse(mouse::Event::ButtonReleased(_))
        | Event::Touch(touch::Event::FingerLifted { .. }) => {
            let tapped = tap.release();
            (tapped, tapped)
        }
        Event::Touch(touch::Event::FingerLost { .. }) => {
            tap.cancel();
            (false, false)
        }
        _ => (false, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32) -> Point {
        Point::new(x, 0.0)
    }

    #[test]
    fn a_pan_swallows_the_movement_that_started_it() {
        let mut g = PanDoubleTap::default();
        let t = Instant::now();
        g.press(at(0.0), t);
        assert_eq!(g.moved(at(1.0)), None);
        assert_eq!(g.moved(at(20.0)), Some(Gesture::PanStart));
        assert_eq!(
            g.moved(at(28.0)),
            Some(Gesture::PanUpdate(Vector::new(8.0, 0.0)))
        );
        assert_eq!(g.release(t), Some(Gesture::PanEnd));
    }

    #[test]
    fn two_quick_taps_make_a_double_tap() {
        let mut g = PanDoubleTap::default();
        let t = Instant::now();
        g.press(at(0.0), t);
        assert_eq!(g.release(t + Duration::from_millis(30)), None);
        g.press(at(3.0), t + Duration::from_millis(120));
        assert_eq!(
            g.release(t + Duration::from_millis(150)),
            Some(Gesture::DoubleTap)
        );
    }

    #[test]
    fn a_late_second_tap_is_a_new_first_tap() {
        let mut g = PanDoubleTap::default();
        let t = Instant::now();
        g.press(at(0.0), t);
        g.release(t);
        g.press(at(0.0), t + Duration::from_millis(400));
        assert_eq!(g.release(t + Duration::from_millis(420)), None);
        g.press(at(0.0), t + Duration::from_millis(500));
        assert_eq!(
            g.release(t + Duration::from_millis(520)),
            Some(Gesture::DoubleTap)
        );
    }

    #[test]
    fn a_pan_between_taps_cancels_the_double_tap() {
        let mut g = PanDoubleTap::default();
        let t = Instant::now();
        g.press(at(0.0), t);
        g.release(t);
        g.press(at(0.0), t + Duration::from_millis(100));
        assert_eq!(g.moved(at(10.0)), Some(Gesture::PanStart));
        assert_eq!(
            g.release(t + Duration::from_millis(150)),
            Some(Gesture::PanEnd)
        );
    }

    #[test]
    fn a_barrier_tap_is_any_button_and_lands_on_release() {
        let mut tap = AnyTap::default();
        tap.press(at(0.0));
        assert!(tap.is_pressed());
        tap.moved(at(18.0));
        assert!(tap.release(), "up within the slop");
        assert!(!tap.is_pressed());
        assert!(!tap.release(), "a release the barrier never saw go down");
    }

    #[test]
    fn a_barrier_press_dragged_past_the_slop_is_no_tap() {
        let mut tap = AnyTap::default();
        tap.press(at(0.0));
        tap.moved(at(18.5));
        tap.moved(at(1.0));
        assert!(!tap.release(), "once past the slop, coming back is no tap");
    }

    #[test]
    fn a_second_button_rejects_the_barrier_tap() {
        let mut tap = AnyTap::default();
        tap.press(at(0.0));
        tap.press(at(0.0));
        assert!(!tap.release());
        assert!(tap.is_pressed(), "one button still down");
        assert!(!tap.release());
        // A fresh press afterwards taps again.
        tap.press(at(0.0));
        assert!(tap.release());
    }

    #[test]
    fn a_barrier_takes_any_buttons_press_on_it_and_the_release_that_taps() {
        use iced::{mouse, Event};
        let press = |b| Event::Mouse(mouse::Event::ButtonPressed(b));
        let release = |b| Event::Mouse(mouse::Event::ButtonReleased(b));
        let on_barrier = |p: Point| p.x < 100.0;
        let mut tap = AnyTap::default();
        // Off the barrier (on the card): not its press.
        let off = Some(at(150.0));
        assert_eq!(
            any_tap_event(&mut tap, &press(mouse::Button::Left), on_barrier, off),
            (false, false)
        );
        assert_eq!(
            any_tap_event(&mut tap, &release(mouse::Button::Left), on_barrier, off),
            (false, false)
        );
        // The secondary button on the barrier taps on its release.
        let on = Some(at(10.0));
        assert_eq!(
            any_tap_event(&mut tap, &press(mouse::Button::Right), on_barrier, on),
            (false, true),
            "taken, no tap yet"
        );
        assert_eq!(
            any_tap_event(&mut tap, &release(mouse::Button::Right), on_barrier, on),
            (true, true)
        );
    }
}
