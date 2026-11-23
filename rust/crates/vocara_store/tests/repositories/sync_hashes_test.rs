//! Port of `client/test/sync_hashes_test.dart`.
//!
//! The whole base-hash scheme rests on ONE invariant: for an UNCHANGED
//! entity, the hash of the loaded row (the base/hydrate side) equals the
//! hash the save path computes from the hydrated domain aggregate. If any
//! entity's two projections drift apart, every save either writes phantom
//! updates or — worse — wrongly skips real ones. These tests prove the
//! agreement for every entity type, both directly (recompute the save-side
//! hash from the loaded aggregate and compare with the returned base) and
//! end-to-end (a with-base re-save of an untouched full graph writes NOTHING
//! and returns an identical base map).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx, TrackSend};
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::dart_json;
use vocara_model::util::id::new_id;
use vocara_store::db::schema::SYNCED_TABLES;
use vocara_store::repositories::TechniqueRepository;
use vocara_store::sync::sync_hashes::{
    clip_content_hash, fx_bus_content_hash, interpretation_content_hash, practice_content_hash,
    practice_note_content_hash, technique_content_hash, technique_marker_content_hash,
    technique_media_content_hash, track_content_hash, ClipContent, FxBusContent,
    InterpretationContent, Loaded, PracticeContent, PracticeNoteContent, TechniqueContent,
    TechniqueMarkerContent, TechniqueMediaContent, TrackContent,
};

use super::{arcs, d2026, lane, take, Fixture};

fn encode(v: &Value) -> String {
    dart_json::encode(v).unwrap()
}

fn sends_json(sends: &[TrackSend]) -> String {
    encode(&Value::Array(
        sends.iter().map(TrackSend::to_json).collect(),
    ))
}

/// Saves a practice exercising EVERY reconciled entity type (practice row
/// with reverb + lyrics + separation source, interpretations, fx bus with
/// fx/sends, accompaniment + vocal tracks with fx/sends, clips, notes with
/// and without a range, markers) and returns the loaded editing session.
fn seed_full_graph(f: &Fixture) -> Loaded<SongPractice> {
    let base = f.create("Full graph");
    let version_id = base.versions[0].id.clone();
    let bus_id = new_id();

    let accompaniment = AudioTrack {
        clips: vec![AudioClip {
            start_ms: 250,
            ..AudioClip::new(new_id(), f.file("backing.wav", &[1, 2, 3]), 4000)
        }],
        ..lane(AudioTrackKind::Accompaniment, "Backing")
    };
    let vocal = AudioTrack {
        color_value: Some(0xFF00FF00),
        muted: true,
        soloed: false,
        volume: 0.7,
        pan: -0.2,
        bus_id: Some(bus_id.clone()),
        fx: Some(TrackFx::flat()),
        sends: vec![TrackSend::new(bus_id.clone(), -6.0)],
        clips: vec![AudioClip {
            start_ms: 100,
            ..AudioClip::new(new_id(), f.file("take.wav", &[9, 8, 7]), 3000)
        }],
        notes: vec![
            PracticeNote {
                id: new_id(),
                position_ms: 1200,
                end_ms: None,
                content: "point note".into(),
                created_at: d2026(),
                updated_at: d2026(),
            },
            PracticeNote {
                id: new_id(),
                position_ms: 1500,
                end_ms: Some(2500),
                content: "range note".into(),
                created_at: d2026(),
                updated_at: d2026(),
            },
        ],
        technique_markers: vec![TechniqueMarker {
            id: new_id(),
            position_ms: 2000,
            end_ms: Some(2600),
            technique_id: "tech-1".into(),
            technique_name: "Twang".into(),
            created_at: d2026(),
            updated_at: d2026(),
        }],
        ..take("Take 1", Some(&version_id))
    };
    let bus = FxBus {
        color_value: Some(0xFF112233),
        volume: 0.9,
        pan: 0.1,
        muted: false,
        soloed: true,
        output_bus_id: None,
        sends: Vec::new(),
        ..FxBus::new(bus_id, "Reverb bus", TrackFx::flat())
    };

    let enriched = SongPractice {
        title: "Full graph".into(),
        accompaniment: Some(Arc::new(accompaniment)),
        vocal_tracks: arcs(vec![vocal]),
        active_version_id: Some(version_id),
        lyrics: vec![
            LyricLine::new("line one", Some(0)),
            LyricLine::new("line two", None),
        ],
        timeline_duration_ms: 30000,
        separation_source: Some(f.file("mix.wav", &[4, 4, 4, 4])),
        reverb: Some(ReverbSettings {
            room_size: 0.6,
            wet: 0.3,
            ..ReverbSettings::DEFAULT
        }),
        master_volume: 0.8,
        fx_buses: arcs(vec![bus]),
        ..base.clone()
    };
    f.save(&enriched);
    f.load(&base.id)
}

