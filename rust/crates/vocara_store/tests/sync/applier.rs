//! Port of `client/test/sync_applier_test.dart`.

use std::sync::Arc;

use vocara_store::db::rows::SyncQuarantineRow;
use vocara_store::db::AppDatabase;
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::quarantine_store::QuarantineStore;
use vocara_store::sync::sync_applier::{ApplyOptions, SyncApplier};
use vocara_store::sync::sync_row::{FieldValue, SyncRow, SyncRowBuilder, CURRENT_RECORD_SCHEMA_V};
use vocara_store::sync::sync_state_store::SyncStateStore;
use vocara_store::sync::sync_writer::SyncWriter;

use crate::support::*;

fn remote_node() -> String {
    id("REMOTE")
}

fn hlc(wall: i64) -> Hlc {
    Hlc::new(wall, 0, remote_node())
}

fn clock_at(now: i64) -> Arc<HlcClock> {
    Arc::new(HlcClock::new(id("LOCAL")).with_now(move || now))
}

fn clock_reading(now: &Now) -> Arc<HlcClock> {
    Arc::new(HlcClock::new(id("LOCAL")).with_now(now.reader()))
}

/// The counter-only applier (no quarantine store), clock at 1000.
fn plain(db: &Arc<AppDatabase>) -> SyncApplier {
    SyncApplier::new(
        db.clone(),
        clock_at(1000),
        Arc::new(SyncRowBuilder::new(db.clone())),
        None,
    )
}

/// The production wiring: the applier parks into a quarantine store.
fn parking_with(db: &Arc<AppDatabase>, clock: Arc<HlcClock>) -> (SyncApplier, QuarantineStore) {
    let store = QuarantineStore::new(db.clone());
    (
        SyncApplier::new(
            db.clone(),
            clock,
            Arc::new(SyncRowBuilder::new(db.clone())),
            Some(store.clone()),
        ),
        store,
    )
}

fn parking(db: &Arc<AppDatabase>) -> (SyncApplier, QuarantineStore) {
    parking_with(db, clock_at(1000))
}

// Every helper below builds a FULL-row snapshot (all non-per-device columns
// of its table), matching the wire contract the applier enforces for
// schema_v == CURRENT_RECORD_SCHEMA_V records: a partial row is quarantined
// at the missing-column gate, never partially applied.

fn practice_full(
    row_id: &str,
    title: &str,
    h: &Hlc,
    deleted_at: Option<i64>,
    separation_asset_id: Option<&str>,
) -> SyncRow {
    SyncRow::new(
        "practices",
        fields! {
            "id" => id(row_id),
            "title" => title,
            "timeline_duration_ms" => 0i64,
            "master_volume" => 1.0,
            "active_interpretation_id" => NULL,
            "separation_asset_id" => separation_asset_id.map(id),
            "reverb_json" => NULL,
            "lyrics_json" => NULL,
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => deleted_at,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    )
}

fn practice(row_id: &str, title: &str, h: &Hlc) -> SyncRow {
    practice_full(row_id, title, h, None, None)
}

fn track(row_id: &str, practice_id: &str, h: &Hlc) -> SyncRow {
    SyncRow::new(
        "tracks",
        fields! {
            "id" => id(row_id),
            "practice_id" => id(practice_id),
            "kind" => "vocal",
            "interpretation_id" => NULL,
            "display_name" => "",
            "color_value" => NULL,
            "muted" => 0i64,
            "soloed" => 0i64,
            "volume" => 1.0,
            "pan" => 0.0,
            "bus_id" => NULL,
            "fx_json" => NULL,
            "sends_json" => NULL,
            "order_index" => 0i64,
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    )
}

fn clip(row_id: &str, track_id: &str, asset_id: &str, h: &Hlc, duration_ms: i64) -> SyncRow {
    SyncRow::new(
        "clips",
        fields! {
            "id" => id(row_id),
            "track_id" => id(track_id),
            "asset_id" => id(asset_id),
            "start_ms" => 0i64,
            "duration_ms" => duration_ms,
            "order_index" => 0i64,
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    )
}

fn asset_full(row_id: &str, h: &Hlc, ext: &str, deleted_at: Option<i64>) -> SyncRow {
    SyncRow::new(
        "assets",
        fields! {
            "id" => id(row_id),
            "content_hash" => format!("HASH_{row_id}"),
            "byte_size" => 8i64,
            "ext" => ext,
            "kind" => "audio",
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => deleted_at,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    )
}

fn asset_record(row_id: &str, h: &Hlc) -> SyncRow {
    asset_full(row_id, h, "wav", None)
}

/// An assets row whose CHECK (byte_size >= 0) fires inside its savepoint.
fn bad_asset(h: &Hlc) -> SyncRow {
    SyncRow::new(
        "assets",
        fields! {
            "id" => id("BAD"),
            "content_hash" => "HASH_BAD",
            "byte_size" => -1i64,
            "ext" => "wav",
            "kind" => "audio",
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    )
}

fn interp_raw(raw_id: &str, practice_id: &str, h: &Hlc, deleted_at: Option<i64>) -> SyncRow {
    SyncRow::new(
        "interpretations",
        fields! {
            "id" => raw_id,
            "practice_id" => id(practice_id),
            "name" => "",
            "order_index" => 0i64,
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => deleted_at,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    )
}

fn title(db: &AppDatabase, row_id: &str) -> Option<String> {
    col(db, "practices", &id(row_id), "title")
}

fn practice_exists(db: &AppDatabase, row_id: &str) -> bool {
    exists(db, "practices", &id(row_id))
}

fn hlc_packed_of(db: &AppDatabase, table: &str, row_id: &str) -> Option<String> {
    col(db, table, &id(row_id), "hlc_packed")
}

fn deleted_at(db: &AppDatabase, table: &str, row_id: &str) -> Option<Option<i64>> {
    col(db, table, &id(row_id), "deleted_at")
}

fn parked(store: &QuarantineStore) -> Vec<SyncQuarantineRow> {
    store.list().unwrap()
}

fn parked_reasons(store: &QuarantineStore) -> Vec<String> {
    parked(store).into_iter().map(|q| q.reason).collect()
}

#[test]
fn a_new_record_is_inserted() {
    let db = memory_db();
    let applier = plain(&db);
    let res = applier.apply(&[practice("P", "Hello", &hlc(900))]);
    assert_eq!(res.applied, 1);
    assert_eq!(title(&db, "P").as_deref(), Some("Hello"));
}

#[test]
fn a_higher_hlc_update_wins_a_lower_hlc_update_is_skipped() {
    let db = memory_db();
    let applier = plain(&db);
    applier.apply(&[practice("P", "v1", &hlc(900))]);

    let win = applier.apply(&[practice("P", "v2", &hlc(950))]);
    assert_eq!(win.applied, 1);
    assert_eq!(title(&db, "P").as_deref(), Some("v2"));

    let lose = applier.apply(&[practice("P", "stale", &hlc(920))]);
    assert_eq!(lose.skipped, 1);
    assert_eq!(lose.applied, 0);
    assert_eq!(title(&db, "P").as_deref(), Some("v2")); // unchanged
}

#[test]
fn a_tombstone_arrives_as_a_higher_hlc_update() {
    let db = memory_db();
    let applier = plain(&db);
    applier.apply(&[practice("P", "v1", &hlc(900))]);
    let res = applier.apply(&[practice_full("P", "v1", &hlc(970), Some(970), None)]);
    assert_eq!(res.applied, 1);
    assert_eq!(deleted_at(&db, "practices", "P"), Some(Some(970)));
}

#[test]
fn an_implausibly_future_record_is_quarantined_not_applied() {
    let db = memory_db();
    let applier = plain(&db);
    // now=1000, default maxDrift=5min -> wall 5min+1 ahead is rejected.
    let res = applier.apply(&[practice(
        "FUT",
        "from the future",
        &hlc(1000 + 5 * 60 * 1000 + 1),
    )]);
    assert_eq!(res.quarantined, 1);
    assert_eq!(res.applied, 0);
    assert!(!practice_exists(&db, "FUT"));
}

#[test]
fn a_malformed_record_is_quarantined_without_discarding_the_batch() {
    let db = memory_db();
    let applier = plain(&db);
    let good = practice("GOOD", "kept", &hlc(900));
    // A record carrying a column that does not exist locally is refused at
    // the applier's structural column gate (its name must never reach the
    // SQL text). It must reject only itself, not the whole batch, so the
    // already-applied good record stays.
    let bad = with(
        &practice("BAD", "dropped", &hlc(910)),
        &[("column_from_the_future", 1i64.into())],
    );
    let res = applier.apply(&[good, bad]);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);
    assert_eq!(title(&db, "GOOD").as_deref(), Some("kept")); // survived the later failure
    assert!(!practice_exists(&db, "BAD")); // never written
}

#[test]
fn an_unknown_table_is_quarantined_at_the_door_and_the_rest_of_the_batch_applies() {
    // Its name must never reach the SQL text.
    let db = memory_db();
    let applier = plain(&db);
    let good = practice("GOOD", "kept", &hlc(900));
    // A table name that is not in the synced registry — whether a hostile
    // payload or a table from a future schema, it must not be interpolated.
    let bad = SyncRow::new(
        "practices; DROP TABLE practices; --",
        practice("BAD", "dropped", &hlc(910)).fields().clone(),
    );
    let res = applier.apply(&[good, bad]);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);
    assert_eq!(title(&db, "GOOD").as_deref(), Some("kept"));
    assert!(!practice_exists(&db, "BAD"));
}

#[test]
fn a_local_only_table_is_refused_even_though_it_exists_in_the_database() {
    // Only SYNCED tables are writable through the remote door.
    let db = memory_db();
    let applier = plain(&db);
    let res = applier.apply(&[SyncRow::new(
        "sync_state", // real table, not in the synced registry
        practice("X", "nope", &hlc(900)).fields().clone(),
    )]);
    assert_eq!(res.failed, 1);
    assert_eq!(res.applied, 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM sync_state", &[]), 0);
}

#[test]
fn a_record_whose_hlc_quad_is_mistyped_is_quarantined_alone() {
    // Regression: a type error out of rec.hlc used to escape the per-record
    // handling, fail the WHOLE batch, and (through the engine) retry the same
    // page forever — one bad record bricked sync and blocked every good
    // record on the page.
    let db = memory_db();
    let applier = plain(&db);
    let bad = with(
        &practice("BAD", "dropped", &hlc(910)),
        &[("hlc_wall", "not-an-int".into())],
    );
    let res = applier.apply(&[
        practice("BEFORE", "first", &hlc(900)),
        bad,
        practice("AFTER", "second", &hlc(920)),
    ]);
    assert_eq!(res.applied, 2);
    assert_eq!(res.failed, 1);
    assert_eq!(title(&db, "BEFORE").as_deref(), Some("first"));
    assert_eq!(title(&db, "AFTER").as_deref(), Some("second"));
    assert!(!practice_exists(&db, "BAD"));
}

#[test]
fn a_record_whose_id_would_escape_the_media_tree_is_quarantined_alone() {
    // `../../x` as a practice id would later be joined into
    // `audio/<practiceId>/` and handed to recursive deletes — a hostile sync
    // record must never get a path metacharacter into the table.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let evil = with(
        &practice("EVIL", "escape", &hlc(910)),
        &[("id", "../../escape".into())],
    );
    // An FK id column is the same primitive: technique media directories are
    // keyed by the REFERENCED id.
    let evil_fk = with(
        &practice("EVILFK", "fk", &hlc(915)),
        &[("separation_asset_id", "../pool".into())],
    );
    let res = applier.apply(&[
        practice("GOODA", "first", &hlc(900)),
        evil,
        evil_fk,
        practice("GOODB", "second", &hlc(920)),
    ]);
    assert_eq!(res.applied, 2);
    assert_eq!(res.failed, 2);
    assert_eq!(title(&db, "GOODA").as_deref(), Some("first"));
    assert_eq!(title(&db, "GOODB").as_deref(), Some("second"));
    let reasons = parked_reasons(&store);
    assert_eq!(reasons.len(), 2);
    assert!(
        reasons.iter().all(|r| r.starts_with("invalid-id")),
        "{reasons:?}"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM practices", &[]),
        2,
        "neither hostile row may land"
    );
}

#[test]
fn an_id_outside_26_to_36_characters_parks_as_invalid_id_instead_of_landing_through_raw_sql() {
    // drift checked that length only on its own inserts, so the applier's
    // raw INSERT let a 5-character track id land — a row the repository's
    // revive path (a checked insert) then refused, failing the save.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[
        practice("P", "kept", &hlc(900)),
        with(&track("T", "P", &hlc(910)), &[("id", "SHORT".into())]),
        interp_raw(&"L".repeat(37), "P", &hlc(911), None),
        // The shapes that do occur: a legacy migration's derived id.
        interp_raw(&format!("{}__v1", id("P")), "P", &hlc(912), None),
    ]);
    assert_eq!(res.applied, 2);
    assert_eq!(res.failed, 2);
    let reasons = parked_reasons(&store);
    assert!(
        reasons.iter().all(|r| r.starts_with("invalid-id")),
        "{reasons:?}"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM tracks", &[]),
        0,
        "the short id must not land"
    );
}

#[test]
fn an_unaddressable_record_parks_as_a_dead_letter_instead_of_vanishing() {
    // A record with no usable id used to be dropped silently while its page
    // committed and advanced the change token — unrecoverable, unaccounted.
    // It now parks under a synthesized content-hash key with the raw
    // evidence; the replay's decode refuses it, so it stays for review.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let no_id = with(&practice("X", "x", &hlc(910)), &[("id", "".into())]);
    let res = applier.apply(&[practice("GOOD", "kept", &hlc(900)), no_id]);
    assert!(res.committed);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    let q = &parked[0];
    assert!(q.reason.starts_with("dead-letter("), "{}", q.reason);
    assert!(q.id.starts_with("deadletter"));
    // Wide enough that two distinct dead letters cannot share a row: a
    // collision here is silent LOSS, because the quarantine upsert compares
    // hlc_packed and a dead letter's is usually empty, so the second would
    // be dropped rather than stored.
    assert!(q.id.len() > 24);
    // Never re-applied: the payload is deliberately not a SyncRow envelope.
    assert!(SyncRow::decode(&q.payload).is_err());
}

#[test]
fn two_different_dead_letters_park_separately() {
    // They key off a content digest, and the quarantine upsert resolves a
    // (table, id) clash by hlc_packed — which a dead letter usually has none
    // of, so a digest collision silently discarded the second record.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[with(
        &practice("X", "first", &hlc(910)),
        &[("id", "".into())],
    )]);
    applier.apply(&[with(
        &practice("Y", "second", &hlc(911)),
        &[("id", "".into())],
    )]);
    let parked = parked(&store);
    assert_eq!(parked.len(), 2);
    assert_ne!(parked[0].id, parked[1].id);
}

