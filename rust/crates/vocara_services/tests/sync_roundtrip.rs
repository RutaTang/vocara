//! Port of `client/integration_test/sync_roundtrip_test.dart`: end-to-end
//! sync tests across TWO simulated devices, each its own fully-wired
//! in-memory stack (`Persistence::memory()`) with a distinct HLC node id.
//! Every flow mirrors what a real sync pusher/puller does: the local repo
//! writes through the SyncWriter chokepoint, the pusher drains the oplog to
//! learn WHICH rows changed, reads those rows' full column state, ships
//! them as `RemoteRecord`s, and the remote side adopts them through the
//! `SyncApplier` "remote door" (entity-level LWW by HLC, never
//! re-journaled).

use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

use vocara_model::models::song_practice::SongPractice;
use vocara_services::persistence::Persistence;
use vocara_store::db::rows::OplogRow;
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::sync_applier::{RemoteRecord, SyncApplier};
use vocara_store::sync::sync_row::{FieldValue, Fields, SyncRow, SyncRowBuilder};

/// Device A and device B, and each one's remote door — sharing that
/// device's real HlcClock (the same instance the local SyncWriter ticks) so
/// quarantine + clock-poisoning assertions reflect the actual device clock.
struct Devices {
    a: Persistence,
    b: Persistence,
    applier_a: SyncApplier,
    applier_b: SyncApplier,
}

impl Devices {
    fn new() -> Devices {
        let a = Persistence::memory().unwrap();
        let b = Persistence::memory().unwrap();
        let applier_a = door(&a);
        let applier_b = door(&b);
        Devices {
            a,
            b,
            applier_a,
            applier_b,
        }
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        // The in-memory libraries close with their last handle; the pools
        // `Persistence::memory()` made are the caller's to remove.
        for dev in [&self.a, &self.b] {
            let _ = fs::remove_dir_all(dev.assets().root());
        }
    }
}

fn door(dev: &Persistence) -> SyncApplier {
    SyncApplier::new(
        dev.db().clone(),
        dev.writer().clock().clone(),
        Arc::new(SyncRowBuilder::new(dev.db().clone())),
        None,
    )
}

/// Reads a full row (all columns) of [table]/[id] on [dev], or None.
fn row_of(dev: &Persistence, table: &str, id: &str) -> Option<Fields> {
    dev.db().with_conn(|c| {
        let mut st = c
            .prepare(&format!("SELECT * FROM {table} WHERE id = ?"))
            .unwrap();
        let names: Vec<String> = st.column_names().map(str::to_string).collect();
        let mut rows = st.query([id]).unwrap();
        let row = rows.next().unwrap()?;
        let mut fields = Fields::new();
        for (i, name) in names.iter().enumerate() {
            let value = FieldValue::from_sql(row.get_ref(i).unwrap()).expect("no BLOB columns");
            fields.insert(name.clone(), value);
        }
        Some(fields)
    })
}

/// All currently-pending oplog rows on [dev], oldest first. This is exactly
/// the change-set a pusher would drain.
fn oplog_of(dev: &Persistence) -> Vec<OplogRow> {
    dev.db().select("1 ORDER BY seq", []).unwrap()
}

/// Builds the [`RemoteRecord`]s a pusher would ship from [src] for the given
/// oplog entries: for each (table, id) take the current full row state.
/// Skips rows that no longer exist (defensive; shouldn't happen for our
/// tables).
fn ship_from(src: &Persistence, ops: &[OplogRow]) -> Vec<RemoteRecord> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for op in ops {
        let key = format!("{}/{}", op.entity_table, op.entity_id);
        if !seen.insert(key) {
            continue; // collapse to latest row state
        }
        let Some(cols) = row_of(src, &op.entity_table, &op.entity_id) else {
            continue;
        };
        out.push(SyncRow::new(op.entity_table.clone(), cols));
    }
    out
}

/// Live (non-tombstoned) practice titles visible to a device's repository.
fn live_titles(dev: &Persistence) -> Vec<String> {
    dev.song_repo()
        .list_practices()
        .unwrap()
        .into_iter()
        .map(|p| p.title)
        .collect()
}

/// `SongPractice(id:, title:, createdAt:, updatedAt:, versions:,
/// activeVersionId:)` built from [base] with [title].
fn renamed(base: &SongPractice, title: &str) -> SongPractice {
    SongPractice {
        versions: base.versions.clone(),
        active_version_id: base.active_version_id.clone(),
        ..SongPractice::new(base.id.clone(), title, base.created_at, base.updated_at)
    }
}

fn text<'a>(row: &'a Fields, column: &str) -> &'a str {
    row[column]
        .as_str()
        .unwrap_or_else(|| panic!("{column} is text"))
}

