//! Port of `client/test/song_practice_test.dart`. Dart's `identical` on a
//! part is [`Arc::ptr_eq`]; on a list, [`same_parts`].

use std::sync::Arc;

use serde_json::{json, Value};
use vocara_model::models::audio_fx::TrackFx;
use vocara_model::models::audio_track::{
    default_take_color_index, AudioTrack, AudioTrackKind, TAKE_COLORS,
};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::{same_parts, SongPractice};
use vocara_model::models::take_version::TakeVersion;
use vocara_model::util::dart_date_time::DateTime;

fn d2026() -> DateTime {
    DateTime::local_ymd(2026, 1, 1)
}

fn vocal(id: &str, version_id: Option<&str>) -> AudioTrack {
    AudioTrack {
        version_id: version_id.map(Into::into),
        ..AudioTrack::new(id, AudioTrackKind::Vocal, id, d2026()).with_audio(
            format!("/tmp/{id}.wav"),
            0,
            1000,
            vec![],
        )
    }
}

fn note(id: &str) -> PracticeNote {
    PracticeNote {
        id: id.into(),
        position_ms: 100,
        end_ms: None,
        content: format!("note {id}"),
        created_at: d2026(),
        updated_at: d2026(),
    }
}

/// A lane of the given [kind] carrying [note_count] notes (ids are namespaced
/// by the lane id so they stay unique across lanes).
fn lane_with_notes(
    id: &str,
    kind: AudioTrackKind,
    note_count: usize,
    version_id: Option<&str>,
) -> AudioTrack {
    AudioTrack {
        version_id: version_id.map(Into::into),
        notes: (0..note_count)
            .map(|i| note(&format!("{id}-n{i}")))
            .collect(),
        ..AudioTrack::new(id, kind, id, d2026()).with_audio(
            format!("/tmp/{id}.wav"),
            0,
            1000,
            vec![],
        )
    }
}

fn version(id: &str, name: &str) -> Arc<TakeVersion> {
    Arc::new(TakeVersion::new(id, name, d2026()))
}

fn arcs(tracks: Vec<AudioTrack>) -> Vec<Arc<AudioTrack>> {
    tracks.into_iter().map(Arc::new).collect()
}

fn ids(tracks: &[Arc<AudioTrack>]) -> Vec<String> {
    tracks.iter().map(|t| t.id.clone()).collect()
}

fn base() -> SongPractice {
    SongPractice::new("p", "S", d2026(), d2026())
}

// --- SongPractice interpretations ---

#[test]
fn legacy_json_no_versions_migrates_to_a_default_interpretation() {
    let json = json!({
        "id": "p1",
        "title": "Song",
        // Two takes with no versionId (the pre-feature shape).
        "vocalTracks": [vocal("t1", None).to_json(), vocal("t2", None).to_json()],
        "createdAt": d2026().to_iso8601_string(),
        "updatedAt": d2026().to_iso8601_string(),
    });
    let p = SongPractice::from_json(&json).unwrap();
    assert_eq!(p.versions.len(), 1);
    assert_eq!(p.versions[0].name, "Interpretation 1");
    assert_eq!(
        p.active_version_id.as_deref(),
        Some(p.versions[0].id.as_str())
    );
    // Every legacy take is tagged into the default interpretation and visible.
    assert!(p
        .vocal_tracks
        .iter()
        .all(|t| t.version_id == p.active_version_id));
    assert_eq!(p.active_vocal_tracks().len(), 2);
}

#[test]
fn active_vocal_tracks_shows_only_the_active_interpretations_takes() {
    let p = SongPractice {
        versions: vec![version("v1", "Tender"), version("v2", "Powerful")],
        active_version_id: Some("v2".into()),
        vocal_tracks: arcs(vec![
            vocal("a", Some("v1")),
            vocal("b", Some("v2")),
            vocal("c", Some("v2")),
        ]),
        ..base()
    };
    assert_eq!(ids(&p.active_vocal_tracks()), ["b", "c"]);
    let switched = SongPractice {
        active_version_id: Some("v1".into()),
        ..p
    };
    assert_eq!(ids(&switched.active_vocal_tracks()), ["a"]);
}

