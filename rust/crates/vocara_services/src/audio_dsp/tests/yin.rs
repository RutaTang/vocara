//! YIN: voice_metrics_test.dart's pitch tests and yin_octave_test.dart.

use std::f64::consts::{LN_2, PI};

use vocara_model::models::voice_metrics::{analyze_voice_profile, VoiceProfileOptions};

use super::super::yin::yin_difference;
use super::super::{compute_voice_metrics, yin_pitch, yin_pitch_with, AnalysisParams, YinParams};
use super::support::{
    assert_bits_eq64, assert_close, breathy_vowel, h2_bright, h2_dominant, harmonic, median, noise,
    sine, tones, DartRandom, SR,
};

fn band(fmin: f64, fmax: f64) -> YinParams {
    YinParams {
        fmin,
        fmax,
        ..YinParams::default()
    }
}

#[test]
fn yin_pitch_tracks_a_pure_tone_with_high_periodicity() {
    let sine = tones(&[220.0], 1.0);
    let y = yin_pitch(&sine[..2048], SR as f64);
    assert_close(y.f0, 220.0, 3.0, "f0");
    assert!(y.periodicity > 0.9);
}

#[test]
fn yin_pitch_is_octave_robust() {
    // 150 Hz with a 2nd harmonic LOUDER than the fundamental — the classic
    // case where autocorrelation/cepstrum report the octave (300 Hz). YIN
    // should not.
    let f0 = 150.0;
    let sr = SR as f64;
    let frame: Vec<f32> = (0..2048)
        .map(|i| {
            let i = i as f64;
            (0.6 * (2.0 * PI * f0 * i / sr).sin() + 1.0 * (2.0 * PI * 2.0 * f0 * i / sr).sin())
                as f32
        })
        .collect();
    let y = yin_pitch(&frame, sr);
    assert_close(y.f0, f0, 6.0, "should lock the root, not the octave");
}

#[test]
fn yin_pitch_returns_nan_not_a_panic_on_tiny_frames() {
    for len in 0..4 {
        let y = yin_pitch(&vec![0.0; len], 11025.0);
        assert!(y.f0.is_nan(), "len={len} should be NaN");
    }
}

#[test]
fn yin_difference_matches_the_one_lag_loop_bit_for_bit() {
    // 3000 frames, every lag-count remainder. YIN step 1 as it was: one
    // accumulator per lag, summed in j order.
    fn one_lag(x: &[f32], tau_max: usize, w: usize) -> Vec<f64> {
        let mut d = vec![0.0; tau_max + 1];
        for (tau, d) in d.iter_mut().enumerate().skip(1) {
            let mut sum = 0.0;
            for j in 0..w {
                let diff = f64::from(x[j]) - f64::from(x[j + tau]);
                sum += diff * diff;
            }
            *d = sum;
        }
        d
    }

    let mut rng = DartRandom::new(11);
    for t in 0..3000usize {
        let n = [1024usize, 2048, 1000, 517, 9][t % 5];
        let rate = [24000.0, 22050.0, 48000.0, 44100.0][t % 4];
        let hz = 60.0 + rng.next_double() * 1200.0;
        let noise = (t % 3) as f64 * 0.2;
        let frame: Vec<f32> = (0..n)
            .map(|i| {
                (0.5 * (2.0 * PI * hz * i as f64 / rate).sin() + noise * (rng.next_double() - 0.5))
                    as f32
            })
            .collect();
        // yin_pitch's own bounds, plus arbitrary ones so every tauMax % 8
        // (the eight-lag sweep's tail) is exercised.
        let tau_max = if t % 2 == 0 {
            ((rate / 63.0).ceil() as usize).min(n - 1)
        } else {
            1 + rng.next_int(n as u32 - 2) as usize
        };
        let w = n - tau_max;
        assert_bits_eq64(
            &yin_difference(&frame, tau_max, w),
            &one_lag(&frame, tau_max, w),
            &format!("frame {t} (n={n}, tauMax={tau_max})"),
        );
    }
}

#[test]
fn yin_pitch_rejects_a_sub_fmin_tone_instead_of_reporting_a_false_fmin() {
    // 60 Hz is below fmin (70 Hz). The boundary lag used to read a false
    // ~70 Hz with high confidence and lock the tuner a semitone sharp; now
    // it's NaN.
    let y = yin_pitch_with(&sine(60.0, 11025.0, 512), 11025.0, band(70.0, 1100.0));
    assert!(y.f0.is_nan(), "got {} Hz (should be out-of-band)", y.f0);
}

#[test]
fn yin_pitch_resolves_a_tone_at_the_very_top_of_the_range() {
    let y = yin_pitch_with(&sine(1100.0, 11025.0, 512), 11025.0, band(70.0, 1100.0));
    assert_close(y.f0, 1100.0, 20.0, "top-of-range tone must not drop");
}

#[test]
fn yin_pitch_reads_an_above_fmax_tone_as_nan_not_one_octave_low() {
    // A period shorter than the search band's first lag has its first d′ dip
    // at 2T, which the range gate accepted as f0/2 — every whistle-register
    // note above ~1.36 kHz read an octave low with full confidence.
    for hz in [1397.0, 1568.0, 1760.0, 2093.0] {
        let y = yin_pitch(&sine(hz, SR as f64, 2048), SR as f64);
        assert!(y.f0.is_nan(), "{hz} Hz read as {} Hz", y.f0);
    }
    // The batch pass agrees: no frame of a held high note carries a pitch.
    let m = compute_voice_metrics(&tones(&[1760.0], 1.0), SR, AnalysisParams::default());
    assert!(m.f0.iter().all(|v| v.is_nan()));
}

