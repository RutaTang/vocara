//! Port of `client/test/asset_gc_test.dart`.
//!
//! Two cases ran through code not ported yet, and are ported here at the
//! asset-store level with their rows written directly: the wire-arrived
//! asset row (the SyncApplier's record-synced stamp is written as the
//! applier writes it) and the referenced asset (the repository's practice,
//! track and clip rows are written as its save writes them).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use vocara_store::db::rows::{AssetRow, ClipRow, OplogRow, PracticeRow, TrackRow};
use vocara_store::db::schema::ALL_TABLES;
use vocara_store::db::AssetStore;
use vocara_store::sync::hlc::Hlc;
use vocara_store::sync::quarantine_store::{QuarantineEntry, QuarantineStore};
use vocara_store::sync::sync_state_store::SyncStateStore;

use super::{names_in, pad_id, set_mtime_from_now, shared, Fixture};

const ZERO: std::time::Duration = std::time::Duration::ZERO;

/// Settles the durability ledger, exactly as a push ack would
/// (`OplogDrain::ack` hard-DELETES the covered entries). The sweep holds
/// back any asset whose journal entry is still pending — a pushed-but-unacked
/// row may already be server-side — so tests exercising the sweep's OTHER
/// mechanics drain the ledger first; the pending-journal guard has its own
/// test below.
fn ack_journal(f: &Fixture) {
    f.db.execute("DELETE FROM oplog", []).unwrap();
}

