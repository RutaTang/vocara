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
}
