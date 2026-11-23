//! Ports of the Dart unit tests that exercise audio_dsp
//! (`client/test/*_test.dart`), file by file; see each module's header for
//! its source and for what was left to the screens and services that own it.

mod edge;
mod fft;
mod framing;
mod live;
mod metrics;
mod spectrogram;
pub(crate) mod support;
mod wav;
mod yin;

/// Dart ran the whole-take work in `Isolate.run`; here the caller moves the
/// analyzers to a worker thread, so every one of them must be `Send`.
#[test]
fn every_analyzer_can_move_to_a_worker_thread() {
    fn send<T: Send>() {}
    send::<super::StreamingSpectrogramBuilder>();
    send::<super::StreamingTakeAnalyzer>();
    send::<super::StreamingStft>();
    send::<super::LiveVoiceAnalyzer>();
    send::<super::TunerTracker>();
    send::<super::PcmAudio>();
    send::<super::MetricsAndSpectrogram>();
    send::<super::StreamedTakeAnalysis>();
}
