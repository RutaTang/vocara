//! The view model as each controller's host (Dart's constructor closures
//! onto it). A host is built for one call from the view model's state and
//! the sibling controllers the closures read, borrowed apart; the read-only
//! hosts serve the getters the views call through `&self`.

use std::sync::Arc;
use std::time::Duration;

use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::song_practice::SongPractice;
use vocara_services::waveform_service::WaveformResult;

use super::super::analysis_cache::{AnalysisCache, AnalysisHost};
use super::super::interpretations_controller::{InterpretationsAnalysis, InterpretationsHost};
use super::super::mixer_controller::MixerHost;
use super::super::playback_controller::{PlaybackController, PlaybackHost};
use super::super::recording_controller::{RecordingController, RecordingHost};
use super::Core;
use crate::util::busy_gate::BusyGate;

/// A host's hold on the view model's state: shared (the getters, whose
/// hosts can only be read through `&dyn`) or exclusive (the actions).
pub(super) trait CoreAccess {
    fn core(&self) -> &Core;
    /// `None` for a shared hold: the trait methods that write are never
    /// reachable through the `&dyn` a getter takes.
    fn core_mut(&mut self) -> Option<&mut Core>;
}

impl CoreAccess for &Core {
    fn core(&self) -> &Core {
        self
    }

    fn core_mut(&mut self) -> Option<&mut Core> {
        None
    }
}

impl CoreAccess for &mut Core {
    fn core(&self) -> &Core {
        self
    }

    fn core_mut(&mut self) -> Option<&mut Core> {
        Some(self)
    }
}

// --- Playback ---------------------------------------------------------------

pub(super) struct PlaybackView<'a, C> {
    pub c: C,
    pub recording: &'a RecordingController,
}

impl<C: CoreAccess> PlaybackHost for PlaybackView<'_, C> {
    fn practice(&self) -> Option<&SongPractice> {
        self.c.core().practice.as_deref()
    }

    // The engine's position stream must keep yielding to the record clock
    // while a take is in flight (the recording tick writes the playhead).
    fn is_recording(&self) -> bool {
        self.recording.is_recording()
    }

    fn take_holds_start(&self) -> bool {
        self.recording.holds_start_position()
    }

    fn engine_available(&self) -> bool {
        self.c
            .core()
            .engine
            .graph()
            .is_none_or(|g| g.is_available())
    }

    fn toast(&mut self, message: &str) {
        if let Some(c) = self.c.core_mut() {
            c.toast(message);
        }
    }
}

// --- Mixer ------------------------------------------------------------------

pub(super) struct MixerView<'a, C> {
    pub c: C,
    pub recording: &'a RecordingController,
    /// The take a recording punches into (frozen while it records, which is
    /// the only time the mixer reads it).
    pub target: Option<String>,
    pub position: Duration,
}

impl<C: CoreAccess> MixerHost for MixerView<'_, C> {
    fn practice(&self) -> Option<&SongPractice> {
        self.c.core().practice.as_deref()
    }

    fn set_practice(&mut self, practice: SongPractice) {
        if let Some(c) = self.c.core_mut() {
            c.set_practice(practice);
        }
    }

    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>> {
        self.c.core().track_by_id(id)
    }

    fn all_tracks(&self) -> Vec<Arc<AudioTrack>> {
        self.c.core().all_tracks()
    }

    fn update_track(&mut self, id: &str, update: &dyn Fn(&AudioTrack) -> AudioTrack) {
        if let Some(c) = self.c.core_mut() {
            c.update_track(id, update);
        }
    }

    fn toast(&mut self, message: &str) {
        if let Some(c) = self.c.core_mut() {
            c.toast(message);
        }
    }

    fn select_bus_after_mutation(&mut self, bus_id: &str) {
        if let Some(c) = self.c.core_mut() {
            c.select_bus_after_mutation(bus_id);
        }
    }

    fn clear_bus_selection_if(&mut self, bus_id: &str) {
        if let Some(c) = self.c.core_mut() {
            if c.selected_bus_id.as_deref() == Some(bus_id) {
                c.selected_bus_id = None;
            }
        }
    }

    // The take being punched into must be silent in the engine while its
    // live envelope folds into the master display waveform.
    fn is_recording(&self) -> bool {
        self.recording.is_recording()
    }

    fn recording_target_id(&self) -> Option<&str> {
        self.target.as_deref()
    }

    fn recording_peaks(&self) -> &[f64] {
        self.recording.recording_peaks()
    }

    fn recording_start_ms(&self) -> i64 {
        self.recording.live_take_start_ms()
    }

    fn recording_duration_ms(&self) -> i64 {
        self.recording.recording_duration_ms()
    }

    fn position(&self) -> Duration {
        self.position
    }
}

// --- Analysis ---------------------------------------------------------------