fn tombstones_journaled(f: &Fixture) -> usize {
    f.db.query(
        "SELECT 1 AS x FROM oplog WHERE op = 'tombstone'",
        [],
        |_| Ok(()),
    )
    .unwrap()
    .len()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A clock once set a year ahead keeps the HLC ahead on every boot after
/// it (for LWW). An asset imported then is stamped by the real clock, so it
/// ages as any other and is swept once unreferenced past the grace window;
/// a row stamped by that clock before (more than a day ahead) is not taken
/// for one in flight either.
#[test]
fn a_clock_once_set_ahead_does_not_keep_unreferenced_assets_forever() {
    use std::sync::Arc;
    use vocara_store::sync::hlc::HlcClock;
    use vocara_store::sync::sync_writer::SyncWriter;
    let _g = shared();
    let f = Fixture::new();
    let year_ahead = now_ms() + 365 * 24 * 60 * 60 * 1000;
    let clock = HlcClock::new(super::node()).with_initial(Hlc::new(year_ahead, 0, super::node()));
    let writer = Arc::new(SyncWriter::new(f.db.clone(), Arc::new(clock)));
    let store = AssetStore::new(f.db.clone(), f.root().to_path_buf(), writer);
    let id = store
        .import_file(
            &f.make_src("ahead.wav", &[4, 5, 6]),
            AssetStore::DEFAULT_KIND,
        )
        .unwrap();
    let row = f.asset(&id);
    assert!(row.hlc_wall >= year_ahead, "the HLC stays ahead, for LWW");
    assert!(
        (row.created_at - now_ms()).abs() < 60_000,
        "created by the real clock: {}",
        row.created_at
    );
    // A row the clock set ahead stamped before this fix.
    f.db.insert(&AssetRow {
        id: pad_id("LEGACY"),
        content_hash: "HASH_LEGACY".to_string(),
        byte_size: 3,
        ext: "wav".to_string(),
        kind: "audio".to_string(),
        local_rel_path: Some("legacy.wav".to_string()),
        created_at: year_ahead,
        updated_at: year_ahead,
        hlc_wall: year_ahead,
        hlc_ctr: 0,
        hlc_node: super::node(),
        hlc_packed: Hlc::new(year_ahead, 0, super::node()).packed(),
        ..Default::default()
    })
    .unwrap();
    ack_journal(&f);
    // A grace window of a minute: the fresh import is in flight, the row
    // stamped a year ahead is not.
    let swept = store
        .sweep_unreferenced(std::time::Duration::from_secs(60))
        .unwrap();
    assert_eq!(swept, 1);
    assert!(f.asset(&pad_id("LEGACY")).deleted_at.is_some());
    assert!(f.asset(&id).deleted_at.is_none());
    // Past the window, the import goes too.
    assert_eq!(store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert!(f.asset(&id).deleted_at.is_some());
}

#[test]
fn sweep_tombstones_and_deletes_a_wholly_unreferenced_asset() {
    let _g = shared();
    let f = Fixture::new();
    let id = f.import(&f.make_src("lone.wav", &[1, 2, 3]));
    let path = f.resolve(&id).unwrap();
    assert!(Path::new(&path).exists());
    ack_journal(&f);

    // minAge zero: these mechanics tests exercise the reclaim itself; the
    // default age grace window has its own test below.
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert_eq!(f.resolve(&id), None); // tombstoned
    assert!(!Path::new(&path).exists()); // file reclaimed
}

#[test]
fn sweep_skips_assets_younger_than_the_age_grace_window() {
    let _g = shared();
    // A transiently-unreferenced row is in flight, not garbage.
    let f = Fixture::new();
    let id = f.import(&f.make_src("young.wav", &[1, 2]));
    ack_journal(&f);
    // Default grace: a brand-new unreferenced row survives the sweep (its
    // referencing clip/media rows may simply not have committed yet, and a
    // journaled tombstone would propagate fleet-wide once sync ships).
    assert_eq!(
        f.store
            .sweep_unreferenced(AssetStore::DEFAULT_SWEEP_MIN_AGE)
            .unwrap(),
        0
    );
    assert!(f.resolve(&id).is_some());
    // Aged past the window it is ordinary garbage again.
    f.age(&id);
    assert_eq!(
        f.store
            .sweep_unreferenced(AssetStore::DEFAULT_SWEEP_MIN_AGE)
            .unwrap(),
        1
    );
    assert_eq!(f.resolve(&id), None);
}

#[test]
fn sweep_treats_assets_referenced_by_quarantined_payloads_as_reachable() {
    let _g = shared();
    // A parked page must not become a tombstone.
    let f = Fixture::new();
    let id = f.import(&f.make_src("parked.wav", &[3, 4, 5]));
    ack_journal(&f);
    let clip_id = "CLIPCLIPCLIPCLIPCLIPCLIP01";
    // A rejected remote clip referencing the asset, parked for review: until
    // an upgrade or manual review re-applies it, the parked payload may hold
    // the ONLY pointer at these bytes.
    let payload = json!({
        "table": "clips",
        "id": clip_id,
        "schemaV": 1,
        "hlcPacked": "z",
        "fields": {"id": clip_id, "asset_id": id},
        "fieldClocks": null,
    })
    .to_string();
    let quarantine = QuarantineStore::new(f.db.clone());
    quarantine
        .put(&QuarantineEntry {
            table: "clips",
            id: clip_id,
            hlc_packed: "z",
            schema_v: 1,
            payload: &payload,
            reason: "apply-failed",
            quarantined_at: now_ms(),
        })
        .unwrap();

    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 0);
    assert!(f.resolve(&id).is_some());

    // Unparked (re-applied or discarded): the asset is sweepable again.
    quarantine.delete("clips", clip_id).unwrap();
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert_eq!(f.resolve(&id), None);
}

#[test]
fn sweep_never_touches_an_asset_that_has_ever_been_synced() {
    let _g = shared();
    // Local reachability is not knowledge for a synced asset; a future
    // server-side refcount owns that lifecycle. Three unreferenced assets:
    // one whose RECORD crossed the wire, one whose BYTES did, one fully
    // local.
    let f = Fixture::new();
    let synced_record = f.import(&f.make_src("synced_record.wav", &[1, 1]));
    let synced_bytes = f.import(&f.make_src("synced_bytes.wav", &[2, 2]));
    let local_only = f.import(&f.make_src("local_only.wav", &[3, 3]));
    f.db.execute(
        "UPDATE assets SET record_sync_state = 'synced' WHERE id = ?",
        [&synced_record],
    )
    .unwrap();
    f.db.execute(
        "UPDATE assets SET bytes_sync_state = 'uploaded' WHERE id = ?",
        [&synced_bytes],
    )
    .unwrap();
    ack_journal(&f);

    // Only the never-synced asset is swept: a device behind on pull cannot
    // tell a remotely-referenced synced asset from garbage, and the
    // journaled tombstone would propagate as a fleet-wide delete.
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert_eq!(f.resolve(&local_only), None); // tombstoned
    assert!(f.resolve(&synced_record).is_some()); // left alone
    assert!(f.resolve(&synced_bytes).is_some()); // left alone
}

#[test]
fn sweep_holds_back_an_asset_whose_journal_entry_is_still_pending() {
    let _g = shared();
    // A pushed-but-unacked row may already be server-side; once the entry is
    // acked it is sweepable again. The pending-journal guard engages only
    // once a transport has ever existed (the durable marker the engine
    // writes before its first wire call): before that, no server can hold
    // anything.
    let f = Fixture::new();
    SyncStateStore::new(f.db.clone())
        .mark_transport_seen()
        .unwrap();
    let id = f.import(&f.make_src("inflight.wav", &[8, 8]));
    // Aged past the grace window, unreferenced, both sync states
    // 'localOnly' — sweepable on every other guard. But the import's journal
    // entry is still pending, and a crash between the server durably
    // persisting a push and the ack landing looks EXACTLY like this:
    // record_sync_state never advanced, yet peers may already have pulled the
    // row. Sweeping it would journal a tombstone that broadcasts a
    // fleet-wide delete.
    f.age(&id);
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 0);
    assert!(f.resolve(&id).is_some(), "fate not decidable");

    // The ack deletes the journal entry (simulated as the bare delete — on
    // the happy path the same round also marks the record synced, but the
    // crash-window row this guard protects is exactly the one that marking
    // missed). With the ledger settled the row is ordinary garbage again.
    ack_journal(&f);
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert_eq!(f.resolve(&id), None);
}