// --- missing-column gate (full-row wire contract) ----------------------------

#[test]
fn a_partial_row_at_the_current_schema_v_is_quarantined_whole() {
    // Neither content nor hlc_packed moves, so a later genuine full row with
    // an intermediate HLC still wins LWW.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[practice("P", "v1", &hlc(900))]);

    // A partial row with the NEWEST HLC. Accepting it would write hlc_packed
    // 950 while keeping stale content for the missing column — and the
    // genuine full row below (HLC 920) would then lose LWW forever.
    let partial = without(&practice("P", "poisoner", &hlc(950)), "reverb_json");
    let res = applier.apply(&[partial]);
    assert_eq!(res.failed, 1);
    assert_eq!(res.applied, 0);
    assert_eq!(title(&db, "P").as_deref(), Some("v1"), "content unchanged");
    assert_eq!(
        hlc_packed_of(&db, "practices", "P"),
        Some(hlc(900).packed()),
        "the LWW key must not advance on a rejected partial row"
    );
    let reasons = parked_reasons(&store);
    assert_eq!(reasons.len(), 1);
    assert!(reasons[0].starts_with("missing-column"));
    assert!(reasons[0].contains("reverb_json"));

    // The genuine full row between the two HLCs applies normally.
    let late = applier.apply(&[practice("P", "v2", &hlc(920))]);
    assert_eq!(late.applied, 1);
    assert_eq!(title(&db, "P").as_deref(), Some("v2"));
    assert_eq!(
        hlc_packed_of(&db, "practices", "P"),
        Some(hlc(920).packed())
    );
}

#[test]
fn a_below_range_schema_v_never_reaches_the_older_peer_exemption() {
    // Wire versions start at 1, so a v0/negative record is quarantined whole
    // — with missing columns it would otherwise walk between the
    // newer-schema and full-row gates and partially apply.
    let db = memory_db();
    let applier = plain(&db);
    applier.apply(&[practice("P", "v1", &hlc(900))]);

    let h = hlc(950);
    let res = applier.apply(&[SyncRow::new(
        "practices",
        fields! {
            "id" => id("P"),
            "title" => "from a below-range build",
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 0i64,
        },
    )]);
    assert_eq!(res.failed, 1);
    assert_eq!(res.applied, 0);
    assert_eq!(title(&db, "P").as_deref(), Some("v1"), "content unchanged");
    assert_eq!(
        hlc_packed_of(&db, "practices", "P"),
        Some(hlc(900).packed()),
        "the LWW key must not advance on a rejected record"
    );
}

// --- clock advance timing ----------------------------------------------------

#[test]
fn a_record_that_fails_inside_its_savepoint_leaves_the_clock_exactly_where_it_was() {
    // The gate is check-only; the merge runs post-settle.
    let db = memory_db();
    let applier = plain(&db);
    let before = applier.clock().last();
    // Passes every door gate (full columns, plausible HLC one minute ahead)
    // and then violates the byte_size CHECK inside its savepoint.
    let res = applier.apply(&[bad_asset(&hlc(1000 + 60 * 1000))]);
    assert_eq!(res.failed, 1);
    assert_eq!(res.applied, 0);
    assert_eq!(
        applier.clock().last(),
        before,
        "a rolled-back record must leave NO side effect — an advanced in-memory clock included"
    );
}

#[test]
fn an_applied_record_advances_the_clock_past_its_stamp() {
    let db = memory_db();
    let applier = plain(&db);
    let h = hlc(1000 + 60 * 1000); // one minute ahead, within drift
    let res = applier.apply(&[practice("P", "applied", &h)]);
    assert_eq!(res.applied, 1);
    assert!(applier.clock().last().wall_ms >= h.wall_ms);
}

#[test]
fn a_batch_whose_outer_commit_fails_leaves_the_clock_untouched() {
    // Even though its records had settled inside the transaction.
    let db = memory_db();
    let applier = plain(&db);
    let before = applier.clock().last();
    // The record itself is fine and settles as applied inside the loop; the
    // failing before_commit hook (the engine's token-write seam) then rolls
    // the whole outer transaction back. The database forgot the record — the
    // in-memory clock must have forgotten it too.
    let kill = || -> vocara_store::Result<()> {
        Err(vocara_store::Error::State("kill the outer commit".into()))
    };
    let res = applier.apply_with(
        &[practice(
            "P",
            "settled then rolled back",
            &hlc(1000 + 60 * 1000),
        )],
        ApplyOptions {
            before_commit: Some(&kill),
            ..ApplyOptions::default()
        },
    );
    assert!(!res.committed);
    assert!(!practice_exists(&db, "P"), "the batch rolled back");
    assert_eq!(
        applier.clock().last(),
        before,
        "the database rolled back the settled record; an advanced in-memory clock would be a \
         side effect surviving the rollback"
    );
}

#[test]
fn a_committed_batch_advances_the_clock_exactly_once_per_settled_record() {
    // Never for rejected ones. (Dart counted mergeVerified calls on a
    // subclassed clock; here every merge at the held wall bumps the counter
    // by exactly one, so the counter delta IS the merge count.)
    let db = memory_db();
    let applier = plain(&db);
    // Seed P so a stale copy below settles as a clean LWW skip at the door
    // (the pre-savepoint settle, which never advances the clock).
    applier.apply(&[practice("P", "v1", &hlc(950))]);
    let before = applier.clock().last();
    assert_eq!(before.wall_ms, 1000);

    let res = applier.apply(&[
        practice("A", "applied", &hlc(960)),
        practice("P", "stale", &hlc(900)), // clean LWW skip at the door
        practice("FUT", "poison", &hlc(1000 + 6 * 60 * 1000)), // quarantined
    ]);
    assert_eq!(res.applied, 1);
    assert_eq!(res.skipped, 1);
    assert_eq!(res.quarantined, 1);
    let after = applier.clock().last();
    assert_eq!(after.wall_ms, before.wall_ms);
    assert_eq!(
        after.counter - before.counter,
        1,
        "exactly the applied record — a door-settled skip never merged, and a quarantined record \
         must leave the clock alone"
    );
}

#[test]
fn a_door_parked_record_with_a_newer_schema_v_still_advances_the_clock_and_the_high_water_mark() {
    // So a later local edit can never stamp below the parked record.
    let db = memory_db();
    let (applier, store) = parking(&db);
    // Plausible (one minute ahead, within drift) and from a newer build: the
    // schema gate parks it, and the replay lands it after the app upgrade —
    // the DESIGNED path for a mixed-version fleet. A local edit stamped below
    // h would then be silently reverted by that replay under LWW.
    let h = hlc(1000 + 60 * 1000);
    let res = applier.apply(&[with(
        &practice("P", "from a newer build", &h),
        &[("schema_v", (CURRENT_RECORD_SCHEMA_V + 1).into())],
    )]);
    assert!(res.committed);
    assert_eq!(res.failed, 1);
    assert_eq!(parked(&store)[0].remote_hlc_packed, h.packed());

    // In-memory: the very next local stamp is causally after the parked
    // record.
    assert!(
        applier.clock().tick() > h,
        "a local edit must out-rank the parked record"
    );
    // Durable: the mark survives a restart even though no table row carries
    // the parked stamp for the boot seed to find.
    assert_eq!(
        SyncStateStore::new(db.clone()).hlc_high_water().unwrap(),
        Some(h.packed()),
        "the parked stamp must reach the durable high-water mark"
    );
}

#[test]
fn a_transient_savepoint_failure_advances_the_clock_like_a_door_park_a_check_failure_does_not() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    // A failure no door gate can see and the CHECK / NOT NULL classifier
    // does not claim: a trigger aborting the INSERT inside the record's
    // savepoint (SQLITE_CONSTRAINT_TRIGGER) — standing in for the IO
    // pressure or racing writer the replayable apply-failed class exists to
    // retry. Its parked snapshot can win LWW at replay, so a later local
    // edit must stamp above it.
    db.execute_batch(
        "CREATE TRIGGER flaky_insert BEFORE INSERT ON practices \
         WHEN NEW.title = 'flaky' BEGIN SELECT RAISE(ABORT, 'injected'); END",
    )
    .unwrap();
    let transient = hlc(1000 + 60 * 1000);
    // A LATER stamp whose savepoint fails deterministically: parked
    // unreplayable, it can never land, so it must ratchet nothing.
    let deterministic = hlc(1000 + 120 * 1000);
    let res = applier.apply(&[
        practice("P", "flaky", &transient),
        bad_asset(&deterministic),
    ]);
    assert!(res.committed);
    assert_eq!(res.failed, 2);
    let reasons: std::collections::HashMap<String, String> = parked(&store)
        .into_iter()
        .map(|q| (q.id, q.reason))
        .collect();
    assert!(reasons[&id("P")].starts_with("apply-failed"), "{reasons:?}");
    assert!(
        reasons[&id("BAD")].starts_with("invalid-value"),
        "{reasons:?}"
    );

    assert!(
        applier.clock().tick() > transient,
        "a local edit must out-rank the replayable parked record"
    );
    assert!(
        applier.clock().last() < deterministic,
        "an unreplayable savepoint failure ratchets nothing"
    );
    assert_eq!(
        SyncStateStore::new(db.clone()).hlc_high_water().unwrap(),
        Some(transient.packed()),
        "the replayable parked stamp must survive a restart, where no table row carries it for \
         the boot seed to find"
    );
}

#[test]
fn a_check_failure_is_parked_unreplayable_on_an_on_disk_library_too() {
    // Dart: "on a background database too, as the live library runs" — there
    // drift handed the SQLite error over wrapped, and the classifier, matching
    // only the bare type, parked the failure as replayable. The Rust store has
    // no isolate wrapper; this pins the classifier on a file-backed library.
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(AppDatabase::open(dir.path().join("lib.sqlite")).unwrap());
    let (applier, store) = parking(&db);
    let res = applier.apply(&[bad_asset(&hlc(1000 + 60 * 1000))]);
    assert_eq!(res.failed, 1);
    let reasons = parked_reasons(&store);
    assert!(reasons[0].starts_with("invalid-value"), "{reasons:?}");
}

