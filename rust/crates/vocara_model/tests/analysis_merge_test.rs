//! Port of `client/test/analysis_merge_test.dart`. Dart's `identical` on a
//! merge result is [`Arc::ptr_eq`].

use std::sync::Arc;

use vocara_model::models::spectrogram::{
    merge_spectrograms, AnalysisFraming, Spectrogram, SpectrogramPart, MAX_MERGED_LANE_FRAMES,
};
use vocara_model::models::voice_metrics::{merge_voice_metrics, VoiceMetrics, VoiceMetricsPart};

// Test framing: hop=100 @ 1 kHz → one frame per 100 ms, so an offset of
// N·100 ms lands exactly at frame N and the placement math stays readable.
const HOP: i64 = 100;
const SR: i64 = 1000;

struct M {
    ring: Vec<f32>,
    active: Vec<u8>,
    hop: i64,
    fft_size: i64,
    sample_rate: i64,
    clipped_fraction: f64,
    snr_db: f64,
    f0: Option<Vec<f32>>,
}

fn m(ring: &[f32], active: &[u8]) -> M {
    M {
        ring: ring.to_vec(),
        active: active.to_vec(),
        hop: HOP,
        fft_size: 2048,
        sample_rate: SR,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
        f0: None,
    }
}

impl M {
    fn build(self) -> Arc<VoiceMetrics> {
        let n = self.ring.len();
        let flat = |v: f32| vec![v; n];
        Arc::new(VoiceMetrics {
            frames: n,
            hop: self.hop,
            fft_size: self.fft_size,
            sample_rate: self.sample_rate,
            ring: self.ring,
            tilt: flat(0.0),
            cpp: flat(0.0),
            loudness: flat(0.0),
            // Voiced by default (finite F0) so the timbre averages count these frames;
            // pass an explicit f0 with NaNs to model unvoiced (breath/consonant) frames.
            f0: self.f0.unwrap_or_else(|| flat(200.0)),
            f1: flat(f32::NAN),
            f2: flat(f32::NAN),
            f3: flat(f32::NAN),
            h1h2: flat(f32::NAN),
            active: self.active,
            ring_avg: 0.0,
            tilt_avg: 0.0,
            cpp_avg: 0.0,
            peak_dbfs: f64::NAN,
            clipped_fraction: self.clipped_fraction,
            snr_db: self.snr_db,
        })
    }
}

fn spec(db: &[f32], frames: usize, min_db: f64, max_db: f64, bins: usize) -> Arc<Spectrogram> {
    Arc::new(Spectrogram {
        frames,
        bins,
        max_hz: 5000.0,
        sample_rate: SR,
        fft_size: 2048,
        hop: HOP,
        db: db.to_vec(),
        min_db,
        max_db,
    })
}

/// A clip's metrics positioned at [offset_ms] after the lane's span start.
fn m_at(offset_ms: i64, metrics: &Arc<VoiceMetrics>) -> VoiceMetricsPart {
    VoiceMetricsPart {
        offset_ms,
        metrics: Arc::clone(metrics),
    }
}

/// A clip's spectrogram positioned at [offset_ms] after the lane's span start.
fn s_at(offset_ms: i64, spectrogram: &Arc<Spectrogram>) -> SpectrogramPart {
    SpectrogramPart {
        offset_ms,
        spectrogram: Arc::clone(spectrogram),
    }
}

fn all_nan(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_nan())
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9
}

// --- mergeVoiceMetrics ---

#[test]
fn returns_none_for_empty_and_a_lone_origin_part_unchanged() {
    assert!(merge_voice_metrics(&[]).is_none());
    let one = m(&[1.0, 2.0], &[1, 1]).build();
    // Single-clip lane: the very same object as the input.
    assert!(Arc::ptr_eq(
        &merge_voice_metrics(&[m_at(0, &one)]).unwrap(),
        &one
    ));
}

