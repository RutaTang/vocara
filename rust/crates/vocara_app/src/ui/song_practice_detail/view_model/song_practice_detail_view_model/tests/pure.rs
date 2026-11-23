//! The pure helpers: `marker_range_test.dart`'s `normalizeMarkerRange` and
//! `clampNoteEnd` / `clampMarkerEnd` groups, and
//! `song_practice_structural_edit_test.dart`'s uncovered spans.

use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::technique_marker::TechniqueMarker;

use super::{at, clip};
use crate::ui::song_practice_detail::view_model::song_practice_detail_view_model::{
    clamp_marker_end, clamp_note_end, normalize_marker_range, uncovered_spans, MIN_MARKER_RANGE_MS,
};

fn note(start: i64, end: Option<i64>) -> PracticeNote {
    PracticeNote {
        id: "n".into(),
        position_ms: start,
        end_ms: end,
        content: "x".into(),
        created_at: at(),
        updated_at: at(),
    }
}

fn marker(start: i64, end: Option<i64>) -> TechniqueMarker {
    TechniqueMarker {
        id: "m".into(),
        position_ms: start,
        end_ms: end,
        technique_id: "t".into(),
        technique_name: "Twang".into(),
        created_at: at(),
        updated_at: at(),
    }
}

#[test]
fn a_missing_end_stays_a_point() {
    assert_eq!(normalize_marker_range(1000, None), (1000, None));
}

#[test]
fn orders_a_backwards_drag() {
    assert_eq!(normalize_marker_range(3000, Some(1000)), (1000, Some(3000)));
}

#[test]
fn keeps_a_long_enough_range() {
    assert_eq!(
        normalize_marker_range(1000, Some(1000 + MIN_MARKER_RANGE_MS)),
        (1000, Some(1000 + MIN_MARKER_RANGE_MS))
    );
}

#[test]
fn collapses_a_too_short_drag_to_a_point_at_the_start() {
    // Below the threshold → a point, anchored at the (ordered) start.
    assert_eq!(normalize_marker_range(1000, Some(1050)), (1000, None));
    assert_eq!(normalize_marker_range(1050, Some(1000)), (1000, None));
}

#[test]
fn never_returns_a_negative_start() {
    assert_eq!(normalize_marker_range(-50, None), (0, None));
    assert_eq!(normalize_marker_range(-50, Some(2000)), (0, Some(2000)));
}

#[test]
fn a_point_is_returned_unchanged() {
    let n = note(0, None);
    assert_eq!(clamp_note_end(&n, 500), n);
}

#[test]
fn a_range_ending_before_the_boundary_is_unchanged() {
    assert_eq!(
        clamp_note_end(&note(1000, Some(2000)), 5000).end_ms,
        Some(2000)
    );
}

#[test]
fn a_range_past_the_boundary_is_clamped_to_it() {
    assert_eq!(
        clamp_note_end(&note(1000, Some(5000)), 3000).end_ms,
        Some(3000)
    );
}

#[test]
fn clamping_below_the_minimum_span_collapses_to_a_point() {
    // The boundary leaves only 50 ms < MIN_MARKER_RANGE_MS → a point.
    let clamped = clamp_note_end(&note(1000, Some(5000)), 1050);
    assert_eq!(clamped.end_ms, None);
    assert!(!clamped.is_range());
}

#[test]
fn the_marker_variant_clamps_and_collapses_the_same_way() {
    let m = marker(0, Some(9000));
    assert_eq!(clamp_marker_end(&m, 4000).end_ms, Some(4000));
    assert_eq!(clamp_marker_end(&m, 80).end_ms, None);
}

#[test]
fn the_uncovered_spans_of_a_clip() {
    let c = |start: i64, dur: i64| clip(&start.to_string(), "/x", start, dur);
    assert_eq!(
        uncovered_spans(&c(1000, 3000), &[c(0, 3000)]),
        [(3000, 4000)]
    );
    assert_eq!(
        uncovered_spans(&c(0, 4000), &[c(1000, 1000)]),
        [(0, 1000), (2000, 4000)]
    );
    assert!(uncovered_spans(&c(0, 1000), &[c(0, 1000)]).is_empty());
    assert_eq!(uncovered_spans(&c(0, 1000), &[c(2000, 1000)]), [(0, 1000)]);
}