#[test]
fn re_verification_a_first_push_between_scan_and_tombstone_aborts_the_tombstone() {
    let _g = shared();
    // The transport marker + a pending journal entry appear between the
    // candidate scan and the tombstone: nothing journaled, bytes kept.
    let f = Fixture::new();
    let id = f.import(&f.make_src("race.wav", &[6, 6]));
    ack_journal(&f); // scan-time state: clean and sweepable
    f.age(&id);
    let path = f.resolve(&id).unwrap();

    let mut fired = 0;
    let db = f.db.clone();
    let expected = id.clone();
    let mut hook = |asset_id: &str| {
        fired += 1;
        assert_eq!(asset_id, expected);
        // The first-ever push races the sweep: the engine durably marks the
        // transport BEFORE its first wire call, and the asset's row is
        // journaled for the wire — the exact crash-window shape the
        // pending-journal guard exists for, appearing AFTER the candidate
        // scan already read clean state. The conditional tombstone must
        // re-see both atomically and stand down.
        SyncStateStore::new(db.clone())
            .mark_transport_seen()
            .unwrap();
        db.with_conn(|c| {
            OplogRow {
                entity_table: "assets".to_string(),
                entity_id: asset_id.to_string(),
                op: "insert".to_string(),
                hlc_packed: "x".to_string(),
                payload_hash: String::new(),
                created_at: 1,
                ..Default::default()
            }
            .insert_new(c)
        })
        .unwrap();
    };
    let removed = f
        .store
        .sweep_unreferenced_with(ZERO, Some(&mut hook))
        .unwrap();

    assert_eq!(fired, 1);
    assert_eq!(removed, 0, "the conditional tombstone matched 0 rows");
    assert!(f.resolve(&id).is_some(), "row still live");
    assert!(Path::new(&path).exists(), "bytes untouched");
    assert_eq!(
        tombstones_journaled(&f),
        0,
        "no fleet-wide delete journaled"
    );
}

#[test]
fn re_verification_record_sync_state_advancing_after_the_scan_aborts_the_tombstone() {
    let _g = shared();
    // A concurrent push ack marking the record synced.
    let f = Fixture::new();
    let id = f.import(&f.make_src("marked.wav", &[4, 2]));
    ack_journal(&f);
    f.age(&id);

    let db = f.db.clone();
    let mut hook = |asset_id: &str| {
        db.execute(
            "UPDATE assets SET record_sync_state = 'synced' WHERE id = ?",
            [asset_id],
        )
        .unwrap();
    };
    let removed = f
        .store
        .sweep_unreferenced_with(ZERO, Some(&mut hook))
        .unwrap();
    assert_eq!(removed, 0);
    assert!(f.resolve(&id).is_some());
    assert!(
        f.oplog().is_empty(),
        "no tombstone journaled for a row the UPDATE refused"
    );
}

#[test]
fn re_verification_changes_nothing_when_no_race_occurs() {
    let _g = shared();
    // A clean candidate still tombstones, journals and reclaims.
    let f = Fixture::new();
    let id = f.import(&f.make_src("clean.wav", &[5, 5]));
    ack_journal(&f);
    f.age(&id);
    let path = f.resolve(&id).unwrap();

    let mut hook = |_: &str| {}; // seam armed, state untouched
    let removed = f
        .store
        .sweep_unreferenced_with(ZERO, Some(&mut hook))
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(f.resolve(&id), None);
    assert!(!Path::new(&path).exists());
    assert_eq!(tombstones_journaled(&f), 1, "the delete still propagates");
}

