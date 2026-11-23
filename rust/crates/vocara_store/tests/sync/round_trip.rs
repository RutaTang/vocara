//! Port of `client/test/sync_round_trip_test.dart`: two devices sharing ONE
//! fake cloud — the end-to-end convergence suite for P0 sync:
//! create/edit/delete on one device, push/pull, and prove identical row
//! state on the other.
//!
//! As in Dart, every local edit goes through each device's song repository
//! (`Device::repo`: the legacy save to seed, then loads and three-way saves
//! over their base hashes, deletes). Dart's `nextMs()` — wait for the wall
//! clock's next millisecond so B's next stamp is strictly later — is a step
//! of B's settable clock past A's. The "three-way stale editor END TO END"
//! case is `vocara_services/tests/sync_round_trip_three_way.rs`, on two
//! `Persistence` stacks as in Dart.

use std::collections::BTreeSet;
use std::sync::Arc;

use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_store::sync::sync_transport::FakeCloud;

use crate::device::Device;
use crate::engine::T0;
use crate::support::*;

struct Pair {
    a: Device,
    b: Device,
}

fn pair() -> Pair {
    let cloud = FakeCloud::new();
    Pair {
        a: Device::new(&cloud, "DEVA", T0),
        b: Device::new(&cloud, "DEVB", T0),
    }
}

impl Pair {
    /// Full sync rounds until quiescent: pushes then pulls on both devices,
    /// repeated so post-pull integrity repairs propagate too; then asserts
    /// both oplogs fully drained (nothing left unsynced).
    fn settle(&self) {
        for _ in 0..4 {
            assert!(!self.a.engine.pump_push().failed());
            assert!(!self.b.engine.pump_push().failed());
            assert!(!self.a.engine.pump_pull().failed());
            assert!(!self.b.engine.pump_pull().failed());
        }
        assert_eq!(self.a.oplog_count(), 0);
        assert_eq!(self.b.oplog_count(), 0);
    }

    fn expect_converged(&self) {
        assert_eq!(self.b.snapshot(), self.a.snapshot());
    }

    /// Dart's `nextMs()` before a write on B: B's clock moves strictly past
    /// A's, so B's next stamp out-ranks A's last one.
    fn next_ms_on_b(&self) {
        self.b.now.set(self.a.now.get() + 5);
    }
}

/// Seeds a practice on [dev]: title, one vocal take with a real (tiny) audio
/// clip, and one note. Returns the saved aggregate.
fn seed_practice(dev: &Device, title: &str) -> SongPractice {
    let created = dev.repo().create_practice(title).unwrap();
    let f = dev.source_file("take", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let now = DateTime::now();
    let track = AudioTrack {
        version_id: created.active_version_id.clone(),
        notes: vec![PracticeNote {
            id: new_id(),
            position_ms: 100,
            end_ms: None,
            content: "original note".to_string(),
            created_at: now,
            updated_at: now,
        }],
        ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, "Take 1", now).with_audio(
            f,
            0,
            1200,
            Vec::new(),
        )
    };
    // Creation flow: the legacy save is the documented path for a fresh
    // aggregate (base := current); editors below always save with base.
    dev.repo()
        .save_practice(&SongPractice {
            vocal_tracks: vec![Arc::new(track)],
            ..created
        })
        .unwrap()
}

/// [p] with every note of every take set to [content].
fn with_note_content(p: &SongPractice, content: &str) -> SongPractice {
    SongPractice {
        vocal_tracks: p
            .vocal_tracks
            .iter()
            .map(|t| {
                Arc::new(AudioTrack {
                    notes: t
                        .notes
                        .iter()
                        .map(|n| PracticeNote {
                            content: content.to_string(),
                            updated_at: DateTime::now(),
                            ..n.clone()
                        })
                        .collect(),
                    ..(**t).clone()
                })
            })
            .collect(),
        ..p.clone()
    }
}

/// [p] renamed [title] (Dart's `copyWith(title:)`).
fn titled(p: &SongPractice, title: &str) -> SongPractice {
    SongPractice {
        title: title.to_string(),
        ..p.clone()
    }
}

/// The single take of [p].
fn single_take(p: &SongPractice) -> &AudioTrack {
    assert_eq!(p.vocal_tracks.len(), 1);
    &p.vocal_tracks[0]
}

/// The content of the single note of the single take of [p].
fn single_note(p: &SongPractice) -> &str {
    let take = single_take(p);
    assert_eq!(take.notes.len(), 1);
    &take.notes[0].content
}

