//! The song practice screen's widgets (port of
//! `ui/song_practice_detail/widgets`).

pub mod fx_panel;
pub mod live_spectrogram;
mod mixer_shared;
pub mod mixer_strip;
pub mod pitch_lane;
pub mod technique_picker;
pub mod track_context_menu;
pub mod track_row;
pub mod voiceprint_view;

#[cfg(test)]
mod live_ui;
