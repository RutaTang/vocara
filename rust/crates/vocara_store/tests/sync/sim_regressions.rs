//! Port of `client/test/sync_sim_regressions_test.dart`: targeted
//! regressions for the REAL sync bugs found by the convergence simulation,
//! one per fix, each named after the seed that first exposed it. The full
//! episodes stay in the simulation battery (`tests/sync_simulation`); these
//! pin the exact mechanism at the unit level.
//!
//! The multi-device cases drive each device's song repository, as Dart's
//! drove full Persistence stacks (see `device.rs`); the 0xab1a57 cases
//! write rows directly, as Dart's did.

use std::sync::Arc;

use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_store::db::AppDatabase;
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_row::{SyncRow, SyncRowBuilder};
use vocara_store::sync::sync_transport::FakeCloud;
use vocara_store::sync::sync_writer::SyncWriter;

use crate::device::Device;
use crate::engine::T0;
use crate::support::*;

// --- seed 0x5eed0001: cascade delete must upload parent-first --------------

#[test]
fn a_practice_deleted_before_it_ever_synced_arrives_whole_on_a_page_size_1_puller() {
    // No FK-swept row loss. The oplog collapses each row to its LATEST op,
    // and deletePractice journals children before the practice row — so
    // without the pusher's parent-first sort the cloud log carried the
    // cascade CHILD-FIRST. A device that never saw the aggregate applies
    // those rows as fresh inserts; with one row per page the clip landed
    // before its track (and the track before its practice), the FK sweep
    // hard-deleted them, and the advanced change token meant they never
    // re-arrived: permanent row loss and divergence.
    let cloud = FakeCloud::new();
    let a = Device::new(&cloud, "DEVA", T0);
    let b = Device::new(&cloud, "DEVB", T0);
    let created = a.repo().create_practice("Doomed").unwrap();
    let f = a.source_file("take", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let take =
        AudioTrack {
            version_id: created.active_version_id.clone(),
            ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, "Take 1", DateTime::now())
                .with_audio(f, 0, 800, Vec::new())
        };
    let saved = a
        .repo()
        .save_practice(&SongPractice {
            vocal_tracks: vec![Arc::new(take)],
            ..created
        })
        .unwrap();
    // Delete BEFORE anything was pushed: every row ships as a tombstoned
    // fresh insert for B.
    a.repo().delete_practice(&saved.id).unwrap();
    let push = a.engine.pump_push();
    assert!(!push.failed());

    b.transport.set_page_size(1); // worst case: every page boundary splits the cascade
    let pull = b.engine.pump_pull();
    assert!(!pull.failed());
    assert_eq!(
        pull.failed_records, 0,
        "no row of the cascade may be FK-swept"
    );

    // Every synced row of the aggregate exists on B, equal in (hlc,
    // tombstone) to A — nothing was lost.
    for table in ["practices", "interpretations", "tracks", "clips", "assets"] {
        assert_eq!(
            b.row_states(table),
            a.row_states(table),
            "{table} must fully replicate"
        );
    }
    assert_eq!(
        count(
            &b.db,
            "SELECT COUNT(*) FROM practices WHERE deleted_at IS NULL",
            &[]
        ),
        0,
        "the practice arrived tombstoned"
    );
}

// --- seeds 0x5eed0009 / 0xabd782: single-slot resolution must replicate ----

/// An accompaniment imported from [path] (Dart's `accompaniment(path,
/// name:)`).
fn accompaniment(path: &str, name: &str) -> AudioTrack {
    AudioTrack::new(
        new_id(),
        AudioTrackKind::Accompaniment,
        name,
        DateTime::now(),
    )
    .with_audio(path, 0, 1000, Vec::new())
}

