//! Ports of `client/test/audio_track_color_test.dart`,
//! `client/test/audio_track_span_test.dart`, the "lane span" group of
//! `client/test/analysis_edge_inputs_test.dart` and
//! `client/test/technique_marker_test.dart`.

use serde_json::Value;
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;

fn without(mut v: Value, key: &str) -> Value {
    v.as_object_mut().unwrap().remove(key);
    v
}

// --- audio_track_color_test.dart ---

fn colored(color_value: Option<i64>) -> AudioTrack {
    AudioTrack {
        color_value,
        ..AudioTrack::new(
            "t1",
            AudioTrackKind::Vocal,
            "Take 1",
            DateTime::local_ymd(2026, 1, 1),
        )
        .with_audio("/tmp/t1.wav", 0, 1000, vec![])
    }
}

#[test]
fn copy_with_sets_a_custom_color() {
    let t = AudioTrack {
        color_value: Some(0xFF5B8DEF),
        ..colored(None)
    };
    assert_eq!(t.color_value, Some(0xFF5B8DEF));
}

#[test]
fn copy_with_with_no_color_args_keeps_the_existing_color() {
    let t = AudioTrack {
        display_name: "X".into(),
        ..colored(Some(0xFFE3604E))
    };
    assert_eq!(t.color_value, Some(0xFFE3604E));
    assert_eq!(t.display_name, "X");
}

#[test]
fn copy_with_clear_color_resets_to_the_default_none() {
    let t = AudioTrack {
        color_value: None,
        ..colored(Some(0xFFE3604E))
    };
    assert!(t.color_value.is_none());
}

#[test]
fn color_value_round_trips_through_json() {
    let json = colored(Some(0xFF6FB562)).to_json();
    assert_eq!(
        AudioTrack::from_json(&json).unwrap().color_value,
        Some(0xFF6FB562)
    );
    // A track with no custom color stays None across JSON.
    assert!(AudioTrack::from_json(&colored(None).to_json())
        .unwrap()
        .color_value
        .is_none());
}

// --- audio_track_span_test.dart ---

fn clip(id: &str, start_ms: i64, duration_ms: i64) -> AudioClip {
    AudioClip {
        start_ms,
        ..AudioClip::new(id, format!("/tmp/{id}.wav"), duration_ms)
    }
}

fn lane(clips: Vec<AudioClip>) -> AudioTrack {
    AudioTrack {
        clips,
        ..AudioTrack::new(
            "lane",
            AudioTrackKind::Vocal,
            "Take 1",
            DateTime::local_ymd(2026, 1, 1),
        )
    }
}

#[test]
fn a_single_clip_lane_spans_exactly_its_clip() {
    let track = lane(vec![clip("c1", 1500, 4000)]);
    assert_eq!(track.start_ms(), 1500);
    assert_eq!(track.end_ms(), 5500);
    assert_eq!(track.duration_ms(), 4000);
    assert_eq!(track.primary_clip().unwrap().id, "c1");
}

#[test]
fn the_single_clip_factory_convenience_matches_the_clip_fields_too() {
    let track = AudioTrack::new(
        "t",
        AudioTrackKind::Vocal,
        "Take 1",
        DateTime::local_ymd(2026, 1, 1),
    )
    .with_audio("/tmp/t.wav", 250, 1000, vec![]);
    assert_eq!(track.start_ms(), 250);
    assert_eq!(track.end_ms(), 1250);
    assert_eq!(track.duration_ms(), 1000);
    assert_eq!(track.clips[0].id, "t-c0");
}

#[test]
fn an_empty_lane_reads_0_0_0_and_has_no_primary_clip() {
    let track = lane(vec![]);
    assert_eq!(track.start_ms(), 0);
    assert_eq!(track.end_ms(), 0);
    assert_eq!(track.duration_ms(), 0);
    assert!(track.primary_clip().is_none());
    assert!(!track.has_clip());
    assert!(track.is_empty_take());
}

#[test]
fn a_multi_clip_lane_spans_min_start_to_max_end_gap_included() {
    let track = lane(vec![clip("c1", 1000, 2000), clip("c2", 5000, 3000)]);
    assert_eq!(track.start_ms(), 1000);
    assert_eq!(track.end_ms(), 8000);
    assert_eq!(track.duration_ms(), 7000); // includes the 3000..5000 gap
}

#[test]
fn the_span_does_not_depend_on_the_clips_list_order() {
    // A punch-in recorded BEFORE the first clip lands later in the list;
    // primary_clip stays the first LIST entry, but the span is min..max.
    let track = lane(vec![clip("c1", 5000, 3000), clip("c2", 1000, 2000)]);
    assert_eq!(track.primary_clip().unwrap().id, "c1");
    assert_eq!(track.start_ms(), 1000);
    assert_eq!(track.end_ms(), 8000);
    assert_eq!(track.duration_ms(), 7000);
}