#[test]
fn a_creates_a_practice_with_a_take_and_notes_and_b_pulls_an_identical_aggregate() {
    let p = pair();
    let saved = seed_practice(&p.a, "Duet");
    assert!(!p.a.engine.pump_push().failed());
    let pull = p.b.engine.pump_pull();
    assert!(!pull.failed());
    assert!(pull.applied > 0);

    let la = p.a.load(&saved.id);
    let lb = p.b.load(&saved.id);
    // The wave-1 hash maps are the row-content fingerprint of the aggregate:
    // equal maps mean every synced row (practice, interpretation, track,
    // clip, note) is content-identical on both devices.
    assert_eq!(lb.base_hashes, la.base_hashes);
    assert_eq!(lb.aggregate.title, "Duet");
    assert_eq!(single_take(&lb.aggregate).display_name, "Take 1");
    assert_eq!(single_take(&lb.aggregate).clips.len(), 1);
    assert_eq!(single_note(&lb.aggregate), "original note");
    // Applying remote changes journaled NOTHING on B (no echo).
    assert_eq!(p.b.oplog_count(), 0);
    p.expect_converged();
}

#[test]
fn concurrent_edits_to_different_rows_of_one_practice_converge_to_the_union() {
    let p = pair();
    let seeded = seed_practice(&p.a, "Song");
    p.settle();

    // A edits the note; B renames the practice — different rows, no
    // conflict.
    let la = p.a.load(&seeded.id);
    p.a.save_with_base(&with_note_content(&la.aggregate, "note edited on A"), &la);
    let lb = p.b.load(&seeded.id);
    p.b.save_with_base(&titled(&lb.aggregate, "renamed on B"), &lb);

    p.settle();

    for dev in [&p.a, &p.b] {
        let loaded = dev.load(&seeded.id);
        assert_eq!(loaded.aggregate.title, "renamed on B");
        assert_eq!(single_note(&loaded.aggregate), "note edited on A");
    }
    p.expect_converged();
}

#[test]
fn concurrent_edits_to_the_same_row_resolve_by_lww_to_the_later_writer() {
    let p = pair();
    let seeded = seed_practice(&p.a, "Song");
    p.settle();

    let la = p.a.load(&seeded.id);
    p.a.save_with_base(&titled(&la.aggregate, "from A"), &la);
    p.next_ms_on_b(); // B's HLC is now strictly greater: B deterministically wins
    let lb = p.b.load(&seeded.id);
    p.b.save_with_base(&titled(&lb.aggregate, "from B"), &lb);

    p.settle();

    assert_eq!(p.a.load(&seeded.id).aggregate.title, "from B");
    assert_eq!(p.b.load(&seeded.id).aggregate.title, "from B");
    p.expect_converged();
}

#[test]
fn tombstone_propagation_leaves_no_zombie_live_children_after_a_stale_child_edit() {
    // A deletes; B pushes a STALE child edit after the delete; no zombie live
    // children survive anywhere.
    let p = pair();
    let seeded = seed_practice(&p.a, "Song");
    p.settle();

    // B opens the practice BEFORE the delete reaches it.
    let lb = p.b.load(&seeded.id);

    // A deletes the whole practice (cascade tombstones) and pushes.
    p.a.repo().delete_practice(&seeded.id).unwrap();
    assert!(!p.a.engine.pump_push().failed());

    // B, unaware, edits a child track of the now-deleted practice — with a
    // strictly newer HLC, so plain LWW alone would keep the track alive.
    p.next_ms_on_b();
    let stale = SongPractice {
        vocal_tracks: vec![Arc::new(AudioTrack {
            display_name: "zombie rename".to_string(),
            ..single_take(&lb.aggregate).clone()
        })],
        ..lb.aggregate.clone()
    };
    p.b.save_with_base(&stale, &lb);

    // Full exchange, including the extra rounds that propagate each side's
    // integrity repairs (the engine runs them after every pull run).
    p.settle();

    for dev in [&p.a, &p.b] {
        let pid: &str = &seeded.id;
        // The practice is tombstoned...
        assert_eq!(
            count(
                &dev.db,
                "SELECT COUNT(*) FROM practices WHERE id = ? AND deleted_at IS NULL",
                &[pid]
            ),
            0
        );
        // ...and NOTHING under it is left alive (the applier + the post-pull
        // IntegrityChecker keep the tombstoned-parent invariant).
        for (what, sql) in [
            (
                "tracks",
                "SELECT COUNT(*) FROM tracks WHERE practice_id = ? AND deleted_at IS NULL",
            ),
            (
                "interpretations",
                "SELECT COUNT(*) FROM interpretations WHERE practice_id = ? AND deleted_at IS NULL",
            ),
            (
                "clips",
                "SELECT COUNT(*) FROM clips c JOIN tracks t ON t.id = c.track_id \
                 WHERE t.practice_id = ? AND c.deleted_at IS NULL",
            ),
            (
                "notes",
                "SELECT COUNT(*) FROM practice_notes n JOIN tracks t ON t.id = n.track_id \
                 WHERE t.practice_id = ? AND n.deleted_at IS NULL",
            ),
        ] {
            assert_eq!(count(&dev.db, sql, &[pid]), 0, "no zombie live {what}");
        }
        // The list no longer shows it.
        let list = dev.repo().list_practices().unwrap();
        assert!(list.iter().all(|x| x.id != seeded.id));
    }
    p.expect_converged();
}

