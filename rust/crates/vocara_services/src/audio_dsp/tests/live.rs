//! The live path: voice_metrics_test.dart's LiveVoiceAnalyzer, StreamingStft,
//! TunerTracker and decimatePair tests.

use std::f64::consts::{PI, SQRT_2};

use super::super::filter::decimate_pair;
use super::super::{LiveFrame, LiveVoiceAnalyzer, StreamingStft, TunerOptions, TunerTracker};
use super::support::{assert_bits_eq, assert_close, noise, sort_dart, vowel_at, DartRandom};

/// [samples] fed in ~1000-sample packets, like a mic stream.
fn live_frames(samples: &[f32]) -> Vec<LiveFrame> {
    let mut analyzer = LiveVoiceAnalyzer::default();
    let mut frames = Vec::new();
    for chunk in samples.chunks(1000) {
        analyzer.add_samples(chunk, |f| frames.push(f));
    }
    frames
}

#[test]
fn live_voice_analyzer_emits_correct_metrics_incrementally() {
    let frames = live_frames(&vowel_at(700.0, 1200.0, 2600.0, 200.0));
    assert!(frames.len() > 10);
    let mut f1s: Vec<f64> = frames
        .iter()
        .map(|f| f.f1)
        .filter(|v| !v.is_nan())
        .collect();
    let mut f0s: Vec<f64> = frames
        .iter()
        .map(|f| f.f0)
        .filter(|v| !v.is_nan())
        .collect();
    sort_dart(&mut f1s);
    sort_dart(&mut f0s);
    assert!(!f1s.is_empty());
    assert!(!f0s.is_empty());
    let f1 = f1s[f1s.len() / 2];
    let f0 = f0s[f0s.len() / 2];
    assert!((560.0..=860.0).contains(&f1), "live F1 median {f1}");
    assert!((186.0..=214.0).contains(&f0), "live F0 median {f0}");
}

#[test]
fn live_voice_analyzer_draws_no_f0_or_formant_dots_on_loud_noise() {
    // Loud white noise: well above the RMS floor (so a loudness-only gate
    // would have drawn formant dots all over it) but aperiodic, so the YIN
    // periodicity gate must reject it.
    let frames = live_frames(&noise(1.0, 7));
    assert!(frames.len() > 10);
    let voiced = frames.iter().filter(|f| f.active).count();
    assert!(
        (voiced as f64 / frames.len() as f64) < 0.15,
        "aperiodic noise should almost never read as voiced"
    );
    let dotted = frames
        .iter()
        .filter(|f| !f.f1.is_nan() || !f.f0.is_nan())
        .count();
    assert!(
        (dotted as f64 / frames.len() as f64) < 0.15,
        "no pitch/formant dots on noise"
    );
}

#[test]
fn live_voice_analyzer_delays_dots_until_a_note_is_sustained() {
    // The overlay sustain gate suppresses the first few voiced frames so
    // brief voiced blips never draw dots; a held note still shows F0 once
    // sustained.
    let frames = live_frames(&vowel_at(700.0, 1200.0, 2600.0, 200.0));
    let first_dot = frames.iter().position(|f| !f.f0.is_nan());
    assert!(
        first_dot.is_some_and(|i| i > 0),
        "onset frames must be suppressed, and a held note still shows F0"
    );
}

#[test]
fn the_live_analyzers_do_not_depend_on_chunking_and_reset_starts_afresh() {
    // A vowel between silences, so frames cross the voicing and sustain
    // gates both ways.
    let v = vowel_at(700.0, 1200.0, 2600.0, 200.0);
    let mut samples = vec![0.0f32; v.len() + 20000];
    samples[9000..9000 + v.len()].copy_from_slice(&v);
    let n = samples.len();
    // Every LiveFrame field and every StreamingStft column, as bits.
    let run = |live: &mut LiveVoiceAnalyzer,
               stft: &mut StreamingStft,
               chunks: &[usize],
               stop_at: Option<usize>|
     -> Vec<u64> {
        let (mut out, mut cols) = (Vec::<f64>::new(), Vec::<f64>::new());
        let (mut at, mut k) = (0usize, 0usize);
        while at < stop_at.unwrap_or(n) {
            let chunk = &samples[at..n.min(at + chunks[k % chunks.len()])];
            k += 1;
            live.add_samples(chunk, |f| {
                out.extend(f.column.iter().map(|&v| f64::from(v)));
                out.extend([f.ring, f.tilt, f.cpp, f.f0, f.f1, f.f2, f.f3]);
                out.extend([
                    f.f0_confidence,
                    f.h1h2,
                    f.loudness,
                    if f.active { 1.0 } else { 0.0 },
                ]);
            });
            stft.add_samples(chunk, |c| cols.extend(c.iter().map(|&v| f64::from(v))));
            at += chunk.len();
        }
        out.iter().chain(&cols).map(|v| v.to_bits()).collect()
    };

    let whole = run(
        &mut LiveVoiceAnalyzer::default(),
        &mut StreamingStft::default(),
        &[n],
        None,
    );
    assert!(whole.len() > 1000);
    for chunks in [
        vec![1],
        vec![480],
        vec![1440],
        vec![1, 511, 2049, 0, 7, 9000],
    ] {
        let got = run(
            &mut LiveVoiceAnalyzer::default(),
            &mut StreamingStft::default(),
            &chunks,
            None,
        );
        assert!(got == whole, "chunks {chunks:?}");
    }
    // A session cut off mid-vowel, then reset: the next one is a fresh one.
    let mut live = LiveVoiceAnalyzer::default();
    let mut stft = StreamingStft::default();
    run(&mut live, &mut stft, &[1000], Some(30000));
    live.reset();
    stft.reset();
    assert!(
        run(&mut live, &mut stft, &[1440], None) == whole,
        "after reset"
    );
}

