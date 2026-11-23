//! The take-style analysis of any media file (port of
//! `lib/data/services/media_analysis_service.dart`).
//!
//! Dart ran the gate on the UI isolate, the decode in the engine's decode
//! isolate and the DSP in an `Isolate.run`. Here [`MediaAnalysisService::analyze`]
//! is one blocking call for a worker thread (`spawn_blocking`): it waits its
//! turn at the app-wide [`analysis_gate`] on that thread, decodes and
//! analyzes there, and returns the result. The `wanted` predicate is only
//! borrowed and called on that same thread, so it never has to be `Send`:
//! nothing of the caller rides along with the work, the Rust shape of the
//! Dart rule that the isolate closure must not capture the view model.

use std::sync::Arc;
use std::time::Duration;

use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::voice_metrics::{
    analyze_register, analyze_voice_profile, compute_ltas, RegisterOptions, VoiceProfileOptions,
};
use vocara_model::util::analysis_gate::analysis_gate;
use vocara_model::util::dart_num::round_to_i64;

use crate::audio_dsp::{
    compute_voice_metrics_and_spectrogram, AnalysisParams, MetricsAndSpectrogram,
};
use crate::engine::media::{decode_media_to_mono, MediaError, ANALYSIS_DECODE_RATE};
use crate::engine::Pcm;

/// Decodes the media at a path to mono PCM, at most `max_duration` long:
/// [`decode_media_to_mono`]'s shape at the analysis rate. `Ok(None)` when the
/// file cannot be decoded or is empty; `Err` for a refusal the user is told
/// about (too long, too dense, unreadable, no engine).
pub type MediaDecoder =
    dyn Fn(&str, Option<Duration>) -> Result<Option<Pcm>, MediaError> + Send + Sync;

/// Longest media the analysis path will decode. The whole file materializes
/// as f32 PCM (see [`decode_media_to_mono`]), so an unbounded input could
/// exhaust memory. 10 minutes: takes and technique references are minutes
/// long.
///
/// The ≈ 86 MB figure measured by the WAV path's peak measurement is the WAV
/// path's (`SpectrogramService`'s windowed `StreamingTakeAnalyzer`, which
/// never materializes the track) — NOT this one. THIS path decodes through
/// the native engine, which holds the whole track, and then runs BATCH DSP
/// over it, so its peak is dominated by the track itself: ~4 bytes per frame
/// native (the downmix happens inside that buffer, so stereo and mono never
/// coexist), plus the results. (Dart paid the same again for the copy its
/// isolate hop needed; here the analysis reads the engine's buffer in place.)
/// A unit test cannot load the engine, so this stays a derivation; the
/// duration cap is what bounds it, and the global analysis gate's two slots
/// bound how many can stack.
///
/// Routing this path through the same windowed analyzer — a chunked native
/// decode feeding `StreamingTakeAnalyzer` — is what would remove the
/// whole-track term; the per-frame math is already pinned bit-identical to
/// the batch functions, so the analyzer is not the obstacle, the decode's
/// shape is.
pub const MAX_ANALYSIS_DURATION: Duration = Duration::from_secs(10 * 60);

/// What the technique screens need from the media analysis (Dart tests faked
/// this class by overriding `analyze`).
pub trait MediaAnalyses: Send + Sync {
    /// See [`MediaAnalysisService::analyze`].
    fn analyze(
        &self,
        path: &str,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> Result<Option<TechniqueAnalysis>, MediaError>;
}

/// Computes the full take-style analysis bundle for an arbitrary media file
/// (any format the engine can decode), for the technique library's reference
/// "ground truth" and the ephemeral practice comparison. Blocking: run it on
/// a worker thread.
pub struct MediaAnalysisService {
    decode: Box<MediaDecoder>,
}

impl Default for MediaAnalysisService {
    fn default() -> Self {
        MediaAnalysisService::new()
    }
}

impl MediaAnalysisService {
    /// The service over the native engine's decode
    /// ([`decode_media_to_mono`] at [`ANALYSIS_DECODE_RATE`]).
    pub fn new() -> Self {
        MediaAnalysisService::with_decoder(|path, max_duration| {
            decode_media_to_mono(path, ANALYSIS_DECODE_RATE, max_duration)
        })
    }

