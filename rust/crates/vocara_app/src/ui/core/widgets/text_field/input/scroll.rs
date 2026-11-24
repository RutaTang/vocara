//! The single-line field's horizontal scroll, as Flutter 3.38's
//! `EditableText` drove the `Scrollable` its line sits in, on macOS, under
//! the app's `ClampingScrollPhysics` (`_NoBounceScrollBehavior`).
//!
//! The offset (`_scrollController.offset`, `pixels`) is the field's own and
//! stays where it was put: losing the focus or the window leaves it, and a
//! later change moves it only as one of these did:
//!
//! - **A reveal** (`_getOffsetToRevealCaret` of `getLocalRectForCaret`):
//!   the least scroll that shows the caret's 2 px rectangle (2 device
//!   pixels left of the glyph edge, never left of the text nor past it, its
//!   left on a device pixel), clamped to the scroll range. A caret already
//!   in view moves nothing.
//! - **Animated** (`_scheduleShowCaretOnScreen(withAnimation: true)`), for
//!   a user's edit (typing, a paste, a cut, a deletion, an input method's
//!   commit), a click, a double or triple click, a right-click that moved
//!   the selection, and the field gaining the focus: after the frame that
//!   shows the change, the offset runs to the reveal over 100 ms on
//!   `Curves.fastOutSlowIn` (`_caretAnimationDuration`,
//!   `_caretAnimationCurve`), from wherever it is, as `animateTo` did; less
//!   than a device pixel away, it jumps there.
//! - **At once** (`bringIntoView`), for a keyboard move of the caret or the
//!   selection (arrows, Select All) and a drag of the selection: the reveal
//!   of the selection's moving end, in the same frame. The menu's Copy
//!   reveals at once, its Cut and Paste right after the frame that shows
//!   them.
//! - **The scroll range** (`maxScrollExtent`) is the line's width and the
//!   caret's room (`_caretMargin`, 3 px) less the field's: text that shrank
//!   under an idle offset leaves it out of range for a frame, and it then
//!   snaps into range (the clamping physics' ballistic settle, which starts
//!   at its end); a run that would cross an end stops there.
//! - **A wheel or a trackpad** scrolls it by the horizontal delta
//!   (`pointerScroll`), within the range.

use iced::time::{Duration, Instant};
use iced::Rectangle;

use super::super::caret;
use crate::ui::core::motion::Curve;

/// How long an animated reveal takes (`_caretAnimationDuration`).
pub const CARET_ANIMATION: Duration = Duration::from_millis(100);

/// Its curve (`_caretAnimationCurve`).
pub const CARET_CURVE: Curve = Curve::FastOutSlowIn;

/// `precisionErrorTolerance`.
const PRECISION: f32 = 1e-10;

/// What the scroll needs of the field's layout.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    /// The text's box (the `RenderEditable`), in layout coordinates.
    pub bounds: Rectangle,
    /// The width of the text's line (`_textPainter`'s, before the box's
    /// minimum).
    pub text_width: f32,
}

impl Geometry {
    /// `maxScrollExtent`: how far the line and the caret's room reach past
    /// the box.
    pub fn max(&self) -> f32 {
        (self.text_width + caret::MARGIN - self.bounds.width).max(0.0)
    }

    /// The left of the caret at the glyph edge `x`, in the line's
    /// coordinates (`getLocalRectForCaret` before the paint offset).
    fn caret_left(&self, x: f32) -> f32 {
        caret::rect(
            x,
            0.0,
            0.0,
            self.text_width.max(self.bounds.width - caret::MARGIN),
        )
        .x
    }
}

/// How a scheduled reveal moves the offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reveal {
    /// `_scheduleShowCaretOnScreen(withAnimation: true)`.
    Animate,
    /// A post-frame `bringIntoView` (the menu's Cut and Paste), which runs
    /// after the animated one and stops it.
    Jump,
}

/// A run of the offset (`DrivenScrollActivity`).
#[derive(Debug, Clone, Copy)]
struct Drive {
    from: f32,
    to: f32,
    /// Its first frame, once it had one (a ticker's time starts at its
    /// first tick).
    start: Option<Instant>,
}

/// A reveal asked for, to run after the next frame (a post-frame
/// callback).
#[derive(Debug, Clone, Copy)]
struct PostFrame {
    reveal: Reveal,
    /// The frame it waits for has been drawn: it runs before anything
    /// else happens.
    drawn: bool,
}

/// See the module docs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Scroll {
    pixels: f32,
    drive: Option<Drive>,
    post_frame: Option<PostFrame>,
    /// An idle offset was out of range at the last frame: it snaps in at
    /// the next.
    out_of_range: bool,
}

/// `clampDouble(x, min, max)`, `min` winning should they cross.
fn clamp_double(x: f32, min: f32, max: f32) -> f32 {
    if x < min {
        min
    } else if x > max {
        max
    } else {
        x
    }
}