#[test]
fn a_door_park_with_an_implausible_stamp_still_leaves_the_clock_alone() {
    // The drift discipline holds at every gate.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let before = applier.clock().last();
    // Far-future AND mistyped: the value gate parks it before the drift gate
    // ever runs, but its suspect stamp must not ratchet anything.
    let h = hlc(1000 + 6 * 60 * 1000);
    let res = applier.apply(&[with(
        &practice("P", "poison", &h),
        &[("master_volume", "loud".into())],
    )]);
    assert_eq!(res.failed, 1);
    assert!(parked_reasons(&store)[0].starts_with("invalid-value"));
    assert_eq!(applier.clock().last(), before);
    assert_eq!(
        SyncStateStore::new(db.clone()).hlc_high_water().unwrap(),
        None
    );
}

#[test]
fn the_post_commit_advance_survives_a_wall_clock_rollback_between_the_drift_gate_and_the_commit() {
    // The run must not report failure for a page that is already durable.
    let db = memory_db();
    // A stamp exactly at the drift boundary: assert_plausible admits it at
    // gate time (now = 1000, reference = max(now, max_wall_ever_seen) =
    // 1000).
    let now = Now::new(1000);
    let clock = clock_reading(&now);
    let applier = SyncApplier::new(
        db.clone(),
        clock.clone(),
        Arc::new(SyncRowBuilder::new(db.clone())),
        None,
    );
    let stamp = hlc(1000 + HlcClock::DEFAULT_MAX_DRIFT_MS);
    let roll_back = || -> vocara_store::Result<()> {
        // The wall clock rolls BACK inside the batch's transaction — after
        // every record passed its assert_plausible gate, before the
        // post-commit advance. The check-only gate never raised
        // max_wall_ever_seen, so the drift reference shrank with the clock:
        // re-running the drift check in receive() would now FAIL for the very
        // stamps whose data and change token just committed.
        now.set(500);
        Ok(())
    };
    let res = applier.apply_with(
        &[practice("P", "vetted", &stamp)],
        ApplyOptions {
            before_commit: Some(&roll_back),
            ..ApplyOptions::default()
        },
    );
    assert!(res.committed, "the page is durable");
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 0);
    assert_eq!(title(&db, "P").as_deref(), Some("vetted"));
    assert!(
        clock.last() > stamp,
        "the clock still advanced past the committed stamp"
    );
}

#[test]
fn applying_remote_changes_writes_no_oplog() {
    // No ping-pong.
    let db = memory_db();
    let applier = plain(&db);
    applier.apply(&[practice("A", "a", &hlc(900)), practice("B", "b", &hlc(910))]);
    applier.apply(&[practice("A", "a2", &hlc(950))]);
    assert_eq!(oplog_count(&db), 0);
}

#[test]
fn the_local_clock_advances_past_an_applied_remote_stamp() {
    let db = memory_db();
    let applier = plain(&db);
    applier.apply(&[practice("P", "x", &hlc(2000))]);
    // A subsequent local tick must be strictly after the remote stamp we saw.
    let next = applier.clock().tick();
    assert!(next.wall_ms >= 2000);
}

// --- FK sweep ------------------------------------------------------------------
// The sweep parks orphans instead of losing them, so this group runs with the
// persistent parking lot wired (production wiring).

#[test]
fn the_whole_store_sweep_runs_only_for_a_batch_that_wrote_a_row() {
    // A batch that only parked or skipped cannot have left one dangling.
    let (db, statements) = traced_db();
    let (a, _) = parking(&db);
    let sweeps = || statements.matching("foreign_key_check");

    // Past the drift bound: parked, nothing written.
    let parked = a.apply(&[practice("P", "ahead", &hlc(1000 + 6 * 60 * 1000))]);
    assert_eq!(parked.quarantined, 1);
    assert_eq!(sweeps(), 0);

    // A write keeps the sweep.
    let written = a.apply(&[practice("Q", "now", &hlc(900))]);
    assert_eq!(written.applied, 1);
    assert_eq!(sweeps(), 1);

    // A clean LWW skip writes nothing either.
    let skipped = a.apply(&[practice("Q", "stale", &hlc(800))]);
    assert_eq!(skipped.skipped, 1);
    assert_eq!(sweeps(), 1);
}

#[test]
fn a_child_arriving_before_its_parent_within_a_batch_applies_fine() {
    let db = memory_db();
    let (applier, _) = parking(&db);
    let res = applier.apply(&[
        track("T", "P", &hlc(900)),         // child first
        practice("P", "parent", &hlc(901)), // parent later in the same batch
    ]);
    assert_eq!(res.applied, 2);
    assert_eq!(res.failed, 0);
    assert_eq!(title(&db, "P").as_deref(), Some("parent"));
    assert!(exists(&db, "tracks", &id("T")));
}

#[test]
fn a_true_orphan_whose_parent_never_arrives_is_parked_alone_not_lost() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[
        practice("P", "kept", &hlc(900)),
        track("T", "NEVER", &hlc(910)), // parent not in this batch, not local
    ]);
    assert_eq!(res.applied, 1); // the good record stuck
    assert_eq!(res.failed, 1); // the orphan did not
    assert_eq!(title(&db, "P").as_deref(), Some("kept"));
    // Deleted from the table (the deferred FK still requires the row gone
    // before the outer commit)...
    assert!(!exists(&db, "tracks", &id("T")));
    // ...but parked, never dropped: the change token advances past this
    // page, so only the quarantine replay can ever land the row again.
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].tbl, "tracks");
    // In the row's own sweep slot, beside (never instead of) any door park
    // of the same row — see QuarantineStore::put_swept.
    assert_eq!(parked[0].id, QuarantineStore::sweep_slot_id(&id("T")));
    assert_eq!(
        parked[0].reason,
        format!("fk-orphan: parent practices/{} missing", id("NEVER"))
    );
}

#[test]
fn a_child_whose_parent_sits_quarantined_is_parked_as_an_fk_orphan_with_its_own_hlc() {
    // The parent is from a newer build: the schema gate parks it, so it never
    // applies — yet the child (current schema_v) does. The child's page is
    // then behind the change token and the server never re-delivers it;
    // hard-deleting it here (the old sweep) lost it forever, surfacing only
    // after the app upgrade replayed the parent from quarantine with no child
    // to be found.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let parent = with(
        &practice("P", "from a newer build", &hlc(900)),
        &[("schema_v", (CURRENT_RECORD_SCHEMA_V + 1).into())],
    );
    let child = track("T", "P", &hlc(910));
    let res = applier.apply(&[parent, child.clone()]);
    assert_eq!(res.applied, 0);
    assert_eq!(res.failed, 2); // the parked parent and the swept child
    assert!(!practice_exists(&db, "P"));
    assert!(!exists(&db, "tracks", &id("T")), "deleted from the table");

    let parked = parked(&store);
    assert_eq!(parked.len(), 2);
    let parked_child = parked.iter().find(|q| q.tbl == "tracks").unwrap();
    assert_eq!(parked_child.id, QuarantineStore::sweep_slot_id(&id("T")));
    assert_eq!(
        parked_child.reason,
        format!("fk-orphan: parent practices/{} missing", id("P"))
    );
    // The snapshot carries the record's OWN HLC (not a fresh local stamp): a
    // later, newer state of the same row must still be able to settle the
    // parked one by plain LWW.
    assert_eq!(parked_child.remote_hlc_packed, hlc(910).packed());
    assert_eq!(parked_child.schema_v, CURRENT_RECORD_SCHEMA_V);
    // The payload is the full wire snapshot, replayable verbatim — the shared
    // outbound projection, not a hand-written second one.
    let replayable = SyncRow::decode(&parked_child.payload).unwrap();
    assert_eq!(replayable.table(), "tracks");
    assert_eq!(replayable.fields(), child.fields());
}

#[test]
fn a_check_violating_payload_parks_under_the_unreplayable_invalid_value_class() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[bad_asset(&hlc(1000 + 60 * 1000))]);
    assert_eq!(res.failed, 1);
    let reasons = parked_reasons(&store);
    assert_eq!(reasons.len(), 1);
    // A constraint verdict is a pure function of the payload — its replay
    // re-parks forever, so it must not sit in a replayable class where it can
    // displace the FK sweep's only copy of a row.
    assert!(reasons[0].starts_with("invalid-value:"), "{reasons:?}");
}

#[test]
fn a_non_finite_real_is_refused_at_the_value_gate_and_the_page_still_commits() {
    // JSON's 1e999 decodes to Infinity in Dart. Landed, it made the practice
    // unloadable (content hashing refuses non-finite doubles) and, as an FK
    // orphan, its sweep snapshot could not be encoded — the page rolled back
    // and the same page failed every pull from then on.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[
        with(
            &practice("P", "loud", &hlc(900)),
            &[("master_volume", f64::INFINITY.into())],
        ),
        practice("Q", "kept", &hlc(901)),
        with(
            &track("T", "NEVER", &hlc(910)),
            &[("volume", f64::NEG_INFINITY.into())],
        ),
    ]);
    assert!(res.committed, "the page must still commit");
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 2);
    assert!(!practice_exists(&db, "P"));
    assert_eq!(title(&db, "Q").as_deref(), Some("kept"));
    assert!(!exists(&db, "tracks", &id("T")));
    // Refused by the value gate, and parked as dead letters: the payload
    // cannot be JSON-encoded under its own id, so the evidence is kept
    // spelled out instead (unreplayable either way).
    let parked = parked(&store);
    assert_eq!(parked.len(), 2);
    for q in parked {
        assert!(
            q.reason.starts_with("dead-letter(invalid-value:"),
            "{}",
            q.reason
        );
        assert!(q.payload.contains("Infinity"));
    }
}

#[test]
fn a_time_or_timeline_length_out_of_range_is_refused_at_the_value_gate() {
    // Landed, a practice updated at 2^62 ms made every list of the library
    // fail (no DateTime holds it), and a timeline of 2^62 ms froze the
    // practice screen laying out and gridding it.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[
        with(
            &practice("P", "late", &hlc(900)),
            &[("updated_at", (1i64 << 62).into())],
        ),
        with(
            &practice("R", "long", &hlc(902)),
            &[("timeline_duration_ms", (1i64 << 62).into())],
        ),
        // The most negative int has no absolute value: abs() left it as is.
        with(
            &practice("S", "early", &hlc(903)),
            &[("created_at", i64::MIN.into())],
        ),
        practice("Q", "kept", &hlc(901)),
    ]);
    assert!(res.committed, "the page must still commit");
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 3);
    for p in ["P", "R", "S"] {
        assert!(!practice_exists(&db, p), "{p}");
    }
    assert_eq!(title(&db, "Q").as_deref(), Some("kept"));
    let reasons = parked_reasons(&store);
    assert_eq!(reasons.len(), 3);
    for r in reasons {
        assert!(r.contains("out of range"), "{r}");
    }
}

#[test]
fn a_check_violating_newer_state_cannot_void_the_fk_sweep_s_park() {
    // The audited black hole: an honest clip C@H1 (asset not yet delivered)
    // plus a CHECK-violating update C@H2 in one page. H2 fails its savepoint
    // and parks; the end-of-batch FK sweep then parks and deletes C@H1. The
    // sweep's park must survive — it holds the only copy of applied data,
    // while H2 can never re-apply anywhere.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let h1 = hlc(910);
    let res = applier.apply(&[
        practice("P", "kept", &hlc(900)),
        track("T", "P", &hlc(905)),
        clip("C", "T", "NOASSET", &h1, 0),
        clip("C", "T", "NOASSET", &hlc(920), -1),
    ]);
    assert!(res.failed >= 1);
    let parked = parked(&store);
    let c = parked
        .iter()
        .find(|p| p.id == QuarantineStore::sweep_slot_id(&id("C")))
        .unwrap();
    assert!(
        c.reason.starts_with("fk-orphan"),
        "the fk-orphan park is the only copy of the applied row; a permanently-unreplayable \
         newer state must not hold the slot"
    );
    assert_eq!(
        c.remote_hlc_packed,
        h1.packed(),
        "the preserved state must be the applied H1 content"
    );
}

#[test]
fn an_orphan_violating_two_foreign_keys_is_parked_and_counted_once() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[
        practice("P", "kept", &hlc(900)),
        // Both track_id and asset_id dangle -> two foreign_key_check entries
        // for one row. It must decrement applied / increment failed only once.
        clip("C", "NOTRACK", "NOASSET", &hlc(910), 0),
    ]);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);
    assert_eq!(title(&db, "P").as_deref(), Some("kept"));
    // One parked entry, not one per violated constraint (the second
    // foreign_key_check entry finds the row already deleted).
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, QuarantineStore::sweep_slot_id(&id("C")));
    assert!(parked[0].reason.starts_with("fk-orphan"));
}