    /// The service over [decode] instead: a unit test, which cannot load the
    /// engine, passes its own.
    pub fn with_decoder(
        decode: impl Fn(&str, Option<Duration>) -> Result<Option<Pcm>, MediaError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        MediaAnalysisService {
            decode: Box::new(decode),
        }
    }

    /// Decodes [path] and runs the complete analysis suite. `Ok(None)` when
    /// the file can't be decoded or is empty; [`MediaError::TooLong`] when it
    /// exceeds [`MAX_ANALYSIS_DURATION`] (a clear, user-presentable message —
    /// callers already handle analyze() failing), and the decode's other
    /// refusals as they come.
    ///
    /// [wanted], when given, is asked once the gate admits the call and again
    /// after the decode: false abandons it and returns `Ok(None)`. A call can
    /// wait behind other computes long enough to go stale (its example
    /// deselected, its screen left), and running it anyway only delays the
    /// ones still wanted. A caller passing it tells such a skip from a failed
    /// decode itself.
    pub fn analyze(
        &self,
        path: &str,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> Result<Option<TechniqueAnalysis>, MediaError> {
        let unwanted = || wanted.is_some_and(|w| !w());
        // Gated end to end: the DECODE is the larger peak of this pipeline, so
        // it must sit inside the same admission as the analysis — otherwise
        // two callers could stack full-track decodes while "only" their
        // analyses were serialized.
        analysis_gate().run(|| {
            if unwanted() {
                return Ok(None);
            }
            let Some(mut pcm) = (self.decode)(path, Some(MAX_ANALYSIS_DURATION))? else {
                return Ok(None);
            };
            if pcm.samples().is_empty() {
                return Ok(None);
            }
            // Again: the decode of a long clip takes seconds, ample to
            // deselect it.
            if unwanted() {
                return Ok(None);
            }
            let rate = i64::from(pcm.rate());
            Ok(analyze_samples(pcm.samples_mut(), rate))
        })
    }
}

impl MediaAnalyses for MediaAnalysisService {
    fn analyze(
        &self,
        path: &str,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> Result<Option<TechniqueAnalysis>, MediaError> {
        MediaAnalysisService::analyze(self, path, wanted)
    }
}

/// The analysis itself (Dart's isolate body), over the decoded buffer.
fn analyze_samples(samples: &mut [f32], sample_rate: i64) -> Option<TechniqueAnalysis> {
    if samples.is_empty() || sample_rate <= 0 {
        return None;
    }
    // Drop non-finite samples before any DSP touches them, exactly as the WAV
    // path does when it narrows to f32 (see SpectrogramService's chunked
    // reader): one NaN or Inf poisons the FFT, every aggregate derived from
    // it, and — because the spectrogram's peak goes with it — the whole heat
    // image. This path decodes ARBITRARY media through the native engine, so
    // it has strictly more ways to produce one than the WAV path does.
    for s in samples.iter_mut() {
        if !s.is_finite() {
            *s = 0.0;
        }
    }
    let MetricsAndSpectrogram {
        metrics,
        spectrogram,
    } = compute_voice_metrics_and_spectrogram(samples, sample_rate, AnalysisParams::default());
    let ltas = compute_ltas(&spectrogram, Some(&metrics));
    let profile = analyze_voice_profile(&metrics, VoiceProfileOptions::default());
    let register = analyze_register(&metrics, RegisterOptions::default());
    Some(TechniqueAnalysis {
        metrics: Arc::new(metrics),
        spectrogram: Arc::new(spectrogram),
        ltas: Arc::new(ltas),
        profile: Arc::new(profile),
        register: Arc::new(register),
        duration_ms: round_to_i64(samples.len() as f64 / sample_rate as f64 * 1000.0),
    })
}

#[cfg(test)]
mod tests {
    //! Ports of media_analysis_isolate_test.dart and of
    //! media_decode_cap_test.dart's service case (its other cases are the
    //! decode policy's, ported with `engine::media`), plus the service's own
    //! contract: the gate around the decode, the cap handed to the decoder,
    //! the refusals passed through, the scrub and the suite it returns.

    use std::cell::Cell;
    use std::f64::consts::PI;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    const RATE: u32 = 16000;