/// `ClampingScrollPhysics.applyBoundaryConditions`: how much of a move
/// from `pixels` to `value` would cross the range `0..=max`.
fn overscroll(pixels: f32, value: f32, max: f32) -> f32 {
    // Under the start, or over the end, already.
    if (value < pixels && pixels <= 0.0) || (max <= pixels && pixels < value) {
        value - pixels
    } else if value < 0.0 && 0.0 < pixels {
        value
    } else if pixels < max && max < value {
        value - max
    } else {
        0.0
    }
}

impl Scroll {
    /// The offset, as last drawn: what the text is shifted left by.
    pub fn pixels(&self) -> f32 {
        self.pixels
    }

    /// Whether it is running or due to move at a coming frame.
    pub fn is_moving(&self) -> bool {
        self.drive.is_some() || self.post_frame.is_some() || self.out_of_range
    }

    /// The offset that reveals the caret at the glyph edge `x`
    /// (`_getOffsetToRevealCaret(getLocalRectForCaret(...))`).
    pub fn reveal_target(&self, geometry: &Geometry, x: f32) -> f32 {
        let scale = caret::scale_factor();
        let bounds = geometry.bounds;
        // In the box (the paint offset applied), its top left then put on
        // a device pixel of the window (`_snapToPhysicalPixel`).
        let left = geometry.caret_left(x) - self.pixels;
        let left = ((bounds.x + left) * scale).round() / scale - bounds.x;
        let right = left + caret::WIDTH;
        let additional = if caret::WIDTH >= bounds.width {
            // Wider than the box: centred.
            bounds.width / 2.0 - (left + right) / 2.0
        } else {
            clamp_double(0.0, right - bounds.width, left)
        };
        (additional + self.pixels).clamp(0.0, geometry.max())
    }

    /// `jumpTo(to)`: there at once, any run stopped.
    fn jump_to(&mut self, to: f32) {
        self.drive = None;
        self.pixels = to;
        self.out_of_range = false;
    }

    /// `animateTo(to, 100 ms, fastOutSlowIn)`: from where it is; within a
    /// device pixel (`nearEqual` against the physics' tolerance, `1 /
    /// devicePixelRatio`), a jump.
    fn animate_to(&mut self, to: f32) {
        if (to - self.pixels).abs() < 1.0 / caret::scale_factor() {
            self.jump_to(to);
        } else {
            self.drive = Some(Drive {
                from: self.pixels,
                to,
                start: None,
            });
        }
    }

    /// `bringIntoView`: reveals the caret at the glyph edge `x` at once.
    pub fn bring_into_view(&mut self, geometry: &Geometry, x: f32) {
        let to = self.reveal_target(geometry, x);
        self.jump_to(to);
    }

    /// Asks for `reveal` of the caret after the next frame; one already
    /// asked for runs once (`_showCaretOnScreenScheduled`).
    pub fn schedule(&mut self, reveal: Reveal) {
        let reveal = self
            .post_frame
            .map_or(reveal, |pending| pending.reveal.max(reveal));
        self.post_frame = Some(PostFrame {
            reveal,
            drawn: false,
        });
    }

    /// Runs a reveal whose frame has been drawn: anything happening now
    /// comes after it. `x` is the caret's glyph edge.
    pub fn flush(&mut self, geometry: &Geometry, x: f32) {
        if let Some(PostFrame {
            reveal,
            drawn: true,
        }) = self.post_frame
        {
            self.post_frame = None;
            let to = self.reveal_target(geometry, x);
            match reveal {
                Reveal::Animate => self.animate_to(to),
                Reveal::Jump => self.jump_to(to),
            }
        }
    }

    /// `pointerScroll(delta)`: by `delta` within the range, any run
    /// stopped; whether it moved.
    pub fn scroll_by(&mut self, geometry: &Geometry, delta: f32) -> bool {
        let to = (self.pixels + delta).clamp(0.0, geometry.max());
        if delta == 0.0 || to == self.pixels {
            return false;
        }
        self.jump_to(to);
        true
    }

