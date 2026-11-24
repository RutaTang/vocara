//! The window's scale factor: Flutter's `devicePixelRatio`, which widgets
//! that work in device pixels read (a text field's caret and its scroll's
//! snap, the scrollbar's nearness tolerance).
//!
//! iced hands a widget no scale, so the app asks the window for it when it
//! opens (`Message::ScaleFactor`) and the widgets that read it take it
//! from `window::Event::Rescaled` when the window moves to a screen of
//! another scale.

use std::cell::Cell;

thread_local! {
    /// The window's scale factor. Widgets are laid out, updated and drawn
    /// on the thread that runs the app's `update`, which sets it.
    static SCALE_FACTOR: Cell<f32> = const { Cell::new(1.0) };
}

/// Hands the widgets the window's scale factor (`devicePixelRatio`).
pub fn set_scale_factor(scale_factor: f32) {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        SCALE_FACTOR.with(|s| s.set(scale_factor));
    }
}

/// The scale factor [`set_scale_factor`] last set (1 before it was).
pub fn scale_factor() -> f32 {
    SCALE_FACTOR.with(Cell::get)
}
