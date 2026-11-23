//! Port of the repository groups of `client/test/reconcile_three_way_test.dart`
//! (its unit group is `sync::reconcile`'s own tests).
//!
//! Three-way reconcile: BASE (what the editor loaded) vs MINE (what the
//! editor wants) vs DB (what is persisted now). These replay the verbatim
//! regression scenarios with TWO sequential edit sessions against one
//! database, simulating a remote apply with raw SQL (adopting a foreign HLC
//! and appending NO oplog, exactly like the SyncApplier).

use std::sync::Arc;

use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::id::new_id;
use vocara_store::db::AssetStore;
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};

use super::{d2026, lane, note, take, with_takes, Fixture};

/// The foreign HLC every simulated remote apply stamps its row with.
const REMOTE_HLC: &str =
    "hlc_wall = 9999999999999, hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED'";

mod practice_merge {
    use super::*;

    /// Seeds a practice holding one vocal take with one clip and one note,
    /// through the normal local save path.
    fn seed_practice(f: &Fixture) -> SongPractice {
        let base = f.create("Song");
        let vocal = AudioTrack {
            clips: vec![AudioClip::new(
                new_id(),
                f.file("take.wav", &[1, 2, 3]),
                900,
            )],
            notes: vec![PracticeNote {
                position_ms: 100,
                ..note("original note")
            }],
            ..take("Take 1", Some(&base.versions[0].id))
        };
        f.save(&with_takes(&base, vec![vocal]))
    }

    /// A "remote apply" over raw SQL: content + foreign HLC, NO oplog — the
    /// SyncApplier contract, so these rows look exactly like synced-in
    /// state.
    fn remote_sql(f: &Fixture, sql: &str, params: impl rusqlite::Params) {
        f.raw(sql, params);
    }

    fn is_deleted(f: &Fixture, table: &str, id: &str) -> bool {
        f.column::<Option<i64>>(table, "deleted_at", id).is_some()
    }

    #[test]
    fn regression_a_a_stale_save_must_not_revert_a_remote_updated_row_the_user_never_touched() {
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let note_id = seeded.vocal_tracks[0].notes[0].id.clone();

        // Session: the editor loads (and holds) the aggregate.
        let session = f.load(&seeded.id);

        // A remote apply lands while the aggregate is open.
        remote_sql(
            &f,
            &format!("UPDATE practice_notes SET content = ?, {REMOTE_HLC} WHERE id = ?"),
            ["remote-edited", note_id.as_str()],
        );
        let note_ops = f.ops_for("practice_notes", &note_id);

        // The user edits ONLY the title, then saves the (now stale)
        // aggregate.
        let saved = f
            .save_with_base(
                &SongPractice {
                    title: "Renamed".into(),
                    ..session.aggregate.clone()
                },
                &session.base_hashes,
            )
            .unwrap();

        // The remote note update survives: content, HLC, and no echo op.
        assert_eq!(
            f.column::<String>("practice_notes", "content", &note_id),
            "remote-edited"
        );
        assert_eq!(
            f.column::<String>("practice_notes", "hlc_packed", &note_id),
            "REMOTE-PACKED"
        );
        assert_eq!(f.ops_for("practice_notes", &note_id), note_ops);

        // The user's own edit did land.
        assert_eq!(saved.aggregate.title, "Renamed");
        // And the returned aggregate converges onto the remote value.
        assert_eq!(
            saved.aggregate.vocal_tracks[0].notes[0].content,
            "remote-edited"
        );
    }