#[test]
fn a_lone_part_off_the_lane_origin_is_padded_with_a_leading_gap() {
    // The earliest clip's analysis failed, so the only surviving part starts
    // 300 ms into the lane — it must NOT slide back to the origin.
    let one = m(&[7.0, 8.0], &[1, 1]).build();
    let r = merge_voice_metrics(&[m_at(300, &one)]).unwrap();
    assert_eq!(r.frames, 5);
    assert!(all_nan(&r.ring[..3]));
    assert!(all_nan(&r.f0[..3]));
    assert!(r.active[..3].iter().all(|a| *a == 0));
    assert_eq!(r.ring[3..], [7.0, 8.0]);
}

#[test]
fn back_to_back_clips_concatenate_and_recompute_the_averages() {
    let a = m(&[1.0, 2.0], &[1, 1]).build();
    let b = m(&[3.0, 4.0], &[1, 0]).build();
    // b starts exactly where a ends (2 frames = 200 ms).
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(200, &b)]).unwrap();
    assert_eq!(r.frames, 4);
    assert_eq!(r.ring, [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(r.active, [1, 1, 1, 0]);
    // ring_avg over active voiced frames (0,1,2) = (1+2+3)/3.
    assert!(close(r.ring_avg, 2.0));
}

#[test]
fn a_gap_between_clips_becomes_nan_inactive_frames() {
    let a = m(&[1.0, 2.0], &[1, 1]).build();
    let b = m(&[5.0, 6.0], &[1, 1]).build();
    // a covers frames 0–1; b starts at 500 ms → frames 5–6; 2–4 are the gap.
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(500, &b)]).unwrap();
    assert_eq!(r.frames, 7);
    assert_eq!(r.ring[..2], [1.0, 2.0]);
    for fr in 2..5 {
        assert!(r.ring[fr].is_nan());
        assert!(r.f0[fr].is_nan());
        assert!(r.loudness[fr].is_nan());
        assert_eq!(r.active[fr], 0);
    }
    assert_eq!(r.ring[5..], [5.0, 6.0]);
    // The gap frames are unvoiced, so they don't dilute the average.
    assert!(close(r.ring_avg, (1.0 + 2.0 + 5.0 + 6.0) / 4.0));
}

#[test]
fn overlapping_clips_the_later_starting_clip_wins_tails_survive() {
    let a = m(&[1.0, 2.0, 3.0, 4.0, 5.0], &[1, 1, 1, 1, 1]).build();
    let b = m(&[9.0, 9.0], &[0, 0]).build();
    // b overlaps a's frames 1–2; a's tail (frames 3–4) stays visible.
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(100, &b)]).unwrap();
    assert_eq!(r.frames, 5);
    assert_eq!(r.ring, [1.0, 9.0, 9.0, 4.0, 5.0]);
    assert_eq!(r.active, [1, 0, 0, 1, 1]);
    // Overwritten frames don't count toward the average.
    assert!(close(r.ring_avg, (1.0 + 4.0 + 5.0) / 3.0));
}

#[test]
fn placement_follows_offsets_not_input_order() {
    let a = m(&[1.0, 2.0, 3.0, 4.0, 5.0], &[1, 1, 1, 1, 1]).build();
    let b = m(&[9.0, 9.0], &[0, 0]).build();
    let in_order = merge_voice_metrics(&[m_at(0, &a), m_at(100, &b)]).unwrap();
    let reversed = merge_voice_metrics(&[m_at(100, &b), m_at(0, &a)]).unwrap();
    assert_eq!(reversed.ring, in_order.ring);
    assert_eq!(reversed.active, in_order.active);
}

#[test]
fn timbre_averages_gate_on_voiced_frames() {
    // Frame 1 is audible but unvoiced (NaN F0) — it must NOT pull ring_avg.
    let a = M {
        f0: Some(vec![200.0, f32::NAN]),
        ..m(&[2.0, 99.0], &[1, 1])
    }
    .build();
    let b = M {
        f0: Some(vec![200.0, f32::NAN]),
        ..m(&[4.0, 99.0], &[1, 1])
    }
    .build();
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(200, &b)]).unwrap();
    // Only the two voiced frames (ring 2 and 4) count -> avg 3.
    assert!(close(r.ring_avg, 3.0));
}

