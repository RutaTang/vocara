//! Ports of `client/test/voice_analysis_test.dart`,
//! `client/test/voice_profile_test.dart`, the note-name tests of
//! `client/test/voice_metrics_test.dart` (its other tests exercise the DSP in
//! `audio_dsp.dart` and move with it), and the model-level "shared
//! frame↔time mapping" tests of `client/test/analysis_frame_timing_test.dart`
//! (on synthetic metrics of the production framing, where Dart ran the DSP).

use std::f64::consts::PI;
use std::sync::Arc;

use vocara_model::models::spectrogram::{
    merge_frame_of, merge_spectrograms, AnalysisFraming, Spectrogram, SpectrogramPart,
};
use vocara_model::models::voice_metrics::*;
use vocara_model::util::dart_num::{pow_f64, round_to_i64};

const SR: i64 = 44100;
const HOP: i64 = 512;
const FFT: i64 = 2048;
const BIN_HZ: f64 = SR as f64 / FFT as f64; // ≈ 21.5

fn vm(f0: Option<Vec<f32>>, h1h2: Option<Vec<f32>>, frames: usize) -> VoiceMetrics {
    let n = if frames > 0 {
        frames
    } else {
        f0.as_ref().or(h1h2.as_ref()).map_or(0, Vec::len)
    };
    let z = vec![0.0f32; n];
    let nan = vec![f32::NAN; n];
    VoiceMetrics {
        frames: n,
        hop: HOP,
        fft_size: 2048,
        sample_rate: SR,
        ring: z.clone(),
        tilt: z.clone(),
        cpp: z.clone(),
        loudness: z,
        f0: f0.unwrap_or_else(|| nan.clone()),
        f1: nan.clone(),
        f2: nan.clone(),
        f3: nan.clone(),
        h1h2: h1h2.unwrap_or(nan),
        active: vec![1; n],
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: f64::NAN,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    }
}

/// A spectrogram whose every frame has [db_of_bin] per bin.
fn spec(frames: usize, bins: usize, db_of_bin: impl Fn(usize) -> f64) -> Spectrogram {
    let mut db = vec![0.0f32; frames * bins];
    for f in 0..frames {
        for b in 0..bins {
            db[f * bins + b] = db_of_bin(b) as f32;
        }
    }
    Spectrogram {
        frames,
        bins,
        max_hz: 5000.0,
        sample_rate: SR,
        fft_size: FFT,
        hop: HOP,
        db,
        min_db: -90.0,
        max_db: 0.0,
    }
}

fn argmax(xs: &[f32]) -> usize {
    let mut bi = 0;
    for i in 1..xs.len() {
        if xs[i] > xs[bi] {
            bi = i;
        }
    }
    bi
}

// --- LTAS ---

const LTAS_BINS: usize = 233; // ~5000 Hz at 21.5 Hz/bin

#[test]
fn peaks_at_the_dominant_bin_alpha_ratio_bright_vs_dark() {
    let peak_3k = round_to_i64(3000.0 / BIN_HZ) as usize;
    let peak_300 = round_to_i64(300.0 / BIN_HZ) as usize;
    let m = vm(None, None, 10);
    let bright = compute_ltas(
        &spec(10, LTAS_BINS, |b| if b == peak_3k { 0.0 } else { -60.0 }),
        Some(&m),
    );
    let dark = compute_ltas(
        &spec(10, LTAS_BINS, |b| if b == peak_300 { 0.0 } else { -60.0 }),
        Some(&m),
    );

    assert_eq!(argmax(&bright.db), peak_3k);
    assert!(bright.alpha_ratio > 0.0); // energy mostly >1 kHz
    assert!(dark.alpha_ratio < 0.0); // energy mostly <1 kHz
                                     // Singer's-formant prominence: a 3 kHz peak reads far higher than 300 Hz.
    assert!(bright.singers_formant_db > dark.singers_formant_db + 30.0);
    assert!(bright.has_data());
}