/// (title, separation_asset_id, hlc_packed) of practice [row_id].
fn practice_state(db: &AppDatabase, row_id: &str) -> (String, Option<String>, String) {
    db.query_row(
        "SELECT title, separation_asset_id, hlc_packed FROM practices WHERE id = ?",
        [id(row_id)],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

/// (asset_id, hlc_packed) of clip [row_id].
fn clip_state(db: &AppDatabase, row_id: &str) -> (String, String) {
    db.query_row(
        "SELECT asset_id, hlc_packed FROM clips WHERE id = ?",
        [id(row_id)],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

/// Mimics the engine's quarantine replay at applier level: parent tables
/// first (the same parent-before-child contract the table ranks derive), one
/// single-record apply per parked row, entry deleted when the record applied
/// or settled as a stale LWW skip.
fn replay_parked(applier: &SyncApplier, store: &QuarantineStore) {
    let rank = ["assets", "practices", "tracks", "clips"];
    let mut parked = store.list().unwrap();
    parked.sort_by_key(|q| rank.iter().position(|t| *t == q.tbl));
    for q in parked {
        let res = applier.apply(&[SyncRow::decode(&q.payload).unwrap()]);
        if res.applied > 0 || res.skipped > 0 {
            store.delete(&q.tbl, &q.id).unwrap();
        }
    }
}

#[test]
fn park_and_restore_a_remote_update_repointing_at_a_quarantined_parent_restores_verbatim() {
    // The parent is locally quarantined — here by an implausibly-future
    // stamp, the same locally-parked mechanics as a newer-schema parent
    // (which a test cannot un-park, since CURRENT_RECORD_SCHEMA_V is a
    // build-time const, while a drift park recovers once the clock catches
    // up). Either way the child's page ends up behind the change token, the
    // server never re-delivers it, and only the parking lot can converge the
    // pair.
    let db = memory_db();
    let now = Now::new(1000);
    let (applier, store) = parking_with(&db, clock_reading(&now));
    applier.apply(&[practice("P", "v1", &hlc(900))]);

    let parent_stamp = hlc(1000 + HlcClock::DEFAULT_MAX_DRIFT_MS + 60 * 1000);
    let res = applier.apply(&[
        asset_record("A", &parent_stamp), // quarantined at the drift gate
        practice_full("P", "v2", &hlc(950), None, Some("A")),
    ]);
    assert!(res.committed);
    assert_eq!(res.quarantined, 1); // the parent
    assert_eq!(res.applied, 0, "the update applied but did not stick");
    assert_eq!(res.failed, 1); // the parked update

    // The row was restored VERBATIM to its exact pre-batch state: pointer
    // intact (null), prior content, prior HLC — a pure revert, no derived
    // stamp minted. NOT the old heal (update kept, pointer nulled, remote HLC
    // adopted).
    assert_eq!(
        practice_state(&db, "P"),
        ("v1".to_string(), None, hlc(900).packed())
    );

    let parked = parked(&store);
    assert_eq!(parked.len(), 2);
    let parked_update = parked.iter().find(|q| q.tbl == "practices").unwrap();
    assert_eq!(parked_update.id, QuarantineStore::sweep_slot_id(&id("P")));
    assert_eq!(
        parked_update.reason,
        format!("fk-orphan: parent assets/{} missing", id("A"))
    );
    // The parked snapshot is the POST-update state under the remote HLC:
    // strictly newer than the restored row, so replay wins by plain LWW.
    assert_eq!(parked_update.remote_hlc_packed, hlc(950).packed());
    let replayable = SyncRow::decode(&parked_update.payload).unwrap();
    assert_eq!(replayable.field("title"), Some(&FieldValue::from("v2")));
    assert_eq!(
        replayable.field("separation_asset_id"),
        Some(&FieldValue::from(id("A")))
    );

    // The clock catches up (the drift analogue of the app upgrade that
    // un-parks a newer-schema parent) and the replay runs: the parent lands
    // first, then the parked update — BOTH land, the pointer is correct, and
    // the parking lot is empty.
    now.set(parent_stamp.wall_ms);
    replay_parked(&applier, &store);
    assert_eq!(
        practice_state(&db, "P"),
        ("v2".to_string(), Some(id("A")), hlc(950).packed())
    );
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn a_re_delivered_identical_update_is_strictly_newer_than_the_restored_row() {
    // It re-applies and re-parks instead of the old permanently-nulled
    // pointer.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[practice("P", "v1", &hlc(900))]);
    let update = practice_full("P", "v2", &hlc(950), None, Some("A"));

    let first = applier.apply(&[update.clone()]);
    assert_eq!(first.applied, 0);
    assert_eq!(first.failed, 1);
    assert_eq!(practice_state(&db, "P").2, hlc(900).packed());

    // The server re-delivers the identical snapshot (e.g. a token reset
    // replays the zone). Under the old null-the-pointer heal the row held the
    // update's OWN HLC with the pointer nulled, so the re-delivery was
    // LWW-EQUAL and skipped — the reference was lost forever unless some
    // strictly-newer update ever arrived. Against the RESTORED row (prior
    // HLC) it is strictly newer: it re-applies and is parked and restored
    // again, converging on the same recoverable state.
    let second = applier.apply(&[update]);
    assert_eq!(second.skipped, 0, "not LWW-equal to the restored row");
    assert_eq!(second.applied, 0);
    assert_eq!(second.failed, 1);
    assert_eq!(
        practice_state(&db, "P"),
        ("v1".to_string(), None, hlc(900).packed())
    );
    let parked = parked(&store);
    assert_eq!(parked.len(), 1); // upserted, not piled up
    assert_eq!(parked[0].remote_hlc_packed, hlc(950).packed());

    // Once the parent exists, the replay converges to the updated state.
    let landed = applier.apply(&[asset_record("A", &hlc(960))]);
    assert_eq!(landed.applied, 1);
    replay_parked(&applier, &store);
    assert_eq!(
        practice_state(&db, "P"),
        ("v2".to_string(), Some(id("A")), hlc(950).packed())
    );
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn a_dangling_not_null_fk_on_an_updated_this_batch_row_commits_the_batch() {
    // The token seam runs, via the same park-and-restore path; replay
    // converges after the parent lands.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(900)),
        track("T", "P", &hlc(901)),
        asset_record("A1", &hlc(902)),
        clip("C", "T", "A1", &hlc(903), 0),
    ]);

    // clips.asset_id is NOT NULL: the old sweep could only abort, and with
    // the parent locally quarantined (never re-delivered) the engine retried
    // the same page forever, until an app upgrade. The restore heals every
    // repointed column at once, so the NOT NULL case rides the same path as
    // the nullable one.
    let token_writes = std::cell::Cell::new(0);
    let token = || -> vocara_store::Result<()> {
        token_writes.set(token_writes.get() + 1);
        Ok(())
    };
    let res = applier.apply_with(
        &[clip("C", "T", "A2", &hlc(950), 0)], // A2 never arrived
        ApplyOptions {
            before_commit: Some(&token),
            ..ApplyOptions::default()
        },
    );
    assert!(res.committed, "no permanent page retry");
    assert_eq!(
        token_writes.get(),
        1,
        "the change token advances past the page"
    );
    assert_eq!(res.applied, 0);
    assert_eq!(res.failed, 1);

    assert_eq!(
        clip_state(&db, "C"),
        (id("A1"), hlc(903).packed()),
        "restored verbatim, not aborted"
    );

    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].tbl, "clips");
    assert_eq!(
        parked[0].reason,
        format!("fk-orphan: parent assets/{} missing", id("A2"))
    );

    // The missing asset lands (a later page, or an upgrade replay); the
    // parked repoint then converges by plain LWW.
    applier.apply(&[asset_record("A2", &hlc(960))]);
    replay_parked(&applier, &store);
    assert_eq!(clip_state(&db, "C"), (id("A2"), hlc(950).packed()));
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn a_dangling_pre_existing_row_this_batch_never_touched_keeps_the_null_the_pointer_heal() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[practice("P", "v1", &hlc(900))]);
    // No remote apply can produce this state any more (every dangling
    // pre-existing row is one the batch just updated), so manufacture the
    // residual case directly: a pointer left dangling outside the applier's
    // control.
    db.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    db.execute(
        "UPDATE practices SET separation_asset_id = ? WHERE id = ?",
        [id("GONE"), id("P")],
    )
    .unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON").unwrap();

    // Any batch triggers the sweep (foreign_key_check scans the whole
    // database). P is in neither the inserted nor the updated set, so only
    // its dangling column is nulled — no park, no restore, and no counter
    // moves.
    let res = applier.apply(&[practice("Q", "other", &hlc(950))]);
    assert!(res.committed);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 0);
    let (t, sep, _) = practice_state(&db, "P");
    assert_eq!(t, "v1", "row kept");
    assert_eq!(sep, None, "pointer nulled");
    assert!(store.list().unwrap().is_empty());
}

// --- per-device asset columns --------------------------------------------------

/// (ext, local_rel_path, record_sync_state, bytes_sync_state, remote_ref).
type AssetBookkeeping = (String, Option<String>, String, String, Option<String>);

fn asset_row(db: &AppDatabase, row_id: &str) -> AssetBookkeeping {
    db.query_row(
        "SELECT ext, local_rel_path, record_sync_state, bytes_sync_state, remote_ref \
         FROM assets WHERE id = ?",
        [id(row_id)],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .unwrap()
}

#[test]
fn a_remote_update_still_carrying_per_device_columns_wins_lww_without_clobbering_bookkeeping() {
    // An older-build peer.
    let db = memory_db();
    let applier = plain(&db);
    // Local state: bytes materialized HERE as an .mp3 under this device's
    // pool path, with its own sync-state bookkeeping.
    applier.apply(&[asset_full("A", &hlc(900), "mp3", None)]);
    db.execute(
        "UPDATE assets SET local_rel_path = 'assets/HA/SH/HASH_A.mp3', \
         record_sync_state = 'synced', bytes_sync_state = 'present', \
         remote_ref = 'blob://HASH_A' WHERE id = ?",
        [id("A")],
    )
    .unwrap();

    // A newer remote version arrives carrying the SENDER's per-device values
    // (a .wav rel path that does not exist here, localOnly states).
    let res = applier.apply(&[with(
        &asset_full("A", &hlc(950), "wav", None),
        &[
            ("local_rel_path", "assets/HA/SH/HASH_A.wav".into()),
            ("record_sync_state", "localOnly".into()),
            ("bytes_sync_state", "localOnly".into()),
            ("remote_ref", NULL),
        ],
    )]);
    assert_eq!(res.applied, 1);

    let (ext, rel, record, bytes, remote) = asset_row(&db, "A");
    assert_eq!(ext, "wav", "the synced content applied");
    // The per-device bookkeeping survived the winning remote apply.
    assert_eq!(rel.as_deref(), Some("assets/HA/SH/HASH_A.mp3"));
    assert_eq!(record, "synced");
    assert_eq!(bytes, "present");
    assert_eq!(remote.as_deref(), Some("blob://HASH_A"));
}

#[test]
fn a_remote_insert_with_per_device_columns_adopts_no_foreign_bookkeeping() {
    // Schema defaults, except record_sync_state which is forced to synced
    // (the row demonstrably crossed the wire).
    let db = memory_db();
    let applier = plain(&db);
    let res = applier.apply(&[with(
        &asset_record("B", &hlc(900)),
        &[
            ("local_rel_path", "assets/XX/YY/HASH_B.wav".into()),
            ("record_sync_state", "localOnly".into()),
            ("bytes_sync_state", "present".into()),
            ("remote_ref", "blob://HASH_B".into()),
        ],
    )]);
    assert_eq!(res.applied, 1);

    let (_, rel, record, bytes, remote) = asset_row(&db, "B");
    assert_eq!(rel, None, "no bytes here yet");
    // NOT the sender's value and NOT the schema default: a wire-arrived
    // assets row is by definition record-synced, and leaving it at
    // 'localOnly' would make it sweepable by the asset-GC's never-synced
    // guard — the exact rows the guard exists to protect.
    assert_eq!(record, "synced");
    assert_eq!(bytes, "localOnly", "no bytes here");
    assert_eq!(remote, None);
}

#[test]
fn a_remote_update_of_an_existing_local_only_asset_row_marks_it_record_synced() {
    // Another device now provably holds the record.
    let db = memory_db();
    let applier = plain(&db);
    // A locally-created asset row, never synced (schema defaults).
    db.execute(
        "INSERT INTO assets (id, content_hash, byte_size, ext, kind, created_at, updated_at, \
         hlc_wall, hlc_ctr, hlc_node, hlc_packed, schema_v) \
         VALUES (?, ?, 8, ?, ?, 900, 900, 900, 0, ?, ?, 1)",
        [
            id("A"),
            "HASH_A".to_string(),
            "wav".to_string(),
            "audio".to_string(),
            remote_node(),
            hlc(900).packed(),
        ],
    )
    .unwrap();
    assert_eq!(asset_row(&db, "A").2, "localOnly");

    // A newer remote version of the SAME row arrives: the update must advance
    // record_sync_state along with the content — a remote peer holding (and
    // editing) the record is proof it crossed the wire.
    let res = applier.apply(&[asset_full("A", &hlc(950), "mp3", None)]);
    assert_eq!(res.applied, 1);
    let (ext, _, record, bytes, _) = asset_row(&db, "A");
    assert_eq!(ext, "mp3", "the content update applied");
    assert_eq!(record, "synced");
    assert_eq!(
        bytes, "localOnly",
        "blob upload state is untouched — it has no transport yet"
    );
}

// --- quarantine persistence --------------------------------------------------

#[test]
fn an_hlc_drift_record_is_parked_with_its_full_wire_payload() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let future = hlc(1000 + 5 * 60 * 1000 + 1);
    let rec = practice("FUT", "from the future", &future);
    let res = applier.apply(&[rec.clone()]);
    assert_eq!(res.quarantined, 1);
    assert!(!practice_exists(&db, "FUT")); // still NOT applied

    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    let q = &parked[0];
    assert_eq!(q.tbl, "practices");
    assert_eq!(q.id, id("FUT"));
    assert_eq!(q.remote_hlc_packed, future.packed());
    assert_eq!(q.schema_v, 1);
    assert!(q.reason.starts_with("hlc-drift"));
    // The payload is the SyncRow wire format: replayable later, verbatim.
    let replay = SyncRow::decode(&q.payload).unwrap();
    assert_eq!(replay.table(), "practices");
    assert_eq!(replay.fields(), rec.fields());
    assert_eq!(q.payload, rec.encode().unwrap());
}