#[test]
fn a_lane_with_no_voiced_frame_has_no_timbre_averages() {
    // Audible but unvoiced throughout (breath between phrases on both
    // clips): nothing to average, which the readouts show as "—".
    let a = M {
        f0: Some(vec![f32::NAN, f32::NAN]),
        ..m(&[2.0, 3.0], &[1, 1])
    }
    .build();
    let b = M {
        f0: Some(vec![f32::NAN, f32::NAN]),
        ..m(&[4.0, 5.0], &[1, 1])
    }
    .build();
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(200, &b)]).unwrap();
    assert!(r.ring_avg.is_nan());
    assert!(r.tilt_avg.is_nan());
    assert!(r.cpp_avg.is_nan());
}

#[test]
fn aggregates_the_quality_gate_so_a_clipped_or_noisy_clip_still_warns() {
    let clean = M {
        snr_db: 30.0,
        ..m(&[1.0, 1.0], &[1, 1])
    }
    .build();
    let bad = M {
        clipped_fraction: 0.05,
        snr_db: 5.0,
        ..m(&[1.0, 1.0], &[1, 1])
    }
    .build();
    let r = merge_voice_metrics(&[m_at(0, &clean), m_at(200, &bad)]).unwrap();
    assert!(r.is_clipped()); // worst clip drives it
    assert!(r.is_low_snr()); // worst (lowest) snr drives it
    assert_eq!(r.clipped_fraction, 0.05);
    assert_eq!(r.snr_db, 5.0);
}

#[test]
fn clean_clips_merge_with_no_false_quality_warning() {
    let a = M {
        snr_db: 30.0,
        ..m(&[1.0, 1.0], &[1, 1])
    }
    .build();
    let b = M {
        snr_db: 28.0,
        ..m(&[1.0, 1.0], &[1, 1])
    }
    .build();
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(200, &b)]).unwrap();
    assert!(!r.is_clipped());
    assert!(!r.is_low_snr());
}

#[test]
fn skips_parts_whose_framing_differs_from_the_first() {
    let a = m(&[1.0, 2.0], &[1, 1]).build();
    let b = M {
        sample_rate: 48000,
        ..m(&[9.0], &[1])
    }
    .build();
    // b is dropped (different sample_rate), so only a survives — unchanged.
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(200, &b)]).unwrap();
    assert!(Arc::ptr_eq(&r, &a));
}

#[test]
fn a_part_with_a_different_window_is_a_different_framing_too() {
    // Same hop and rate, but frames are placed at their window centre, so a
    // 1024-sample window puts every frame of b at another time than the
    // merged result's frame would claim: b is dropped, not misplaced.
    let a = m(&[1.0, 2.0], &[1, 1]).build();
    let b = M {
        fft_size: 1024,
        ..m(&[9.0], &[1])
    }
    .build();
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(200, &b)]).unwrap();
    assert!(Arc::ptr_eq(&r, &a));
    assert_eq!(r.fft_size, 2048);
}

#[test]
fn the_merged_result_keeps_the_parts_window_length() {
    let a = M {
        fft_size: 400,
        ..m(&[1.0, 2.0], &[1, 1])
    }
    .build();
    let b = M {
        fft_size: 400,
        ..m(&[3.0], &[1])
    }
    .build();
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(300, &b)]).unwrap();
    assert_eq!(r.fft_size, 400);
    // hop 100 @ 1 kHz with a 400-sample window: frame f is centred at
    // f·100 + 200 ms, so b's lone frame (placed at frame 3) reads 500 ms.
    assert!(close(r.time_of_frame(3), 0.5));
    assert!(close(r.time_of_frame(3), 0.3 + b.time_of_frame(0)));
}

