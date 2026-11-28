//! The shared widgets (port of `client/lib/ui/core/widgets`), and the
//! design system's own (`rust/DESIGN.md`): the desktop controls
//! ([`controls`]), the line icons drawn to one optical size
//! ([`line_icon`]) and the unified toolbar every page draws ([`toolbar`]).

pub mod animated;
pub mod cap_label;
pub mod click_through;
pub mod confirm_dialog;
pub mod controls;
pub mod dismissible;
pub mod fading_scrollbar;
pub mod fatal_open_screen;
pub mod focus_traversal;
pub mod holds_keyboard;
pub mod line_icon;
pub mod material_slider;
pub mod material_switch;
pub mod material_tooltip;
pub mod opacity_layer;
pub mod pan_zoom;
pub mod rsuperellipse;
pub mod scroll_fling;
pub mod slide;
pub mod spectrogram_painter;
pub mod stable_subtree;
pub mod studio_button;
pub mod studio_controls;
pub mod studio_hint;
pub mod studio_text;
pub mod text_field;
pub mod tick_boundary;
pub mod toolbar;
pub mod visible_drop_target;
pub mod waveform_painter;

pub(crate) mod gesture;
pub(crate) mod painter;
pub(crate) mod wrap;
