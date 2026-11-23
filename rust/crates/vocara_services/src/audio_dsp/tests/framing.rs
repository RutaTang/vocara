//! analysis_frame_timing_test.dart's "the shared frame↔time mapping" group:
//! frame f analyses the fft_size-sample window starting at sample f·hop, so
//! what it measures is the audio around the window's CENTRE. These pin a
//! real tone burst's onset as the DSP sees it to that mapping. (Its other
//! groups — the lanes, the dock spectrogram and the readouts that draw these
//! frames — are the analysis screens' tests.)

use std::f64::consts::PI;
use std::sync::Arc;

use vocara_model::models::spectrogram::{
    merge_frame_of, merge_spectrograms, AnalysisFraming, SpectrogramPart,
};
use vocara_model::models::voice_metrics::{merge_voice_metrics, VoiceMetricsPart};

use super::super::{compute_spectrogram, compute_voice_metrics, AnalysisParams};
use super::support::assert_close;

const SR: i64 = 48000;
const HOP: i64 = 512;
const FFT: i64 = 2048;
/// One hop in ms — the grid every onset is quantized to.
const HOP_MS: f64 = HOP as f64 / SR as f64 * 1000.0;
/// The tone burst's onset, and the take's length.
const ONSET_MS: i64 = 500;
const TAKE_MS: i64 = 1200;

/// Silence, then a steady 220 Hz tone from exactly [ONSET_MS].
fn burst() -> Vec<f32> {
    let n = (TAKE_MS * SR / 1000) as usize;
    let onset = (ONSET_MS * SR / 1000) as usize;
    let mut s = vec![0.0f32; n];
    for (i, v) in s.iter_mut().enumerate().skip(onset) {
        *v = (0.3 * (2.0 * PI * 220.0 * (i - onset) as f64 / SR as f64).sin()) as f32;
    }
    s
}

/// The window-centre time of frame [f] for the production framing, spelled
/// out rather than read back through the mapping under test.
fn centre_seconds(f: i64) -> f64 {
    ((f * HOP) as f64 + FFT as f64 / 2.0) / SR as f64
}

fn first_frame(frames: usize, test: impl Fn(usize) -> bool) -> i64 {
    (0..frames).position(test).map_or(-1, |f| f as i64)
}

#[test]
fn a_frame_sits_at_its_window_centre_not_its_start() {
    let samples = burst();
    let m = compute_voice_metrics(&samples, SR, AnalysisParams::default());
    let spec = compute_spectrogram(&samples, SR, AnalysisParams::default());
    assert_eq!(m.fft_size, FFT);
    let framings: [&dyn AnalysisFraming; 2] = [&m, &spec];
    for a in framings {
        assert_close(a.time_of_frame(0), centre_seconds(0), 1e-12, "frame 0");
        assert_close(a.time_of_frame(45), centre_seconds(45), 1e-12, "frame 45");
        // A frame's cell is hop-wide and centred on it.
        let half_hop = HOP as f64 / 2.0 / SR as f64;
        assert_close(
            a.cell_start(45),
            centre_seconds(45) - half_hop,
            1e-12,
            "cell start",
        );
        assert_close(
            a.cell_end(45),
            centre_seconds(45) + half_hop,
            1e-12,
            "cell end",
        );
    }
}

#[test]
fn a_playhead_at_a_frame_centre_reads_that_frame() {
    let samples = burst();
    let m = compute_voice_metrics(&samples, SR, AnalysisParams::default());
    let spec = compute_spectrogram(&samples, SR, AnalysisParams::default());
    let bins = spec.bins;
    for f in 0..m.frames {
        let t = centre_seconds(f as i64);
        assert_eq!(m.frame_at_time(t), f);
        assert_eq!(spec.frame_at_time(t), f);
        assert_eq!(m.sample_at(&m.loudness, t), f64::from(m.loudness[f]));
        assert_eq!(spec.column_at_time(t), &spec.db[f * bins..f * bins + bins]);
    }
    // Anywhere inside the cell still reads it; times off either end clamp.
    let off = 0.4 * HOP as f64 / SR as f64;
    assert_eq!(m.frame_at_time(centre_seconds(30) + off), 30);
    assert_eq!(m.frame_at_time(centre_seconds(30) - off), 30);
    assert_eq!(m.frame_at_time(0.0), 0);
    assert_eq!(m.frame_at_time(100.0), m.frames - 1);
}

#[test]
fn a_tone_burst_at_500_ms_reads_its_onset_at_500_ms_in_every_analysis() {
    let samples = burst();
    let m = compute_voice_metrics(&samples, SR, AnalysisParams::default());
    let spec = compute_spectrogram(&samples, SR, AnalysisParams::default());
    // Pitch: the first voiced frame.
    let voiced = first_frame(m.frames, |f| m.f0[f].is_finite());
    // Loudness: the first frame within 3 dB of the steady tone — the frame
    // whose window is at least half tone, i.e. whose CENTRE is past onset.
    let steady = f64::from(m.loudness[m.frames / 2 + 20]);
    let loud = first_frame(m.frames, |f| f64::from(m.loudness[f]) >= steady - 3.0103);
    // Spectrogram: the first column within 6 dB of the steady tone at its
    // bin (a Hann window half-filled with tone carries half the amplitude).
    let bin = (220.0 / spec.bin_hz()).round() as usize;
    let steady_db = spec.value_at(spec.frames / 2 + 20, bin);
    let col = first_frame(spec.frames, |f| spec.value_at(f, bin) >= steady_db - 6.0206);
    for (what, ms) in [
        ("pitch", m.time_of_frame(voiced) * 1000.0),
        ("loudness", m.time_of_frame(loud) * 1000.0),
        ("spectrogram", spec.time_of_frame(col) * 1000.0),
    ] {
        assert_close(
            ms,
            ONSET_MS as f64,
            HOP_MS,
            &format!("{what} onset at {ms} ms"),
        );
    }
}

#[test]
fn a_merged_lane_keeps_every_clip_frame_at_its_own_window_centre() {
    // The same burst as a second clip 1.5 s into the lane: each of its frames
    // reads, on the lane axis, its clip offset plus its clip-local centre.
    let samples = burst();
    let m = Arc::new(compute_voice_metrics(
        &samples,
        SR,
        AnalysisParams::default(),
    ));
    let spec = Arc::new(compute_spectrogram(&samples, SR, AnalysisParams::default()));
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
    .expect("merges");
    let merged_spec = merge_spectrograms(&[
        SpectrogramPart {
            offset_ms: 0,
            spectrogram: Arc::clone(&spec),
        },
        SpectrogramPart {
            offset_ms: 1500,
            spectrogram: Arc::clone(&spec),
        },
    ])
    .expect("merges");
    let off = merge_frame_of(1500, SR, HOP);
    for f in [0, 45, m.frames as i64 - 1] {
        let lane = 1.5 + m.time_of_frame(f);
        // Within the merge's own half-hop offset quantization.
        assert_close(
            merged.time_of_frame(off + f),
            lane,
            HOP as f64 / 2.0 / SR as f64,
            "lane time",
        );
        assert_eq!(
            merged_spec.time_of_frame(off + f),
            merged.time_of_frame(off + f)
        );
        assert_eq!(merged.loudness[(off + f) as usize], m.loudness[f as usize]);
    }
}