    #[test]
    fn regression_b_a_stale_save_must_not_tombstone_rows_created_remotely_while_open() {
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let track_id = seeded.vocal_tracks[0].id.clone();
        let session = f.load(&seeded.id);

        // Remote creates a note under the existing track, a whole new vocal
        // track, and an fx bus — none of which the open editor knows about.
        let remote_note = new_id();
        let remote_track = new_id();
        let remote_bus = new_id();
        remote_sql(
            &f,
            "INSERT INTO practice_notes \
             (id, track_id, position_ms, content, created_at, updated_at, \
              hlc_wall, hlc_ctr, hlc_node, hlc_packed) \
             VALUES (?, ?, 5, 'remote note', 1, 1, 9999999999999, 0, 'REMOTE', \
             'REMOTE-PACKED')",
            [remote_note.as_str(), track_id.as_str()],
        );
        remote_sql(
            &f,
            "INSERT INTO tracks \
             (id, practice_id, kind, display_name, created_at, updated_at, \
              hlc_wall, hlc_ctr, hlc_node, hlc_packed) \
             VALUES (?, ?, 'vocal', 'Remote take', 1, 1, 9999999999999, 0, \
             'REMOTE', 'REMOTE-PACKED')",
            [remote_track.as_str(), seeded.id.as_str()],
        );
        remote_sql(
            &f,
            "INSERT INTO fx_buses \
             (id, practice_id, name, created_at, updated_at, \
              hlc_wall, hlc_ctr, hlc_node, hlc_packed) \
             VALUES (?, ?, 'Remote bus', 1, 1, 9999999999999, 0, 'REMOTE', \
             'REMOTE-PACKED')",
            [remote_bus.as_str(), seeded.id.as_str()],
        );

        // The stale save (a real local edit rides along).
        f.save_with_base(
            &SongPractice {
                title: "Renamed".into(),
                ..session.aggregate.clone()
            },
            &session.base_hashes,
        )
        .unwrap();

        // Every remote-created row is still live and un-journaled.
        for (table, id) in [
            ("practice_notes", &remote_note),
            ("tracks", &remote_track),
            ("fx_buses", &remote_bus),
        ] {
            assert!(!is_deleted(&f, table, id), "{table}/{id} must stay live");
            assert_eq!(f.ops_for(table, id), 0);
        }
    }

    #[test]
    fn delete_intent_wins_a_row_the_user_removed_is_tombstoned_though_remote_edited_it() {
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let note_id = seeded.vocal_tracks[0].notes[0].id.clone();
        let session = f.load(&seeded.id);

        remote_sql(
            &f,
            &format!("UPDATE practice_notes SET content = ?, {REMOTE_HLC} WHERE id = ?"),
            ["remote-edited", note_id.as_str()],
        );

        // The user explicitly removes the note they held.
        let track = &session.aggregate.vocal_tracks[0];
        let without = with_takes(
            &session.aggregate,
            vec![AudioTrack {
                notes: Vec::new(),
                ..(**track).clone()
            }],
        );
        f.save_with_base(&without, &session.base_hashes).unwrap();
        assert!(is_deleted(&f, "practice_notes", &note_id));
    }

    #[test]
    fn a_remote_tombstoned_row_an_untouched_stale_save_carries_is_not_revived() {
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let note_id = seeded.vocal_tracks[0].notes[0].id.clone();
        let session = f.load(&seeded.id);

        remote_sql(
            &f,
            &format!("UPDATE practice_notes SET deleted_at = 999, {REMOTE_HLC} WHERE id = ?"),
            [&note_id],
        );
        let note_ops = f.ops_for("practice_notes", &note_id);

        let saved = f
            .save_with_base(
                &SongPractice {
                    title: "Renamed".into(),
                    ..session.aggregate.clone()
                },
                &session.base_hashes,
            )
            .unwrap();
        assert!(is_deleted(&f, "practice_notes", &note_id));
        assert_eq!(f.ops_for("practice_notes", &note_id), note_ops);
        // The returned aggregate converges: the note is gone from the model.
        assert!(saved.aggregate.vocal_tracks[0].notes.is_empty());
    }

    #[test]
    fn edit_vs_delete_a_note_the_user_edited_revives_over_a_remote_tombstone() {
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let note_id = seeded.vocal_tracks[0].notes[0].id.clone();
        let session = f.load(&seeded.id);

        remote_sql(
            &f,
            &format!("UPDATE practice_notes SET deleted_at = 999, {REMOTE_HLC} WHERE id = ?"),
            [&note_id],
        );

        let track = &session.aggregate.vocal_tracks[0];
        let edited = with_takes(
            &session.aggregate,
            vec![AudioTrack {
                notes: vec![PracticeNote {
                    content: "my edit".into(),
                    ..track.notes[0].clone()
                }],
                ..(**track).clone()
            }],
        );
        f.save_with_base(&edited, &session.base_hashes).unwrap();
        assert!(
            !is_deleted(&f, "practice_notes", &note_id),
            "the active editor wins"
        );
        assert_eq!(
            f.column::<String>("practice_notes", "content", &note_id),
            "my edit"
        );
        assert_ne!(
            f.column::<String>("practice_notes", "hlc_packed", &note_id),
            "REMOTE-PACKED"
        );
    }