#[test]
fn every_practice_entity_type_the_loaded_row_base_hash_equals_the_recomputed_save_side_hash() {
    let f = Fixture::new();
    let session = seed_full_graph(&f);
    let practice = &session.aggregate;
    let base_hashes = &session.base_hashes;

    // --- practice row (JSON columns re-encoded exactly like the save path;
    // the separation asset id is store-assigned, read from the row).
    let separation_asset_id = f.column::<String>("practices", "separation_asset_id", &practice.id);
    let reverb_json = encode(&practice.reverb.unwrap().to_json());
    let lyrics_json = encode(&Value::Array(
        practice.lyrics.iter().map(LyricLine::to_json).collect(),
    ));
    assert_eq!(
        base_hashes[&practice.id],
        practice_content_hash(&PracticeContent {
            title: &practice.title,
            timeline_duration_ms: practice.timeline_duration_ms,
            master_volume: practice.master_volume,
            active_interpretation_id: practice.active_version_id.as_deref(),
            separation_asset_id: Some(&separation_asset_id),
            reverb_json: Some(&reverb_json),
            lyrics_json: Some(&lyrics_json),
        })
        .unwrap(),
        "practices projection must agree"
    );

    // --- interpretations (order index = list position, like the save).
    for (i, v) in practice.versions.iter().enumerate() {
        assert_eq!(
            base_hashes[&v.id],
            interpretation_content_hash(&InterpretationContent {
                name: &v.name,
                order_index: i as i64,
            }),
            "interpretations projection must agree"
        );
    }

    // --- tracks, flattened in save order (accompaniment, reference,
    // vocals); a take's order index is its position among the takes, a
    // slot's is 0.
    let all_tracks: Vec<&Arc<AudioTrack>> = practice
        .accompaniment
        .iter()
        .chain(practice.reference.iter())
        .chain(practice.vocal_tracks.iter())
        .collect();
    for t in all_tracks {
        let i = if t.is_backing() {
            0
        } else {
            practice
                .vocal_tracks
                .iter()
                .position(|v| Arc::ptr_eq(v, t))
                .unwrap() as i64
        };
        let fx_json = t.fx.as_ref().map(|fx| encode(&fx.to_json()));
        assert_eq!(
            base_hashes[&t.id],
            track_content_hash(&TrackContent {
                kind: t.kind.name(),
                interpretation_id: t.version_id.as_deref(),
                display_name: &t.display_name,
                color_value: t.color_value,
                muted: t.muted,
                soloed: t.soloed,
                volume: t.volume,
                pan: t.pan,
                bus_id: t.bus_id.as_deref(),
                fx_json: fx_json.as_deref(),
                sends_json: &sends_json(&t.sends),
                order_index: i,
            })
            .unwrap(),
            "tracks projection must agree ({})",
            t.display_name
        );

        // --- clips of this track (the asset id is store-assigned).
        for (c, clip) in t.clips.iter().enumerate() {
            assert_eq!(
                base_hashes[&clip.id],
                clip_content_hash(&ClipContent {
                    asset_id: &f.column::<String>("clips", "asset_id", &clip.id),
                    start_ms: clip.start_ms,
                    duration_ms: clip.duration_ms,
                    order_index: c as i64,
                }),
                "clips projection must agree"
            );
        }

        // --- notes (point and range) and markers.
        for n in &t.notes {
            assert_eq!(
                base_hashes[&n.id],
                practice_note_content_hash(&PracticeNoteContent {
                    position_ms: n.position_ms,
                    end_ms: n.end_ms,
                    content: &n.content,
                }),
                "practice_notes projection must agree"
            );
        }
        for m in &t.technique_markers {
            assert_eq!(
                base_hashes[&m.id],
                technique_marker_content_hash(&TechniqueMarkerContent {
                    position_ms: m.position_ms,
                    end_ms: m.end_ms,
                    technique_id: &m.technique_id,
                    technique_name: &m.technique_name,
                }),
                "technique_markers projection must agree"
            );
        }
    }

    // --- fx buses.
    for (i, b) in practice.fx_buses.iter().enumerate() {
        assert_eq!(
            base_hashes[&b.id],
            fx_bus_content_hash(&FxBusContent {
                name: &b.name,
                color_value: b.color_value,
                volume: b.volume,
                pan: b.pan,
                muted: b.muted,
                soloed: b.soloed,
                output_bus_id: b.output_bus_id.as_deref(),
                fx_json: &encode(&b.fx.to_json()),
                sends_json: &sends_json(&b.sends),
                order_index: i as i64,
            })
            .unwrap(),
            "fx_buses projection must agree"
        );
    }

    // Every reconciled row is covered: practice + 1 interpretation + 2
    // tracks + 2 clips + 2 notes + 1 marker + 1 bus.
    assert_eq!(base_hashes.len(), 10);
}

