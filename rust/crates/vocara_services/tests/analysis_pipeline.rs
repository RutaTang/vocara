//! Port of `client/integration_test/analysis_pipeline_test.dart`: the
//! analysis pipeline end to end on synthetic audio — the DSP directly
//! (voice metrics, spectrogram, LTAS, profile, register, vibrato), the
//! file-path `SpectrogramService` over a WAV written to a temp dir, and
//! `MediaAnalysisService` decoding that WAV.
//!
//! The media-analysis group decodes through the engine bridge, which exists
//! only with the `device` feature (the macOS CI run, as Dart's integration
//! test ran on the Mac). A build without it has no engine to decode with, so
//! there the same files go through the same service on the WAV reader's
//! decode instead (every fixture is already 44.1 kHz mono, the rate the
//! engine resamples to), and the decode itself is what the device run adds.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::voice_metrics::{
    analyze_register, analyze_vibrato, analyze_voice_profile, compute_ltas, RegisterOptions,
    VibratoOptions, VoiceProfileOptions,
};
use vocara_services::audio_dsp::{
    compute_spectrogram, compute_voice_metrics, write_wav_mono, AnalysisParams,
};
use vocara_services::media_analysis_service::MediaAnalysisService;
use vocara_services::spectrogram_service::SpectrogramService;

const SR: i64 = 44100;

/// Writes mono 16-bit PCM WAV from samples in [-1, 1] and returns its path.
fn write_wav(path: &Path, samples: &[f32]) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, write_wav_mono(samples, SR)).unwrap();
    path.to_path_buf()
}

/// A steady sine at [hz] with a few harmonics so the framing/voicing gate has
/// a real periodic source to lock onto (a pure single sine can read
/// low-confidence in some frames). Amplitude is kept well below clipping.
fn tone(hz: f64, sec: f64) -> Vec<f32> {
    let n = (SR as f64 * sec).round() as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            (0.6 * (2.0 * PI * hz * t).sin()
                + 0.3 * (2.0 * PI * 2.0 * hz * t).sin()
                + 0.15 * (2.0 * PI * 3.0 * hz * t).sin()) as f32
        })
        .collect()
}

/// A pitch glide from [lo_hz] to [hi_hz] over [sec] seconds, harmonically
/// rich.
fn glide(lo_hz: f64, hi_hz: f64, sec: f64) -> Vec<f32> {
    let n = (SR as f64 * sec).round() as usize;
    let mut phase = 0.0;
    (0..n)
        .map(|i| {
            let f = lo_hz + (hi_hz - lo_hz) * (i as f64 / (n - 1) as f64);
            phase += 2.0 * PI * f / SR as f64;
            (0.6 * phase.sin() + 0.3 * (2.0 * phase).sin() + 0.15 * (3.0 * phase).sin()) as f32
        })
        .collect()
}

/// A sustained tone whose pitch oscillates sinusoidally — a synthetic
/// vibrato at [rate_hz] / ±[extent_cents] around [f0].
fn vibrato_tone(f0: f64, rate_hz: f64, extent_cents: f64, sec: f64) -> Vec<f32> {
    let n = (SR as f64 * sec).round() as usize;
    let mut phase = 0.0;
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            let ratio = 2f64.powf((extent_cents / 1200.0) * (2.0 * PI * rate_hz * t).sin());
            phase += 2.0 * PI * f0 * ratio / SR as f64;
            (0.6 * phase.sin() + 0.3 * (2.0 * phase).sin() + 0.15 * (3.0 * phase).sin()) as f32
        })
        .collect()
}

/// The middle (upper median) of the non-NaN values; NaN when there are none.
fn median(xs: &[f32]) -> f64 {
    let mut v: Vec<f64> = xs
        .iter()
        .filter(|x| !x.is_nan())
        .map(|&x| f64::from(x))
        .collect();
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        f64::NAN
    } else {
        v[v.len() / 2]
    }
}

fn voiced(xs: &[f32]) -> Vec<f32> {
    xs.iter()
        .copied()
        .filter(|x| !x.is_nan() && *x > 0.0)
        .collect()
}

