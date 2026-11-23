//! The take's voice metrics: voice_metrics_test.dart's batch, streaming,
//! formant, timbre, quality and gating tests. (Its segmentPitchNotes,
//! pitchSemitoneRange and the off-pitch-note tests exercise the pitch
//! lane's note segmentation, a screen helper ported with the pitch lane;
//! noteOf/noteLabel are vocara_model's.)

use std::f64::consts::PI;

use vocara_model::models::voice_metrics::{
    analyze_vibrato, compute_ltas, VibratoOptions, VoiceMetrics,
};

use super::super::lpc::ordered_formants;
use super::super::{
    compute_spectrogram, compute_voice_metrics, AnalysisParams, StreamingTakeAnalyzer,
    MAX_ANALYSIS_SAMPLE_RATE,
};
use super::support::{
    assert_bits_eq, assert_close, assert_scalar_eq, finite_sorted, median, noise, sung_vowel,
    tones, vibrato_tone, vowel, vowel_at, DartRandom, SR,
};

fn metrics(samples: &[f32], sr: i64) -> VoiceMetrics {
    compute_voice_metrics(samples, sr, AnalysisParams::default())
}

#[test]
fn streaming_take_analyzer_equals_compute_voice_metrics_bit_for_bit_over_ragged_chunks() {
    // Same pass body, decimators reproducing the batch decimation exactly,
    // identical zero-pad edges — pinned here, NaNs compared by bits (the
    // batch is the oracle).
    for (sr, seconds) in [(SR, 1.2), (48000, 0.9), (22050, 0.7)] {
        let base = vibrato_tone(220.0, 5.5, 30.0, seconds, sr);
        // Splice in silence and a noisy tail so voicing gates/NaN paths and
        // the zero-padded final frame all exercise.
        let n = base.len();
        let mut samples = vec![0.0f32; n + 700];
        samples[..n].copy_from_slice(&base);
        let mut rng = DartRandom::new(11);
        for i in 0..500 {
            samples[n + 200 + i] = ((rng.next_double() - 0.5) * 0.02) as f32;
        }
        let batch = metrics(&samples, sr);
        let b_spec = compute_spectrogram(&samples, sr, AnalysisParams::default());

        let len = samples.len();
        let mut uniform = vec![777; len / 777];
        uniform.push(len % 777);
        for chunk_sizes in [vec![len], vec![1, 313, 4097, len - 1 - 313 - 4097], uniform] {
            let mut an =
                StreamingTakeAnalyzer::new(sr, AnalysisParams::default(), batch.frames, true);
            let mut at = 0;
            for c in chunk_sizes {
                if c == 0 {
                    continue;
                }
                an.add(&samples[at..at + c], None);
                at += c;
            }
            assert_eq!(at, len, "fixture sanity");
            let out = an.finish();
            let m = &out.metrics;
            assert_eq!(m.frames, batch.frames, "sr={sr} frames");
            assert_bits_eq(&m.ring, &batch.ring, "ring");
            assert_bits_eq(&m.tilt, &batch.tilt, "tilt");
            assert_bits_eq(&m.cpp, &batch.cpp, "cpp");
            assert_bits_eq(&m.loudness, &batch.loudness, "loudness");
            assert_bits_eq(&m.f0, &batch.f0, "f0");
            assert_bits_eq(&m.f1, &batch.f1, "f1");
            assert_bits_eq(&m.f2, &batch.f2, "f2");
            assert_bits_eq(&m.f3, &batch.f3, "f3");
            assert_bits_eq(&m.h1h2, &batch.h1h2, "h1h2");
            assert_eq!(m.active, batch.active, "active");
            assert_scalar_eq(m.ring_avg, batch.ring_avg, "ringAvg");
            assert_scalar_eq(m.tilt_avg, batch.tilt_avg, "tiltAvg");
            assert_scalar_eq(m.cpp_avg, batch.cpp_avg, "cppAvg");
            assert_scalar_eq(m.peak_dbfs, batch.peak_dbfs, "peakDbfs");
            assert_scalar_eq(
                m.clipped_fraction,
                batch.clipped_fraction,
                "clippedFraction",
            );
            assert_scalar_eq(m.snr_db, batch.snr_db, "snrDb");
            // The bundled spectrogram equals the batch one too.
            let spec = out.spectrogram.expect("asked for a spectrogram");
            assert_eq!(spec.frames, b_spec.frames);
            assert_bits_eq(&spec.db, &b_spec.db, "spectrogram db");
        }
    }

    // Sub-fftSize and empty inputs match the batch edge semantics.
    let tiny: Vec<f32> = (0..300)
        .map(|i| ((i as f64 / 5.0).sin() * 0.4) as f32)
        .collect();
    let tb = metrics(&tiny, SR);
    let mut ta = StreamingTakeAnalyzer::new(SR, AnalysisParams::default(), tb.frames, true);
    ta.add(&tiny, None);
    assert_eq!(ta.finish().metrics.frames, tb.frames);
    let eb = metrics(&[], SR);
    let ea = StreamingTakeAnalyzer::new(SR, AnalysisParams::default(), 0, true);
    assert_eq!(ea.finish().metrics.frames, eb.frames);
}