    #[test]
    fn practice_row_a_remote_title_change_survives_a_stale_child_only_save() {
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let session = f.load(&seeded.id);

        remote_sql(
            &f,
            &format!("UPDATE practices SET title = ?, {REMOTE_HLC} WHERE id = ?"),
            ["remote-title", seeded.id.as_str()],
        );
        let practice_ops = f.ops_for("practices", &seeded.id);

        // The user only adds a note (a child edit); the practice-level
        // fields in the stale aggregate still carry the OLD title.
        let track = &session.aggregate.vocal_tracks[0];
        let mut notes = track.notes.clone();
        notes.push(PracticeNote {
            position_ms: 42,
            ..note("new local note")
        });
        let saved = f
            .save_with_base(
                &with_takes(
                    &session.aggregate,
                    vec![AudioTrack {
                        notes,
                        ..(**track).clone()
                    }],
                ),
                &session.base_hashes,
            )
            .unwrap();
        assert_eq!(
            f.column::<String>("practices", "title", &seeded.id),
            "remote-title",
            "the untouched practice row must not be reverted"
        );
        // No practices op was journaled (the local updatedAt ordering bump
        // is deliberately un-journaled).
        assert_eq!(f.ops_for("practices", &seeded.id), practice_ops);
        assert_eq!(saved.aggregate.title, "remote-title");
    }

    #[test]
    fn practice_row_conflict_the_active_editor_wins_the_whole_row() {
        // The documented policy.
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let session = f.load(&seeded.id);

        remote_sql(
            &f,
            &format!("UPDATE practices SET title = ?, {REMOTE_HLC} WHERE id = ?"),
            ["remote-title", seeded.id.as_str()],
        );

        // The user ALSO touched the practice row (its one content hash spans
        // all practice-level fields), so this is a true conflict: local wins.
        f.save_with_base(
            &SongPractice {
                master_volume: 0.5,
                ..session.aggregate.clone()
            },
            &session.base_hashes,
        )
        .unwrap();
        assert_eq!(
            f.column::<f64>("practices", "master_volume", &seeded.id),
            0.5
        );
        assert_eq!(
            f.column::<String>("practices", "title", &seeded.id),
            "Song",
            "the whole-row local write includes the stale title — the \
             documented cost of row-granularity conflict resolution"
        );
        assert_ne!(
            f.column::<String>("practices", "hlc_packed", &seeded.id),
            "REMOTE-PACKED"
        );
    }

    #[test]
    fn saving_over_a_tombstoned_practice_still_aborts_no_zombie_children() {
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let session = f.load(&seeded.id);
        f.songs.delete_practice(&seeded.id).unwrap();

        let saved = f.save_with_base(
            &SongPractice {
                title: "Zombie".into(),
                ..session.aggregate.clone()
            },
            &session.base_hashes,
        );
        assert!(saved.is_err());
        assert_eq!(
            f.live_count("tracks"),
            0,
            "the abort must leave no live children"
        );
    }

    #[test]
    fn documented_edge_a_remote_created_slot_row_colliding_with_a_local_slot_insert_fails_the_save()
    {
        // Instead of deleting the remote row.
        let f = Fixture::new();
        let seeded = seed_practice(&f);
        let session = f.load(&seeded.id);

        // Remote creates an accompaniment (the single-slot kind) while the
        // local editor also imports one.
        let remote_accomp = new_id();
        remote_sql(
            &f,
            "INSERT INTO tracks \
             (id, practice_id, kind, display_name, created_at, updated_at, \
              hlc_wall, hlc_ctr, hlc_node, hlc_packed) \
             VALUES (?, ?, 'accompaniment', 'Remote backing', 1, 1, \
             9999999999999, 0, 'REMOTE', 'REMOTE-PACKED')",
            [remote_accomp.as_str(), seeded.id.as_str()],
        );

        let saved = f.save_with_base(
            &SongPractice {
                accompaniment: Some(Arc::new(AudioTrack {
                    clips: vec![AudioClip::new(
                        new_id(),
                        f.file("backing.wav", &[7, 7, 7]),
                        100,
                    )],
                    ..lane(AudioTrackKind::Accompaniment, "My backing")
                })),
                ..session.aggregate.clone()
            },
            &session.base_hashes,
        );

        // Rule 4 refuses to tombstone the remote row, so the local insert
        // hits the partial-UNIQUE slot and the whole save fails loudly.
        assert!(saved.is_err());
        assert!(
            !is_deleted(&f, "tracks", &remote_accomp),
            "the remote-created accompaniment must survive"
        );
    }
}