#[test]
fn json_round_trips_versions_active_id_and_take_version_id() {
    let p = SongPractice {
        versions: vec![Arc::new(TakeVersion::new(
            "v1",
            "Tender",
            DateTime::local_ymd(2026, 1, 2),
        ))],
        active_version_id: Some("v1".into()),
        vocal_tracks: arcs(vec![vocal("a", Some("v1"))]),
        ..base()
    };
    let back = SongPractice::from_json(&p.to_json()).unwrap();
    assert_eq!(
        back.versions
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        ["Tender"]
    );
    assert_eq!(back.active_version_id.as_deref(), Some("v1"));
    assert_eq!(back.vocal_tracks[0].version_id.as_deref(), Some("v1"));
}

#[test]
fn separation_source_round_trips_through_json_and_defaults_to_none() {
    let p = SongPractice {
        separation_source: Some("/audio/p/original.mp3".into()),
        ..base()
    };
    assert_eq!(
        SongPractice::from_json(&p.to_json())
            .unwrap()
            .separation_source
            .as_deref(),
        Some("/audio/p/original.mp3")
    );
    // A practice that was never separated stays None.
    let cleared = SongPractice {
        separation_source: None,
        ..p
    };
    assert!(cleared.separation_source.is_none());
    let bare = json!({
        "id": "p",
        "title": "S",
        "createdAt": d2026().to_iso8601_string(),
        "updatedAt": d2026().to_iso8601_string(),
    });
    assert!(SongPractice::from_json(&bare)
        .unwrap()
        .separation_source
        .is_none());
}

#[test]
fn a_take_with_a_stale_deleted_version_version_id_is_reattached_not_lost() {
    let json = json!({
        "id": "p",
        "title": "S",
        "versions": [TakeVersion::new("v1", "Keep", d2026()).to_json()],
        "activeVersionId": "v1",
        "vocalTracks": [
            vocal("good", Some("v1")).to_json(),
            // Points to a version that no longer exists (orphaned id).
            vocal("orphan", Some("deleted")).to_json(),
        ],
        "createdAt": d2026().to_iso8601_string(),
        "updatedAt": d2026().to_iso8601_string(),
    });
    let p = SongPractice::from_json(&json).unwrap();
    // The orphan is healed into the active interpretation — never hidden.
    let orphan = p.vocal_tracks.iter().find(|t| t.id == "orphan").unwrap();
    assert_eq!(orphan.version_id.as_deref(), Some("v1"));
    let active = ids(&p.active_vocal_tracks());
    assert!(active.contains(&"good".to_string()) && active.contains(&"orphan".to_string()));
}

#[test]
fn note_count_counts_the_reference_lane_not_just_accompaniment_and_takes() {
    // The reference lane can carry notes (note updates route to it, and sync
    // can deliver them), so it must never be silently uncounted.
    let p = SongPractice {
        accompaniment: Some(Arc::new(lane_with_notes(
            "acc",
            AudioTrackKind::Accompaniment,
            1,
            None,
        ))),
        reference: Some(Arc::new(lane_with_notes(
            "ref",
            AudioTrackKind::Reference,
            2,
            None,
        ))),
        vocal_tracks: arcs(vec![lane_with_notes(
            "take",
            AudioTrackKind::Vocal,
            3,
            None,
        )]),
        ..base()
    };
    assert_eq!(p.note_count(), 6);
    // Without a reference the count still works (nullable lane).
    let no_reference = SongPractice {
        reference: None,
        ..p
    };
    assert_eq!(no_reference.note_count(), 4);
}