#[test]
fn the_streaming_analyzer_without_a_spectrogram_and_with_a_wrong_frame_hint() {
    // spectrogram: false keeps none; a hint below the real count still
    // yields every frame (the outputs grow past their reservation).
    let samples = vibrato_tone(220.0, 5.5, 30.0, 0.5, SR);
    let batch = metrics(&samples, SR);
    for hint in [0, batch.frames / 2, batch.frames + 9] {
        let mut an = StreamingTakeAnalyzer::new(SR, AnalysisParams::default(), hint, false);
        for c in samples.chunks(1000) {
            an.add(c, None);
        }
        let out = an.finish();
        assert!(out.spectrogram.is_none());
        assert_eq!(out.metrics.frames, batch.frames);
        assert_bits_eq(&out.metrics.f0, &batch.f0, &format!("f0 (hint {hint})"));
    }
}

#[test]
fn the_loudest_channel_drives_the_peak_and_clipping_scan() {
    // A full-scale channel beside a silent one averages to half scale; the
    // scan reads the loudest channel when the caller hands it over.
    let loud: Vec<f32> = (0..SR as usize)
        .map(|i| (2.0 * (2.0 * PI * 220.0 * i as f64 / SR as f64).sin()).clamp(-1.0, 1.0) as f32)
        .collect();
    let mixed: Vec<f32> = loud.iter().map(|&v| v / 2.0).collect();
    let mut an = StreamingTakeAnalyzer::new(SR, AnalysisParams::default(), 0, false);
    an.add(&mixed, Some(&loud));
    let m = an.finish().metrics;
    assert!(m.is_clipped());
    assert_close(m.peak_dbfs, 0.0, 1e-6, "peak of the loudest channel");
}

#[test]
fn formants_land_near_a_synthetic_a() {
    // F1~700, F2~1200.
    let m = metrics(&vowel(700.0, 1200.0, 2600.0, 130.0, 1.0), SR);
    let (f1, f2) = (median(&m.f1), median(&m.f2));
    assert!((560.0..=860.0).contains(&f1), "F1 median {f1}");
    assert!((1040.0..=1420.0).contains(&f2), "F2 median {f2}");
}

#[test]
fn formants_track_a_different_vowel_i() {
    // F1~320, F2~2300.
    let m = metrics(&vowel(320.0, 2300.0, 3200.0, 130.0, 1.0), SR);
    let (f1, f2) = (median(&m.f1), median(&m.f2));
    assert!((220.0..=500.0).contains(&f1), "F1 median {f1}");
    assert!((2020.0..=2580.0).contains(&f2), "F2 median {f2}");
}

#[test]
fn lpc_formant_tracks_are_stable_on_a_sustained_vowel() {
    let m = metrics(&vowel(700.0, 1200.0, 2600.0, 130.0, 1.0), SR);
    let vals: Vec<f64> =
        m.f1.iter()
            .filter(|x| !x.is_nan())
            .map(|&x| f64::from(x))
            .collect();
    assert!(vals.len() > 10);
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let sd = (vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / vals.len() as f64).sqrt();
    assert!(sd < 60.0, "F1 stddev={sd} Hz (should be steady)");
}

#[test]
fn f0_tracks_the_fundamental_of_a_synthetic_vowel() {
    let m = metrics(&vowel_at(700.0, 1200.0, 2600.0, 196.0), SR);
    let vals = finite_sorted(&m.f0);
    assert!(vals.len() > 10);
    let med = vals[vals.len() / 2];
    assert!((186.0..=206.0).contains(&med), "F0 median={med} Hz");
}