#[test]
fn with_no_transport_ever_seen_the_pending_journal_guard_stands_down() {
    let _g = shared();
    // A journaled local asset still reclaims (production has no transport
    // and nothing ever acks, so the guard would otherwise hold everything
    // forever).
    let f = Fixture::new();
    let id = f.import(&f.make_src("offline.wav", &[7, 7]));
    f.age(&id);
    // The journal entry is deliberately NOT acked — today's production
    // shape.
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert_eq!(f.resolve(&id), None, "local reclaim survives");
}

#[test]
fn a_wire_arrived_asset_row_marked_record_synced_is_skipped_by_the_sweep() {
    let _g = shared();
    // A never-synced local asset stays sweepable. (Dart applied the remote
    // row through the SyncApplier, which stamps record_sync_state 'synced';
    // the row is written here as the applier leaves it.)
    let f = Fixture::new();
    // Local, never-synced garbage for contrast — aged past the grace window.
    let local_id = f.import(&f.make_src("local.wav", &[9]));
    f.age(&local_id);
    ack_journal(&f);

    // A remote asset record (no local bytes at all): unreferenced and
    // ancient, i.e. exactly what the sweep would eat if the applier left the
    // row on the 'localOnly' default. Another device may reference it —
    // local reachability is not knowledge here.
    let remote_id = "ASSETASSETASSETASSETASSET1";
    let h = Hlc::new(1, 0, pad_id("REMOTE"));
    f.db.insert(&AssetRow {
        id: remote_id.to_string(),
        content_hash: "HASH_REMOTE".to_string(),
        byte_size: 8,
        ext: "wav".to_string(),
        kind: "audio".to_string(),
        created_at: h.wall_ms,
        updated_at: h.wall_ms,
        hlc_wall: h.wall_ms,
        hlc_ctr: h.counter,
        hlc_node: h.node.clone(),
        hlc_packed: h.packed(),
        record_sync_state: "synced".to_string(),
        ..Default::default()
    })
    .unwrap();

    // Only the local never-synced asset is swept; the wire-arrived row is
    // protected by its record_sync_state.
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert_eq!(f.resolve(&local_id), None, "still sweepable");
    let remote = f.asset(remote_id);
    assert_eq!(remote.record_sync_state, "synced");
    assert_eq!(remote.deleted_at, None, "never tombstoned");
}

#[test]
fn sweep_orphan_pool_files_reclaims_a_file_left_after_a_tombstone() {
    let _g = shared();
    // A crash between the tombstone commit and the file reclaim.
    let f = Fixture::new();
    // A second, LIVE asset must exist: with zero live rows the sweep's
    // lost-database fuse (deliberately) protects the whole pool instead.
    f.import(&f.make_src("live.wav", &[1]));
    let id = f.import(&f.make_src("crash.wav", &[5, 6, 7]));
    let path = PathBuf::from(f.resolve(&id).unwrap());
    // The row is tombstoned but the pooled file is still on disk. The
    // unreferenced sweep never revisits it (it iterates only live rows), so
    // the orphan sweep — trusting only LIVE hashes — must reclaim it.
    f.tombstone(&id);
    assert!(path.exists());
    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 1);
    assert!(!path.exists()); // gone from the pool...
                             // ...but parked in the trash, not unlinked.
    let base = path.file_name().unwrap().to_string_lossy().into_owned();
    let trashed: Vec<String> = names_in(&f.store.trash_dir())
        .into_iter()
        .filter(|n| n.ends_with(&base))
        .collect();
    assert_eq!(trashed.len(), 1);
}

