//! Port of `ui/core/widgets/studio_hint.dart`: a centred faint hint for a
//! dock view's empty / loading state, with an optional spinner while an
//! analysis is running; and [`lane_hint`], the inset empty / loading state
//! that fills a full-width analysis lane.
//!
//! [`spinner`] is Material's indeterminate `CircularProgressIndicator` (the
//! same head/tail/offset/rotation curves), drawn on a canvas that asks for
//! the next frame while it is shown.

use std::f32::consts::PI;
use std::time::Instant;

use iced::alignment::Horizontal;
use iced::widget::canvas::{self, path::Arc, Frame, Geometry, Path, Stroke};
use iced::widget::{canvas as canvas_widget, center, column, container, text, Space};
use iced::{
    mouse, window, Color, Element, Length, Padding, Point, Radians, Rectangle, Renderer, Theme,
};

use crate::ui::core::theme::Studio;

/// `StudioHint(text, spinner: ..., fontSize: ..., padding: ...)`.
pub struct StudioHint {
    text: String,
    spinner: bool,
    font_size: f32,
    padding: Padding,
}

/// A hint with no spinner, 13 px, no padding.
pub fn studio_hint(text: impl Into<String>) -> StudioHint {
    StudioHint {
        text: text.into(),
        spinner: false,
        font_size: 13.0,
        padding: Padding::ZERO,
    }
}

impl StudioHint {
    pub fn spinner(mut self, spinner: bool) -> Self {
        self.spinner = spinner;
        self
    }

    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }
}

impl<'a, M: 'a> From<StudioHint> for Element<'a, M> {
    fn from(hint: StudioHint) -> Self {
        let mut col = column![].align_x(Horizontal::Center);
        if hint.spinner {
            col = col
                .push(spinner(18.0, 2.0, Studio::ACCENT))
                .push(Space::new().height(14));
        }
        col = col.push(
            text(hint.text)
                .size(hint.font_size)
                .line_height(1.4)
                .align_x(Horizontal::Center)
                .color(Studio::TEXT_FAINT),
        );
        center(container(col).padding(hint.padding)).into()
    }
}

/// `LaneHint(text)`: `Studio.inset` fill with a centred 12 px faint label.
pub fn lane_hint<'a, M: 'a>(label: impl Into<String>) -> Element<'a, M> {
    container(center(
        text(label.into()).size(12).color(Studio::TEXT_FAINT),
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .style(crate::ui::core::theme::fill(Studio::INSET))
    .into()
}

/// An indeterminate circular progress indicator `size` px square, stroke
/// `stroke_width`, in `color`.
pub fn spinner<'a, M: 'a>(size: f32, stroke_width: f32, color: Color) -> Element<'a, M> {
    canvas_widget(Spinner {
        color,
        stroke_width,
    })
    .width(size)
    .height(size)
    .into()
}

struct Spinner {
    color: Color,
    stroke_width: f32,
}

/// When the spinner started, and the frame it last drew.
#[derive(Debug, Clone, Copy)]
pub struct SpinnerState {
    start: Instant,
    now: Instant,
}

impl Default for SpinnerState {
    fn default() -> Self {
        let now = Instant::now();
        Self { start: now, now }
    }
}

/// Material's `Curves.fastOutSlowIn` (cubic Bézier 0.4, 0.0, 0.2, 1.0).
pub fn fast_out_slow_in(t: f32) -> f32 {
    cubic(0.4, 0.0, 0.2, 1.0, t)
}

/// Flutter's `Cubic(a, b, c, d).transform(t)`: the curve's y at x = t,
/// found by bisection as Flutter does.
fn cubic(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    fn eval(p1: f32, p2: f32, m: f32) -> f32 {
        3.0 * p1 * (1.0 - m) * (1.0 - m) * m + 3.0 * p2 * (1.0 - m) * m * m + m * m * m
    }
    let (mut start, mut end) = (0.0f32, 1.0f32);
    loop {
        let midpoint = (start + end) / 2.0;
        let estimate = eval(a, c, midpoint);
        if (t - estimate).abs() < 0.001 {
            return eval(b, d, midpoint);
        }
        if estimate < t {
            start = midpoint;
        } else {
            end = midpoint;
        }
    }
}

/// The indeterminate arc at `elapsed_ms` into the animation: `(start angle,
/// sweep)` in radians, as `_CircularProgressIndicatorPainter` computes them
/// (the head and tail chase each other every 1333 ms; the whole arc turns
/// once every 2222 ms).
pub fn arc_at(elapsed_ms: f32) -> (f32, f32) {
    let path = (elapsed_ms / 1333.0).fract();
    let rotation = (elapsed_ms / 2222.0).fract();
    let interval = |lo: f32, hi: f32| fast_out_slow_in(((path - lo) / (hi - lo)).clamp(0.0, 1.0));
    let head = interval(0.0, 0.5);
    let tail = interval(0.5, 1.0);
    let start = -PI / 2.0 + tail * 3.0 / 2.0 * PI + rotation * PI * 2.0 + path * 0.5 * PI;
    let sweep = (head * 3.0 / 2.0 * PI - tail * 3.0 / 2.0 * PI).max(0.001);
    (start, sweep)
}

impl<M> canvas::Program<M> for Spinner {
    type State = SpinnerState;

    fn update(
        &self,
        state: &mut SpinnerState,
        event: &canvas::Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<canvas::Action<M>> {
        if let canvas::Event::Window(window::Event::RedrawRequested(now)) = event {
            state.now = *now;
            return Some(canvas::Action::request_redraw());
        }
        None
    }

    fn draw(
        &self,
        state: &SpinnerState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let elapsed = state
            .now
            .saturating_duration_since(state.start)
            .as_secs_f32()
            * 1000.0;
        let (start, sweep) = arc_at(elapsed);
        let radius = (bounds.width.min(bounds.height) - self.stroke_width) / 2.0;
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let arc = Path::new(|b| {
            b.arc(Arc {
                center,
                radius,
                start_angle: Radians(start),
                end_angle: Radians(start + sweep),
            })
        });
        frame.stroke(
            &arc,
            Stroke::default()
                .with_color(self.color)
                .with_width(self.stroke_width)
                .with_line_cap(canvas::LineCap::Square),
        );
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_shows_its_text() {
        let mut ui = iced_test::simulator::<(), _, _>(Element::from(
            studio_hint("Analyzing…").spinner(true),
        ));
        assert!(ui.find("Analyzing…").is_ok());
    }

    #[test]
    fn fast_out_slow_in_matches_flutter_at_its_ends_and_middle() {
        assert!(fast_out_slow_in(0.0).abs() < 0.002);
        assert!((fast_out_slow_in(1.0) - 1.0).abs() < 0.002);
        // Curves.fastOutSlowIn.transform(0.5) ≈ 0.7748.
        assert!((fast_out_slow_in(0.5) - 0.7748).abs() < 0.01);
    }

    #[test]
    fn the_arc_grows_then_shrinks_every_1333_ms() {
        let (_, at_start) = arc_at(0.0);
        let (_, at_half) = arc_at(1333.0 / 2.0);
        assert!(at_start < 0.01);
        assert!((at_half - 1.5 * PI).abs() < 0.01, "full head, no tail");
        let (_, at_end) = arc_at(1332.0);
        assert!(at_end < 0.05);
    }
}