#[test]
fn f0_stays_accurate_for_a_high_note() {
    // E5 (~660 Hz): with YIN on the coarse 11 kHz formant decimation, the lag
    // spacing here is ~1 semitone and prone to octave/quantization error. The
    // ~22 kHz YIN path must still land on the root.
    let m = metrics(&vowel_at(700.0, 1200.0, 2600.0, 660.0), SR);
    let vals = finite_sorted(&m.f0);
    assert!(vals.len() > 10);
    let med = vals[vals.len() / 2];
    assert!(
        (630.0..=690.0).contains(&med),
        "F0 median={med} Hz (E5≈660)"
    );
}

#[test]
fn vibrato_jitter_is_finite_and_low_for_an_even_vibrato() {
    let m = metrics(&vibrato_tone(440.0, 5.5, 60.0, 1.6, SR), SR);
    let v = analyze_vibrato(&m, 0, m.frames as i64 - 1, VibratoOptions::default());
    assert!(v.present, "should detect the 5.5 Hz vibrato");
    assert_close(v.rate_hz, 5.5, 1.0, "rate");
    assert!(!v.jitter.is_nan());
    assert!(v.jitter < 0.15, "even-vibrato jitter={}", v.jitter);
}

#[test]
fn loudness_tracks_signal_amplitude() {
    // dB drops ~20 for a 0.1x signal.
    let base = tones(&[440.0], 1.0);
    let loud = metrics(&base, SR);
    let quiet_samples: Vec<f32> = base.iter().map(|&s| (f64::from(s) * 0.1) as f32).collect();
    let quiet = metrics(&quiet_samples, SR);
    let peak = |x: &[f32]| x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    assert!(
        peak(&loud.loudness) > peak(&quiet.loudness) + 10.0,
        "loud peak={} quiet={}",
        peak(&loud.loudness),
        peak(&quiet.loudness)
    );
}

#[test]
fn max_analysis_sample_rate_pins_the_yin_envelope() {
    // Voiced at the ceiling, structurally unvoiced past the dead bound. The
    // YIN slice spans fftSize / sampleRate seconds while the fmin search lag
    // is a fixed 1/63 s, so past ~129 kHz yin_pitch bails out unvoiced for
    // EVERY frame regardless of content — the reason the admission gate
    // refuses rates above MAX_ANALYSIS_SAMPLE_RATE.
    let tone = |sr: i64| -> Vec<f32> {
        (0..sr as usize)
            .map(|i| (0.6 * (2.0 * PI * 220.0 * i as f64 / sr as f64).sin()) as f32)
            .collect()
    };
    let voiced = |m: &VoiceMetrics| m.f0.iter().filter(|f| !f.is_nan()).count();
    let at_ceiling = metrics(&tone(MAX_ANALYSIS_SAMPLE_RATE), MAX_ANALYSIS_SAMPLE_RATE);
    assert!(
        voiced(&at_ceiling) > 0,
        "the admitted envelope must actually be analyzable"
    );
    let dead = metrics(&tone(192000), 192000);
    assert_eq!(
        voiced(&dead),
        0,
        "192 kHz is past the structural dead bound — what the gate refuses"
    );
}

#[test]
fn cpp_is_higher_for_a_periodic_voice_than_for_noise() {
    let voice = metrics(&vowel(700.0, 1200.0, 2600.0, 130.0, 1.0), SR);
    let noise = metrics(&noise(1.0, 7), SR);
    assert!(!voice.cpp_avg.is_nan());
    // Noise voices no frame, so it has no CPP average at all ("—"), which is
    // the sharpest form of "lower". Per frame, the voice still beats it.
    assert!(noise.cpp_avg.is_nan());
    assert!(median(&voice.cpp) > median(&noise.cpp));
}

#[test]
fn a_take_with_no_voiced_frame_reports_its_timbre_averages_as_no_data() {
    // Breath, a whispered exercise or a muted mic: there is no voiced frame
    // to average. NaN is what every readout renders as "—".
    for (what, m) in [
        ("noise", metrics(&noise(1.0, 7), SR)),
        ("silence", metrics(&vec![0.0; SR as usize], SR)),
        ("empty", metrics(&[], SR)),
    ] {
        assert!(m.f0.iter().all(|v| !v.is_finite()), "{what}");
        assert!(m.ring_avg.is_nan(), "{what} ringAvg");
        assert!(m.tilt_avg.is_nan(), "{what} tiltAvg");
        assert!(m.cpp_avg.is_nan(), "{what} cppAvg");
    }
}