#[test]
fn an_unknown_column_at_an_accepted_schema_v_is_parked_with_reason_unknown_column() {
    // And the good record still applies.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let good = practice("GOOD", "kept", &hlc(900));
    let bad = with(
        &practice("BAD", "dropped", &hlc(910)),
        &[("column_from_the_future", 1i64.into())],
    );
    let res = applier.apply(&[good, bad]);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);
    assert_eq!(title(&db, "GOOD").as_deref(), Some("kept"));
    assert!(!practice_exists(&db, "BAD"));

    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, id("BAD"));
    assert!(parked[0].reason.starts_with("unknown-column"));
    // The unknown column rides along in the parked payload for a future
    // (schema-upgraded) re-apply.
    let replay = SyncRow::decode(&parked[0].payload).unwrap();
    assert_eq!(
        replay.field("column_from_the_future"),
        Some(&FieldValue::Int(1))
    );
}

#[test]
fn a_genuinely_newer_schema_v_is_parked_with_the_apply_failed_reason_before_the_column_gate() {
    // An upgrade un-parks it via replay.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let bad = with(
        &practice("BAD", "from a newer build", &hlc(910)),
        &[
            ("schema_v", (CURRENT_RECORD_SCHEMA_V + 1).into()),
            ("column_from_the_future", 1i64.into()),
        ],
    );
    let res = applier.apply(&[bad]);
    assert_eq!(res.failed, 1);
    assert!(!practice_exists(&db, "BAD"));

    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].schema_v, CURRENT_RECORD_SCHEMA_V + 1);
    assert!(parked[0].reason.starts_with("apply-failed"));
}

#[test]
fn a_schema_v_of_0_is_parked_with_reason_invalid_schema_v_and_the_rest_of_the_batch_applies() {
    // Versions start at 1, and a below-range value would bypass both schema
    // gates.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[
        practice("GOOD", "kept", &hlc(900)),
        with(
            &practice("BAD", "dropped", &hlc(910)),
            &[("schema_v", 0i64.into())],
        ),
        practice("ALSO", "behind", &hlc(920)),
    ]);
    assert_eq!(res.failed, 1);
    assert_eq!(res.applied, 2, "the batch continues around it");
    assert_eq!(title(&db, "GOOD").as_deref(), Some("kept"));
    assert_eq!(title(&db, "ALSO").as_deref(), Some("behind"));
    assert!(!practice_exists(&db, "BAD"), "never applied");

    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, id("BAD"));
    assert!(parked[0].reason.starts_with("invalid-schema_v"));
    assert!(parked[0].reason.contains('0'));
}

#[test]
fn a_negative_schema_v_is_parked_the_same_way() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[with(
        &practice("BAD", "dropped", &hlc(910)),
        &[("schema_v", (-1i64).into())],
    )]);
    assert_eq!(res.failed, 1);
    assert_eq!(res.applied, 0);
    assert!(!practice_exists(&db, "BAD"));
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert!(parked[0].reason.starts_with("invalid-schema_v"));
    assert!(parked[0].reason.contains("-1"));
}

#[test]
fn an_unknown_table_is_parked_with_reason_unknown_table() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[SyncRow::new(
        "not_a_table",
        practice("BAD", "dropped", &hlc(910)).fields().clone(),
    )]);
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].tbl, "not_a_table");
    assert!(parked[0].reason.starts_with("unknown-table"));
}

#[test]
fn a_mistyped_hlc_quad_is_parked_with_reason_invalid_value() {
    // Any per-record failure quarantines that record alone.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[with(
        &practice("BAD", "dropped", &hlc(910)),
        &[("hlc_ctr", "seven".into())],
    )]);
    assert_eq!(res.failed, 1);
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    // The column gate names the offending column at the door, instead of
    // letting the mistyped value fail deep inside apply as a generic
    // 'apply-failed' with nothing to act on.
    assert!(parked[0].reason.starts_with("invalid-value"));
}

#[test]
fn a_value_that_could_not_bind_to_its_column_is_parked_with_the_column_named() {
    // SQLite's dynamic typing takes a string into an INTEGER column without
    // complaint, so the failure used to surface far away — the row hydrating
    // and failing inside a view model — with nothing to act on.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[with(
        &practice("BAD", "x", &hlc(910)),
        &[("timeline_duration_ms", "not a number".into())],
    )]);
    assert_eq!(res.failed, 1);
    assert!(!practice_exists(&db, "BAD"));
    assert!(parked_reasons(&store)[0].contains("timeline_duration_ms"));
}

#[test]
fn valid_json_of_the_wrong_shape_is_parked_at_the_door() {
    // '{}' parses, binds to a TEXT column and commits — and then the
    // hydrator's `as List` cast fails inside a view model: the practice
    // disappears from the list and cannot be opened, with nothing parked to
    // act on. The door runs the real decoder, so what commits hydrates.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[with(
        &practice("BAD", "x", &hlc(910)),
        &[("lyrics_json", "{}".into())],
    )]);
    assert_eq!(res.failed, 1);
    assert!(!practice_exists(&db, "BAD"));
    let reason = parked_reasons(&store).remove(0);
    assert!(reason.starts_with("invalid-value"), "{reason}");
    assert!(reason.contains("lyrics_json"), "{reason}");
}

#[test]
fn a_well_formed_json_body_still_applies() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[with(
        &practice("GOOD", "x", &hlc(910)),
        &[
            (
                "lyrics_json",
                "[{\"text\":\"line one\",\"timeMs\":120}]".into(),
            ),
            (
                "reverb_json",
                "{\"roomSize\":0.4,\"damping\":0.5,\"wet\":0.2,\"width\":1.0}".into(),
            ),
        ],
    )]);
    assert_eq!(res.applied, 1);
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn a_json_array_whose_elements_are_wrong_is_parked_too() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let res = applier.apply(&[with(
        &practice("BAD2", "x", &hlc(910)),
        &[("lyrics_json", "[\"just a string\"]".into())],
    )]);
    assert_eq!(res.failed, 1);
    assert!(parked_reasons(&store)[0].contains("lyrics_json"));
}

#[test]
fn every_json_column_runs_its_real_decoder_at_the_door() {
    // The other registered body shapes refuse what their hydrators refuse.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let technique = |row_id: &str, aliases: &str, h: &Hlc| {
        SyncRow::new(
            "techniques",
            merged(
                fields! {
                    "id" => id(row_id),
                    "name" => "belt",
                    "aliases_json" => aliases,
                    "tags_json" => "[\"a\"]",
                    "summary" => "",
                    "description" => "",
                    "cues" => "",
                    "exercises" => "",
                    "acoustic_signature" => "",
                    "order_index" => 0i64,
                },
                sync_cols(h, None),
            ),
        )
    };
    let res = applier.apply(&[
        technique("GOOD", "[\"x\",\"y\"]", &hlc(900)),
        technique("BAD", "[1]", &hlc(901)),
        with(
            &track("T", "P", &hlc(902)),
            &[(
                "sends_json",
                "[{\"busId\":\"b\",\"levelDb\":-6.0}, 3]".into(),
            )],
        ),
        with(&track("U", "P", &hlc(903)), &[("fx_json", "[]".into())]),
    ]);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 3);
    let reasons = parked_reasons(&store);
    assert!(
        reasons.iter().any(|r| r.contains("aliases_json")),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r.contains("sends_json")),
        "{reasons:?}"
    );
    assert!(reasons.iter().any(|r| r.contains("fx_json")), "{reasons:?}");
}

#[test]
fn re_quarantining_the_same_row_keeps_only_the_latest_state() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let drift1 = hlc(1000 + 6 * 60 * 1000);
    let drift2 = hlc(1000 + 7 * 60 * 1000);
    applier.apply(&[practice("FUT", "v1", &drift1)]);
    applier.apply(&[practice("FUT", "v2", &drift2)]);

    let parked = parked(&store);
    assert_eq!(parked.len(), 1); // latest per (tbl, id), not a pile-up
    assert_eq!(parked[0].remote_hlc_packed, drift2.packed());
    assert_eq!(
        SyncRow::decode(&parked[0].payload).unwrap().field("title"),
        Some(&FieldValue::from("v2"))
    );
}

#[test]
fn a_quarantine_storage_failure_fails_the_whole_batch_and_the_same_page_succeeds_once_it_recovers()
{
    // The token seam never runs, nothing applies. (Dart subclassed the store
    // to throw; here a trigger makes the parking lot refuse every write —
    // the same storage failure, through the real store.)
    let db = memory_db();
    let (applier, store) = parking(&db);
    db.execute_batch(
        "CREATE TRIGGER quarantine_down BEFORE INSERT ON sync_quarantine \
         BEGIN SELECT RAISE(ABORT, 'quarantine storage unavailable'); END",
    )
    .unwrap();
    let token_writes = std::cell::Cell::new(0);
    let token = || -> vocara_store::Result<()> {
        token_writes.set(token_writes.get() + 1);
        Ok(())
    };
    let page = [
        practice("OK", "fine", &hlc(900)),
        // Parked at the drift gate — the failing put is what must not be
        // swallowed: a committed batch would advance the token past a record
        // that was never parked, losing it permanently.
        practice("FUT", "parked", &hlc(1000 + 6 * 60 * 1000)),
    ];
    let options = || ApplyOptions {
        before_commit: Some(&token),
        ..ApplyOptions::default()
    };
    let res = applier.apply_with(&page, options());
    assert!(!res.committed);
    assert_eq!(token_writes.get(), 0, "the token seam must not run");
    assert!(!practice_exists(&db, "OK"), "the whole page rolled back");
    assert!(store.list().unwrap().is_empty());

    // The store recovers: the engine retries the SAME page (its token never
    // moved) and it now applies cleanly, park included.
    db.execute_batch("DROP TRIGGER quarantine_down").unwrap();
    let retry = applier.apply_with(&page, options());
    assert!(retry.committed);
    assert_eq!(token_writes.get(), 1);
    assert_eq!(retry.applied, 1);
    assert_eq!(retry.quarantined, 1);
    assert_eq!(title(&db, "OK").as_deref(), Some("fine"));
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, id("FUT"));
    assert!(parked[0].reason.starts_with("hlc-drift"));
}

#[test]
fn quarantine_rows_commit_atomically_with_the_batch() {
    // Same outer transaction, no oplog echo.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("OK", "applied", &hlc(900)),
        practice("FUT", "parked", &hlc(1000 + 6 * 60 * 1000)),
    ]);
    // Both the applied row and the parked row landed...
    assert_eq!(title(&db, "OK").as_deref(), Some("applied"));
    assert_eq!(store.list().unwrap().len(), 1);
    // ...and neither the apply nor the quarantine journaled anything.
    assert_eq!(oplog_count(&db), 0);
}