pub(super) struct AnalysisView<'a> {
    pub c: &'a Core,
    pub playback: &'a PlaybackController,
    pub recording: &'a RecordingController,
}

impl AnalysisHost for AnalysisView<'_> {
    fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        self.c.selected_track()
    }

    fn selected_clip_id(&self) -> Option<&str> {
        self.c.selected_clip_id.as_deref()
    }

    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>> {
        self.c.track_by_id(id)
    }

    fn is_moving_live(&self, track_id: &str) -> bool {
        self.c.moving_live.contains(track_id)
    }

    fn is_playing(&self) -> bool {
        self.playback.is_playing()
    }

    // Recording owns the capture — the live view can't start during a take,
    // INCLUDING the arm window before is_recording flips true (a live start
    // slipping in there would pull input frames from under the take).
    fn is_recording(&self) -> bool {
        self.recording.is_recording() || self.recording.is_starting_take()
    }

    // The live view pauses the transport before taking over the capture.
    fn pause_playback(&self) {
        self.c.engine.transport().pause();
    }

    fn position(&self) -> Duration {
        self.playback.position()
    }
}

// --- Recording --------------------------------------------------------------

pub(super) struct RecordingView<'a> {
    pub c: &'a mut Core,
    pub position: Duration,
    pub live_monitor: bool,
}

impl RecordingHost for RecordingView<'_> {
    fn practice(&self) -> Option<&SongPractice> {
        self.c.practice.as_deref()
    }

    fn selected_track_id(&self) -> Option<&str> {
        self.c.selected_track_id.as_deref()
    }

    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>> {
        self.c.track_by_id(id)
    }

    fn update_track(&mut self, id: &str, update: &dyn Fn(&AudioTrack) -> AudioTrack) {
        self.c.update_track(id, update);
    }

    // Take naming stays here (numbering reads the active interpretation).
    fn append_vocal(&mut self, path: &str, analysis: &WaveformResult, start_ms: i64) -> String {
        let name = format!("Take {}", self.c.next_take_number());
        self.c.append_vocal(path, &name, analysis, start_ms, None)
    }

    fn select_recorded_clip(&mut self, track_id: &str, clip_id: Option<&str>) {
        self.c.select_recorded_clip(track_id, clip_id);
    }

    fn position(&self) -> Duration {
        self.position
    }

    // The ONE busy gate: start guard (Stop→Record re-arm race) + finalize.
    fn busy_gate(&mut self) -> &mut BusyGate {
        &mut self.c.gate
    }

    fn live_monitor(&self) -> bool {
        self.live_monitor
    }

    fn toast(&mut self, message: &str) {
        self.c.toast(message);
    }

    /// Every error a start can throw is one Dart's `_friendlyError` said as
    /// "Something went wrong." (an id that is not a path component, a file
    /// system failure, a panic): the empty take's failed save says its own,
    /// and the mic's own failures are worded by the recorder.
    fn friendly_error(&self, _error: &str) -> String {
        super::jobs::SOMETHING_WENT_WRONG.to_string()
    }
}

// --- Interpretations --------------------------------------------------------

pub(super) struct VersionsView<'a> {
    pub c: &'a mut Core,
    pub analysis: &'a mut AnalysisCache,
    pub playback: &'a PlaybackController,
    pub recording: &'a RecordingController,
    pub play_reference: bool,
}

impl InterpretationsHost for VersionsView<'_> {
    fn practice(&self) -> Option<&SongPractice> {
        self.c.practice.as_deref()
    }

    fn set_practice(&mut self, practice: SongPractice) {
        self.c.set_practice(practice);
    }

    fn ensure_selection(&mut self) {
        if self.c.ensure_selection(self.play_reference)
            && ((self.analysis.dock_open() && self.analysis.dock_shows_take())
                || self.c.inspector_open)
        {
            let effects = self.analysis.ensure_selected_spectrogram(&AnalysisView {
                c: self.c,
                playback: self.playback,
                recording: self.recording,
            });
            self.c.push_analysis(effects);
        }
    }

    fn toast(&mut self, message: String) {
        self.c.toast(&message);
    }

    // The whole take window, not just the armed span: a switch while the
    // mic is still arming (or the stopped take is still landing) moved the
    // screen to another interpretation, filing the take where the user no
    // longer looks — no forming waveform, and no take after Stop.
    fn is_recording(&self) -> bool {
        super::take_in_flight(self.recording)
    }

    fn is_separating(&self) -> bool {
        self.c.separation_progress.is_some()
    }

    fn take_wait(&self) -> Option<String> {
        super::take_wait(self.recording).map(str::to_string)
    }

    fn transport_moves(&self) -> u64 {
        self.playback.transport_moves()
    }

    fn busy_gate(&mut self) -> &mut BusyGate {
        &mut self.c.gate
    }

    fn analysis(&mut self) -> &mut dyn InterpretationsAnalysis {
        self.analysis
    }
}
