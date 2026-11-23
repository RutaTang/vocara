//! The song practice screen's widgets (port of
//! `ui/song_practice_detail/widgets`).

pub mod analysis_lanes;
pub mod bottom_dock;
pub mod detail_transport_bar;
pub mod fx_panel;
pub mod left_panel;
pub mod live_spectrogram;
pub mod lyrics_panel;
mod mixer_shared;
pub mod mixer_strip;
pub mod note_editor;
pub mod pitch_lane;
pub mod ruler_bar;
pub mod song_practice_detail_screen;
pub mod technique_picker;
pub mod timeline;
pub mod track_context_menu;
pub mod track_row;
pub mod voice_profile_view;
pub mod voiceprint_view;

#[cfg(test)]
mod live_ui;