#[test]
fn live_voice_analyzer_gates_out_quiet_room_noise_level_frames() {
    // A vowel attenuated to ~-66 dBFS — below the VAD floor.
    let quiet: Vec<f32> = vowel_at(700.0, 1200.0, 2600.0, 200.0)
        .iter()
        .map(|&s| (f64::from(s) * 0.002) as f32)
        .collect();
    let frames = live_frames(&quiet);
    assert!(
        frames.iter().all(|f| f.f0.is_nan()),
        "noise-level input should not register as voiced pitch"
    );
}

#[test]
fn live_voice_analyzer_reports_its_geometry() {
    let a = LiveVoiceAnalyzer::new(48000, Default::default());
    assert_eq!(a.bins(), 215); // ceil(5000·2048/48000) + 1
    assert_eq!(a.bin_hz(), 48000.0 / 2048.0);
    assert_eq!(a.frame_rate(), 48000.0 / 512.0);
    assert_eq!(a.max_hz(), 5000.0);
}

#[test]
fn tuner_tracker_locks_a_sustained_note_after_the_attack_and_releases_silence() {
    let mut t = TunerTracker::default();
    assert!(t.add(f64::NAN, 0.0).is_nan()); // silence → nothing
    let mut out = f64::NAN;
    for _ in 0..10 {
        out = t.add(220.0, 1.0); // a sustained, clearly-voiced 220 Hz note
    }
    assert_close(out, 220.0, 5.0, "locked pitch");
    for _ in 0..10 {
        out = t.add(f64::NAN, 0.0); // go silent
    }
    assert!(out.is_nan()); // released
}

#[test]
fn tuner_tracker_ignores_a_brief_blip_shorter_than_the_attack() {
    let mut t = TunerTracker::new(TunerOptions {
        attack: 4,
        ..TunerOptions::default()
    });
    assert!(t.add(220.0, 1.0).is_nan());
    assert!(t.add(220.0, 1.0).is_nan());
    assert!(t.add(f64::NAN, 0.0).is_nan()); // blip ended, never locked
}

#[test]
fn tuner_tracker_rejects_low_confidence_frames() {
    let mut t = TunerTracker::new(TunerOptions {
        min_confidence: 0.4,
        ..TunerOptions::default()
    });
    let mut out = 1.0;
    for _ in 0..10 {
        out = t.add(220.0, 0.2); // below the periodicity threshold
    }
    assert!(out.is_nan());
}

#[test]
fn decimate_pair_is_two_one_filter_decimations_bit_for_bit() {
    // The live analyzer's decimation, as defined: three cascaded RBJ
    // Butterworth low-passes at 0.45·fs/factor, Direct Form I in double,
    // each stage's output stored as f32 and read back by the next, then
    // every factor-th sample.
    fn decimate(x: &[f32], factor: usize, fs: f64) -> Vec<f32> {
        let w0 = 2.0 * PI * (0.45 * fs / factor as f64) / fs;
        let (cosw, sinw) = (w0.cos(), w0.sin());
        let alpha = sinw / SQRT_2;
        let a0 = 1.0 + alpha;
        let (b0, b1) = ((1.0 - cosw) / 2.0 / a0, (1.0 - cosw) / a0);
        let b2 = b0;
        let (a1, a2) = (-2.0 * cosw / a0, (1.0 - alpha) / a0);
        let mut lp = x.to_vec();
        for _ in 0..3 {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for v in &mut lp {
                let xi = f64::from(*v);
                let yi = b0 * xi + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
                *v = yi as f32;
                x2 = x1;
                x1 = xi;
                y2 = y1;
                y1 = yi;
            }
        }
        (0..lp.len() / factor).map(|i| lp[i * factor]).collect()
    }

    let mut rng = DartRandom::new(31);
    for (fs, f1, f2) in [
        (48000.0, 4, 2), // the app's
        (44100.0, 4, 2),
        (96000.0, 8, 2),
        (33075.0, 3, 2),
        (24000.0, 2, 2),
    ] {
        for n in [0, 1, 7, 2048, 4097] {
            // Signed zeros, denormal-scale and full-scale values, clipping.
            let x: Vec<f32> = (0..n)
                .map(|i| match rng.next_int(10) {
                    0 => 0.0,
                    1 => -0.0,
                    2 => (1e-40 * (rng.next_double() - 0.5)) as f32,
                    3 => {
                        if rng.next_bool() {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    _ => (0.5 * (i as f64 / 3.7).sin() + 0.3 * (rng.next_double() - 0.5)) as f32,
                })
                .collect();
            let (a, b) = decimate_pair(&x, f1, f2, fs);
            let why = format!("fs={fs} f={f1}/{f2} n={n}");
            assert_bits_eq(&a, &decimate(&x, f1, fs), &format!("{why} first"));
            assert_bits_eq(&b, &decimate(&x, f2, fs), &format!("{why} second"));
        }
    }
}