fn all_finite(xs: &[f32]) -> bool {
    xs.iter().all(|x| x.is_finite())
}

fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
    (actual - expected).abs() <= tolerance
}

/// The register fractions sum to 0 (no data) or 100.
fn register_sums_to_0_or_100(chest: f64, mix: f64, head: f64) -> bool {
    let sum = chest + mix + head;
    close(sum, 0.0, 1e-6) || close(sum, 100.0, 1e-3)
}

/// The medians of the voiced f0 in the first and the last third of the
/// frames.
fn front_and_back_medians(f0: &[f32]) -> (f64, f64) {
    let n = f0.len();
    let mut front = Vec::new();
    let mut back = Vec::new();
    for (i, &v) in f0.iter().enumerate() {
        if v.is_nan() || v <= 0.0 {
            continue;
        }
        if i < n / 3 {
            front.push(f64::from(v));
        }
        if i >= 2 * n / 3 {
            back.push(f64::from(v));
        }
    }
    assert!(!front.is_empty());
    assert!(!back.is_empty());
    front.sort_by(f64::total_cmp);
    back.sort_by(f64::total_cmp);
    (front[front.len() / 2], back[back.len() / 2])
}

fn p() -> AnalysisParams {
    AnalysisParams::default()
}

// --- direct DSP pipeline (computeVoiceMetrics + computeSpectrogram) ---------------

#[test]
fn a_steady_440_hz_tone_yields_a_voiced_f0_within_a_few_hz_of_440() {
    let m = compute_voice_metrics(&tone(440.0, 1.5), SR, p());
    assert!(m.frames > 10);

    assert!(!voiced(&m.f0).is_empty(), "a steady tone must voice frames");
    // YIN runs on a ~22 kHz decimation — assert a few-Hz band, not sub-Hz.
    let med = median(&m.f0);
    assert!(!med.is_nan());
    assert!(close(med, 440.0, 6.0), "median f0={med} Hz (A4=440)");

    // Timbre means are over VOICED frames only, by design — they must be real.
    assert!(m.ring_avg.is_finite(), "ringAvg={}", m.ring_avg);
    assert!(m.tilt_avg.is_finite(), "tiltAvg={}", m.tilt_avg);
    assert!(m.cpp_avg.is_finite(), "cppAvg={}", m.cpp_avg);

    // Per-frame curves are finite (NaN only allowed on the F0/formant/H1−H2
    // unvoiced fields — ring/tilt/cpp/loudness are dense).
    assert!(all_finite(&m.ring));
    assert!(all_finite(&m.tilt));
    assert!(all_finite(&m.cpp));
    assert!(all_finite(&m.loudness));

    // H1−H2 is defined on voiced frames; values are either NaN (unvoiced) or
    // finite — never Inf — and at least some voiced frames carry a value.
    let mut any_finite_h = false;
    for &x in &m.h1h2 {
        if x.is_finite() {
            any_finite_h = true;
        }
        assert!(x.is_nan() || x.is_finite());
    }
    assert!(any_finite_h, "voiced frames should carry H1−H2");
}

#[test]
fn the_glide_220_to_440_hz_f0_increases_over_time() {
    let m = compute_voice_metrics(&glide(220.0, 440.0, 2.0), SR, p());
    assert!(m.frames > 20);

    // Compare the median f0 of the first third vs the last third of voiced
    // frames; the back should sit clearly higher than the front.
    let (lo_med, hi_med) = front_and_back_medians(&m.f0);
    assert!(
        hi_med > lo_med + 50.0,
        "glide front={lo_med} Hz back={hi_med} Hz should rise"
    );
    // Sanity: the endpoints should bracket the 220→440 sweep loosely.
    assert!(close(lo_med, 220.0, 40.0), "{lo_med}");
    assert!(close(hi_med, 440.0, 40.0), "{hi_med}");
}

