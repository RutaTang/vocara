//! The read-only surface the bottom-dock analysis views need from their host
//! (port of `ui/song_practice_detail/view_model/analysis_source.dart`).
//!
//! Dart's `AnalysisSource` was a `Listenable` the views rebuilt on. An iced
//! view is rebuilt from state on every message, so the listener half becomes
//! [`AnalysisSource::revision`]: a counter bumped on every Dart
//! `notifyListeners`, which a view that caches its drawing compares to know
//! when to redraw. Dart's `Future<void> seek(Duration)` is not here: a view
//! cannot act on its source while it draws, so a seek is the view's message,
//! which the screen hands to its playback (the host's `seek`).

use std::sync::Arc;
use std::time::Duration;

use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};
use vocara_services::monitor_service::MonitorService;

/// The read-only surface the bottom-dock analysis views need from their host
/// view model: the selected subject's computed analysis plus playback /
/// live-monitor state.
///
/// Implemented for the song screen by the analysis cache bound to its host
/// (`AnalysisCache::source`: a vocal take) and by the technique library's
/// analysis source (a reference clip or an ephemeral practice recording), so
/// the same spectrogram / power / vowel / pitch / timbre / LTAS / vibrato /
/// register / range / dynamics views render in both places without
/// duplication.
pub trait AnalysisSource {
    /// The take/clip whose analysis is shown. `None` when nothing is
    /// selected; for the technique screen this is a synthetic track wrapping
    /// the reference or practice clip. The time axis of the analysis views is
    /// NOT this track's lane span but the subject axis below, which can be
    /// narrower.
    fn selected_track(&self) -> Option<Arc<AudioTrack>>;

    /// The absolute-timeline start (ms, on [`AnalysisSource::position`]'s
    /// clock) of the span the current analysis subject occupies — the span
    /// [`AnalysisSource::selected_metrics`] /
    /// [`AnalysisSource::selected_spectrogram`] actually cover. For a
    /// whole-lane subject (no clip selected, or a single-clip lane) this is
    /// the lane span's start; when a specific clip of a multi-clip lane is
    /// analyzed it is that clip's `start_ms`. 0 when there is no subject.
    /// Unused while [`AnalysisSource::live_monitor`] is on: the live views
    /// render the mic feed and never map the playhead onto a take.
    fn analysis_start_ms(&self) -> i64;

    /// The length (ms) of the analysis-subject span; see
    /// [`AnalysisSource::analysis_start_ms`]. 0 when there is no subject.
    fn analysis_duration_ms(&self) -> i64;

    fn selected_metrics(&self) -> Option<Arc<VoiceMetrics>>;
    fn selected_spectrogram(&self) -> Option<Arc<Spectrogram>>;
    fn selected_profile(&self) -> Option<Arc<VoiceProfile>>;
    fn selected_ltas(&self) -> Option<Arc<Ltas>>;
    fn selected_register(&self) -> Option<Arc<RegisterMap>>;

    /// True while the subject's spectrogram is still computing.
    fn spectrogram_loading(&self) -> bool;

    /// True while the subject's voice metrics are still computing. The halves
    /// are separate: one can be held while the other computes (the byte
    /// budget drops one, or an analysis lane asked for its own), and a view
    /// that waited on the other showed a spinner over what it already had.
    fn metrics_loading(&self) -> bool;

    /// True when the views should show the live mic instead of the subject.
    fn live_monitor(&self) -> bool;

    fn monitor(&self) -> &MonitorService;

    /// The shared playhead (subject-local for the technique screen).
    fn position(&self) -> Duration;

    /// Bumped on every change a view may draw (Dart's `notifyListeners`).
    fn revision(&self) -> u64;
}