#[test]
fn active_note_count_counts_both_backings_plus_only_the_active_interpretations_takes() {
    // Deliberate semantics: the Accompaniment / Original toggle is transient
    // view-state on the detail screen's mixer, which the model cannot know,
    // so active_note_count is "active interpretation" — not "visible on
    // screen".
    let p = SongPractice {
        versions: vec![version("v1", "Tender"), version("v2", "Powerful")],
        active_version_id: Some("v2".into()),
        accompaniment: Some(Arc::new(lane_with_notes(
            "acc",
            AudioTrackKind::Accompaniment,
            1,
            None,
        ))),
        reference: Some(Arc::new(lane_with_notes(
            "ref",
            AudioTrackKind::Reference,
            2,
            None,
        ))),
        vocal_tracks: arcs(vec![
            lane_with_notes("a", AudioTrackKind::Vocal, 4, Some("v1")),
            lane_with_notes("b", AudioTrackKind::Vocal, 8, Some("v2")),
        ]),
        ..base()
    };
    // Accompaniment (1) + reference (2) + active take b (8); the inactive
    // interpretation's take a (4) stays out.
    assert_eq!(p.active_note_count(), 11);
    // Every lane counts in the total regardless of interpretation.
    assert_eq!(p.note_count(), 15);
}

#[test]
fn a_stale_active_version_id_falls_back_to_the_first_interpretation() {
    let p = SongPractice {
        versions: vec![version("v1", "A")],
        active_version_id: Some("gone".into()),
        ..base()
    };
    assert_eq!(p.active_version().map(|v| v.id.as_str()), Some("v1")); // getter resolves the stale id
    assert_eq!(
        SongPractice::from_json(&p.to_json())
            .unwrap()
            .active_version_id
            .as_deref(),
        Some("v1")
    ); // and load repairs it
}

// --- SongPractice take numbering ---

fn practice_with(names: &[&str]) -> SongPractice {
    SongPractice {
        vocal_tracks: names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Arc::new(AudioTrack {
                    display_name: (*name).into(),
                    ..vocal(&format!("t{i}"), None)
                })
            })
            .collect(),
        ..base()
    }
}

#[test]
fn counts_plain_and_recovered_take_names_alike() {
    // The one shared numbering for new takes AND boot recovery.
    assert_eq!(practice_with(&[]).next_take_number(), 1);
    assert_eq!(practice_with(&["Take 1", "Take 2"]).next_take_number(), 3);
    // A recovered take's number must be visible to the normal path…
    assert_eq!(
        practice_with(&["Take 1", "Take 3 (recovered)"]).next_take_number(),
        4
    );
    // …and renamed/foreign labels stay out, as before.
    assert_eq!(
        practice_with(&["Chorus try", "Take 2 (edited)"]).next_take_number(),
        1
    );
}

#[test]
fn an_absurd_take_number_neither_throws_nor_wraps_negative() {
    // 20 digits overflowed int.parse (FormatException on every new take and
    // every boot recovery); max-int made max + 1 wrap negative.
    assert_eq!(
        practice_with(&["Take 2", "Take 99999999999999999999"]).next_take_number(),
        3
    );
    assert_eq!(
        practice_with(&["Take 2", "Take 9223372036854775807"]).next_take_number(),
        3
    );
    // Large-but-sane numbers still count.
    assert_eq!(
        practice_with(&["Take 2", "Take 1000000"]).next_take_number(),
        1000001
    );
}

// --- SongPractice take colours ---

/// A take `id` made `minute` minutes into 2026, coloured `color`.
fn take_made(id: &str, minute: i64, color: Option<i64>) -> Arc<AudioTrack> {
    let made =
        DateTime::from_millis_since_epoch(d2026().millis_since_epoch() + minute * 60_000, false)
            .expect("a time");
    Arc::new(AudioTrack {
        created_at: made,
        color_value: color,
        ..vocal(id, None)
    })
}

fn practice_of(takes: Vec<Arc<AudioTrack>>) -> SongPractice {
    SongPractice {
        vocal_tracks: takes,
        ..base()
    }
}