    /// A frame at `now`, laid out as `geometry`, the caret at the glyph
    /// edge `x`: the reveal the frame before asked for, the settle of an
    /// offset out of range, the run's step. Whether it needs another
    /// frame.
    pub fn frame(&mut self, now: Instant, geometry: &Geometry, x: f32) -> bool {
        self.flush(geometry, x);
        let max = geometry.max();
        if self.drive.is_none() && self.out_of_range {
            self.pixels = self.pixels.clamp(0.0, max);
            self.out_of_range = false;
        }
        let mut more = false;
        if let Some(drive) = &mut self.drive {
            let start = *drive.start.get_or_insert(now);
            let t = (now.saturating_duration_since(start).as_secs_f32()
                / CARET_ANIMATION.as_secs_f32())
            .clamp(0.0, 1.0);
            let value = drive.from + (drive.to - drive.from) * CARET_CURVE.transform(t);
            let over = if value == self.pixels {
                0.0
            } else {
                overscroll(self.pixels, value, max)
            };
            self.pixels = value - over;
            if over.abs() > PRECISION || t >= 1.0 {
                // Stopped at an end, or done.
                self.drive = None;
            } else {
                more = true;
            }
        }
        // Out of range and idle: the settle runs at the next frame.
        self.out_of_range = self.drive.is_none() && !(0.0..=max).contains(&self.pixels);
        more |= self.out_of_range;
        if let Some(post_frame) = &mut self.post_frame {
            post_frame.drawn = true;
            more = true;
        }
        more
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// A 100 px box at x 20 over a `text` px line.
    fn geometry(text: f32) -> Geometry {
        Geometry {
            bounds: Rectangle {
                x: 20.0,
                y: 0.0,
                width: 100.0,
                height: 14.0,
            },
            text_width: text,
        }
    }

    fn at(pixels: f32) -> Scroll {
        Scroll {
            pixels,
            ..Scroll::default()
        }
    }

    #[test]
    fn the_range_is_the_line_and_the_carets_room_past_the_box() {
        assert_eq!(geometry(50.0).max(), 0.0);
        assert_eq!(geometry(97.0).max(), 0.0);
        assert_eq!(geometry(150.0).max(), 53.0);
    }

    #[test]
    fn a_reveal_scrolls_only_as_far_as_shows_the_caret() {
        let g = geometry(300.0);
        // In view: nothing.
        assert_eq!(at(0.0).reveal_target(&g, 50.0), 0.0);
        // Past the right edge: its right edge on the box's (the caret 2
        // device pixels, here 2 px, left of the glyph edge, 2 wide).
        assert_eq!(at(0.0).reveal_target(&g, 150.0), 50.0);
        // Left of the box: its left edge on the box's.
        assert_eq!(at(100.0).reveal_target(&g, 60.0), 58.0);
        // At the start: never left of the text.
        assert_eq!(at(100.0).reveal_target(&g, 0.0), 0.0);
        // At the end: within the range, the caret's room past the text.
        assert_eq!(at(0.0).reveal_target(&g, 300.0), 200.0);
        assert_eq!(g.max(), 203.0);
    }

    #[test]
    fn an_animated_reveal_runs_over_100_ms_on_fast_out_slow_in_after_the_frame() {
        let g = geometry(300.0);
        let mut s = at(0.0);
        s.schedule(Reveal::Animate);
        let t0 = Instant::now();
        // The frame that shows the change: still where it was.
        assert!(s.frame(t0, &g, 150.0));
        assert_eq!(s.pixels(), 0.0);
        // The run's first frame.
        assert!(s.frame(t0 + 16 * MS, &g, 150.0));
        assert_eq!(s.pixels(), 0.0);
        assert!(s.frame(t0 + 66 * MS, &g, 150.0));
        let half = 50.0 * CARET_CURVE.transform(0.5);
        assert!((s.pixels() - half).abs() < 1e-3, "{}", s.pixels());
        assert!(!s.frame(t0 + 116 * MS, &g, 150.0));
        assert_eq!(s.pixels(), 50.0);
    }

    #[test]
    fn under_a_device_pixel_away_it_jumps() {
        // The box off the pixel grid: the caret's snapped right edge ends
        // 0.7 px past it.
        let mut g = geometry(300.0);
        g.bounds.x = 20.3;
        let mut s = at(49.0);
        assert!((s.reveal_target(&g, 150.0) - 49.7).abs() < 1e-4);
        s.schedule(Reveal::Animate);
        let t0 = Instant::now();
        s.frame(t0, &g, 150.0);
        assert!(!s.frame(t0 + 16 * MS, &g, 150.0), "no run");
        assert!((s.pixels() - 49.7).abs() < 1e-4);
    }

    #[test]
    fn a_shrunk_line_leaves_an_idle_offset_out_of_range_for_a_frame() {
        let mut s = at(203.0);
        let t0 = Instant::now();
        assert!(s.frame(t0, &geometry(250.0), 0.0));
        assert_eq!(s.pixels(), 203.0);
        assert!(!s.frame(t0 + 16 * MS, &geometry(250.0), 0.0));
        assert_eq!(s.pixels(), 153.0);
    }

    #[test]
    fn a_run_stops_at_the_end_of_the_range() {
        let mut s = at(0.0);
        s.drive = Some(Drive {
            from: 0.0,
            to: 100.0,
            start: None,
        });
        let t0 = Instant::now();
        let g = geometry(140.0);
        s.frame(t0, &g, 0.0);
        s.frame(t0 + 90 * MS, &g, 0.0);
        assert_eq!(s.pixels(), g.max());
        assert!(s.drive.is_none());
    }

    #[test]
    fn the_wheel_scrolls_within_the_range_and_stops_a_run() {
        let g = geometry(300.0);
        let mut s = at(10.0);
        s.animate_to(150.0);
        assert!(s.scroll_by(&g, -30.0));
        assert_eq!(s.pixels(), 0.0);
        assert!(s.drive.is_none());
        assert!(!s.scroll_by(&g, -30.0), "at the start");
        assert!(s.scroll_by(&g, 500.0));
        assert_eq!(s.pixels(), 203.0);
    }
}