#[test]
fn a_dropped_part_leaves_its_gap_instead_of_shifting_the_survivor() {
    // The clip at the lane origin has a mismatched framing and is dropped;
    // the surviving later clip must keep its offset, not slide back to 0.
    let mismatched = M {
        sample_rate: 48000,
        ..m(&[9.0], &[1])
    }
    .build();
    let b = m(&[7.0, 8.0], &[1, 1]).build();
    let r = merge_voice_metrics(&[m_at(300, &b), m_at(0, &mismatched)]).unwrap();
    assert_eq!(r.frames, 5);
    assert!(all_nan(&r.ring[..3]));
    assert_eq!(r.ring[3..], [7.0, 8.0]);
}

#[test]
fn frameless_metrics_parts_are_filtered_out() {
    let a = m(&[1.0, 2.0], &[1, 1]).build();
    let empty = m(&[], &[]).build();
    // The empty clip (e.g. a degenerate recording) contributes nothing and
    // must not stretch the merged span to its offset.
    let r = merge_voice_metrics(&[m_at(0, &a), m_at(500, &empty)]).unwrap();
    assert!(Arc::ptr_eq(&r, &a));
    assert!(merge_voice_metrics(&[m_at(0, &empty)]).is_none());
}

// --- mergeSpectrograms ---

#[test]
fn spectrograms_return_none_for_empty_and_a_lone_origin_part_unchanged() {
    assert!(merge_spectrograms(&[]).is_none());
    let one = spec(&[1.0, 2.0], 1, 0.0, 2.0, 2);
    assert!(Arc::ptr_eq(
        &merge_spectrograms(&[s_at(0, &one)]).unwrap(),
        &one
    ));
}

