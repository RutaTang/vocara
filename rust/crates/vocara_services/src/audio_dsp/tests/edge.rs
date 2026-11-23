//! analysis_edge_inputs_test.dart: the analysis runs on whatever the user
//! imported or recorded — a take stopped after one buffer, a muted input, a
//! DC-offset interface, a decoder that emitted a non-finite sample. None of
//! those may panic. Every window/frame size in the chain (YIN, the
//! spectrogram STFT, the LTAS voiced gate) has its own minimum-length
//! branch; the loop below crosses each of them from both sides at every rate
//! the decoder can hand us. (Its lane-span group tests the AudioTrack model,
//! vocara_model's.)

use std::f64::consts::PI;

use vocara_model::models::voice_metrics::{
    analyze_register, analyze_voice_profile, compute_ltas, RegisterOptions, VoiceProfileOptions,
};

use super::super::{
    compute_spectrogram, compute_voice_metrics, AnalysisParams, LiveVoiceAnalyzer,
    StreamingTakeAnalyzer, TunerTracker,
};

fn sine(n: usize, f: f64, sr: i64) -> Vec<f32> {
    (0..n)
        .map(|i| (0.3 * (2.0 * PI * f * i as f64 / sr as f64).sin()) as f32)
        .collect()
}

/// The whole suite as the take dock and the technique reference run it,
/// plus the streaming and live paths over the same samples.
fn full_pass(samples: &[f32], sr: i64) {
    let p = AnalysisParams::default();
    let metrics = compute_voice_metrics(samples, sr, p);
    let spectrogram = compute_spectrogram(samples, sr, p);
    compute_ltas(&spectrogram, Some(&metrics));
    analyze_voice_profile(&metrics, VoiceProfileOptions::default());
    analyze_register(&metrics, RegisterOptions::default());

    let mut take = StreamingTakeAnalyzer::new(sr, p, metrics.frames, true);
    take.add(samples, None);
    let streamed = take.finish();
    assert_eq!(streamed.metrics.frames, metrics.frames);

    let mut live = LiveVoiceAnalyzer::new(sr, p);
    let mut tuner = TunerTracker::default();
    live.add_samples(samples, |f| {
        tuner.add(f.f0, f.f0_confidence);
    });
}

#[test]
fn the_analysis_suite_survives_every_input_length_and_rate() {
    for sr in [8000, 22050, 44100, 48000] {
        for n in [1, 2, 63, 64, 255, 256, 1000] {
            full_pass(&sine(n, 220.0, sr), sr);
        }
    }
}

#[test]
fn silence_and_dc_analyze_without_panicking() {
    let sr = 48000;
    for data in [
        vec![0.0f32; 4800], // a muted or unplugged input
        vec![1.0f32; 4800], // full-scale DC offset
        vec![-1.0f32; 4800],
    ] {
        full_pass(&data, sr);
    }
}

#[test]
fn non_finite_samples_analyze_without_panicking() {
    let sr = 48000;
    let mut data = sine(4800, 220.0, sr);
    data[100] = f32::NAN;
    data[200] = f32::INFINITY;
    data[300] = f32::NEG_INFINITY;
    full_pass(&data, sr);
}