#[test]
fn averages_in_power_a_loud_bin_dominates_the_mean() {
    // Half the frames have a +0 dB bin, half −60 dB at the same bin. The
    // power mean is dominated by the loud frames, so the LTAS sits well above
    // the −30 dB linear-dB midpoint.
    let peak = round_to_i64(3000.0 / BIN_HZ) as usize;
    let mut db = vec![-90.0f32; 10 * LTAS_BINS];
    for f in 0..10 {
        db[f * LTAS_BINS + peak] = if f < 5 { 0.0 } else { -60.0 };
    }
    let s = Spectrogram {
        frames: 10,
        bins: LTAS_BINS,
        max_hz: 5000.0,
        sample_rate: SR,
        fft_size: FFT,
        hop: HOP,
        db,
        min_db: -90.0,
        max_db: 0.0,
    };
    let ltas = compute_ltas(&s, Some(&vm(None, None, 10)));
    // mean power = (5·1 + 5·1e-6)/10 ≈ 0.5 → 10·log10(0.5) ≈ −3 dB ≫ −30.
    assert!(f64::from(ltas.db[peak]) > -6.0);
}

// --- Vibrato ---

const FRAME_RATE: f64 = SR as f64 / HOP as f64; // ≈ 86.13

fn vibrato_f0(rate: f64, extent_cents: f64, seconds: f64) -> Vec<f32> {
    let n = round_to_i64(FRAME_RATE * seconds) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / FRAME_RATE;
            let cents = extent_cents * (2.0 * PI * rate * t).sin();
            (220.0 * pow_f64(2.0, cents / 1200.0)) as f32
        })
        .collect()
}

fn in_range(v: f64, lo: f64, hi: f64) -> bool {
    v >= lo && v <= hi
}

#[test]
fn measures_a_6_hz_50_cent_vibrato() {
    let m = vm(Some(vibrato_f0(6.0, 50.0, 1.5)), None, 0);
    let v = analyze_vibrato(&m, 0, m.frames as i64 - 1, VibratoOptions::default());
    assert!(v.present);
    assert!(in_range(v.rate_hz, 5.0, 7.0));
    assert!(in_range(v.extent_cents, 40.0, 60.0));
    assert!(v.regularity > 0.7);
}

#[test]
fn a_straight_tone_has_no_vibrato() {
    let f0 = vec![220.0f32; round_to_i64(FRAME_RATE * 1.5) as usize];
    let n = f0.len() as i64;
    let v = analyze_vibrato(&vm(Some(f0), None, 0), 0, n - 1, VibratoOptions::default());
    assert!(!v.present);
}

#[test]
fn a_too_short_note_reports_nothing() {
    let v = analyze_vibrato(
        &vm(Some(vibrato_f0(6.0, 50.0, 0.2)), None, 0),
        0,
        100,
        VibratoOptions::default(),
    );
    assert!(!v.present);
}

#[test]
fn a_scoop_or_slide_is_not_reported_as_vibrato() {
    // Pitch sliding up 150¢ over 1.5 s, no oscillation.
    let n = round_to_i64(FRAME_RATE * 1.5) as usize;
    let f0: Vec<f32> = (0..n)
        .map(|i| (220.0 * pow_f64(2.0, 150.0 * (i as f64 / (n - 1) as f64) / 1200.0)) as f32)
        .collect();
    let v = analyze_vibrato(
        &vm(Some(f0), None, 0),
        0,
        n as i64 - 1,
        VibratoOptions::default(),
    );
    assert!(!v.present);
}

#[test]
fn vibrato_riding_on_a_drift_reports_the_true_extent() {
    let n = round_to_i64(FRAME_RATE * 1.6) as usize;
    let f0: Vec<f32> = (0..n)
        .map(|i| {
            let cents = 40.0 * (2.0 * PI * 6.0 * (i as f64 / FRAME_RATE)).sin()
                + 150.0 * (i as f64 / (n - 1) as f64);
            (220.0 * pow_f64(2.0, cents / 1200.0)) as f32
        })
        .collect();
    let v = analyze_vibrato(
        &vm(Some(f0), None, 0),
        0,
        n as i64 - 1,
        VibratoOptions::default(),
    );
    assert!(v.present);
    assert!(in_range(v.extent_cents, 30.0, 55.0)); // ~40, not inflated
    assert!(in_range(v.rate_hz, 5.0, 7.0));
}