#[test]
fn a_creates_and_edits_a_practice_and_the_change_set_converges_onto_b() {
    let devs = Devices::new();
    let (a, b) = (&devs.a, &devs.b);

    // --- A: create then rename a practice through its repo (local writes). ---
    let created = a.song_repo().create_practice("Hai Kuo").unwrap();
    let renamed = a
        .song_repo()
        .save_practice(&renamed(&created, "Hai Kuo Tian Kong"))
        .unwrap();
    assert_eq!(renamed.title, "Hai Kuo Tian Kong");

    // --- Pusher: drain A's oplog, ship the touched rows. ---
    let ops = oplog_of(a);
    assert!(!ops.is_empty(), "local writes must journal");
    let records = ship_from(a, &ops);
    assert!(!records.is_empty());

    // --- B: apply through the remote door. ---
    let res = devs.applier_b.apply(&records);
    assert_eq!(res.quarantined, 0);
    assert!(res.applied > 0);

    // --- B converges to A's state (entity-level LWW). ---
    let on_b = b.song_repo().get_practice(&created.id);
    let pb = on_b.expect("the practice + its interpretation should have arrived");
    assert_eq!(pb.title, "Hai Kuo Tian Kong");
    assert_eq!(pb.id, created.id);
    assert!(
        !pb.versions.is_empty(),
        "the seeded interpretation rode along"
    );

    // Row-level: B's practice HLC equals A's (it ADOPTED the remote stamp).
    let a_row = row_of(a, "practices", &created.id).unwrap();
    let b_row = row_of(b, "practices", &created.id).unwrap();
    assert_eq!(b_row["hlc_packed"], a_row["hlc_packed"]);
    assert_eq!(b_row["title"], a_row["title"]);
}

#[test]
fn concurrent_conflict_the_higher_hlc_write_wins_on_both_devices_lww() {
    // Same entity created independently, then edited on both with DIFFERENT
    // HLCs. We compare the two candidate row states by packed HLC: the higher
    // one must end up live on both devices regardless of apply direction.
    let devs = Devices::new();
    let (a, b) = (&devs.a, &devs.b);
    let base = a.song_repo().create_practice("shared").unwrap();
    let id = base.id.clone();

    // Seed B with the same id at a baseline state so both have the row.
    devs.applier_b.apply(&ship_from(a, &oplog_of(a)));

    // A edits.
    a.song_repo()
        .save_practice(&renamed(&base, "edited-on-A"))
        .unwrap();
    // B edits the SAME row (its own local write, its own node + clock).
    let b_base = b.song_repo().get_practice(&id).unwrap();
    b.song_repo()
        .save_practice(&renamed(&b_base, "edited-on-B"))
        .unwrap();

    let a_edit = row_of(a, "practices", &id).unwrap();
    let b_edit = row_of(b, "practices", &id).unwrap();
    let a_packed = text(&a_edit, "hlc_packed").to_string();
    let b_packed = text(&b_edit, "hlc_packed").to_string();
    assert_ne!(a_packed, b_packed, "distinct nodes -> distinct HLCs");

    // The deterministic winner by total HLC order.
    let a_wins = a_packed > b_packed;
    let expected_title = if a_wins { "edited-on-A" } else { "edited-on-B" };

    // Cross-apply both directions: A's edit -> B, and B's edit -> A. Only the
    // higher HLC is adopted; the lower one is skipped.
    let a_record = SyncRow::new("practices", a_edit.clone());
    let b_record = SyncRow::new("practices", b_edit.clone());

    let to_b = devs.applier_b.apply(&[a_record]);
    let to_a = devs.applier_a.apply(&[b_record]);

    // Exactly one side adopts, the other skips — deterministically.
    assert_eq!(to_b.applied + to_b.skipped, 1);
    assert_eq!(to_a.applied + to_a.skipped, 1);
    assert_eq!(to_b.applied, usize::from(a_wins));
    assert_eq!(to_a.applied, usize::from(!a_wins));

    // Both devices now hold the identical winning row.
    let final_a = row_of(a, "practices", &id).unwrap();
    let final_b = row_of(b, "practices", &id).unwrap();
    assert_eq!(text(&final_a, "title"), expected_title);
    assert_eq!(text(&final_b, "title"), expected_title);
    assert_eq!(
        final_a["hlc_packed"], final_b["hlc_packed"],
        "converged to the same winner -> no split brain"
    );
}