#[test]
fn a_new_take_gets_the_colour_after_the_newest_takes() {
    // Hashed from each take's id, three takes in six wore sage, one of them
    // right under the take it was duplicated from, and no take wore lilac.
    let [coral, ochre, sage, periwinkle, lilac] = TAKE_COLORS;
    assert_eq!(practice_of(Vec::new()).next_take_color(), coral);
    let p = practice_of(vec![take_made("a", 0, Some(coral))]);
    assert_eq!(p.next_take_color(), ochre);
    // The newest by when it was made, wherever it is listed.
    let p = practice_of(vec![
        take_made("a", 5, Some(periwinkle)),
        take_made("b", 0, Some(sage)),
    ]);
    assert_eq!(p.next_take_color(), lilac);
    // After the last colour, the first again.
    let p = practice_of(vec![take_made("a", 0, Some(lilac))]);
    assert_eq!(p.next_take_color(), coral);
    // A take with no colour of its own counts as the one it shows.
    let shown = TAKE_COLORS[default_take_color_index("old")];
    let after = TAKE_COLORS[(default_take_color_index("old") + 1) % TAKE_COLORS.len()];
    assert_ne!(shown, after);
    let p = practice_of(vec![take_made("old", 0, None)]);
    assert_eq!(p.next_take_color(), after);
    // One off the palette (an old saturated tint) starts it over.
    let p = practice_of(vec![take_made("a", 0, Some(0xFF00_FF00))]);
    assert_eq!(p.next_take_color(), coral);
    // Made in the same instant, the one listed last is the newest.
    let p = practice_of(vec![
        take_made("a", 0, Some(coral)),
        take_made("b", 0, Some(sage)),
    ]);
    assert_eq!(p.next_take_color(), periwinkle);
}

#[test]
fn numbers_over_the_active_interpretation_only() {
    let p = SongPractice {
        versions: vec![version("v1", "A"), version("v2", "B")],
        active_version_id: Some("v1".into()),
        vocal_tracks: arcs(vec![
            AudioTrack {
                display_name: "Take 1".into(),
                ..vocal("a", Some("v1"))
            },
            AudioTrack {
                display_name: "Take 9".into(),
                ..vocal("b", Some("v2"))
            },
        ]),
        ..base()
    };
    assert_eq!(p.next_take_number(), 2);
}

// --- SongPractice.sharing ---

fn practice(
    vocals: Option<Vec<AudioTrack>>,
    backing: Option<AudioTrack>,
    buses: Option<Vec<FxBus>>,
    lyrics: Vec<LyricLine>,
) -> SongPractice {
    SongPractice {
        accompaniment: backing.map(Arc::new),
        vocal_tracks: arcs(
            vocals.unwrap_or_else(|| vec![vocal("a", None), vocal("b", None), vocal("c", None)]),
        ),
        versions: vec![version("v", "I")],
        active_version_id: Some("v".into()),
        lyrics,
        fx_buses: buses
            .unwrap_or_else(|| vec![FxBus::new("x", "Verb", TrackFx::flat())])
            .into_iter()
            .map(Arc::new)
            .collect(),
        ..SongPractice::new("p", "Song", d2026(), d2026())
    }
}

fn plain() -> SongPractice {
    practice(None, None, None, vec![])
}

#[test]
fn keeps_every_equal_part_and_each_list_all_of_whose_parts_it_kept() {
    let la = || vec![LyricLine::new("la", None)];
    let acc = || lane_with_notes("acc", AudioTrackKind::Accompaniment, 2, None);
    let held = practice(None, Some(acc()), None, la());
    let shared = practice(None, Some(acc()), None, la()).sharing(&held);
    assert_eq!(shared, held);
    assert!(Arc::ptr_eq(
        shared.accompaniment.as_ref().unwrap(),
        held.accompaniment.as_ref().unwrap()
    ));
    assert!(same_parts(&shared.vocal_tracks, &held.vocal_tracks));
    assert!(same_parts(&shared.versions, &held.versions));
    assert!(same_parts(&shared.fx_buses, &held.fx_buses));
    assert_eq!(shared.lyrics, held.lyrics);
}

#[test]
fn takes_what_changed_and_a_new_list_around_it() {
    let held = plain();
    let shared = practice(
        Some(vec![
            vocal("a", None),
            AudioTrack {
                muted: true,
                ..vocal("b", None)
            },
            vocal("c", None),
        ]),
        None,
        None,
        vec![],
    )
    .sharing(&held);
    assert!(shared.vocal_tracks[1].muted);
    assert!(Arc::ptr_eq(&shared.vocal_tracks[0], &held.vocal_tracks[0]));
    assert!(Arc::ptr_eq(&shared.vocal_tracks[2], &held.vocal_tracks[2]));
    assert!(!same_parts(&shared.vocal_tracks, &held.vocal_tracks));
    assert!(same_parts(&shared.fx_buses, &held.fx_buses));
}