#[test]
fn the_live_analyzer_measures_the_trailing_window() {
    // 3 s of straight tone then a 6 Hz ±50¢ vibrato: the trailing-window live
    // analyzer should lock onto the recent oscillation, not the silence/flat.
    let flat = vec![220.0f64; round_to_i64(FRAME_RATE * 3.0) as usize];
    let vib = vibrato_f0(6.0, 50.0, 2.0);
    let hist: Vec<f64> = flat
        .into_iter()
        .chain(vib.iter().map(|v| f64::from(*v)))
        .collect();
    let v = analyze_vibrato_live(&hist, FRAME_RATE, 1.6, VibratoOptions::default());
    assert!(v.present);
    assert!(in_range(v.rate_hz, 5.0, 7.0));
    assert!(in_range(v.extent_cents, 40.0, 60.0));
}

// --- RegisterMap ---

#[test]
fn classifies_chest_low_h1h2_vs_head_high_h1h2() {
    let n = 100;
    let f0 = vec![220.0f32; n];
    let h: Vec<f32> = (0..n).map(|i| if i < 50 { -5.0 } else { 12.0 }).collect(); // chest then head
    let r = analyze_register(&vm(Some(f0), Some(h), 0), RegisterOptions::default());
    assert!(r.has_data());
    assert!(r.chest_pct > 40.0);
    assert!(r.head_pct > 40.0);
    assert!(r.score[10] < 0.4); // chest end
    assert!(r.score[90] > 0.6); // head end
}

#[test]
fn no_voiced_h1h2_no_data() {
    let f0 = vec![220.0f32; 40];
    let r = analyze_register(&vm(Some(f0), None, 0), RegisterOptions::default()); // h1h2 all NaN
    assert!(!r.has_data());
}

// --- voice_profile_test.dart ---

/// A VoiceMetrics from synthetic per-frame curves (only the fields the
/// profile reads matter). active=1 where loudness is finite.
fn profile_metrics(f0: &[f64], h1h2: &[f64], loud: &[f64]) -> VoiceMetrics {
    let n = f0.len();
    let f32s = |v: &[f64]| v.iter().map(|x| *x as f32).collect::<Vec<_>>();
    VoiceMetrics {
        frames: n,
        hop: 512,
        fft_size: 2048,
        sample_rate: 44100,
        ring: vec![0.0; n],
        tilt: vec![0.0; n],
        cpp: vec![0.0; n],
        loudness: f32s(loud),
        f0: f32s(f0),
        f1: vec![f32::NAN; n],
        f2: vec![f32::NAN; n],
        f3: vec![f32::NAN; n],
        h1h2: f32s(h1h2),
        active: loud.iter().map(|l| u8::from(l.is_finite())).collect(),
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: f64::NAN,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    }
}

fn midi_to_hz(midi: f64) -> f64 {
    440.0 * pow_f64(2.0, (midi - 69.0) / 12.0)
}

#[test]
fn range_tessitura_passaggio_and_dynamics_on_a_c4_to_c5_glide() {
    let n = 240;
    let (mut f0, mut h, mut loud) = (vec![], vec![], vec![]);
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64; // 0..1
        let midi = 60.0 + 12.0 * t; // C4 → C5
        f0.push(midi_to_hz(midi));
        // Chest below E4 (midi 64), head above — a clean +8 dB H1−H2 step. Decide
        // on the *binned* midi so every semitone bin is internally consistent.
        h.push(if round_to_i64(midi) < 64 { -2.0 } else { 6.0 });
        loud.push(-30.0 + 20.0 * t); // −30 → −10 dBFS
    }
    let p = analyze_voice_profile(
        &profile_metrics(&f0, &h, &loud),
        VoiceProfileOptions::default(),
    );

    assert!(p.has_range());
    assert!((60..=62).contains(&p.low_midi.unwrap())); // ~C4
    assert!((70..=72).contains(&p.high_midi.unwrap())); // ~C5
    assert!((62..=64).contains(&p.tess_lo_midi.unwrap()));
    assert!((68..=70).contains(&p.tess_hi_midi.unwrap()));
    assert_eq!(
        p.passaggio_midi,
        Some(64),
        "register step sits at E4 (midi 64)"
    );
    assert!(p.passaggio_jump_db > 5.0);
    assert!(p.has_dynamics());
    assert!(p.dynamic_range_db() > 14.0);
}