fn hash_file(f: &Fixture, c: char, ext: &str) -> PathBuf {
    let hash: String = std::iter::repeat_n(c, 64).collect();
    let pair: String = std::iter::repeat_n(c, 2).collect();
    let path = f
        .root()
        .join("assets")
        .join(&pair)
        .join(&pair)
        .join(format!("{hash}.{ext}"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

#[test]
fn orphan_sweep_fuse_an_empty_database_with_a_populated_pool_touches_nothing() {
    let _g = shared();
    // Database lost, protect the bytes. Zero asset rows at all (a
    // wiped/replaced DB file) while the pool still holds content-addressed
    // bytes — the only legitimate zero-live-rows state is a fresh install
    // with NO pool, so the sweep must skip.
    let f = Fixture::new();
    let orphan = hash_file(&f, 'a', "wav");
    fs::write(&orphan, [9, 9, 9]).unwrap();

    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 0);
    assert!(orphan.exists()); // bytes protected
    assert!(!f.store.trash_dir().exists()); // nothing trashed
}

fn fuse_tripped_at(f: &Fixture) -> Option<String> {
    f.db.query_opt(
        "SELECT value FROM sync_state WHERE \"key\" = ?",
        ["pool_fuse_at"],
        |r| r.get(0),
    )
    .unwrap()
}

// The fuse switched off as soon as the new library had one live row: the
// first take recorded into it, and the next sweep trashed every byte of the
// lost library (gone for good 30 days later).
#[test]
fn the_fuse_holds_the_lost_librarys_bytes_after_the_new_ones_first_take() {
    let _g = shared();
    // And still reclaims the new one's own leftovers.
    let f = Fixture::new();
    let old = hash_file(&f, 'a', "wav");
    fs::write(&old, [9, 9, 9]).unwrap();
    set_mtime_from_now(&old, -3_600_000);

    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 0); // boot 1: the fuse trips
    f.import(&f.make_src("new.wav", &[1, 2, 3])); // a take

    // A crash leftover of the new library: written after the fuse tripped.
    let leftover = hash_file(&f, 'b', "wav");
    fs::write(&leftover, [7]).unwrap();
    set_mtime_from_now(&leftover, 60_000);

    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 1); // boot 2
    assert!(old.exists(), "the lost library is kept");
    assert!(!leftover.exists());
}

#[test]
fn a_pool_holding_only_a_finder_file_or_a_cut_off_copy_is_no_lost_library() {
    let _g = shared();
    // Remembered once tripped, a fuse set off by these kept every file older
    // than the trip for good.
    let f = Fixture::new();
    f.make_src("assets/.DS_Store", &[]);
    let part = PathBuf::from(f.make_src("assets/.staging/x.part", &[]));

    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 1); // the cut-off copy
    assert!(!part.exists());
    assert_eq!(fuse_tripped_at(&f), None);
}

#[test]
fn a_library_its_user_emptied_still_reclaims_its_own_leftovers_however_old() {
    let _g = shared();
    // Only tombstoned rows: zero live ones, as after a lost database, but
    // these rows prove the files were this library's.
    let f = Fixture::new();
    let id = f.import(&f.make_src("gone.wav", &[4, 5, 6]));
    let path = PathBuf::from(f.resolve(&id).unwrap());
    f.tombstone(&id);
    set_mtime_from_now(&path, -3 * 24 * 3_600_000);

    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 1);
    assert!(!path.exists());
    assert_eq!(fuse_tripped_at(&f), None);
}

#[test]
fn a_tripped_fuse_still_lets_this_library_reclaim_its_own_leftovers_from_before_the_trip() {
    let _g = shared();
    let f = Fixture::new();
    let lost = hash_file(&f, 'a', "wav");
    fs::write(&lost, [9, 9, 9]).unwrap();
    set_mtime_from_now(&lost, -2 * 3_600_000);
    // This library's own import, tombstoned, its reclaim cut short.
    let id = f.import(&f.make_src("own.wav", &[1, 2]));
    let own = PathBuf::from(f.resolve(&id).unwrap());
    f.tombstone(&id);
    set_mtime_from_now(&own, -3_600_000);

    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 0); // the lost file trips it
    assert!(fuse_tripped_at(&f).is_some());
    f.import(&f.make_src("take.wav", &[3, 3, 3])); // a take

    assert_eq!(f.store.sweep_orphan_pool_files().unwrap(), 1);
    assert!(!own.exists());
    assert!(lost.exists(), "the lost library is kept");
}

#[test]
fn a_gc_reclaimed_pool_file_lands_in_the_trash_and_is_recoverable() {
    let _g = shared();
    let f = Fixture::new();
    let id = f.import(&f.make_src("gone.wav", &[1, 2, 3]));
    let path = PathBuf::from(f.resolve(&id).unwrap());
    ack_journal(&f);

    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert!(!path.exists()); // gone from the pool...

    // ...and sitting in trash under `<epochMs>-<originalName>`, bytes
    // intact.
    let trashed = names_in(&f.store.trash_dir());
    assert_eq!(trashed.len(), 1);
    let base = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(trashed[0].ends_with(&base));
    assert_eq!(
        fs::read(f.store.trash_dir().join(&trashed[0])).unwrap(),
        [1, 2, 3]
    );
}