    /// [secs] seconds of a 0.3-amplitude 220 Hz tone at [`RATE`].
    fn tone(secs: u32) -> Vec<f32> {
        (0..secs * RATE)
            .map(|i| (0.3 * (2.0 * PI * 220.0 * f64::from(i) / f64::from(RATE)).sin()) as f32)
            .collect()
    }

    /// Stands in for the technique view model: what a `wanted` predicate
    /// closes over. Not `Send`, as the view model's state is not.
    struct Owner {
        still_wanted: Rc<Cell<bool>>,
    }

    #[test]
    fn a_wanted_predicate_over_an_unsendable_owner_stays_on_the_calling_thread() {
        // Dart's isolate hop closed over the gate closure's context, which
        // held the predicate — and with it the whole technique view model,
        // which the isolate message could not carry: every reference
        // example's analysis threw and was marked failed. Here the predicate
        // is only borrowed, so an owner that cannot cross threads is fine.
        let samples = tone(2);
        let service = MediaAnalysisService::with_decoder(move |_, _| {
            Ok(Some(Pcm::mono(samples.clone(), RATE)))
        });
        let owner = Owner {
            still_wanted: Rc::new(Cell::new(true)),
        };
        let wanted = || owner.still_wanted.get();

        let analysis = service
            .analyze("reference.m4a", Some(&wanted))
            .unwrap()
            .expect("a wanted clip is analyzed");
        assert_eq!(analysis.duration_ms, 2000);
    }