#[test]
fn undecodable_page_records_park_as_dead_letters_inside_the_batch() {
    let db = memory_db();
    let (applier, store) = parking(&db);
    let rejects = [vocara_store::sync::sync_transport::RejectedRecord::new(
        "{\"not\":\"a row\"}",
        "missing id",
    )];
    let res = applier.apply_with(
        &[practice("GOOD", "kept", &hlc(900))],
        ApplyOptions {
            rejects: &rejects,
            ..ApplyOptions::default()
        },
    );
    assert!(res.committed);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);
    let parked = parked(&store);
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].tbl, SyncApplier::DEAD_LETTER_TABLE);
    assert_eq!(parked[0].reason, "dead-letter(undecodable: missing id)");
    assert_eq!(
        parked[0].payload,
        "{\"deadLetter\":true,\"undecodable\":true,\"raw\":\"{\\\"not\\\":\\\"a row\\\"}\"}"
    );
    assert!(SyncRow::decode(&parked[0].payload).is_err());
}

// --- slot-resolution revert ----------------------------------------------------
// A park undoes the resolution it rode in on.

/// A single-slot track (accompaniment) with a routable bus pointer — the FK
/// the sweep trips over when the bus parent never lands.
fn acc_track(row_id: &str, practice_id: &str, h: &Hlc, bus_id: Option<&str>) -> SyncRow {
    with(
        &track(row_id, practice_id, h),
        &[
            ("kind", "accompaniment".into()),
            ("bus_id", bus_id.map(id).into()),
        ],
    )
}

fn fx_bus(row_id: &str, practice_id: &str, h: &Hlc, schema_v: Option<i64>) -> SyncRow {
    SyncRow::new(
        "fx_buses",
        fields! {
            "id" => id(row_id),
            "practice_id" => id(practice_id),
            "name" => "bus",
            "color_value" => NULL,
            "volume" => 1.0,
            "pan" => 0.0,
            "muted" => 0i64,
            "soloed" => 0i64,
            "output_bus_id" => NULL,
            "fx_json" => NULL,
            "sends_json" => NULL,
            "order_index" => 0i64,
            "created_at" => h.wall_ms,
            "updated_at" => h.wall_ms,
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => schema_v.unwrap_or(1),
        },
    )
}

/// (practice_id, deleted_at, hlc_packed, bus_id) of track [row_id].
type TrackState = (String, Option<i64>, String, Option<String>);

fn track_state(db: &AppDatabase, row_id: &str) -> Option<TrackState> {
    db.query_opt(
        "SELECT practice_id, deleted_at, hlc_packed, bus_id FROM tracks WHERE id = ?",
        [id(row_id)],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

/// (entity_id, op, hlc_packed) of every journal entry, in seq order.
fn oplog_rows(db: &AppDatabase) -> Vec<(String, String, String)> {
    db.query(
        "SELECT entity_id, op, hlc_packed FROM oplog ORDER BY seq",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

/// The derived tombstone a slot resolution writes: the loser's own stamp
/// with the node extended by this device — same (wall, counter), so a parked
/// node-space winner's replay still out-ranks it (HlcClock::derived).
fn derived_at(wall: i64) -> Hlc {
    HlcClock::derived(&hlc(wall), &id("LOCAL"))
}

fn parked_track(store: &QuarantineStore) -> SyncQuarantineRow {
    store
        .list()
        .unwrap()
        .into_iter()
        .find(|q| q.tbl == "tracks")
        .unwrap()
}

#[test]
fn park_and_delete_an_inserted_record_that_won_the_slot_takes_the_occupant_tombstone_back() {
    // Its bus parent is quarantined (newer schema): the record takes the
    // occupant tombstone and its journal entry back out with it; the replayed
    // record kills the occupant only when it actually lands.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        acc_track("O", "P", &hlc(850), None), // the current slot occupant
    ]);
    assert!(oplog_rows(&db).is_empty());

    // The bus parent is from a newer build (parked at the schema gate); the
    // incoming accompaniment routes into it AND wins the (P, acc) slot, so
    // applying it tombstones + journals the occupant BEFORE the sweep
    // discovers the record itself cannot stay.
    let incoming = acc_track("T", "P", &hlc(900), Some("B"));
    let res = applier.apply(&[
        fx_bus("B", "P", &hlc(890), Some(CURRENT_RECORD_SCHEMA_V + 1)),
        incoming.clone(),
    ]);
    assert!(res.committed);
    assert_eq!(res.applied, 0);
    assert_eq!(res.failed, 2); // the parked bus and the swept track

    // The occupant is fully restored — live, prior HLC, no track killed for a
    // record that never landed...
    let occupant = track_state(&db, "O").unwrap();
    assert_eq!(occupant.1, None);
    assert_eq!(occupant.2, hlc(850).packed());
    // ...and no oplog entry from the resolution remains.
    assert!(oplog_rows(&db).is_empty());
    // The incoming record itself was quarantined at the door: row gone,
    // parked under its own HLC with its original payload.
    assert!(track_state(&db, "T").is_none());
    let parked = parked_track(&store);
    assert_eq!(parked.remote_hlc_packed, hlc(900).packed());
    assert_eq!(
        parked.reason,
        format!("fk-orphan: parent fx_buses/{} missing", id("B"))
    );
    let payload = SyncRow::decode(&parked.payload).unwrap();
    assert_eq!(payload.fields(), incoming.fields());

    // The parent lands; the replayed record applies — and only THEN does the
    // occupant lose the slot, with a journaled tombstone.
    applier.apply(&[fx_bus("B", "P", &hlc(905), None)]);
    let replay = applier.apply(&[payload]);
    assert_eq!(replay.applied, 1);
    assert_eq!(track_state(&db, "T").unwrap().1, None);
    let lost = track_state(&db, "O").unwrap();
    assert!(lost.1.is_some());
    let derived = derived_at(850);
    assert_eq!(lost.2, derived.packed());
    assert_eq!(
        oplog_rows(&db),
        vec![(id("O"), "tombstone".to_string(), derived.packed())]
    );
}

#[test]
fn park_and_restore_an_updated_pre_existing_track_that_won_the_slot_reverts_the_occupant_too() {
    // The journal stays consistent with the restored rows.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        practice("Q", "q", &hlc(801)),
        acc_track("O", "P", &hlc(850), None), // occupies (P, accompaniment)
        acc_track("T", "Q", &hlc(820), None), // pre-exists in Q's slot
    ]);
    assert!(oplog_rows(&db).is_empty());

    // The incoming update MOVES T into P's slot (winning it, 900 > 850) while
    // also routing it into a bus from a newer build — the update itself
    // plants the dangling pointer that parks it.
    let incoming = acc_track("T", "P", &hlc(900), Some("B"));
    let res = applier.apply(&[
        fx_bus("B", "P", &hlc(890), Some(CURRENT_RECORD_SCHEMA_V + 1)),
        incoming,
    ]);
    assert!(res.committed);
    assert_eq!(res.applied, 0);
    assert_eq!(res.failed, 2);

    // The updated row is restored verbatim to its pre-batch state...
    assert_eq!(
        track_state(&db, "T").unwrap(),
        (id("Q"), None, hlc(820).packed(), None)
    );
    // ...the occupant is restored live on its own prior stamp...
    let occupant = track_state(&db, "O").unwrap();
    assert_eq!(occupant.1, None);
    assert_eq!(occupant.2, hlc(850).packed());
    // ...and the journal carries no stray resolution entries for either.
    assert!(oplog_rows(&db).is_empty());
    // The post-update snapshot is parked under the remote HLC (strictly newer
    // than both restored rows, so replay wins by plain LWW).
    let parked = parked_track(&store);
    assert_eq!(parked.remote_hlc_packed, hlc(900).packed());

    // Convergence: the parent lands and the replay takes the slot for real —
    // occupant tombstoned, resolution journaled, exactly once.
    applier.apply(&[fx_bus("B", "P", &hlc(905), None)]);
    let replay = applier.apply(&[SyncRow::decode(&parked.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    assert_eq!(track_state(&db, "T").unwrap().0, id("P"));
    assert!(track_state(&db, "O").unwrap().1.is_some());
    let journal = oplog_rows(&db);
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].0, id("O"));
    assert_eq!(journal[0].1, "tombstone");
}

#[test]
fn a_parked_record_that_lost_the_slot_parks_its_original_payload() {
    // Not the tombstoned-at-the-door state whose journal entry the revert
    // just deleted.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        acc_track("O", "P", &hlc(900), None), // newer than the incoming record
    ]);

    let incoming = acc_track("T", "P", &hlc(850), Some("B"));
    let res = applier.apply(&[
        fx_bus("B", "P", &hlc(890), Some(CURRENT_RECORD_SCHEMA_V + 1)),
        incoming.clone(),
    ]);
    assert_eq!(res.applied, 0);
    assert_eq!(res.failed, 2);

    // The occupant was never touched (the incoming record lost) and no
    // journal entry survives for the at-the-door tombstone.
    let occupant = track_state(&db, "O").unwrap();
    assert_eq!(occupant.1, None);
    assert_eq!(occupant.2, hlc(900).packed());
    assert!(oplog_rows(&db).is_empty());
    assert!(track_state(&db, "T").is_none());
    // Parked under the record's OWN hlc with the original live payload:
    // parking the derived tombstone state would later replay an UN-journaled
    // tombstone no peer ever learns about.
    let parked = parked_track(&store);
    assert_eq!(parked.remote_hlc_packed, hlc(850).packed());
    let payload = SyncRow::decode(&parked.payload).unwrap();
    assert_eq!(payload.fields(), incoming.fields());

    // Replay after the parent lands: the resolution re-runs from scratch, the
    // record still loses, and THIS time the tombstone is journaled.
    applier.apply(&[fx_bus("B", "P", &hlc(905), None)]);
    let replay = applier.apply(&[payload]);
    assert_eq!(replay.applied, 1);
    assert!(track_state(&db, "T").unwrap().1.is_some());
    assert_eq!(track_state(&db, "O").unwrap().1, None);
    let journal = oplog_rows(&db);
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].0, id("T"));
    assert_eq!(journal[0].1, "tombstone");
}