#[test]
fn overlapping_clips_span_the_union_of_their_extents() {
    let track = lane(vec![clip("c1", 0, 4000), clip("c2", 2000, 1000)]);
    assert_eq!(track.start_ms(), 0);
    assert_eq!(track.end_ms(), 4000);
    assert_eq!(track.duration_ms(), 4000);
}

#[test]
fn copy_with_clips_recomputes_the_span() {
    let track = lane(vec![clip("c1", 1000, 2000)]);
    let mut clips = track.clips.clone();
    clips.push(clip("c2", 6000, 500));
    let grown = AudioTrack {
        clips,
        ..track.clone()
    };
    assert_eq!(grown.start_ms(), 1000);
    assert_eq!(grown.end_ms(), 6500);
    assert_eq!(grown.duration_ms(), 5500);
    // The original is untouched (immutable model).
    assert_eq!(track.duration_ms(), 2000);
}

#[test]
fn the_span_survives_a_json_round_trip() {
    let track = lane(vec![clip("c1", 5000, 3000), clip("c2", 1000, 2000)]);
    let back = AudioTrack::from_json(&track.to_json()).unwrap();
    assert_eq!(back.start_ms(), 1000);
    assert_eq!(back.end_ms(), 8000);
    assert_eq!(back.duration_ms(), 7000);
}

// --- analysis_edge_inputs_test.dart, "lane span" ---

#[test]
fn an_empty_lane_spans_nothing() {
    let t = lane(vec![]);
    assert_eq!(t.end_ms() - t.start_ms(), 0);
}

#[test]
fn clips_held_out_of_order_still_span_first_to_last() {
    let t = lane(vec![
        AudioClip {
            start_ms: 500,
            ..AudioClip::new("b", "/b", 100)
        },
        AudioClip::new("a", "/a", 100),
    ]);
    assert_eq!(t.start_ms(), 0);
    assert_eq!(t.end_ms(), 600);
}

#[test]
fn a_zero_duration_clip_cannot_invert_the_span() {
    let t = lane(vec![AudioClip {
        start_ms: 900,
        ..AudioClip::new("a", "/a", 0)
    }]);
    assert!(t.end_ms() >= t.start_ms());
}

// --- technique_marker_test.dart ---

fn marker(id: &str, ms: i64) -> TechniqueMarker {
    TechniqueMarker {
        id: id.into(),
        position_ms: ms,
        end_ms: None,
        technique_id: format!("tech-{id}"),
        technique_name: format!("Twang {id}"),
        created_at: DateTime::local_ymd(2026, 6, 20),
        updated_at: DateTime::local_ymd(2026, 6, 20),
    }
}

#[test]
fn technique_marker_round_trips_through_json() {
    let m = marker("1", 1500);
    let back = TechniqueMarker::from_json(&m.to_json()).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.position_ms, 1500);
    assert_eq!(back.technique_id, "tech-1");
    assert_eq!(back.technique_name, "Twang 1");
}

#[test]
fn technique_marker_copy_with_preserves_id_and_created_at_and_updates_fields() {
    let m = marker("1", 1000);
    let moved = TechniqueMarker {
        position_ms: 2000,
        technique_name: "Belt".into(),
        ..m.clone()
    };
    assert_eq!(moved.id, m.id);
    assert_eq!(moved.created_at, m.created_at);
    assert_eq!(moved.position_ms, 2000);
    assert_eq!(moved.technique_name, "Belt");
    assert_eq!(moved.technique_id, m.technique_id);
}

fn marked_track(markers: Vec<TechniqueMarker>) -> AudioTrack {
    AudioTrack {
        technique_markers: markers,
        ..AudioTrack::new(
            "t1",
            AudioTrackKind::Vocal,
            "Take 1",
            DateTime::local_ymd(2026, 6, 20),
        )
        .with_audio("/x.wav", 0, 1000, vec![])
    }
}

#[test]
fn round_trips_markers_through_json() {
    let t = marked_track(vec![marker("1", 500), marker("2", 1200)]);
    let back = AudioTrack::from_json(&t.to_json()).unwrap();
    assert_eq!(back.technique_markers.len(), 2);
    assert_eq!(back.technique_markers[0].technique_name, "Twang 1");
    assert_eq!(back.technique_markers[1].position_ms, 1200);
}

#[test]
fn defaults_to_empty_and_survives_json_without_the_key() {
    assert!(marked_track(vec![]).technique_markers.is_empty());
    let json = without(marked_track(vec![]).to_json(), "techniqueMarkers");
    assert!(AudioTrack::from_json(&json)
        .unwrap()
        .technique_markers
        .is_empty());
}

#[test]
fn copy_with_replaces_markers_but_keeps_notes() {
    let t = marked_track(vec![marker("1", 100)]);
    let updated = AudioTrack {
        technique_markers: vec![marker("2", 200)],
        ..t.clone()
    };
    assert_eq!(updated.technique_markers.len(), 1);
    assert_eq!(updated.technique_markers[0].id, "2");
    // An update that leaves the markers alone keeps the existing ones.
    let renamed = AudioTrack {
        display_name: "X".into(),
        ..t
    };
    assert_eq!(renamed.technique_markers[0].id, "1");
}