#[test]
fn no_passaggio_on_a_flat_pitch() {
    let p = analyze_voice_profile(
        &profile_metrics(&[midi_to_hz(60.0); 120], &[0.0; 120], &[-20.0; 120]),
        VoiceProfileOptions::default(),
    );
    assert!(p.has_range());
    assert_eq!(p.low_midi, Some(60));
    assert_eq!(p.high_midi, Some(60));
    assert!(!p.has_passaggio()); // span < 6 semitones
}

#[test]
fn no_passaggio_when_the_timbre_is_uniform_across_the_range() {
    let n = 200;
    let f0: Vec<f64> = (0..n)
        .map(|i| midi_to_hz(55.0 + 12.0 * (i as f64 / (n - 1) as f64))) // G3 → G4, no timbre step
        .collect();
    let p = analyze_voice_profile(
        &profile_metrics(&f0, &vec![1.0; n], &vec![-18.0; n]),
        VoiceProfileOptions::default(),
    );
    assert!(p.has_range());
    assert!(!p.has_passaggio());
}

#[test]
fn passaggio_never_lands_outside_the_reported_range() {
    // 200 frames at C4 (chest) + a brief 5+5-frame excursion well above. The
    // outlier bins used to be picked as a passaggio above the whole range.
    let (mut f0, mut h, mut loud) = (vec![], vec![], vec![]);
    for _ in 0..200 {
        f0.push(midi_to_hz(60.0));
        h.push(-2.0);
        loud.push(-20.0);
    }
    for _ in 0..5 {
        f0.push(midi_to_hz(66.0));
        h.push(-2.0);
        loud.push(-20.0);
    }
    for _ in 0..5 {
        f0.push(midi_to_hz(67.0));
        h.push(6.0);
        loud.push(-20.0);
    }
    let p = analyze_voice_profile(
        &profile_metrics(&f0, &h, &loud),
        VoiceProfileOptions::default(),
    );
    assert_eq!(p.low_midi, Some(60));
    assert_eq!(p.high_midi, Some(60)); // 66/67 are <5% of frames → trimmed off the range
    assert!(!p.has_passaggio()); // no break above the range anymore
}

#[test]
fn a_single_frame_is_not_a_profile() {
    let p = analyze_voice_profile(
        &profile_metrics(&[midi_to_hz(60.0)], &[0.0], &[-20.0]),
        VoiceProfileOptions::default(),
    );
    assert!(!p.has_range());
    assert!(!p.has_dynamics());
}

#[test]
fn empty_or_unvoiced_input_yields_no_range_passaggio_or_dynamics() {
    let p = analyze_voice_profile(
        &profile_metrics(&[f64::NAN; 50], &[f64::NAN; 50], &[f64::NAN; 50]),
        VoiceProfileOptions::default(),
    );
    assert!(!p.has_range());
    assert!(!p.has_passaggio());
    assert!(!p.has_dynamics());
}

// --- voice_metrics_test.dart: note names ---

#[test]
fn note_label_maps_frequencies_to_note_names() {
    assert!(note_label(440.0).starts_with("A4"));
    assert!(note_label(261.63).starts_with("C4"));
    assert_eq!(note_label(f64::NAN), "—");
    assert_eq!(note_label(223.0), "A3 +23¢");
    assert_eq!(note_label(8.0), "C-1 −38¢");
    assert_eq!(note_name_for_midi(-13), "B-2");
    assert_eq!(note_label(435.0), "A4 −20¢");
    assert_eq!(note_name_for_midi(57), "A3");
    assert_eq!(note_name_for_midi(-1), "B-1");
}

#[test]
fn note_of_returns_the_nearest_note_name_and_signed_cents() {
    let a = note_of(440.0).unwrap();
    assert_eq!(a.name, "A4");
    assert!(a.cents.abs() < 1.0);
    let sharp = note_of(445.0).unwrap(); // ~+20¢ above A4
    assert_eq!(sharp.name, "A4");
    assert!(sharp.cents > 10.0);
    assert!(note_of(f64::NAN).is_none());
    assert!(note_of(0.0).is_none());
}

// --- analysis_frame_timing_test.dart: the shared frame↔time mapping ---

const T_SR: i64 = 48000;
const T_HOP: i64 = 512;
const T_FFT: i64 = 2048;

