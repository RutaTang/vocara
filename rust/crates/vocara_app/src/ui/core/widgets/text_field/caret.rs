//! Flutter's caret as a macOS `TextField` painted it (`RenderEditable`,
//! `_CaretPainter`, with the macOS branch of `_TextFieldState.build`).
//!
//! - `cursorWidth` 2, `cursorRadius` `Radius.circular(2)`: a 2 px bar with
//!   round ends.
//! - The iOS/macOS `_caretPrototype` is `cursorHeight + 2` tall
//!   (`cursorHeight` is the line's `preferredLineHeight`), and
//!   `getLocalRectForCaret` centres it on the line's full height: it
//!   reaches 1 px above and below the line.
//! - `cursorOffset` is `Offset(iOSHorizontalOffset / devicePixelRatio, 0)`
//!   with `iOSHorizontalOffset = -2`: 2 device pixels left of the glyph
//!   edge. Then the caret's left is clamped to the text: not left of 0, not
//!   right of the wider of the text and the box less the caret margin.
//! - `_snapToPhysicalPixel` puts its top left on a device pixel.
//! - `paintCursorAboveText` is true on macOS: it is painted over the text,
//!   so it is drawn in a layer of its own after the text.
//!
//! The blink (`_kCursorBlinkHalfPeriod`, 500 ms on and off, restarted
//! whenever the text or the selection changes) is the one iced's fields
//! already had, kept in the forks.

use std::cell::Cell;

use iced::advanced::renderer::{self, Renderer as _};
use iced::{Border, Color, Rectangle, Renderer, Shadow};

/// `TextField.cursorWidth`'s default.
pub const WIDTH: f32 = 2.0;

/// `cursorRadius` on macOS.
pub const RADIUS: f32 = 2.0;

/// How far the caret reaches above and below its line (half of the 2 the
/// iOS/macOS `_caretPrototype` adds to the line height).
pub const OVERHANG: f32 = 1.0;

/// `iOSHorizontalOffset`, in device pixels.
pub const OFFSET_DEVICE_PIXELS: f32 = -2.0;

/// The room `RenderEditable` keeps for the caret after the text
/// (`_caretMargin`: `_kCaretGap` 1 + `cursorWidth`).
pub const MARGIN: f32 = 1.0 + WIDTH;

thread_local! {
    /// The window's scale factor: Flutter's `devicePixelRatio`. Fields are
    /// laid out and drawn on the thread that runs the app's `update`, which
    /// sets it.
    static SCALE_FACTOR: Cell<f32> = const { Cell::new(1.0) };
}

/// Hands the fields the window's scale factor (`devicePixelRatio`), which
/// places their caret: the app does when its window opens, and the fields
/// themselves when the window moves to a screen of another scale.
pub fn set_scale_factor(scale_factor: f32) {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        SCALE_FACTOR.with(|s| s.set(scale_factor));
    }
}

/// The scale factor [`set_scale_factor`] last set (1 before it was).
pub fn scale_factor() -> f32 {
    SCALE_FACTOR.with(Cell::get)
}

/// The caret's rectangle, in the text's own coordinates, for a caret at the
/// glyph edge `x` on the line from `line_top`, `line_height` tall, whose
/// left edge may reach `max_x` at most (the wider of the text's width and
/// the text box's less [`MARGIN`]).
pub fn rect(x: f32, line_top: f32, line_height: f32, max_x: f32) -> Rectangle {
    let left = (x + OFFSET_DEVICE_PIXELS / scale_factor()).clamp(0.0, max_x.max(0.0));
    Rectangle {
        x: left,
        y: line_top - OVERHANG,
        width: WIDTH,
        height: line_height + 2.0 * OVERHANG,
    }
}

/// Paints the caret `rect` (in draw coordinates) in `color` over everything
/// drawn so far, within `clip`, its top left on a device pixel.
pub fn draw(renderer: &mut Renderer, rect: Rectangle, color: Color, clip: Rectangle) {
    let scale = scale_factor();
    let snap = |v: f32| (v * scale).round() / scale;
    let bounds = Rectangle {
        x: snap(rect.x),
        y: snap(rect.y),
        ..rect
    };
    renderer.with_layer(clip, |renderer| {
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    radius: RADIUS.into(),
                    ..Border::default()
                },
                shadow: Shadow::default(),
                // Under a scroll offset the device grid is the renderer's.
                snap: true,
            },
            color,
        );
    });
}