#[test]
fn prune_trash_removes_only_entries_older_than_the_retention() {
    let _g = shared();
    let f = Fixture::new();
    let trash = f.store.trash_dir();
    fs::create_dir_all(&trash).unwrap();
    let now = now_ms();
    let day = 24 * 3600 * 1000;

    let expired = trash.join(format!("{}-old.wav", now - 31 * day));
    let fresh = trash.join(format!("{now}-fresh.wav"));
    // No parseable epoch prefix: never aged out (don't delete undatable
    // bytes).
    let undated = trash.join("nostamp.wav");
    for p in [&expired, &fresh, &undated] {
        fs::write(p, [1]).unwrap();
    }
    // A swept staging TREE ages out as one entry, recursively.
    let expired_tree = trash.join(format!("{}-audio", now - 40 * day));
    let tree_file = expired_tree.join("p1").join("take.wav");
    fs::create_dir_all(tree_file.parent().unwrap()).unwrap();
    fs::write(&tree_file, [2]).unwrap();

    assert_eq!(
        f.store
            .prune_trash(AssetStore::DEFAULT_TRASH_RETENTION)
            .unwrap(),
        2
    );
    assert!(!expired.exists());
    assert!(!expired_tree.exists());
    assert!(fresh.exists());
    assert!(undated.exists());
}

#[test]
fn asset_ref_columns_covers_every_schema_column_referencing_assets() {
    let _g = shared();
    // Derive the asset-referencing columns STRUCTURALLY from the live schema:
    // every foreign key into assets. If a new column referencing assets ever
    // ships without being added to ASSET_REF_COLUMNS, the GC would treat its
    // assets as unreferenced and reclaim their bytes — fail here instead.
    let f = Fixture::new();
    let mut from_schema = HashSet::new();
    for table in ALL_TABLES {
        let fks =
            f.db.query(&format!("PRAGMA foreign_key_list({table})"), [], |r| {
                Ok((r.get::<_, String>("table")?, r.get::<_, String>("from")?))
            })
            .unwrap();
        for (target, from) in fks {
            if target == "assets" {
                from_schema.insert(format!("{table}.{from}"));
            }
        }
    }
    assert!(!from_schema.is_empty()); // the derivation itself must keep working
    let declared: HashSet<String> = AssetStore::ASSET_REF_COLUMNS
        .iter()
        .map(|r| format!("{}.{}", r.table, r.column))
        .collect();
    assert_eq!(
        declared, from_schema,
        "ASSET_REF_COLUMNS must list exactly the schema columns that \
         reference assets: a missing one makes the GC delete live bytes; a \
         stale one breaks the sweep query."
    );
}

#[test]
fn sweep_deletes_every_pooled_file_for_a_hash() {
    let _g = shared();
    // Same bytes, two extensions.
    let f = Fixture::new();
    let id = f.import(&f.make_src("dup.wav", &[7, 7, 7]));
    let wav = f.resolve(&id).unwrap();
    // Simulate identical bytes pooled under a different extension by another
    // device (same hash, `<hash>.wav` AND `<hash>.mp3`).
    let mp3 = format!("{}.mp3", wav.strip_suffix(".wav").unwrap());
    fs::copy(&wav, &mp3).unwrap();
    assert!(Path::new(&mp3).exists());
    ack_journal(&f);
    // The asset is unreferenced -> sweep tombstones it and deletes ALL files
    // for the hash, not just the current row's `.wav`.
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert!(!Path::new(&wav).exists());
    assert!(!Path::new(&mp3).exists());
}

