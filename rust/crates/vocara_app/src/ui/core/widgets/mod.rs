//! The shared widgets (port of `client/lib/ui/core/widgets`).

pub mod animated;
pub mod cap_label;
pub mod confirm_dialog;
pub mod fading_scrollbar;
pub mod fatal_open_screen;
pub mod holds_keyboard;
pub mod material_tooltip;
pub mod slide;
pub mod slider_overlay;
pub mod spectrogram_painter;
pub mod stable_subtree;
pub mod studio_button;
pub mod studio_controls;
pub mod studio_hint;
pub mod studio_text;
pub mod tick_boundary;
pub mod visible_drop_target;
pub mod waveform_painter;
pub mod window_drag_bar;

pub(crate) mod gesture;
pub(crate) mod painter;
pub(crate) mod wrap;