#[test]
fn follows_a_reorder_an_addition_and_a_removal() {
    let held = plain();
    let reordered = practice(
        Some(vec![vocal("c", None), vocal("a", None), vocal("b", None)]),
        None,
        None,
        vec![],
    )
    .sharing(&held);
    assert_eq!(ids(&reordered.vocal_tracks), ["c", "a", "b"]);
    assert!(Arc::ptr_eq(
        &reordered.vocal_tracks[0],
        &held.vocal_tracks[2]
    ));
    assert!(!same_parts(&reordered.vocal_tracks, &held.vocal_tracks));

    let grown = practice(
        Some(vec![
            vocal("a", None),
            vocal("b", None),
            vocal("c", None),
            vocal("d", None),
        ]),
        None,
        None,
        vec![],
    )
    .sharing(&held);
    assert_eq!(ids(&grown.vocal_tracks), ["a", "b", "c", "d"]);
    assert!(!same_parts(&grown.vocal_tracks, &held.vocal_tracks));

    let shrunk = practice(
        Some(vec![vocal("a", None), vocal("b", None)]),
        None,
        None,
        vec![],
    )
    .sharing(&held);
    assert_eq!(ids(&shrunk.vocal_tracks), ["a", "b"]);
    assert!(Arc::ptr_eq(&shrunk.vocal_tracks[1], &held.vocal_tracks[1]));

    let cleared = practice(None, None, Some(vec![]), vec![]).sharing(&held);
    assert!(cleared.fx_buses.is_empty());
}

#[test]
fn keeps_every_scalar_and_a_backing_that_came_or_went() {
    let held = practice(
        None,
        Some(lane_with_notes(
            "acc",
            AudioTrackKind::Accompaniment,
            0,
            None,
        )),
        None,
        vec![],
    );
    let fresh = SongPractice {
        title: "Renamed".into(),
        updated_at: DateTime::local_ymd(2027, 1, 1),
        vocal_tracks: held.vocal_tracks.clone(),
        versions: held.versions.clone(),
        active_version_id: Some("v".into()),
        timeline_duration_ms: 9000,
        separation_source: Some("/song.wav".into()),
        master_volume: 0.5,
        ..SongPractice::new("p", "Song", d2026(), d2026())
    };
    let shared = fresh.sharing(&held);
    assert_eq!(shared, fresh);
    assert!(shared.accompaniment.is_none());
    let gone = SongPractice {
        accompaniment: None,
        ..held.clone()
    };
    assert_eq!(plain().sharing(&gone), plain());
    let came = Arc::new(lane_with_notes(
        "acc",
        AudioTrackKind::Accompaniment,
        0,
        None,
    ));
    let with_came = SongPractice {
        accompaniment: Some(Arc::clone(&came)),
        ..plain()
    };
    assert!(Arc::ptr_eq(
        with_came.sharing(&plain()).accompaniment.as_ref().unwrap(),
        &came
    ));
}

#[test]
fn ignores_another_practices_parts() {
    let other = SongPractice {
        vocal_tracks: arcs(vec![vocal("a", None)]),
        ..SongPractice::new("q", "Other", d2026(), d2026())
    };
    let fresh = plain();
    let shared = fresh.sharing(&other);
    assert!(same_parts(&shared.vocal_tracks, &fresh.vocal_tracks));
    assert!(same_parts(&shared.fx_buses, &fresh.fx_buses));
    assert!(same_parts(&shared.versions, &fresh.versions));
    assert_eq!(shared, fresh);
}

#[test]
fn references_audio_sees_every_lane() {
    let p = SongPractice {
        accompaniment: Some(Arc::new(lane_with_notes(
            "acc",
            AudioTrackKind::Accompaniment,
            0,
            None,
        ))),
        vocal_tracks: arcs(vec![vocal("a", None)]),
        ..base()
    };
    assert!(p.references_audio("/tmp/acc.wav"));
    assert!(p.references_audio("/tmp/a.wav"));
    assert!(!p.references_audio("/tmp/b.wav"));
    let _: Value = p.to_json();
}
