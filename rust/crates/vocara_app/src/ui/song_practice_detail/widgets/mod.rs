//! The song practice screen's widgets (port of
//! `ui/song_practice_detail/widgets`).

pub mod detail_transport_bar;
pub mod fx_panel;
pub mod left_panel;
pub mod live_spectrogram;
mod mixer_shared;
pub mod mixer_strip;
pub mod note_editor;
pub mod pitch_lane;
pub mod ruler_bar;
pub mod technique_picker;
pub mod track_context_menu;
pub mod track_row;
pub mod voiceprint_view;

#[cfg(test)]
mod live_ui;