#[test]
fn end_to_end_a_with_base_resave_of_an_untouched_full_graph_writes_nothing() {
    // And returns an identical base map.
    let f = Fixture::new();
    let session = seed_full_graph(&f);
    let before = f.oplog().len();

    let again = f
        .save_with_base(&session.aggregate, &session.base_hashes)
        .unwrap();
    assert_eq!(
        f.oplog().len(),
        before,
        "any new op means a load-side and a save-side projection disagree"
    );
    assert_eq!(
        again.base_hashes, session.base_hashes,
        "an untouched graph must round-trip to identical base hashes"
    );
}

#[test]
fn a_flat_base_hashes_map_is_sound_no_two_synced_tables_can_hold_the_same_row_id() {
    // reconcile keys BASE by bare row id across the whole aggregate, so two
    // tables sharing an id would cross-contaminate — a track's base hash
    // answering for a clip, which decides the three-way rules and can send
    // an UPDATE down the tombstone branch. That is safe only because of how
    // ids are minted: ULIDs everywhere, plus the legacy `<trackId>-c0` clip
    // shape, which carries a '-' no ULID contains. This test is the tripwire
    // against a future id grammar that breaks the assumption silently.
    let f = Fixture::new();
    seed_full_graph(&f);
    let mut seen: HashMap<String, &str> = HashMap::new();
    for table in SYNCED_TABLES {
        let ids: Vec<String> =
            f.db.query(&format!("SELECT id FROM \"{table}\""), [], |r| r.get(0))
                .unwrap();
        for id in ids {
            let owner = seen.get(&id);
            assert!(
                owner.is_none(),
                "id \"{id}\" is in both \"{}\" and \"{table}\"",
                owner.unwrap()
            );
            seen.insert(id, table);
        }
    }
    assert!(!seen.is_empty(), "the seeded graph must have rows");
}

#[test]
fn technique_and_media_base_hashes_equal_recomputed_save_side_hashes() {
    // And an untouched with-base re-save writes nothing.
    let f = Fixture::new();
    let t = Technique {
        aliases: vec!["Belt".into()],
        tags: vec!["CVT".into(), "Effect".into()],
        summary: "loud + bright".into(),
        description: "a description".into(),
        cues: "call out".into(),
        exercises: "hey hey hey".into(),
        acoustic_signature: "strong H2".into(),
        media: vec![TechniqueMedia {
            id: new_id(),
            path: f.file("ex.wav", &[1, 1, 2]),
            kind: TechniqueMediaKind::Audio,
            label: "demo".into(),
            added_at: d2026(),
        }],
        ..Technique::new(new_id(), "Belting", d2026(), d2026())
    };
    f.save_technique(&t);

    let session = f.techs.load_technique(&t.id).unwrap();
    let loaded = &session.aggregate;
    let strings = |items: &[String]| {
        encode(&Value::Array(
            items.iter().map(|s| Value::String(s.clone())).collect(),
        ))
    };

    // --- technique row (JSON re-encoded exactly like the save path).
    assert_eq!(
        session.base_hashes[&loaded.id],
        technique_content_hash(&TechniqueContent {
            name: &loaded.name,
            aliases_json: &strings(&loaded.aliases),
            tags_json: &strings(&loaded.tags),
            summary: &loaded.summary,
            description: &loaded.description,
            cues: &loaded.cues,
            exercises: &loaded.exercises,
            acoustic_signature: &loaded.acoustic_signature,
        }),
        "techniques projection must agree"
    );

    // --- media rows (the asset id is store-assigned).
    for (i, m) in loaded.media.iter().enumerate() {
        assert_eq!(
            session.base_hashes[&m.id],
            technique_media_content_hash(&TechniqueMediaContent {
                asset_id: &f.column::<String>("technique_media", "asset_id", &m.id),
                kind: if m.is_video() { "video" } else { "audio" },
                label: &m.label,
                order_index: i as i64,
            }),
            "technique_media projection must agree"
        );
    }
    assert_eq!(session.base_hashes.len(), 2); // technique + 1 media

    // --- end-to-end no-op.
    let before = f.oplog().len();
    let again = f
        .techs
        .save_technique_with_base(loaded, &session.base_hashes)
        .unwrap();
    assert_eq!(f.oplog().len(), before);
    assert_eq!(again.base_hashes, session.base_hashes);
}