#[test]
fn a_referenced_asset_survives_sweep_then_is_collectable_once_unused() {
    let _g = shared();
    // Dart saved a practice with one vocal take through the repository; its
    // rows are written here as that save writes them.
    let f = Fixture::new();
    let stamp = |seed: &str| {
        let s = f.writer.stamp();
        (pad_id(seed), s)
    };
    let (practice_id, s) = stamp("P");
    f.db.insert(&PracticeRow {
        id: practice_id.clone(),
        title: "S".to_string(),
        created_at: s.at(),
        updated_at: s.at(),
        hlc_wall: s.hlc.wall_ms,
        hlc_ctr: s.hlc.counter,
        hlc_node: s.hlc.node.clone(),
        hlc_packed: s.hlc.packed(),
        ..Default::default()
    })
    .unwrap();
    let (track_id, s) = stamp("T");
    f.db.insert(&TrackRow {
        id: track_id.clone(),
        practice_id: practice_id.clone(),
        kind: "vocal".to_string(),
        display_name: "Take".to_string(),
        created_at: s.at(),
        updated_at: s.at(),
        hlc_wall: s.hlc.wall_ms,
        hlc_ctr: s.hlc.counter,
        hlc_node: s.hlc.node.clone(),
        hlc_packed: s.hlc.packed(),
        ..Default::default()
    })
    .unwrap();
    let clip_asset = f
        .store
        .resolve_or_import(
            &f.make_src("take.wav", &[9, 9, 9]),
            AssetStore::DEFAULT_KIND,
        )
        .unwrap();
    let (clip_id, s) = stamp("C");
    f.db.insert(&ClipRow {
        id: clip_id.clone(),
        track_id,
        asset_id: clip_asset.clone(),
        duration_ms: 1000,
        created_at: s.at(),
        updated_at: s.at(),
        hlc_wall: s.hlc.wall_ms,
        hlc_ctr: s.hlc.counter,
        hlc_node: s.hlc.node.clone(),
        hlc_packed: s.hlc.packed(),
        ..Default::default()
    })
    .unwrap();

    // One referenced asset + one standalone unreferenced asset.
    f.import(&f.make_src("orphan.wav", &[4, 4]));
    assert_eq!(f.assets().len(), 2);
    ack_journal(&f);

    // Sweep removes only the orphan; the clip's asset survives.
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    let live = f.live_assets();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, clip_asset);

    // Drop the clip (the save that removes the take tombstones its row) ->
    // its asset is now unreferenced -> a later sweep (past the age grace
    // window) reclaims it.
    let s = f.writer.stamp();
    f.db.execute(
        "UPDATE clips SET deleted_at = ?, updated_at = ?, hlc_packed = ? WHERE id = ?",
        rusqlite::params![s.at(), s.at(), s.hlc.packed(), clip_id],
    )
    .unwrap();
    ack_journal(&f);
    assert_eq!(f.store.sweep_unreferenced(ZERO).unwrap(), 1);
    assert!(f.live_assets().is_empty());
}

// Pool content verification.

#[test]
fn a_pooled_file_whose_bytes_changed_under_its_content_address_is_reported_and_left_alone() {
    let _g = shared();
    let f = Fixture::new();
    let id = f.import(&f.make_src("take.wav", &[1, 2, 3]));
    let pooled = f.resolve(&id).unwrap();
    assert!(f.store.verify_pool_integrity().unwrap().is_empty());

    // Same length, different bytes: exactly what the import path's length
    // check cannot see (and what it would never create — this is rot or an
    // outside edit), so the content check has to be the one that finds it.
    fs::write(&pooled, [1, 2, 4]).unwrap();

    assert_eq!(
        f.store.verify_pool_integrity().unwrap(),
        std::slice::from_ref(&pooled)
    );
    // Reported, never touched: it is still the user's recording.
    assert!(Path::new(&pooled).exists());
    assert_eq!(fs::read(&pooled).unwrap(), [1, 2, 4]);
}

#[test]
fn a_truncated_pool_file_is_caught_by_the_import_path_before_verification_is_needed() {
    let _g = shared();
    let f = Fixture::new();
    let src = f.make_src("take.wav", &[1, 2, 3]);
    let id = f.import(&src);
    let pooled = f.resolve(&id).unwrap();
    fs::write(&pooled, [1, 2]).unwrap(); // crash leftover

    // Re-importing the same source replaces the short file with the fresh,
    // verified bytes (the length guard's actual job).
    assert_eq!(f.import(&src), id);
    assert_eq!(fs::read(&pooled).unwrap(), [1, 2, 3]);
    assert!(f.store.verify_pool_integrity().unwrap().is_empty());
}

#[test]
fn an_in_flight_part_copy_is_not_mistaken_for_a_damaged_pool_file() {
    let _g = shared();
    let f = Fixture::new();
    f.make_src("assets/.staging/ABC.part", &[9, 9]);
    assert!(f.store.verify_pool_integrity().unwrap().is_empty());
}