#[test]
fn a_vibrato_modulated_tone_is_detected_with_a_healthy_rate() {
    let m = compute_voice_metrics(&vibrato_tone(440.0, 5.5, 60.0, 1.6), SR, p());
    let v = analyze_vibrato(&m, 0, m.frames as i64 - 1, VibratoOptions::default());
    assert!(v.present, "should detect the ~5.5 Hz vibrato");
    assert!((4.5..=7.0).contains(&v.rate_hz), "{}", v.rate_hz);
    assert!(v.extent_cents.is_finite());
    assert!(v.regularity > 0.5, "{}", v.regularity);
    // jitter is finite (not NaN) when vibrato is present.
    assert!(!v.jitter.is_nan());
}

#[test]
fn the_spectrogram_has_the_expected_frame_and_bin_shape_and_aligns() {
    let samples = tone(440.0, 1.5);
    let params = AnalysisParams {
        fft_size: 2048,
        hop: 512,
        max_hz: 5000.0,
    };
    let spec = compute_spectrogram(&samples, SR, params);
    let m = compute_voice_metrics(&samples, SR, params);

    // bins == min(fftSize/2+1, ceil(maxHz*fftSize/sr)+1) == ceil(5000*2048/44100)+1.
    let expected_bins = (5000.0 * 2048.0 / SR as f64).ceil() as usize + 1;
    assert_eq!(spec.bins, expected_bins, "maxHz=5000 bin count");
    assert!(spec.frames > 10);
    assert_eq!(spec.db.len(), spec.frames * spec.bins);
    // Framing must match the metrics framing (overlay/playhead alignment).
    assert_eq!(spec.frames, m.frames);
    assert!(close(spec.bin_hz(), SR as f64 / 2048.0, 1e-6));
    // The dominant bin should sit at ~440 Hz on average.
    let mut avg = vec![0.0f64; spec.bins];
    for f in 0..spec.frames {
        let base = f * spec.bins;
        for (b, a) in avg.iter_mut().enumerate() {
            *a += f64::from(spec.db[base + b]);
        }
    }
    let mut argmax = 0;
    for b in 1..spec.bins {
        if avg[b] > avg[argmax] {
            argmax = b;
        }
    }
    let peak_hz = argmax as f64 * spec.bin_hz();
    // One STFT bin is ~21.5 Hz wide — accept the fundamental's bin.
    assert!(
        close(peak_hz, 440.0, spec.bin_hz() * 1.5),
        "dominant bin at {peak_hz} Hz"
    );
}

#[test]
fn voice_profile_and_register_are_finite_and_sane_on_the_glide() {
    let m = compute_voice_metrics(&glide(220.0, 440.0, 2.0), SR, p());
    let profile = analyze_voice_profile(&m, VoiceProfileOptions::default());
    // A 220→440 sweep is a full octave: a real range should be reported.
    assert!(profile.has_range());
    let (low, high) = (profile.low_midi.unwrap(), profile.high_midi.unwrap());
    assert!(
        high - low >= 6,
        "an octave glide spans well over 6 semitones ({low}..{high})"
    );
    // Dynamics aggregates are finite or cleanly absent.
    assert!(profile.soft_db.is_nan() || profile.soft_db.is_finite());
    assert!(profile.loud_db.is_nan() || profile.loud_db.is_finite());

    let reg = analyze_register(&m, RegisterOptions::default());
    assert!(
        register_sums_to_0_or_100(reg.chest_pct, reg.mix_pct, reg.head_pct),
        "register fractions sum to 0 (no data) or 100"
    );
}

// --- LTAS bundle (computeLtas over a real spectrogram) ------------------------------

#[test]
fn ltas_aggregates_are_finite_for_a_steady_tone() {
    let samples = tone(440.0, 1.5);
    let spec = compute_spectrogram(&samples, SR, p());
    let m = compute_voice_metrics(&samples, SR, p());
    let ltas = compute_ltas(&spec, Some(&m));
    assert!(ltas.has_data());
    assert_eq!(ltas.db.len(), spec.bins);
    assert!(all_finite(&ltas.db));
    assert!(ltas.alpha_ratio.is_finite());
    assert!(ltas.singers_formant_db.is_finite());
    assert!(ltas.frame_count > 0);
}