    #[test]
    fn a_clip_deselected_during_its_decode_is_not_analyzed() {
        let wanted = Arc::new(AtomicBool::new(true));
        let in_decode = wanted.clone();
        let service = MediaAnalysisService::with_decoder(move |_, _| {
            in_decode.store(false, Ordering::SeqCst); // deselected while it decoded
            Ok(Some(Pcm::mono(vec![0.0; RATE as usize], RATE)))
        });
        let still = || wanted.load(Ordering::SeqCst);
        assert!(service
            .analyze("reference.m4a", Some(&still))
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_clip_unwanted_by_its_turn_is_never_decoded() {
        let decodes = Arc::new(AtomicUsize::new(0));
        let counted = decodes.clone();
        let service = MediaAnalysisService::with_decoder(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Pcm::mono(tone(1), RATE)))
        });
        assert!(service
            .analyze("reference.m4a", Some(&|| false))
            .unwrap()
            .is_none());
        assert_eq!(decodes.load(Ordering::SeqCst), 0);
        // Without a predicate, it always runs.
        assert!(service.analyze("reference.m4a", None).unwrap().is_some());
        assert_eq!(decodes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_analysis_service_caps_at_10_minutes_of_decoded_audio() {
        assert_eq!(MAX_ANALYSIS_DURATION, Duration::from_secs(10 * 60));
        // ...and hands the decoder that cap, which enforces it natively.
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        let service = MediaAnalysisService::with_decoder(move |path, cap| {
            seen.lock().unwrap().push((path.to_string(), cap));
            Ok(None)
        });
        assert!(service.analyze("clip.mp4", None).unwrap().is_none());
        assert_eq!(
            *asked.lock().unwrap(),
            [("clip.mp4".to_string(), Some(MAX_ANALYSIS_DURATION))]
        );
    }

    #[test]
    fn a_decode_refusal_reaches_the_caller_with_its_message() {
        let service = MediaAnalysisService::with_decoder(|_, cap| {
            Err(MediaError::TooLong {
                max_duration: cap.unwrap_or_default(),
            })
        });
        let e = service.analyze("long.m4a", None).unwrap_err();
        assert_eq!(
            e,
            MediaError::TooLong {
                max_duration: MAX_ANALYSIS_DURATION
            }
        );
        assert!(e.to_string().contains("10 minutes"));
        let unreadable = MediaAnalysisService::with_decoder(|_, _| Err(MediaError::Read));
        assert_eq!(
            unreadable.analyze("x.m4a", None).unwrap_err(),
            MediaError::Read
        );
    }

    #[test]
    fn an_undecodable_or_empty_file_has_no_analysis() {
        let none = MediaAnalysisService::with_decoder(|_, _| Ok(None));
        assert!(none.analyze("x.m4a", None).unwrap().is_none());
        let empty =
            MediaAnalysisService::with_decoder(|_, _| Ok(Some(Pcm::mono(Vec::new(), RATE))));
        assert!(empty.analyze("x.m4a", None).unwrap().is_none());
        let rateless = MediaAnalysisService::with_decoder(|_, _| Ok(Some(Pcm::mono(tone(1), 0))));
        assert!(rateless.analyze("x.m4a", None).unwrap().is_none());
    }

    #[test]
    fn the_decode_waits_its_turn_inside_the_analysis_gate() {
        // The decode is the larger peak of this pipeline: it runs inside the
        // gate's admission, not before it.
        let inside = Arc::new(AtomicBool::new(false));
        let seen = inside.clone();
        let service = MediaAnalysisService::with_decoder(move |_, _| {
            seen.store(analysis_gate().running() >= 1, Ordering::SeqCst);
            Ok(None)
        });
        service.analyze("x.m4a", None).unwrap();
        assert!(inside.load(Ordering::SeqCst));
    }

    #[test]
    fn the_result_is_the_take_suite_of_the_decoded_samples_scrubbed() {
        // One NaN or Inf from an arbitrary decoder must not poison the FFT,
        // the aggregates or the heat image.
        let mut poisoned = tone(2);
        poisoned[1000] = f32::NAN;
        poisoned[2000] = f32::INFINITY;
        poisoned[3000] = f32::NEG_INFINITY;
        let input = poisoned.clone();
        let service = MediaAnalysisService::with_decoder(move |_, _| {
            Ok(Some(Pcm::mono(input.clone(), RATE)))
        });
        let got = service.analyze("x.wav", None).unwrap().unwrap();

        let mut clean = poisoned;
        for i in [1000, 2000, 3000] {
            clean[i] = 0.0;
        }
        let rate = i64::from(RATE);
        let want = compute_voice_metrics_and_spectrogram(&clean, rate, AnalysisParams::default());
        assert!(got.spectrogram.db.iter().all(|v| v.is_finite()));
        assert_eq!(got.spectrogram.db, want.spectrogram.db);
        assert_eq!(got.spectrogram.frames, want.spectrogram.frames);
        assert_eq!(got.metrics.frames, want.metrics.frames);
        assert_eq!(got.metrics.f0.len(), want.metrics.f0.len());
        for (a, b) in got.metrics.f0.iter().zip(&want.metrics.f0) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        let ltas = compute_ltas(&want.spectrogram, Some(&want.metrics));
        assert_eq!(got.ltas.db, ltas.db);
        assert_eq!(got.ltas.frame_count, ltas.frame_count);
        assert!(got.ltas.alpha_ratio.is_finite());
        let profile = analyze_voice_profile(&want.metrics, VoiceProfileOptions::default());
        assert_eq!(
            (got.profile.low_midi, got.profile.high_midi),
            (profile.low_midi, profile.high_midi)
        );
        let register = analyze_register(&want.metrics, RegisterOptions::default());
        assert_eq!(got.register.voiced_frames, register.voiced_frames);
        assert!(got.register.has_data());
        assert_eq!(got.duration_ms, 2000);
    }

    #[test]
    fn the_service_is_shared_with_worker_threads() {
        fn send_sync<T: Send + Sync + ?Sized>() {}
        send_sync::<MediaAnalysisService>();
        send_sync::<dyn MediaAnalyses>();
        let service: Arc<dyn MediaAnalyses> =
            Arc::new(MediaAnalysisService::with_decoder(|_, _| {
                Ok(Some(Pcm::mono(tone(1), RATE)))
            }));
        let worker = std::thread::spawn(move || service.analyze("x.m4a", None));
        let analysis = worker.join().unwrap().unwrap().unwrap();
        assert_eq!(analysis.duration_ms, 1000);
    }

    #[cfg(not(feature = "device"))]
    #[test]
    fn without_the_engine_the_default_decoder_reports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("clip.m4a");
        std::fs::write(&file, b"not really audio").unwrap();
        assert_eq!(
            MediaAnalysisService::new()
                .analyze(&file.to_string_lossy(), None)
                .unwrap_err(),
            MediaError::EngineUnavailable
        );
    }
}