#[test]
fn concurrent_accompaniments_end_with_the_loser_tombstoned_everywhere_with_one_row_state() {
    // Both devices add an accompaniment while unaware of each other. The
    // applier resolves the (practice, kind) slot by LWW — but the loser's
    // tombstone used to be an un-journaled, un-bumped local write: whether a
    // device even observed the conflict depended on its apply order, so the
    // loser stayed LIVE in the cloud (and on any device that saw the winner's
    // state first) while being dead elsewhere — a permanent divergence under
    // identical HLCs. The resolution must replicate like any write:
    // derived-stamp tombstone + journal.
    let cloud = FakeCloud::new();
    let a = Device::new(&cloud, "DEVA", T0);
    let b = Device::new(&cloud, "DEVB", T0);
    let c = Device::new(&cloud, "DEVC", T0);
    let f = a.source_file("acc", &[9, 9, 9, 9, 1, 2, 3, 4]);

    let seeded = a.repo().create_practice("Slot race").unwrap();
    assert!(!a.engine.pump_push().failed());
    assert!(!b.engine.pump_pull().failed());

    // A adds accA and pushes it (accA is now LIVE in the cloud).
    let la = a.load(&seeded.id);
    let acc_a = accompaniment(&f, "accA");
    a.save_with_base(
        &SongPractice {
            accompaniment: Some(Arc::new(acc_a.clone())),
            ..la.aggregate.clone()
        },
        &la,
    );
    assert!(!a.engine.pump_push().failed());

    // B, not having pulled accA, adds accB with a strictly later HLC (the
    // same bytes: both devices pool them under one content hash).
    b.now.set(a.now.get() + 5);
    let lb = b.load(&seeded.id);
    let acc_b = accompaniment(&f, "accB");
    b.save_with_base(
        &SongPractice {
            accompaniment: Some(Arc::new(acc_b.clone())),
            ..lb.aggregate.clone()
        },
        &lb,
    );
    assert!(!b.engine.pump_push().failed());

    // Everyone settles (C pulls the whole log from scratch).
    for _ in 0..4 {
        for dev in [&a, &b, &c] {
            assert!(!dev.engine.pump_push().failed());
            assert!(!dev.engine.pump_pull().failed());
        }
    }

    // The resolution replicated: the cloud's current accA is TOMBSTONED.
    let cloud_acc_a = cloud.server_row_for("tracks", &acc_a.id).unwrap();
    assert!(
        !cloud_acc_a.field("deleted_at").unwrap().is_null(),
        "the slot resolution must be pushed, not stay device-local"
    );

    // All three devices hold IDENTICAL row states: accA dead, accB live.
    let on_a = a.row_states("tracks");
    assert_eq!(b.row_states("tracks"), on_a);
    assert_eq!(c.row_states("tracks"), on_a);
    for dev in [&a, &b, &c] {
        let live: Vec<String> = dev
            .db
            .query(
                "SELECT id FROM tracks WHERE deleted_at IS NULL AND kind = 'accompaniment'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, std::slice::from_ref(&acc_b.id));
        // No device is left with a pending resolution un-pushed.
        assert_eq!(dev.oplog_count(), 0);
    }
}

// --- seed 0xc1c2b7: integrity repair must not run between pages ------------

#[test]
fn a_practice_whose_interpretation_arrives_on_the_next_page_is_not_healed_mid_pull() {
    // With one row per page, the practice row lands one page before its
    // interpretation. Running the repair after EVERY page treated that
    // half-delivered aggregate as broken: the active-interp scalar heal
    // nulled the pointer under a fresh, LWW-winning HLC — manufactured intent
    // that could later beat a legitimate delete tombstone and resurrect the
    // practice fleet-wide. The repair belongs at the END of the pull run,
    // once the delivered prefix is causally complete.
    let cloud = FakeCloud::new();
    let a = Device::new(&cloud, "DEVA", T0);
    let b = Device::new(&cloud, "DEVB", T0);
    let created = a.repo().create_practice("Paged").unwrap();
    assert!(!a.engine.pump_push().failed());

    b.transport.set_page_size(1);
    let pull = b.engine.pump_pull();
    assert!(!pull.failed());
    assert_eq!(pull.applied, 2, "practice + interpretation");

    assert_eq!(
        col::<Option<String>>(&b.db, "practices", &created.id, "active_interpretation_id"),
        Some(created.active_version_id.clone()),
        "the pointer must survive the page boundary un-healed"
    );
    // No repair ran, so nothing was journaled: applying remote changes still
    // echoes nothing into push.
    assert_eq!(b.oplog_count(), 0);
    // And B holds A's row verbatim (no repair-minted HLC).
    assert_eq!(b.row_states("practices"), a.row_states("practices"));
}

// --- seed 0xab1a57: repairs are derived state, never fresh intent ----------

/// The healer's clock sits FAR ahead of the row it will repair.
const HEALER_NOW: i64 = 2_000_000_000;

struct Healer {
    db: Arc<AppDatabase>,
    checker: IntegrityChecker,
    applier: SyncApplier,
}

fn healer() -> Healer {
    let db = memory_db();
    let clock = Arc::new(HlcClock::new(id("LOCAL")).with_now(|| HEALER_NOW));
    let writer = Arc::new(SyncWriter::new(db.clone(), clock.clone()));
    Healer {
        checker: IntegrityChecker::new(db.clone(), writer),
        applier: SyncApplier::new(
            db.clone(),
            clock,
            Arc::new(SyncRowBuilder::new(db.clone())),
            None,
        ),
        db,
    }
}

fn insert_practice(db: &AppDatabase, row_id: &str, h: &Hlc, active: Option<&str>) {
    db.execute(
        "INSERT INTO practices (id, title, active_interpretation_id, created_at, updated_at, \
         deleted_at, hlc_wall, hlc_ctr, hlc_node, hlc_packed, schema_v) \
         VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, 1)",
        rusqlite::params![
            row_id,
            "song",
            active,
            h.wall_ms,
            h.wall_ms,
            h.wall_ms,
            h.counter,
            h.node,
            h.packed()
        ],
    )
    .unwrap();
}

#[test]
fn a_scalar_heal_stamps_from_the_row_not_a_clock_tick_so_a_later_real_delete_still_wins() {
    let h = healer();
    // The row was written long "ago" by a remote device...
    let row_hlc = Hlc::new(1_000_000, 0, id("REMOTE"));
    // Dangling: a deleted interp.
    insert_practice(&h.db, &id("P"), &row_hlc, Some(&id("GONE")));
    // Tombstoned, not absent: an absent target is still in flight and is
    // never healed (see the IntegrityChecker's scalar heals).
    h.db.execute(
        "INSERT INTO interpretations (id, practice_id, name, order_index, created_at, \
         updated_at, deleted_at, hlc_wall, hlc_ctr, hlc_node, hlc_packed, schema_v) \
         VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, 1)",
        rusqlite::params![
            id("GONE"),
            id("P"),
            "old",
            row_hlc.wall_ms,
            row_hlc.wall_ms,
            row_hlc.wall_ms,
            row_hlc.wall_ms,
            row_hlc.counter,
            row_hlc.node,
            row_hlc.packed()
        ],
    )
    .unwrap();

    assert!(h.checker.repair().unwrap().is_empty());
    let healed: (Option<String>, i64, i64, String) =
        h.db.query_row(
            "SELECT active_interpretation_id, hlc_wall, hlc_ctr, hlc_node FROM practices \
             WHERE id = ?",
            [id("P")],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(healed.0, None, "the heal itself is right");
    // The stamp is DERIVED from the row's HLC under OUR node — not the
    // healer's (far-future) clock. A fresh tick here out-ranked genuinely
    // later writes from other devices (the healer merely has to be the device
    // with the fastest clock), letting bookkeeping resurrect a deleted
    // practice (simulation seed 0xab1a57).
    assert_eq!(healed.1, row_hlc.wall_ms);
    assert_eq!(healed.2, row_hlc.counter);
    assert_eq!(healed.3, format!("{}!{}", row_hlc.node, id("LOCAL")));

    // A delete stamped causally AFTER the original row — but by a device
    // whose wall clock is far BEHIND the healer's — must still win.
    let delete_hlc = Hlc::new(1_500_000, 0, id("DELETER"));
    let res = h.applier.apply(&[SyncRow::new(
        "practices",
        fields! {
            "id" => id("P"),
            "title" => "song",
            "timeline_duration_ms" => 0i64,
            "master_volume" => 1.0,
            "active_interpretation_id" => NULL,
            "separation_asset_id" => NULL,
            "reverb_json" => NULL,
            "lyrics_json" => NULL,
            "created_at" => row_hlc.wall_ms,
            "updated_at" => delete_hlc.wall_ms,
            "deleted_at" => delete_hlc.wall_ms,
            "hlc_wall" => delete_hlc.wall_ms,
            "hlc_ctr" => delete_hlc.counter,
            "hlc_node" => delete_hlc.node.clone(),
            "hlc_packed" => delete_hlc.packed(),
            "schema_v" => 1i64,
        },
    )]);
    assert_eq!(res.applied, 1, "the tombstone must beat the heal");
    assert!(
        col::<Option<i64>>(&h.db, "practices", &id("P"), "deleted_at")
            .flatten()
            .is_some(),
        "tombstones never resurrect"
    );
}

#[test]
fn a_zombie_child_tombstone_repair_also_derives_its_stamp_from_the_child_row() {
    let h = healer();
    let parent_hlc = Hlc::new(1_000_000, 3, id("REMOTE"));
    let child_hlc = Hlc::new(1_200_000, 7, id("REMOTE"));
    insert_practice(&h.db, &id("P"), &parent_hlc, None);
    h.db.execute(
        "UPDATE practices SET deleted_at = ? WHERE id = ?",
        rusqlite::params![parent_hlc.wall_ms, id("P")],
    )
    .unwrap();
    h.db.execute(
        "INSERT INTO tracks (id, practice_id, kind, display_name, muted, soloed, volume, pan, \
         order_index, created_at, updated_at, deleted_at, hlc_wall, hlc_ctr, hlc_node, \
         hlc_packed, schema_v) VALUES (?, ?, 'vocal', 'zombie', 0, 0, 1.0, 0.0, 0, ?, ?, NULL, \
         ?, ?, ?, ?, 1)",
        rusqlite::params![
            id("T"),
            id("P"),
            child_hlc.wall_ms,
            child_hlc.wall_ms,
            child_hlc.wall_ms,
            child_hlc.counter,
            child_hlc.node,
            child_hlc.packed()
        ],
    )
    .unwrap();

    assert!(h.checker.repair().unwrap().is_empty());
    let t: (Option<i64>, i64, i64, String) =
        h.db.query_row(
            "SELECT deleted_at, hlc_wall, hlc_ctr, hlc_node FROM tracks WHERE id = ?",
            [id("T")],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!(t.0.is_some());
    assert_eq!(t.1, child_hlc.wall_ms);
    assert_eq!(t.2, child_hlc.counter);
    assert_eq!(t.3, format!("{}!{}", child_hlc.node, id("LOCAL")));
}