// --- edge cases must not crash ------------------------------------------------------

#[test]
fn pure_silence_yields_no_voiced_f0_and_finite_or_empty_aggregates() {
    let silence = vec![0.0f32; SR as usize]; // all zeros
    let m = compute_voice_metrics(&silence, SR, p());
    // Framing still produces frames, but nothing should voice.
    assert!(voiced(&m.f0).is_empty(), "silence has no pitch");
    // Voiced-only averages have nothing to average: NaN, shown as "—".
    assert!(m.ring_avg.is_nan());
    assert!(m.tilt_avg.is_nan());
    assert!(m.cpp_avg.is_nan());

    let spec = compute_spectrogram(&silence, SR, p());
    assert_eq!(spec.frames, m.frames, "silence framing still aligns");

    let profile = analyze_voice_profile(&m, VoiceProfileOptions::default());
    assert!(!profile.has_range(), "no pitch → no range");
    assert!(!profile.has_passaggio());

    let ltas = compute_ltas(&spec, Some(&m));
    assert!(all_finite(&ltas.db));

    let reg = analyze_register(&m, RegisterOptions::default());
    assert!(!reg.has_data(), "no voiced H1−H2 → no register data");

    let vib = analyze_vibrato(&m, 0, m.frames as i64 - 1, VibratoOptions::default());
    assert!(!vib.present, "silence has no vibrato");
}

#[test]
fn a_buffer_shorter_than_the_fft_window_returns_one_padded_frame_no_crash() {
    // < fftSize (2048), with a faint 440 Hz tone in it so it is not pure zeros.
    let short_buf: Vec<f32> = (0..1000)
        .map(|i| (0.4 * (2.0 * PI * 440.0 * i as f64 / SR as f64).sin()) as f32)
        .collect();
    let m = compute_voice_metrics(&short_buf, SR, p());
    assert!(
        m.frames >= 1,
        "sub-window input still yields a zero-padded frame"
    );
    // Every curve length matches the frame count (alignment invariant).
    assert_eq!(m.f0.len(), m.frames);
    assert_eq!(m.f1.len(), m.frames);
    assert_eq!(m.ring.len(), m.frames);

    let spec = compute_spectrogram(&short_buf, SR, p());
    assert_eq!(
        spec.frames, m.frames,
        "short-buffer metrics/spectrogram framing must match"
    );

    // Aggregates are finite even when too short to be meaningful.
    assert!(m.ring_avg.is_finite());
    assert!(m.cpp_avg.is_finite());

    // Profile/register/vibrato on a 1-frame-ish take: sane empties, no panic.
    let profile = analyze_voice_profile(&m, VoiceProfileOptions::default());
    assert!(!profile.has_range(), "too few frames to be a range");
    let _ = analyze_register(&m, RegisterOptions::default()).has_data();
    assert!(!analyze_vibrato(&m, 0, m.frames as i64 - 1, VibratoOptions::default()).present);
}

#[test]
fn truly_empty_input_yields_zero_frames() {
    let m = compute_voice_metrics(&[], SR, p());
    assert_eq!(m.frames, 0);
    assert!(m.ring_avg.is_nan());
    let spec = compute_spectrogram(&[], SR, p());
    assert_eq!(spec.frames, 0);
    assert!(!analyze_voice_profile(&m, VoiceProfileOptions::default()).has_range());
}

// --- file-path services (SpectrogramService reads WAV PCM16) ------------------------

#[test]
fn compute_metrics_from_file_and_compute_from_file_on_a_440_hz_wav() {
    let tmp = tempfile::tempdir().unwrap();
    let wav = write_wav(&tmp.path().join("tone440.wav"), &tone(440.0, 1.5));
    let wav = wav.to_string_lossy();

    let svc = SpectrogramService::new();
    let metrics = svc.compute_metrics_from_file(&wav, p(), None).unwrap();
    let spec = svc.compute_from_file(&wav, p(), None).unwrap();

    let metrics = metrics.expect("WAV PCM16 must decode via the WAV reader");
    let spec = spec.expect("a spectrogram");
    let med = median(&metrics.f0);
    assert!(close(med, 440.0, 6.0), "file-path median f0={med} Hz");
    assert!(!voiced(&metrics.f0).is_empty());
    assert_eq!(
        spec.frames, metrics.frames,
        "service framing aligns metrics ↔ spectrogram"
    );
    assert!(all_finite(&spec.db));
}