#[test]
fn ring_index_rises_when_there_is_strong_2_4_khz_energy() {
    let bright = metrics(&tones(&[400.0, 3000.0], 1.0), SR);
    let dark = metrics(&tones(&[400.0, 700.0], 1.0), SR);
    assert!(bright.ring_avg > dark.ring_avg);
}

#[test]
fn spectral_tilt_is_higher_for_more_high_frequency_energy() {
    let bright = metrics(&tones(&[400.0, 3000.0, 4000.0], 1.0), SR);
    let dark = metrics(&tones(&[300.0, 500.0], 1.0), SR);
    assert!(bright.tilt_avg > dark.tilt_avg);
}

#[test]
fn truly_empty_input_yields_zero_frames_without_panicking() {
    let m = metrics(&[], SR);
    assert_eq!(m.frames, 0);
    assert!(m.ring_avg.is_nan()); // nothing to average: no data, not 0 dB
}

#[test]
fn a_sub_fft_size_take_still_yields_a_zero_padded_frame() {
    let m = metrics(&vec![0.0; 1000], SR); // < 2048 samples
    assert!(m.frames >= 1);
    assert_eq!(m.f1.len(), m.frames);
}

#[test]
fn the_voicing_gate_survives_median_smoothing() {
    // No F0/formants painted onto a mid-take silence gap.
    let (fft, hop) = (2048usize, 512usize);
    let v = vowel(700.0, 1200.0, 2600.0, 200.0, 0.7);
    let gap_len = (SR as f64 * 0.35).round() as usize;
    let n = v.len() * 2 + gap_len;
    let mut samples = vec![0.0f32; n];
    samples[..v.len()].copy_from_slice(&v);
    samples[v.len() + gap_len..].copy_from_slice(&v);
    let m = metrics(&samples, SR);

    // Frames whose analysis window lies FULLY inside the gap are pure
    // silence: the gate must keep them NaN even though the ±2-frame median
    // bridges NaN dropouts on voiced frames (the voicing mask is re-applied
    // after smoothing).
    let first_full = v.len().div_ceil(hop);
    let last_full = (v.len() + gap_len - fft) / hop;
    assert!(
        last_full - first_full > 10,
        "the gap should span many frames"
    );
    for fr in first_full..=last_full {
        assert!(m.f0[fr].is_nan(), "f0 painted onto silence @{fr}");
        assert!(m.f1[fr].is_nan(), "f1 painted onto silence @{fr}");
        assert!(m.h1h2[fr].is_nan(), "h1h2 painted onto silence @{fr}");
    }
    // Sanity: the voiced regions themselves do carry pitch.
    assert!(m.f0[10..40].iter().any(|v| !v.is_nan()));
}

#[test]
fn median_smoothing_bridges_measurement_dropouts_on_voiced_frames() {
    // h1h2 holes from 2·F0 crossing Nyquist are filled. A pure FM sine around
    // 1000 Hz at sr 4000: every frame is loud and periodic (voiced) and F0
    // stays in-band, but H1−H2 is only measurable while 2·F0 < Nyquist. The
    // slow FM therefore punches deterministic few-frame NaN holes into the
    // raw h1h2 track on VOICED frames — exactly the measurement dropouts the
    // ±2-frame median must bridge.
    let sr = 4000i64;
    let n = sr as usize * 2;
    let mut phase = 0.0f64;
    let samples: Vec<f32> = (0..n)
        .map(|i| {
            let hz = 1000.0 + 60.0 * (2.0 * PI * 5.0 * i as f64 / sr as f64).sin();
            phase += 2.0 * PI * hz / sr as f64;
            (0.7 * phase.sin()) as f32
        })
        .collect();
    let m = compute_voice_metrics(
        &samples,
        sr,
        AnalysisParams {
            fft_size: 512,
            hop: 128,
            ..AnalysisParams::default()
        },
    );

    // Interior frames only (the zero-padded edge windows may gate unvoiced).
    let (mut voiced, mut h1h2_finite) = (0usize, 0usize);
    for fr in 3..m.frames - 3 {
        if !m.f0[fr].is_nan() {
            voiced += 1;
        }
        if !m.h1h2[fr].is_nan() {
            h1h2_finite += 1;
        }
    }
    assert!(
        voiced as f64 > ((m.frames - 6) as f64 * 0.9).round(),
        "the tone should be voiced throughout"
    );
    // Without bridging, only the below-1000 Hz half of each FM cycle carries
    // h1h2 (~50% of voiced frames); with dropout bridging nearly all do.
    assert!(h1h2_finite as f64 > (voiced as f64 * 0.8).round());
}