mod technique_merge {
    use super::*;

    fn seed_technique(f: &Fixture) -> Technique {
        f.save_technique(&Technique {
            summary: "original summary".into(),
            ..Technique::new(new_id(), "Twang", d2026(), d2026())
        })
    }

    fn example(path: String) -> TechniqueMedia {
        TechniqueMedia {
            id: new_id(),
            path,
            kind: TechniqueMediaKind::Audio,
            label: String::new(),
            added_at: d2026(),
        }
    }

    #[test]
    fn a_stale_technique_save_keeps_a_remote_updated_summary_the_user_never_touched() {
        let f = Fixture::new();
        let t = seed_technique(&f);
        let session = f.techs.load_technique(&t.id).unwrap();

        f.raw(
            &format!("UPDATE techniques SET summary = ?, {REMOTE_HLC} WHERE id = ?"),
            ["remote summary", t.id.as_str()],
        );

        // The user's stale save changes nothing (a pure re-save).
        let saved = f
            .techs
            .save_technique_with_base(&session.aggregate, &session.base_hashes)
            .unwrap();
        assert_eq!(
            f.column::<String>("techniques", "summary", &t.id),
            "remote summary"
        );
        assert_eq!(
            f.column::<String>("techniques", "hlc_packed", &t.id),
            "REMOTE-PACKED"
        );
        assert_eq!(saved.aggregate.summary, "remote summary");
    }

    #[test]
    fn a_stale_technique_save_does_not_tombstone_remote_added_media() {
        let f = Fixture::new();
        let t = seed_technique(&f);
        let session = f.techs.load_technique(&t.id).unwrap();

        // Remote adds an example clip (asset row exists; the media row
        // arrives like a synced-in record: raw SQL, foreign HLC, no oplog).
        let asset_id = f
            .assets
            .import_file(&f.file("remote.wav", &[5, 5]), AssetStore::DEFAULT_KIND)
            .unwrap();
        let remote_media = new_id();
        f.raw(
            "INSERT INTO technique_media \
             (id, technique_id, asset_id, created_at, updated_at, \
              hlc_wall, hlc_ctr, hlc_node, hlc_packed) \
             VALUES (?, ?, ?, 1, 1, 9999999999999, 0, 'REMOTE', 'REMOTE-PACKED')",
            [remote_media.as_str(), t.id.as_str(), asset_id.as_str()],
        );

        let saved = f
            .techs
            .save_technique_with_base(
                &Technique {
                    name: "Twang (edited)".into(),
                    ..session.aggregate.clone()
                },
                &session.base_hashes,
            )
            .unwrap();
        assert_eq!(
            f.column::<Option<i64>>("technique_media", "deleted_at", &remote_media),
            None,
            "the remote media row must stay live"
        );
        // The returned aggregate converges onto the remote-added media.
        assert!(saved.aggregate.media.iter().any(|m| m.id == remote_media));
    }

    #[test]
    fn an_explicitly_removed_media_row_still_tombstones() {
        let f = Fixture::new();
        let mut t = seed_technique(&f);
        t = f.save_technique(&Technique {
            media: vec![example(f.file("ex.wav", &[9]))],
            ..t.clone()
        });
        let media_id = t.media[0].id.clone();

        let session = f.techs.load_technique(&t.id).unwrap();
        f.techs
            .save_technique_with_base(
                &Technique {
                    media: Vec::new(),
                    ..session.aggregate.clone()
                },
                &session.base_hashes,
            )
            .unwrap();
        assert!(f
            .column::<Option<i64>>("technique_media", "deleted_at", &media_id)
            .is_some());
    }

    #[test]
    fn saving_over_a_tombstoned_technique_aborts_no_zombie_media() {
        let f = Fixture::new();
        let t = seed_technique(&f);
        let session = f.techs.load_technique(&t.id).unwrap();
        f.techs.delete_technique(&t.id).unwrap();

        let saved = f.techs.save_technique_with_base(
            &Technique {
                media: vec![example(f.file("zombie.wav", &[3]))],
                ..session.aggregate.clone()
            },
            &session.base_hashes,
        );
        assert!(saved.is_err());
        assert_eq!(
            f.live_count("technique_media"),
            0,
            "the abort must leave no live media"
        );
    }
}