#[test]
fn a_missing_file_returns_none_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does_not_exist.wav");
    let missing = missing.to_string_lossy();
    let svc = SpectrogramService::new();
    assert!(svc
        .compute_from_file(&missing, p(), None)
        .unwrap()
        .is_none());
    assert!(svc
        .compute_metrics_from_file(&missing, p(), None)
        .unwrap()
        .is_none());
}

// --- MediaAnalysisService end-to-end (native engine decode) -------------------------

/// The service as the app builds it: the engine bridge's decode.
#[cfg(feature = "device")]
fn media_analysis() -> MediaAnalysisService {
    MediaAnalysisService::new()
}

/// No engine in this build (see the module doc): the service over the WAV
/// reader's decode of the same file.
#[cfg(not(feature = "device"))]
fn media_analysis() -> MediaAnalysisService {
    use vocara_services::audio_dsp::read_wav_mono;
    use vocara_services::engine::media::MediaError;
    use vocara_services::engine::Pcm;
    MediaAnalysisService::with_decoder(|path, _cap| {
        let bytes = std::fs::read(path).map_err(|_| MediaError::Read)?;
        Ok(read_wav_mono(&bytes)
            .map(|pcm| Pcm::mono(pcm.samples, u32::try_from(pcm.sample_rate).unwrap_or(0))))
    })
}

fn analyze(path: &Path) -> Option<TechniqueAnalysis> {
    media_analysis()
        .analyze(&path.to_string_lossy(), None)
        .expect("the decode refuses nothing here")
}

#[test]
fn analyze_a_440_hz_wav_gives_the_full_bundle_with_sane_f0_and_spectrogram() {
    let tmp = tempfile::tempdir().unwrap();
    let wav = write_wav(&tmp.path().join("media440.wav"), &tone(440.0, 1.5));
    let a = analyze(&wav).expect("the native engine should decode WAV");

    assert!(
        a.duration_ms > 1000,
        "a ~1.5 s tone reports a real duration ({})",
        a.duration_ms
    );
    assert!(a.spectrogram.frames > 10);
    assert_eq!(
        a.spectrogram.frames, a.metrics.frames,
        "bundle framing aligns"
    );

    let med = median(&a.metrics.f0);
    assert!(!med.is_nan());
    // Decode resamples to 44.1 kHz; YIN still on the ~22 kHz path → few-Hz band.
    assert!(close(med, 440.0, 8.0), "decoded median f0={med} Hz");
    assert!(!voiced(&a.metrics.f0).is_empty());

    // Every bundle aggregate is finite.
    assert!(a.metrics.ring_avg.is_finite());
    assert!(a.metrics.tilt_avg.is_finite());
    assert!(a.metrics.cpp_avg.is_finite());
    assert!(a.ltas.alpha_ratio.is_finite());
    assert!(a.ltas.singers_formant_db.is_finite());
    assert!(all_finite(&a.ltas.db));
    assert!(register_sums_to_0_or_100(
        a.register.chest_pct,
        a.register.mix_pct,
        a.register.head_pct
    ));
}

#[test]
fn analyze_a_glide_and_its_f0_rises_front_to_back() {
    let tmp = tempfile::tempdir().unwrap();
    let wav = write_wav(
        &tmp.path().join("mediaGlide.wav"),
        &glide(220.0, 440.0, 2.0),
    );
    let a = analyze(&wav).expect("an analysis");
    let (front, back) = front_and_back_medians(&a.metrics.f0);
    assert!(back > front + 50.0, "decoded glide f0 must rise");
}