#[test]
fn an_offline_device_catches_up_in_pages_and_converges() {
    let p = pair();
    let seeded = seed_practice(&p.a, "Song");
    p.settle();

    p.b.transport.set_partitioned(true);

    // Three sequential edits on A, each pushed separately (three log
    // entries).
    let la = p.a.load(&seeded.id);
    p.a.save_with_base(&titled(&la.aggregate, "title v2"), &la);
    assert!(!p.a.engine.pump_push().failed());

    let la = p.a.load(&seeded.id);
    p.a.save_with_base(&with_note_content(&la.aggregate, "note v2"), &la);
    assert!(!p.a.engine.pump_push().failed());

    let la = p.a.load(&seeded.id);
    let track = single_take(&la.aggregate);
    let now = DateTime::now();
    let mut notes = track.notes.clone();
    notes.push(PracticeNote {
        id: new_id(),
        position_ms: 900,
        end_ms: None,
        content: "second note".to_string(),
        created_at: now,
        updated_at: now,
    });
    let next = SongPractice {
        vocal_tracks: vec![Arc::new(AudioTrack {
            notes,
            ..track.clone()
        })],
        ..la.aggregate.clone()
    };
    p.a.save_with_base(&next, &la);
    assert!(!p.a.engine.pump_push().failed());

    // Partitioned: the pull fails and moves nothing.
    let offline = p.b.engine.pump_pull();
    assert!(offline.failed());
    assert_eq!(offline.pages, 0);

    // Reconnect; catch up in pages of 2 (3 changed rows -> 2 pages).
    p.b.transport.set_partitioned(false);
    p.b.transport.set_page_size(2);
    let catch_up = p.b.engine.pump_pull();
    assert!(!catch_up.failed());
    assert_eq!(catch_up.pages, 2, "3 rows at page size 2");
    assert_eq!(catch_up.applied, 3);

    let on_b = p.b.load(&seeded.id);
    assert_eq!(on_b.aggregate.title, "title v2");
    assert_eq!(
        single_take(&on_b.aggregate)
            .notes
            .iter()
            .map(|n| n.content.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["note v2", "second note"])
    );
    p.expect_converged();
}

#[test]
fn the_shared_cloud_holds_each_row_once_per_push_and_the_ack_drains_the_ledger() {
    // Not in Dart — a check of what the cases above rest on: a seeded
    // aggregate is six journaled rows (practice, interpretation, asset,
    // track, clip, note), pushed parent-first in one batch, and the ack
    // drains the ledger.
    let p = pair();
    let saved = seed_practice(&p.a, "Song");
    assert_eq!(p.a.oplog_count(), 6);
    let run = p.a.engine.pump_push();
    assert!(!run.failed());
    assert_eq!(run.pushed, 6);
    let batches = p.a.transport.pushed_batches();
    assert_eq!(batches.len(), 1);
    let tables: Vec<&str> = batches[0].iter().map(|r| r.table()).collect();
    let pos = |t: &str| tables.iter().position(|x| *x == t).unwrap();
    assert!(pos("assets") < pos("clips"));
    assert!(pos("practices") < pos("interpretations"));
    assert!(pos("interpretations") < pos("tracks"));
    assert!(pos("tracks") < pos("clips"));
    assert!(pos("tracks") < pos("practice_notes"));
    assert_eq!(p.a.oplog_count(), 0);
    assert!(p.a.wire("practices", &saved.id).is_some());
}