#[test]
fn tombstone_propagation_a_delete_on_a_tombstones_and_hides_the_row_on_b() {
    let devs = Devices::new();
    let (a, b) = (&devs.a, &devs.b);
    let created = a.song_repo().create_practice("to delete").unwrap();
    let id = created.id.clone();

    // Get the baseline onto B.
    devs.applier_b.apply(&ship_from(a, &oplog_of(a)));
    assert!(b.song_repo().get_practice(&id).is_ok());

    // A deletes (soft-delete cascade -> new tombstone oplog entries).
    let before_ops = oplog_of(a).len();
    a.song_repo().delete_practice(&id).unwrap();
    let after_ops = oplog_of(a);
    assert!(
        after_ops.len() > before_ops,
        "a delete journals tombstone ops"
    );

    // Ship ONLY the new (post-delete) ops and apply to B.
    let delete_ops = &after_ops[before_ops..];
    let res = devs.applier_b.apply(&ship_from(a, delete_ops));
    assert!(res.applied > 0);

    // B's practice row is now tombstoned and excluded from reads.
    let b_row = row_of(b, "practices", &id).unwrap();
    assert!(!b_row["deleted_at"].is_null());
    assert!(b.song_repo().get_practice(&id).is_err());
    assert!(!live_titles(b).contains(&"to delete".to_string()));
}

#[test]
fn future_clock_quarantine_rejects_an_absurd_future_record_without_poisoning_the_clock_and_repair_reaches_a_fixpoint(
) {
    let devs = Devices::new();
    let (a, b) = (&devs.a, &devs.b);
    let created = a.song_repo().create_practice("normal").unwrap();
    devs.applier_b.apply(&ship_from(a, &oplog_of(a)));

    let clock = b.writer().clock();
    let before_max_wall = clock.max_wall_ever_seen();
    let before_last = clock.last();

    // Forge a record stamped absurdly far in the future (years ahead), well
    // beyond the clock's drift bound.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let far_future = now_ms + HlcClock::DEFAULT_MAX_DRIFT_MS + 365 * 24 * 60 * 60 * 1000;
    let bad_node = "BADCLOCKBADCLOCKBADCLOCK00";
    let mut poisoned = row_of(a, "practices", &created.id).unwrap();
    poisoned.insert("title".into(), "from the year 3000".into());
    poisoned.insert("hlc_wall".into(), far_future.into());
    poisoned.insert("hlc_ctr".into(), 0i64.into());
    poisoned.insert("hlc_node".into(), bad_node.into());
    poisoned.insert(
        "hlc_packed".into(),
        Hlc::new(far_future, 0, bad_node).packed().into(),
    );

    let res = devs.applier_b.apply(&[SyncRow::new("practices", poisoned)]);
    assert_eq!(res.quarantined, 1, "beyond maxDrift -> quarantined");
    assert_eq!(res.applied, 0);

    // The poison did NOT take: title unchanged, and the clock did not
    // ratchet forward to the bogus future wall.
    let b_row = row_of(b, "practices", &created.id).unwrap();
    assert_eq!(text(&b_row, "title"), "normal");
    assert_eq!(
        clock.max_wall_ever_seen(),
        before_max_wall,
        "a quarantined remote must not advance maxWallEverSeen"
    );
    assert_eq!(clock.last(), before_last, "the local clock is not poisoned");
    // The next local tick stays plausibly near wall time, not in the far
    // future.
    let next = clock.tick();
    assert!(next.wall_ms < far_future);
    assert!((next.wall_ms as f64).is_finite());

    // Integrity repair runs to a fixpoint with no remaining dangling refs.
    let checker = IntegrityChecker::new(b.db().clone(), b.writer().clone());
    let remaining = checker.repair().unwrap();
    assert!(remaining.is_empty());
    assert!(
        checker.check().unwrap().is_empty(),
        "a clean store stays clean after repair"
    );
}

#[test]
fn the_sync_applier_never_re_journals_applied_remote_ops_no_ping_pong() {
    let devs = Devices::new();
    let (a, b) = (&devs.a, &devs.b);
    // Build a varied change-set on A: create, rename, delete a second
    // practice.
    let keep = a.song_repo().create_practice("keep").unwrap();
    a.song_repo()
        .save_practice(&renamed(&keep, "keep renamed"))
        .unwrap();
    let gone = a.song_repo().create_practice("gone").unwrap();
    a.song_repo().delete_practice(&gone.id).unwrap();

    // B starts with an empty oplog.
    assert!(oplog_of(b).is_empty());

    let records = ship_from(a, &oplog_of(a));
    assert!(!records.is_empty());

    let res = devs.applier_b.apply(&records);
    assert!(res.applied > 0);

    // Applying remote changes writes NO oplog on B — an applied remote op can
    // never be re-pushed (the two-door invariant that prevents an infinite
    // ping-pong).
    assert!(oplog_of(b).is_empty());

    // Re-applying the SAME records is idempotent and still journals nothing:
    // every record loses LWW against what was just adopted.
    let again = devs.applier_b.apply(&records);
    assert_eq!(again.applied, 0);
    assert!(again.skipped > 0);
    assert!(oplog_of(b).is_empty());

    // B's live state matches A's: 'keep renamed' present, 'gone' absent.
    let titles = live_titles(b);
    assert!(titles.contains(&"keep renamed".to_string()));
    assert!(!titles.contains(&"gone".to_string()));
}