#[test]
fn frame_count_matches_the_spectrogram_framing() {
    // Any non-empty take: the formant overlay and per-frame playhead read
    // must line up with the spectrogram columns.
    for len in [1000, 2048, 3000, 22050, 44100, 50000, 100000] {
        let samples = vec![0.0f32; len];
        let m = metrics(&samples, SR);
        let s = compute_spectrogram(&samples, SR, AnalysisParams::default());
        assert_eq!(m.frames, s.frames, "len={len}");
    }
}

#[test]
fn smoothed_formants_stay_ordered_across_a_vowel_change() {
    // /E/ (600/1800/2500) into a front rounded /y/ (300/1900/2200), at E4.
    let a = sung_vowel(330.0, &[600.0, 1800.0, 2500.0], 0.25, 0.3);
    let b = sung_vowel(330.0, &[300.0, 1900.0, 2200.0], 0.25, 0.3);
    let both: Vec<f32> = a.iter().chain(&b).copied().collect();
    let m = metrics(&both, SR);
    let mut measured = 0;
    for fr in 0..m.frames {
        let (f1, f2, f3) = (m.f1[fr], m.f2[fr], m.f3[fr]);
        if f2.is_finite() && f3.is_finite() {
            measured += 1;
            assert!(f2 < f3, "frame {fr}: F2={f2} F3={f3}");
        }
        if f1.is_finite() && f2.is_finite() {
            assert!(f1 < f2, "frame {fr}: F1={f1} F2={f2}");
        }
    }
    assert!(measured > 10, "the vowels must be measured");
}

#[test]
fn ordered_formants_sorts_the_measured_ones_and_keeps_the_gaps() {
    assert_eq!(
        ordered_formants(600.0, 1900.0, 1800.0),
        (600.0, 1800.0, 1900.0)
    );
    assert_eq!(
        ordered_formants(2500.0, 700.0, 1200.0),
        (700.0, 1200.0, 2500.0)
    );
    let (a, b, c) = ordered_formants(f64::NAN, 2000.0, 1900.0);
    assert!(a.is_nan());
    assert_eq!((b, c), (1900.0, 2000.0));
    let (d, e, f) = ordered_formants(1000.0, f64::NAN, 900.0);
    assert_eq!((d, f), (900.0, 1000.0));
    assert!(e.is_nan());
}

#[test]
fn a_dc_offset_leaves_the_singers_formant_reading_alone() {
    // Soft singing (-30 dBFS peak) on an interface with a -40 dBFS DC offset:
    // read from bin 0, the offset was taken for the voice's low peak.
    let clean = sung_vowel(220.0, &[600.0, 1100.0, 3000.0], 2.0, 0.03);
    let offset: Vec<f32> = clean
        .iter()
        .map(|&v| (f64::from(v) + 0.01) as f32)
        .collect();
    let sf = |s: &[f32]| {
        compute_ltas(
            &compute_spectrogram(s, SR, AnalysisParams::default()),
            Some(&metrics(s, SR)),
        )
        .singers_formant_db
    };
    assert!((sf(&clean) - sf(&offset)).abs() < 1.0);
}

#[test]
fn clipping_reads_a_held_rail_not_a_loud_low_note_cresting_above_it() {
    // The engine records float after its input gain: a clean low note can
    // crest above 1.0, its peak samples a few 1e-5 apart, and it read as
    // clipped ("lower the mic gain") though nothing clipped.
    let tone = |hz: f64, amp: f64, rail: Option<f64>| -> Vec<f32> {
        (0..SR as usize * 2)
            .map(|i| {
                let v = amp * (2.0 * PI * hz * i as f64 / SR as f64).sin();
                (match rail {
                    None => v,
                    Some(r) => v.clamp(-r, r),
                }) as f32
            })
            .collect()
    };
    for hz in [73.4, 82.4, 110.0] {
        let m = metrics(&tone(hz, 2.0, None), SR);
        assert_eq!(m.clipped_fraction, 0.0, "{hz} Hz cresting at 2.0");
        assert!(!m.is_clipped());
    }
    // A file clipped where it was made holds the rail exactly.
    let clipped = metrics(&tone(220.0, 2.0, Some(1.0)), SR);
    assert!(clipped.is_clipped());
    assert!(clipped.clipped_fraction > 0.5);
}