#[test]
fn back_to_back_clips_concatenate_rows_and_span_min_max() {
    let a = spec(&[1.0, 2.0, 3.0, 4.0], 2, 0.0, 5.0, 2);
    let b = spec(&[5.0, 6.0], 1, -1.0, 7.0, 2);
    let s = merge_spectrograms(&[s_at(0, &a), s_at(200, &b)]).unwrap();
    assert_eq!(s.frames, 3);
    assert_eq!(s.db, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    assert_eq!(s.min_db, -1.0);
    assert_eq!(s.max_db, 7.0);
}

#[test]
fn a_gap_between_clips_becomes_floor_columns() {
    let a = spec(&[1.0, 2.0, 3.0, 4.0], 2, 0.0, 5.0, 2);
    let b = spec(&[5.0, 6.0], 1, -1.0, 7.0, 2);
    // a covers frames 0–1; b starts at 400 ms → frame 4; 2–3 are the gap.
    let s = merge_spectrograms(&[s_at(0, &a), s_at(400, &b)]).unwrap();
    assert_eq!(s.frames, 5);
    assert_eq!(s.db, [1.0, 2.0, 3.0, 4.0, -1.0, -1.0, -1.0, -1.0, 5.0, 6.0]);
    assert_eq!(s.min_db, -1.0);
    assert_eq!(s.max_db, 7.0);
}

#[test]
fn overlapping_spectrograms_the_later_starting_clip_wins() {
    let a = spec(&[1.0, 2.0, 3.0, 4.0], 2, 0.0, 5.0, 2);
    let b = spec(&[9.0, 9.0], 1, 0.0, 9.0, 2);
    // b lands on a's frame 1.
    let s = merge_spectrograms(&[s_at(0, &a), s_at(100, &b)]).unwrap();
    assert_eq!(s.frames, 2);
    assert_eq!(s.db, [1.0, 2.0, 9.0, 9.0]);
    // And placement follows offsets, not input order.
    let r = merge_spectrograms(&[s_at(100, &b), s_at(0, &a)]).unwrap();
    assert_eq!(r.db, [1.0, 2.0, 9.0, 9.0]);
}

#[test]
fn a_lone_spectrogram_off_the_lane_origin_is_padded_with_a_leading_gap() {
    let one = spec(&[5.0, 6.0], 1, -2.0, 6.0, 2);
    let s = merge_spectrograms(&[s_at(200, &one)]).unwrap();
    assert_eq!(s.frames, 3);
    assert_eq!(s.db, [-2.0, -2.0, -2.0, -2.0, 5.0, 6.0]);
}

#[test]
fn skips_parts_whose_bin_or_framing_differs_from_the_first() {
    let a = spec(&[1.0, 2.0, 3.0, 4.0], 2, 0.0, 5.0, 2);
    let b = spec(&[9.0, 9.0, 9.0], 1, 0.0, 9.0, 3);
    // b has different bins → dropped; only a survives — unchanged.
    let s = merge_spectrograms(&[s_at(0, &a), s_at(200, &b)]).unwrap();
    assert!(Arc::ptr_eq(&s, &a));
}

#[test]
fn frameless_spectrogram_parts_are_filtered_out() {
    let a = spec(&[1.0, 2.0, 3.0, 4.0], 2, 0.0, 5.0, 2);
    let empty = spec(&[], 0, 0.0, 0.0, 2);
    let s = merge_spectrograms(&[s_at(0, &a), s_at(500, &empty)]).unwrap();
    assert!(Arc::ptr_eq(&s, &a));
    assert!(merge_spectrograms(&[s_at(0, &empty)]).is_none());
}

#[test]
fn an_absurd_lane_span_fails_soft_instead_of_allocating_by_the_gap() {
    // The merged array is sized by offset + frames: two TINY clips with a
    // huge gap between them used to allocate gigabytes of silence columns.
    // Beyond the span cap the merge yields no view rather than an OOM.
    let a = spec(&[1.0, 2.0], 1, 0.0, 5.0, 2);
    let b = spec(&[3.0, 4.0], 1, 0.0, 5.0, 2);
    // ~10 years of lane offset at this framing.
    assert!(merge_spectrograms(&[s_at(0, &a), s_at(320_000_000_000, &b)]).is_none());
    // A sane gap still merges.
    assert!(merge_spectrograms(&[s_at(0, &a), s_at(500, &b)]).is_some());
}

// --- shared lane-span budget ---

#[test]
fn a_lane_span_past_the_budget_is_refused_by_both_merges() {
    // Both merges are sized by the lane SPAN, not by the audio in it. The
    // metrics merge allocates nine span-sized Float32 curves and runs
    // FIRST, so a corrupt or hostile offset used to size that allocation
    // behind a release-stripped assert while the sibling merge's cap never
    // got a say.
    let offset_ms = (MAX_MERGED_LANE_FRAMES + 10) * 1000 * HOP / SR; // past the budget
    let part = m(&[1.0, 2.0], &[1, 1]).build();
    assert!(merge_voice_metrics(&[m_at(0, &part), m_at(offset_ms, &part)]).is_none());
    let s = spec(&[0.0, 0.0, 0.0, 0.0], 2, -60.0, 0.0, 2);
    assert!(merge_spectrograms(&[s_at(0, &s), s_at(offset_ms, &s)]).is_none());
}

#[test]
fn a_span_exactly_at_the_budget_still_merges() {
    // The bound is inclusive, so the admissible lane is not one frame
    // shorter than the constant advertises.
    let offset_ms = (MAX_MERGED_LANE_FRAMES - 2) * 1000 * HOP / SR;
    let part = m(&[1.0, 2.0], &[1, 1]).build();
    let merged = merge_voice_metrics(&[m_at(0, &part), m_at(offset_ms, &part)]).unwrap();
    assert_eq!(merged.frames as i64, MAX_MERGED_LANE_FRAMES);
}

#[test]
fn both_merges_convert_an_offset_to_the_same_frame() {
    // The two outputs are read against ONE lane axis, so a one-frame
    // disagreement slides the pitch/formant overlays off the columns they
    // annotate.
    for offset_ms in [0, 49, 50, 51, 150, 12345] {
        let part = m(&[1.0], &[1]).build();
        let mm = merge_voice_metrics(&[m_at(0, &part), m_at(offset_ms, &part)]).unwrap();
        let s = spec(&[0.0, 0.0], 1, -60.0, 0.0, 2);
        let ss = merge_spectrograms(&[s_at(0, &s), s_at(offset_ms, &s)]).unwrap();
        assert_eq!(mm.frames, ss.frames, "offset {offset_ms}");
    }
}
