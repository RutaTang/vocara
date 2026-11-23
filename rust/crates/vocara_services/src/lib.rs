//! Vocara services: the audio engine bridge and everything above it that is not UI.

pub mod app_menu;
pub mod app_settings_service;
pub mod audio_dsp;
pub mod audio_edit_service;
pub mod audio_exporter;
pub mod audio_file_service;
pub mod engine;
pub mod graph_audio_playback_service;
pub mod local_storage_service;
pub mod media_analysis_service;
pub mod media_player;
pub mod mic_permission;
pub mod mic_recording_controller;
pub mod monitor_service;
pub mod persistence;
pub mod playback_engine;
pub mod playback_engine_factory;
pub mod quit_request;
pub mod recording_finalizer;
pub mod source_separation_service;
pub mod spectrogram_service;
pub mod technique_media_service;
pub mod update_check_service;
pub mod waveform_service;
