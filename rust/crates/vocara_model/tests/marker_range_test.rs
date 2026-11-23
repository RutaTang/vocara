//! Port of the model-level groups of `client/test/marker_range_test.dart`
//! (PracticeNote / TechniqueMarker ranges) and of
//! `client/test/technique_media_formats_test.dart`. The
//! `normalizeMarkerRange` / `clampNoteEnd` groups test the detail view
//! model and move with it.

use serde_json::Value;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::models::technique_media::TechniqueMedia;
use vocara_model::util::dart_date_time::DateTime;

fn without(mut v: Value, key: &str) -> Value {
    v.as_object_mut().unwrap().remove(key);
    v
}

fn note(start: i64, end: Option<i64>) -> PracticeNote {
    PracticeNote {
        id: "n1".into(),
        position_ms: start,
        end_ms: end,
        content: "breathy onset".into(),
        created_at: DateTime::local_ymd(2026, 6, 20),
        updated_at: DateTime::local_ymd(2026, 6, 20),
    }
}

fn marker(start: i64, end: Option<i64>) -> TechniqueMarker {
    TechniqueMarker {
        id: "m1".into(),
        position_ms: start,
        end_ms: end,
        technique_id: "tech-1".into(),
        technique_name: "Twang".into(),
        created_at: DateTime::local_ymd(2026, 6, 20),
        updated_at: DateTime::local_ymd(2026, 6, 20),
    }
}

#[test]
fn note_end_ms_round_trips_is_range_reflects_the_span() {
    let point = note(1000, None);
    assert!(!point.is_range());
    assert!(PracticeNote::from_json(&point.to_json())
        .unwrap()
        .end_ms
        .is_none());

    let range = note(1000, Some(3000));
    assert!(range.is_range());
    let back = PracticeNote::from_json(&range.to_json()).unwrap();
    assert_eq!(back.end_ms, Some(3000));
    assert!(back.is_range());
}

#[test]
fn note_legacy_json_without_end_ms_is_a_point() {
    let json = without(note(500, None).to_json(), "endMs");
    let back = PracticeNote::from_json(&json).unwrap();
    assert!(back.end_ms.is_none());
    assert!(!back.is_range());
}

#[test]
fn end_ms_not_greater_than_start_is_not_a_range() {
    assert!(!note(1000, Some(1000)).is_range());
    assert!(!note(1000, Some(900)).is_range());
}

#[test]
fn copy_with_preserves_end_ms_but_clear_end_resets_to_a_point() {
    let range = note(1000, Some(2000));
    let edited = PracticeNote {
        content: "x".into(),
        ..range.clone()
    };
    assert_eq!(edited.end_ms, Some(2000));
    let cleared = PracticeNote {
        end_ms: None,
        ..range
    };
    assert!(cleared.end_ms.is_none());
    assert!(!cleared.is_range());
}

#[test]
fn marker_end_ms_round_trips_is_range_reflects_the_span() {
    assert!(!marker(1000, None).is_range());
    let range = marker(1000, Some(4000));
    let back = TechniqueMarker::from_json(&range.to_json()).unwrap();
    assert_eq!(back.end_ms, Some(4000));
    assert!(back.is_range());
}

#[test]
fn marker_legacy_json_without_end_ms_is_a_point() {
    let json = without(marker(1000, None).to_json(), "endMs");
    assert!(TechniqueMarker::from_json(&json).unwrap().end_ms.is_none());
}

// --- technique_media_formats_test.dart ---

#[test]
fn offers_the_audio_media_kit_plays_decodable_by_the_engine_or_not() {
    // Examples play through media_kit: dropping Opus and WMA (the engine
    // has no decoder for either) refused files that play, when only their
    // analysis fails.
    for ok in [
        "a.wav", "a.MP3", "a.m4a", "a.aiff", "a.caf", "a.flac",
        // Ogg audio the song screen takes too (the engine decodes it).
        "a.oga", "a.opus", "a.wma", "a.mp4",
    ] {
        assert!(TechniqueMedia::is_supported_media(ok), "{ok}");
    }
    assert!(!TechniqueMedia::is_supported_media("a.pdf"));
}