#[test]
fn a_record_that_tombstoned_two_successive_occupants_reverts_them_as_independent_pairs() {
    // The latest occupant is restored with its journal entry deleted, the
    // earlier one stays tombstoned WITH its journal entry — never tombstoned
    // with a deleted journal — and the batch commits.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        acc_track("O1", "P", &hlc(850), None),
    ]);
    assert!(oplog_rows(&db).is_empty());

    // One page: T@900 (bus parent never arrives) beats O1; O2@950 beats T;
    // T@980 beats O2 — T's revert now carries two occupant pairs. Folding
    // them into one (the FIRST snapshot under the LAST derived stamp) used to
    // restore NEITHER occupant while deleting BOTH journal entries: two
    // tracks tombstoned with their ledger gone.
    let incoming = acc_track("T", "P", &hlc(980), Some("B"));
    let res = applier.apply(&[
        acc_track("T", "P", &hlc(900), Some("B")),
        acc_track("O2", "P", &hlc(950), None),
        incoming.clone(),
    ]);
    assert!(res.committed);
    assert_eq!(res.applied, 2);
    assert_eq!(res.failed, 1);

    // The LATEST occupant (O2) is restored verbatim: live, prior HLC —
    // exactly the state a peer that never saw T derives (O2 beats O1).
    let o2 = track_state(&db, "O2").unwrap();
    assert_eq!(o2.1, None);
    assert_eq!(o2.2, hlc(950).packed());
    // The EARLIER occupant (O1) is not restorable — the restored O2 now holds
    // the slot — so its pair is kept WHOLE: still tombstoned AND still
    // journaled, an accounted tombstone peers learn.
    let o1 = track_state(&db, "O1").unwrap();
    assert_eq!(o1.1, Some(900)); // the winner's wall when T claimed it
    assert_eq!(o1.2, derived_at(850).packed());
    assert!(track_state(&db, "T").is_none());
    // Journal: O1's entry survives with its tombstone; O2's went with its
    // restore. The T entry was minted by O2's OWN resolution (O2 landed, its
    // ledger stands); with T's row gone it is a harmless dirty flag — the
    // pusher pushes CURRENT row state and skips missing rows.
    let journal = oplog_rows(&db);
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0].0, id("O1"));
    assert_eq!(journal[0].2, derived_at(850).packed());
    assert_eq!(journal[1].0, id("T"));
    assert_eq!(journal[1].2, derived_at(900).packed());
    // T parked under its own latest HLC with the ORIGINAL payload.
    let parked = parked_track(&store);
    assert_eq!(parked.remote_hlc_packed, hlc(980).packed());
    assert_eq!(
        SyncRow::decode(&parked.payload).unwrap().fields(),
        incoming.fields()
    );

    // Convergence: the parent lands and the replay takes the slot for real —
    // O2 loses it with a journaled tombstone, exactly once.
    applier.apply(&[fx_bus("B", "P", &hlc(985), None)]);
    let replay = applier.apply(&[SyncRow::decode(&parked.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    assert_eq!(track_state(&db, "T").unwrap().1, None);
    let o2_after = track_state(&db, "O2").unwrap();
    assert_eq!(o2_after.1, Some(980));
    assert_eq!(o2_after.2, derived_at(950).packed());
    assert_eq!(track_state(&db, "O1").unwrap().2, derived_at(850).packed());
    let after = oplog_rows(&db);
    assert_eq!(after.len(), 3);
    assert_eq!(after[2].0, id("O2"));
    assert_eq!(after[2].2, derived_at(950).packed());
}

// A later in-page winner holds the slot when the revert runs (a restore must
// not trip ux_track_slot and brick the page).

#[test]
fn an_inserted_winner_parked_after_a_later_winner_took_the_slot_skips_the_occupant_pair_whole() {
    // And the page commits.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        acc_track("O", "P", &hlc(850), None),
    ]);

    // T (bus parent never arrives) beats O; W then beats T. At revert time W
    // holds the slot live, so restoring O would violate ux_track_slot — the
    // old unconditional restore failed the OUTER commit and retried this same
    // page forever (token never advanced).
    let res = applier.apply(&[
        acc_track("T", "P", &hlc(900), Some("B")),
        acc_track("W", "P", &hlc(950), None),
    ]);
    assert!(res.committed);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);

    let w = track_state(&db, "W").unwrap();
    assert_eq!(w.1, None);
    assert_eq!(w.2, hlc(950).packed());
    // O's pair was skipped WHOLE: tombstoned AND journaled (peers still learn
    // an accounted tombstone), never one without the other.
    let o = track_state(&db, "O").unwrap();
    assert_eq!(o.1, Some(900));
    assert_eq!(o.2, derived_at(850).packed());
    assert!(track_state(&db, "T").is_none());
    let journal = oplog_rows(&db);
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0].0, id("O"));
    assert_eq!(journal[0].2, derived_at(850).packed());
    // T's tombstone entry belongs to W's resolution (W landed, so its ledger
    // stands); the parked snapshot below carries exactly that journaled state
    // — not an un-journaled ghost.
    assert_eq!(journal[1].0, id("T"));
    assert_eq!(journal[1].2, derived_at(900).packed());
    let parked = parked_track(&store);
    assert_eq!(parked.remote_hlc_packed, derived_at(900).packed());
    assert_eq!(
        SyncRow::decode(&parked.payload)
            .unwrap()
            .field("deleted_at"),
        Some(&FieldValue::Int(950))
    );

    // Convergence: the parent lands and the replay applies the tombstoned
    // state as-is (no resolution runs for a dead record).
    applier.apply(&[fx_bus("B", "P", &hlc(985), None)]);
    let replay = applier.apply(&[SyncRow::decode(&parked.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    assert_eq!(track_state(&db, "T").unwrap().1, Some(950));
    assert_eq!(track_state(&db, "W").unwrap().1, None);
    assert_eq!(oplog_rows(&db).len(), 2);
}

#[test]
fn an_updated_winner_parked_after_a_later_winner_took_the_slot_restores_into_its_own_slot() {
    // The prior row restores verbatim into its own (free) slot, the occupant
    // pair is skipped whole, and the page commits.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        practice("Q", "q", &hlc(801)),
        acc_track("O", "P", &hlc(850), None),
        acc_track("T", "Q", &hlc(820), None), // pre-exists in Q's slot
    ]);

    // The update MOVES T into P's slot (beating O) with a dangling bus; W
    // then beats T. The old code restored O under the live W — brick.
    let res = applier.apply(&[
        acc_track("T", "P", &hlc(900), Some("B")),
        acc_track("W", "P", &hlc(950), None),
    ]);
    assert!(res.committed);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);

    // T is back verbatim in Q's slot (free, so no index hazard)...
    assert_eq!(
        track_state(&db, "T").unwrap(),
        (id("Q"), None, hlc(820).packed(), None)
    );
    // ...W holds P's slot, and O's pair was skipped whole.
    assert_eq!(track_state(&db, "W").unwrap().1, None);
    let o = track_state(&db, "O").unwrap();
    assert_eq!(o.1, Some(900));
    assert_eq!(o.2, derived_at(850).packed());
    let journal = oplog_rows(&db);
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0].0, id("O"));
    // W's resolution entry for T outlived the verbatim restore: a harmless
    // dirty flag — the pusher pushes CURRENT row state (T@820, state the
    // fleet already holds), never the stale stamp.
    assert_eq!(journal[1].0, id("T"));
    assert_eq!(journal[1].2, derived_at(900).packed());
    let parked = parked_track(&store);
    assert_eq!(parked.remote_hlc_packed, derived_at(900).packed());

    // Convergence: the parent lands; the parked (slot-lost, tombstoned,
    // journal-backed) state out-ranks the restored row and lands as-is.
    applier.apply(&[fx_bus("B", "P", &hlc(985), None)]);
    let replay = applier.apply(&[SyncRow::decode(&parked.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    let after = track_state(&db, "T").unwrap();
    assert_eq!(after.0, id("P"));
    assert_eq!(after.1, Some(950));
    assert_eq!(after.2, derived_at(900).packed());
    assert_eq!(track_state(&db, "W").unwrap().1, None);
    assert_eq!(oplog_rows(&db).len(), 2);
}

#[test]
fn a_node_only_winner_parked_at_the_door_still_out_ranks_the_restore_tombstone() {
    // So its replay lands (the counter-bump derivation buried it forever).
    // X's update beats the prior state by NODE alone — same (wall, counter),
    // bigger node. The restore tombstone derives from the prior
    // (850, 0, REMOTE…): under the old counter+1 scheme that was (850, 1, …),
    // OUT-RANKING the parked update — its replay then settled as an LWW skip
    // and the update was lost forever. Node-space derivation stays at
    // (850, 0, REMOTE…!LOCAL…), strictly between the prior and the winner.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        acc_track("X", "P", &hlc(850), None), // prior: (850, 0, REMOTE…)
    ]);
    let node_win = Hlc::new(850, 0, id("ZREMOTE")); // > REMOTE… in node space alone
    let res = applier.apply(&[
        acc_track("X", "P", &node_win, Some("B")), // dangling bus -> parked
        acc_track("W", "P", &hlc(950), None),      // a later winner takes the slot
    ]);
    assert!(res.committed);
    assert_eq!(res.failed, 1);

    // The restored row is tombstoned under the derived stamp…
    let x = track_state(&db, "X").unwrap();
    assert_eq!(x.1, Some(950));
    assert_eq!(x.2, derived_at(850).packed());
    // …and the parked update STILL out-ranks it. This comparison is the
    // whole regression: counter-bump derivation reversed it.
    let parked = parked_track(&store);
    assert!(
        parked.remote_hlc_packed > x.2,
        "bookkeeping must never out-rank the state it parked"
    );

    // The parent lands; the replay must LAND, not settle as a skip.
    applier.apply(&[fx_bus("B", "P", &hlc(985), None)]);
    let replay = applier.apply(&[SyncRow::decode(&parked.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    assert_eq!(replay.skipped, 0);
    assert_eq!(track_state(&db, "X").unwrap().2, parked.remote_hlc_packed);
}

#[test]
fn a_prior_row_restore_into_a_slot_a_later_winner_holds_is_restored_tombstoned_and_journaled() {
    // The pre-batch snapshot of the parked update would land in a slot a
    // later winner now holds — it is restored TOMBSTONED under a derived
    // stamp and journaled, and the page commits.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        acc_track("X", "P", &hlc(850), None), // the slot's original occupant
    ]);

    // X updates itself (dangling bus, no occupant to resolve against), then W
    // beats X. The sweep must put X's PRE-BATCH state back, but its slot is
    // now W's: a verbatim live restore would trip ux_track_slot (brick);
    // skipping would leave the dangling row spinning the sweep's fixpoint;
    // nulling just the pointer would leave the table LWW-equal to the parked
    // snapshot, which then never replays. The pinned invariant: restore the
    // snapshot tombstoned + journaled, so the page commits, every remaining
    // state is accounted, and the parked snapshot still wins LWW.
    let res = applier.apply(&[
        acc_track("X", "P", &hlc(900), Some("B")),
        acc_track("W", "P", &hlc(950), None),
    ]);
    assert!(res.committed);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 1);

    // X: pre-batch content, tombstoned at the winner's wall, derived stamp
    // over its own prior HLC.
    assert_eq!(
        track_state(&db, "X").unwrap(),
        (id("P"), Some(950), derived_at(850).packed(), None)
    );
    assert_eq!(track_state(&db, "W").unwrap().1, None);
    // The ledger holds W's resolution entry (superseded WITHIN the ledger —
    // the drain collapses per row to the latest entry) and the
    // tombstone-restore entry, which matches the row exactly.
    let journal = oplog_rows(&db);
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0].0, id("X"));
    assert_eq!(journal[0].2, derived_at(900).packed());
    assert_eq!(journal[1].0, id("X"));
    assert_eq!(journal[1].1, "tombstone");
    assert_eq!(journal[1].2, derived_at(850).packed());
    // The parked post-update snapshot out-ranks the derived stamp...
    let parked = parked_track(&store);
    assert_eq!(parked.remote_hlc_packed, derived_at(900).packed());
    assert!(parked.remote_hlc_packed > derived_at(850).packed());

    // ...so the replay lands it once the parent arrives.
    applier.apply(&[fx_bus("B", "P", &hlc(985), None)]);
    let replay = applier.apply(&[SyncRow::decode(&parked.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    assert_eq!(
        track_state(&db, "X").unwrap(),
        (id("P"), Some(950), derived_at(900).packed(), Some(id("B")))
    );
    assert_eq!(track_state(&db, "W").unwrap().1, None);
    assert_eq!(oplog_rows(&db).len(), 2);
}

#[test]
fn a_three_way_chain_skips_the_original_occupant_under_the_live_final_winner() {
    // The parked record beat the ORIGINAL occupant, was beaten by a second,
    // which a third then beat — the revert skips the original occupant under
    // the live final winner and the page commits.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[
        practice("P", "p", &hlc(800)),
        acc_track("O", "P", &hlc(850), None),
    ]);

    let res = applier.apply(&[
        acc_track("T", "P", &hlc(900), Some("B")),
        acc_track("O2", "P", &hlc(950), None),
        acc_track("W", "P", &hlc(980), None),
    ]);
    assert!(res.committed);
    assert_eq!(res.applied, 2);
    assert_eq!(res.failed, 1);

    assert_eq!(track_state(&db, "W").unwrap().1, None);
    let o2 = track_state(&db, "O2").unwrap();
    assert_eq!(o2.1, Some(980)); // W's wall — O2's loss stands
    assert_eq!(o2.2, derived_at(950).packed());
    let o = track_state(&db, "O").unwrap();
    assert_eq!(o.1, Some(900));
    assert_eq!(o.2, derived_at(850).packed());
    assert!(track_state(&db, "T").is_none());
    // Every tombstone is journaled: O by the parked T's kept pair, T by O2's
    // resolution, O2 by W's — none reverted, none half-reverted.
    let journal = oplog_rows(&db);
    assert_eq!(journal.len(), 3);
    assert_eq!(
        (journal[0].0.clone(), journal[0].2.clone()),
        (id("O"), derived_at(850).packed())
    );
    assert_eq!(
        (journal[1].0.clone(), journal[1].2.clone()),
        (id("T"), derived_at(900).packed())
    );
    assert_eq!(
        (journal[2].0.clone(), journal[2].2.clone()),
        (id("O2"), derived_at(950).packed())
    );

    // Convergence after the parent lands.
    applier.apply(&[fx_bus("B", "P", &hlc(985), None)]);
    let parked = parked_track(&store);
    let replay = applier.apply(&[SyncRow::decode(&parked.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    assert_eq!(track_state(&db, "T").unwrap().1, Some(950)); // O2's wall
    assert_eq!(track_state(&db, "W").unwrap().1, None);
    assert_eq!(oplog_rows(&db).len(), 3);
}

// --- LWW quarantine store at the applier -------------------------------------
// A stale park never overwrites a newer one.

fn practice_row(db: &AppDatabase) -> (String, Option<String>, String) {
    practice_state(db, "P")
}

#[test]
fn legal_same_page_ordering_parks_both_the_swept_and_the_drift_parked_state() {
    // P@950 applies then is FK-swept, P@1000 was parked at the drift gate —
    // parks BOTH: P@950 lands with its parent while the clock is still
    // behind, P@1000 once it catches up.
    let db = memory_db();
    let now = Now::new(1000);
    let (applier, store) = parking_with(&db, clock_reading(&now));
    let future = hlc(1000 + HlcClock::DEFAULT_MAX_DRIFT_MS + 60 * 1000);
    let res = applier.apply(&[
        // Applies first, into a missing asset parent (locally never seen).
        practice_full("P", "v950", &hlc(950), None, Some("A")),
        // Arrives later on the same page; parked at the drift gate @1000.
        practice("P", "v1000", &future),
    ]);
    assert!(res.committed);
    assert_eq!(res.quarantined, 1); // P@1000
    assert_eq!(res.applied, 0); // P@950 applied but was swept
    assert_eq!(res.failed, 1); // ...and parked as an fk-orphan
    assert!(!practice_exists(&db, "P"));

    // The sweep's park of the CURRENT db row (P@950) ran LAST. Under an
    // unconditional upsert it overwrote the newer state; under the shared
    // slot's LWW guard it was filtered out instead, and the DELETE then took
    // P@950's only copy. Neither may happen: the door entry keeps P@1000 and
    // the row's sweep slot keeps P@950.
    let parked = parked(&store);
    assert_eq!(parked.len(), 2);
    let door = parked.iter().find(|q| q.id == id("P")).unwrap().clone();
    assert_eq!(door.remote_hlc_packed, future.packed());
    assert!(door.reason.starts_with("hlc-drift"));
    let swept = parked
        .iter()
        .find(|q| q.id == QuarantineStore::sweep_slot_id(&id("P")))
        .unwrap()
        .clone();
    assert_eq!(swept.remote_hlc_packed, hlc(950).packed());
    assert!(swept.reason.starts_with("fk-orphan"));

    // The parent lands while P@1000 is still implausible: the replay lands
    // the honest P@950 NOW instead of leaving the row deleted until the clock
    // catches up with the runaway peer.
    applier.apply(&[asset_record("A", &hlc(960))]);
    assert_eq!(
        applier
            .apply(&[SyncRow::decode(&door.payload).unwrap()])
            .quarantined,
        1
    );
    assert_eq!(
        applier
            .apply(&[SyncRow::decode(&swept.payload).unwrap()])
            .applied,
        1
    );
    assert_eq!(practice_row(&db).0, "v950");

    // The clock catches up: P@1000 lands over it by plain LWW.
    now.set(future.wall_ms);
    let replay = applier.apply(&[SyncRow::decode(&door.payload).unwrap()]);
    assert_eq!(replay.applied, 1);
    let r = practice_row(&db);
    assert_eq!(r.0, "v1000");
    assert_eq!(r.2, future.packed());
}

#[test]
fn park_and_restore_a_drift_parked_newer_state_does_not_displace_the_parked_update() {
    // The restored row moves to it once its parent lands.
    let db = memory_db();
    let (applier, store) = parking(&db);
    applier.apply(&[practice("P", "v900", &hlc(900))]);
    let future = hlc(1000 + HlcClock::DEFAULT_MAX_DRIFT_MS + 60 * 1000);
    let res = applier.apply(&[
        // Repoints the pre-existing row at a missing asset: parked by the
        // sweep, the row restored to P@900 verbatim.
        practice_full("P", "v950", &hlc(950), None, Some("A")),
        practice("P", "v1000", &future), // parked at the drift gate
    ]);
    assert!(res.committed);
    assert_eq!(practice_row(&db).2, hlc(900).packed());
    assert_eq!(store.list().unwrap().len(), 2);

    // Under the shared slot only P@1000 survived: once the asset landed the
    // row stayed on the restored P@900 — the update every peer holding the
    // asset shows was gone from this device.
    applier.apply(&[asset_record("A", &hlc(960))]);
    for q in store.list().unwrap() {
        applier.apply(&[SyncRow::decode(&q.payload).unwrap()]);
    }
    assert_eq!(
        practice_row(&db),
        ("v950".to_string(), Some(id("A")), hlc(950).packed())
    );
}

#[test]
fn a_permanently_unreplayable_newer_record_cannot_void_the_fk_sweep_park() {
    // P@950 applies, P@960 is parked at the value gate, the sweep still parks
    // (and later replays) the honest P@950.
    let db = memory_db();
    let (applier, store) = parking(&db);
    let honest = hlc(950);
    let garbage = hlc(960);
    let res = applier.apply(&[
        // Applies first, into a missing asset parent (locally never seen).
        practice_full("P", "honest", &honest, None, Some("A")),
        // A newer state from a non-conforming writer: lyrics_json is valid
        // JSON of the wrong shape, parked at the value gate — a payload the
        // replay's identical gate re-rejects every cycle, forever.
        with(
            &practice("P", "garbage", &garbage),
            &[("lyrics_json", "{}".into())],
        ),
    ]);
    assert!(res.committed);
    assert_eq!(res.failed, 2); // the parked garbage and the swept honest row
    assert!(!practice_exists(&db, "P"), "the sweep deleted the row");

    // The sweep's park ran LAST with the OLDER stamp. Under plain
    // newest-stamp retention in one shared slot the dead P@960 held it and the
    // honest P@950 — the only surviving copy of a row the change token is
    // already past — was silently dropped with the DELETE. The sweep parks
    // into the row's own slot: the dead state stays beside it as evidence and
    // can never displace it.
    let parked = parked(&store);
    assert_eq!(parked.len(), 2);
    assert!(parked
        .iter()
        .find(|q| q.id == id("P"))
        .unwrap()
        .reason
        .starts_with("invalid-value"));
    let swept = parked
        .iter()
        .find(|q| q.id == QuarantineStore::sweep_slot_id(&id("P")))
        .unwrap();
    assert_eq!(swept.remote_hlc_packed, honest.packed());
    assert!(swept.reason.starts_with("fk-orphan"));
    let snapshot = SyncRow::decode(&swept.payload).unwrap();
    assert_eq!(snapshot.field("title"), Some(&FieldValue::from("honest")));

    // The parent lands: the replayed park restores the honest row.
    applier.apply(&[asset_record("A", &hlc(955))]);
    let replay = applier.apply(&[snapshot]);
    assert_eq!(replay.applied, 1);
    let r = practice_row(&db);
    assert_eq!(r.0, "honest");
    assert_eq!(r.2, honest.packed());
}

// --- derived-node stamps -------------------------------------------------------
// Bookkeeping can never revert a held-wall author edit.

/// (title, separation_asset_id, hlc_ctr, hlc_node) of practice P.
fn practice_stamp_row(db: &AppDatabase) -> (String, Option<String>, i64, String) {
    db.query_row(
        "SELECT title, separation_asset_id, hlc_ctr, hlc_node FROM practices WHERE id = ?",
        [id("P")],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

fn authored_practice(
    title: &str,
    created_at: i64,
    h: &Hlc,
    active_interpretation_id: Option<&str>,
) -> SyncRow {
    SyncRow::new(
        "practices",
        fields! {
            "id" => id("P"),
            "title" => title,
            "timeline_duration_ms" => 0i64,
            "master_volume" => 1.0,
            "active_interpretation_id" => active_interpretation_id.map(id),
            "separation_asset_id" => id("A"),
            "reverb_json" => NULL,
            "lyrics_json" => NULL,
            "created_at" => created_at,
            "updated_at" => h.wall_ms,
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    )
}

fn healer(db: &Arc<AppDatabase>) -> IntegrityChecker {
    IntegrityChecker::new(
        db.clone(),
        Arc::new(SyncWriter::new(db.clone(), clock_at(2000))),
    )
}

#[test]
fn a_repair_tying_a_concurrent_real_edit_on_wall_and_counter_loses_to_it() {
    // And the heal re-derives above the edit. The protocol's documented lost
    // update: an author whose clock wall is HELD stamps a real edit at
    // (base.wall, base.counter + 1) — exactly the stamp a healer derived from
    // the same base under the old counter scheme. Author node 'AAA…' sorts
    // BELOW the healer device 'LOCAL…', the precise draw where the old
    // real-node repair stamp won the tie and its full-row snapshot reverted
    // the author's title fleet-wide.
    let db = memory_db();
    let applier = plain(&db);
    let author = id("AAA");
    let base = Hlc::new(1000, 5, author.clone());
    applier.apply(&[asset_record("A", &hlc(900))]);
    let seeded = applier.apply(&[authored_practice("original", base.wall_ms, &base, None)]);
    assert_eq!(seeded.applied, 1);
    // The asset is tombstoned remotely: P's pointer now dangles (the row
    // still exists, so the FK is satisfied — only the live-target invariant
    // broke, which is exactly what the IntegrityChecker heals).
    applier.apply(&[asset_full("A", &hlc(1100), "wav", Some(1100))]);

    // The local heal derives from the base in node space.
    let checker = healer(&db);
    assert!(checker.repair().unwrap().is_empty());
    let (_, sep, ctr, node) = practice_stamp_row(&db);
    assert_eq!(sep, None);
    // Node-space derivation: same (wall, counter) as the base, node extended
    // — the repair replicates over the base yet loses to EVERY stamp above
    // it.
    assert_eq!(ctr, base.counter);
    assert!(
        node.contains('!'),
        "repairs must stamp in the derived node space"
    );

    // The author's held-wall edit arrives: the next counter under its real
    // node, a renamed title, and the pre-heal dangling pointer. It must win —
    // the old healer-node stamp beat it and silently threw the rename away.
    let edit = Hlc::new(base.wall_ms, base.counter + 1, author);
    let applied = applier.apply(&[authored_practice("renamed", base.wall_ms, &edit, None)]);
    assert_eq!(applied.applied, 1);
    assert_eq!(
        practice_stamp_row(&db).0,
        "renamed",
        "user intent must win the (wall, counter) tie"
    );

    // The heal is DERIVABLE state: the next pass re-derives it above the
    // author's edit, keeping both the rename and the heal.
    assert!(checker.repair().unwrap().is_empty());
    let (t, sep, ctr, node) = practice_stamp_row(&db);
    assert_eq!(t, "renamed");
    assert_eq!(sep, None);
    assert_eq!(ctr, edit.counter);
    assert!(node.contains('!'));
}

#[test]
fn chained_repairs_on_one_row_still_lose_to_a_held_wall_author_edit() {
    // Counter escalation was base+2 beating base+1 real. TWO dangling
    // pointers on one practice row make the SAME repair pass derive twice:
    // heal #1 from the base, heal #2 from heal #1's row. The old counter
    // derivation stamped base+1 then base+2 — and base+2 bookkeeping beat a
    // base+1 REAL edit, silently reverting it fleet-wide. Node-space
    // derivation never leaves the base's (wall, counter), so any number of
    // chained heals stays below it.
    let db = memory_db();
    let applier = plain(&db);
    let author = id("AAA");
    let base = Hlc::new(1000, 5, author.clone());
    applier.apply(&[asset_record("A", &hlc(900))]);
    let seeded = applier.apply(&[
        authored_practice("original", base.wall_ms, &base, Some("I")),
        interp_raw(&id("I"), "P", &hlc(905), None),
    ]);
    assert_eq!(seeded.applied, 2);
    // Both targets die: the repair pass must heal BOTH pointers — two
    // sequential derived writes on the same practices row.
    applier.apply(&[asset_full("A", &hlc(1100), "wav", Some(1100))]);
    applier.apply(&[interp_raw(
        &id("I"),
        "P",
        &Hlc::new(1105, 0, remote_node()),
        Some(1105),
    )]);
    let checker = healer(&db);
    assert!(checker.repair().unwrap().is_empty());
    let (_, sep, ctr, chained) = practice_stamp_row(&db);
    assert_eq!(sep, None);
    assert!(
        chained.matches('!').count() >= 2,
        "the second heal must derive from the first (a chain): {chained}"
    );
    assert_eq!(ctr, base.counter, "never a counter bump");

    // The held-wall author edit at base+1 must beat the WHOLE chain.
    let edit = Hlc::new(base.wall_ms, base.counter + 1, author);
    let applied = applier.apply(&[authored_practice("renamed", base.wall_ms, &edit, Some("I"))]);
    assert_eq!(applied.applied, 1);
    assert_eq!(
        practice_stamp_row(&db).0,
        "renamed",
        "chained bookkeeping lost"
    );

    // And the heals re-derive above the edit, keeping the rename.
    assert!(checker.repair().unwrap().is_empty());
    let (t, sep, ctr, _) = practice_stamp_row(&db);
    assert_eq!(t, "renamed");
    assert_eq!(sep, None);
    assert_eq!(ctr, edit.counter);
}