/// The window-centre time of frame [f] for the production framing, spelled
/// out rather than read back through the mapping under test.
fn centre_seconds(f: i64) -> f64 {
    (f * T_HOP) as f64 / T_SR as f64 + T_FFT as f64 / 2.0 / T_SR as f64
}

fn timing_metrics(frames: usize) -> VoiceMetrics {
    VoiceMetrics {
        hop: T_HOP,
        fft_size: T_FFT,
        sample_rate: T_SR,
        loudness: (0..frames).map(|i| i as f32).collect(),
        ..vm(None, None, frames)
    }
}

fn timing_spec(frames: usize, bins: usize) -> Spectrogram {
    Spectrogram {
        sample_rate: T_SR,
        hop: T_HOP,
        fft_size: T_FFT,
        db: (0..frames * bins).map(|i| i as f32).collect(),
        ..spec(frames, bins, |_| 0.0)
    }
}

#[test]
fn a_frame_sits_at_its_window_centre_not_its_start() {
    let m = timing_metrics(109);
    let s = timing_spec(109, 4);
    assert_eq!(m.fft_size, T_FFT);
    let mappings: [&dyn AnalysisFraming; 2] = [&m, &s];
    for a in mappings {
        assert!((a.time_of_frame(0) - centre_seconds(0)).abs() < 1e-12);
        assert!((a.time_of_frame(45) - centre_seconds(45)).abs() < 1e-12);
        // A frame's cell is hop-wide and centred on it.
        let half = T_HOP as f64 / 2.0 / T_SR as f64;
        assert!((a.cell_start(45) - (centre_seconds(45) - half)).abs() < 1e-12);
        assert!((a.cell_end(45) - (centre_seconds(45) + half)).abs() < 1e-12);
    }
}

#[test]
fn a_playhead_at_a_frame_centre_reads_that_frame() {
    let m = timing_metrics(109);
    let s = timing_spec(109, 4);
    let bins = s.bins;
    for f in 0..m.frames {
        let t = centre_seconds(f as i64);
        assert_eq!(m.frame_at_time(t), f);
        assert_eq!(s.frame_at_time(t), f);
        assert_eq!(m.sample_at(&m.loudness, t), f64::from(m.loudness[f]));
        assert_eq!(s.column_at_time(t), &s.db[f * bins..f * bins + bins]);
    }
    // Anywhere inside the cell still reads it; times off either end clamp.
    let q = 0.4 * T_HOP as f64 / T_SR as f64;
    assert_eq!(m.frame_at_time(centre_seconds(30) + q), 30);
    assert_eq!(m.frame_at_time(centre_seconds(30) - q), 30);
    assert_eq!(m.frame_at_time(0.0), 0);
    assert_eq!(m.frame_at_time(100.0), m.frames - 1);
}

#[test]
fn a_merged_lane_keeps_every_clip_frame_at_its_own_window_centre() {
    // The same take as a second clip 1.5 s into the lane: each of its frames
    // reads, on the lane axis, its clip offset plus its clip-local centre.
    let m = Arc::new(timing_metrics(109));
    let s = Arc::new(timing_spec(109, 4));
    let merged = merge_voice_metrics(&[
        VoiceMetricsPart {
            offset_ms: 0,
            metrics: Arc::clone(&m),
        },
        VoiceMetricsPart {
            offset_ms: 1500,
            metrics: Arc::clone(&m),
        },
    ])
    .unwrap();
    let merged_spec = merge_spectrograms(&[
        SpectrogramPart {
            offset_ms: 0,
            spectrogram: Arc::clone(&s),
        },
        SpectrogramPart {
            offset_ms: 1500,
            spectrogram: Arc::clone(&s),
        },
    ])
    .unwrap();
    let off = merge_frame_of(1500, T_SR, T_HOP);
    for f in [0, 45, m.frames as i64 - 1] {
        let lane = 1.5 + m.time_of_frame(f);
        // Within the merge's own half-hop offset quantization.
        assert!((merged.time_of_frame(off + f) - lane).abs() <= T_HOP as f64 / 2.0 / T_SR as f64);
        assert_eq!(
            merged_spec.time_of_frame(off + f),
            merged.time_of_frame(off + f)
        );
        assert_eq!(merged.loudness[(off + f) as usize], m.loudness[f as usize]);
    }
}