#[test]
fn yin_pitch_reads_a_high_note_whose_2nd_harmonic_dominates_at_its_own_pitch() {
    // H2 16-20 dB over H1/H3 repeats almost exactly at T/2, below the band
    // for any note above ~605 Hz: the scan's first dip sat there and the
    // range gate dropped a real, in-band note. 22.05/24 kHz are the
    // decimated YIN rates the take analysis runs at for 44.1/48 kHz input.
    for sr in [22050.0, 24000.0, 44100.0, 48000.0] {
        let n = if sr < 30000.0 { 1024 } else { 2048 };
        for hz in [620.0, 700.0, 800.0, 950.0, 1050.0] {
            for db in [16.0, 20.0] {
                let y = yin_pitch(&h2_bright(hz, sr, n, db), sr);
                assert_close(
                    y.f0,
                    hz,
                    hz * 0.01,
                    &format!("{hz} Hz, H2 +{db} dB at {sr} Hz"),
                );
                assert!(y.periodicity > 0.9);
            }
        }
    }
    // End to end: a held bright 700 Hz note carries its pitch in the take.
    let note = h2_bright(700.0, SR as f64, SR as usize, 16.0);
    let m = compute_voice_metrics(&note, SR, AnalysisParams::default());
    let voiced: Vec<f32> = m.f0.iter().copied().filter(|v| !v.is_nan()).collect();
    assert!(!voiced.is_empty());
    assert_close(median(&voiced), 700.0, 7.0, "median F0");
}

#[test]
fn an_above_band_tone_still_reads_nan_bright_timbres_included() {
    // The rescue above must never turn an above-band tone into its
    // octave-low double: integer-lag misalignment can leave a bright tone's
    // 2T dip deeper than its T dip, and only the absent fundamental gives it
    // away.
    for sr in [22050.0, 24000.0, 44100.0] {
        let n = if sr < 30000.0 { 1024 } else { 2048 };
        let mut hz = 1250.0;
        while hz < sr / 4.0 {
            for rolloff in [0.0, 1.0, 2.0] {
                let harmonics = if rolloff == 0.0 { 1 } else { 6 };
                let y = yin_pitch(&harmonic(hz, sr, n, harmonics, rolloff, None), sr);
                assert!(
                    y.f0.is_nan(),
                    "{hz} Hz (1/h^{rolloff}) at {sr} Hz read as {} Hz",
                    y.f0
                );
            }
            hz *= 1.005;
        }
    }
}

#[test]
fn yin_pitch_periodicity_is_far_lower_for_noise_than_for_a_tone() {
    let tone = yin_pitch(&tones(&[220.0], 1.0)[..2048], SR as f64);
    let noise = yin_pitch(&noise(1.0, 7)[..2048], SR as f64);
    assert!(tone.periodicity > 0.9);
    assert!(
        noise.periodicity < tone.periodicity - 0.4,
        "noise periodicity={} tone={}",
        noise.periodicity,
        tone.periodicity
    );
}

#[test]
fn a_breathy_a3_keeps_its_octave_and_its_range() {
    // yin_octave_test: no dip falls under YIN's threshold, but the frame
    // still passes the voicing gate; the fallback took the deepest dip, and
    // with noise the dips at T, 2T and 3T are nearly equal — a third of the
    // frames read an octave or a twelfth low, and the profile put a steady
    // A3's range low end at D2.
    let x = breathy_vowel(48000, 8.0, true, 2);
    let m = compute_voice_metrics(&x, 48000, AnalysisParams::default());
    let (mut voiced, mut wrong_octave) = (0usize, 0usize);
    for &f in &m.f0 {
        if f.is_nan() {
            continue;
        }
        voiced += 1;
        if (1200.0 * (f64::from(f) / 220.0).ln() / LN_2).abs() > 600.0 {
            wrong_octave += 1;
        }
    }
    assert!(voiced > 100);
    assert!(
        wrong_octave < voiced / 20,
        "{wrong_octave} of {voiced} voiced frames an octave or more off"
    );
    let low = analyze_voice_profile(&m, VoiceProfileOptions::default()).low_midi;
    assert!(low.is_some_and(|l| l >= 55), "range low end {low:?}");
}

#[test]
fn a_note_with_its_2nd_harmonic_dominant_reads_its_own_pitch_below_567_hz_too() {
    // yin_octave_test: the double-period rescue ran only for a first dip
    // above the band, so an E4-C5 belt with F1 on H2 read an octave high:
    // the same timbre was right at 620 Hz and exactly doubled at 523 Hz.
    let mut bad = Vec::new();
    for sr in [22050.0, 24000.0] {
        for hz in [330.0, 392.0, 440.0, 494.0, 523.0, 620.0, 700.0] {
            for db in [12.0, 16.0, 20.0] {
                let y = yin_pitch(&h2_dominant(hz, sr, 1024, db), sr);
                if !(y.f0 > hz * 0.99 && y.f0 < hz * 1.01) {
                    bad.push(format!("{hz} Hz, H2 +{db} dB at {sr}: {:.1}", y.f0));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
