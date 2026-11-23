//! Port of `client/test/sync_engine_test.dart`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rusqlite::Connection;
use vocara_store::db::rows::{AssetRow, AssetSyncRow, PracticeRow};
use vocara_store::db::AppDatabase;
use vocara_store::error::{Error, Result};
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::integrity_checker::{IntegrityChecker, IntegrityViolation};
use vocara_store::sync::oplog_drain::OplogDrain;
use vocara_store::sync::quarantine_store::{QuarantineEntry, QuarantineStore};
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_engine::{
    IntegrityRepair, OplogInserts, RemoteApplied, SyncEngine, SyncEngineOptions, SyncEngineParts,
    SyncTimer, SyncTimerFactory,
};
use vocara_store::sync::sync_row::{FieldValue, SyncRow, SyncRowBuilder};
use vocara_store::sync::sync_state_store::SyncStateStore;
use vocara_store::sync::sync_transport::{
    FakeCloud, FakeSyncTransport, FetchPage, PushAck, PushRowResult, RejectedRecord, SyncTransport,
    SyncTransportException, TransportError,
};
use vocara_store::sync::sync_writer::SyncWriter;

use crate::support::*;

/// A fixed epoch anchor (2026-01-01T00:00:00Z): every stamp in these tests
/// is derived from it, so nothing depends on the machine's wall clock.
pub const T0: i64 = 1_767_225_600_000;

fn remote_hlc(wall: i64) -> Hlc {
    Hlc::new(wall, 0, id("REMOTE"))
}

/// A manually-fired timer so the debounce wiring is testable without time.
pub struct FakeTimer {
    pub duration: Duration,
    callback: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    cancelled: AtomicBool,
}

impl FakeTimer {
    pub fn fire(&self) {
        if self.cancelled.load(Ordering::SeqCst) {
            return;
        }
        let callback = self.callback.lock().take();
        if let Some(run) = callback {
            run();
        }
    }
}

struct FakeTimerHandle(Arc<FakeTimer>);

impl SyncTimer for FakeTimerHandle {
    fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
    }
}

/// A timer factory recording every timer it mints.
fn fake_timers() -> (SyncTimerFactory, Arc<Mutex<Vec<Arc<FakeTimer>>>>) {
    let created: Arc<Mutex<Vec<Arc<FakeTimer>>>> = Arc::default();
    let sink = created.clone();
    let factory: SyncTimerFactory = Arc::new(move |duration, run| {
        let timer = Arc::new(FakeTimer {
            duration,
            callback: Mutex::new(Some(run)),
            cancelled: AtomicBool::new(false),
        });
        sink.lock().push(timer.clone());
        Box::new(FakeTimerHandle(timer)) as Box<dyn SyncTimer>
    });
    (factory, created)
}

/// Stands in for a process that dies after a pull run's pages committed but
/// before its end-of-run repair transaction ran.
struct DyingChecker;

impl IntegrityRepair for DyingChecker {
    fn repair(&self) -> Result<Vec<IntegrityViolation>> {
        Err(Error::State("process killed before the repair".into()))
    }
}

/// A NON-conforming transport for the pull loop's rows-bearing
/// refuse-to-spin guard: it serves the same rows-bearing page with has_more
/// set and, once a token exists, never advances it — the violation the
/// token contract forbids ("a transport must advance the token whenever it
/// returns rows"). Fetches are BOUNDED so an engine that does spin fails the
/// test on the fetch count instead of hanging it.
struct StuckTokenTransport {
    rows: Vec<SyncRow>,
    fetch_calls: AtomicUsize,
}

impl SyncTransport for StuckTokenTransport {
    fn push_rows(&self, rows: &[SyncRow]) -> std::result::Result<PushAck, TransportError> {
        Ok(PushAck::new(
            rows.iter().cloned().map(PushRowResult::accepted).collect(),
        ))
    }

    fn fetch_changes(
        &self,
        since_token: Option<&str>,
    ) -> std::result::Result<FetchPage, TransportError> {
        if self.fetch_calls.fetch_add(1, Ordering::SeqCst) + 1 > 4 {
            return Err(SyncTransportException::new("spun: the engine kept refetching").into());
        }
        Ok(FetchPage::new(
            self.rows.clone(),
            since_token.unwrap_or("T0"),
            true,
        ))
    }

    fn push_asset_bytes(&self, _: &str, _: &Path) -> std::result::Result<(), TransportError> {
        Err(TransportError::Other("UnimplementedError".into()))
    }

    fn fetch_asset_bytes(&self, _: &str, _: &Path) -> std::result::Result<(), TransportError> {
        Err(TransportError::Other("UnimplementedError".into()))
    }
}

/// One simulated device (the Dart suite's setUp state).
pub struct Env {
    pub db: Arc<AppDatabase>,
    pub now: Now,
    pub clock: Arc<HlcClock>,
    pub writer: Arc<SyncWriter>,
    pub cloud: Arc<FakeCloud>,
    pub transport: Arc<FakeSyncTransport>,
    pub state: SyncStateStore,
    pub quarantine: QuarantineStore,
    pub engine: Arc<SyncEngine>,
    pub events: Arc<Mutex<Vec<RemoteApplied>>>,
    pub kill_next_commit: Arc<AtomicBool>,
}

impl Env {
    pub fn new() -> Env {
        Self::over(memory_db())
    }

    /// The suite over [db] (a fresh library).
    pub fn over(db: Arc<AppDatabase>) -> Env {
        let now = Now::new(T0);
        let clock = Arc::new(HlcClock::new(id("LOCAL")).with_now(now.reader()));
        let cloud = FakeCloud::new();
        let transport = Arc::new(FakeSyncTransport::new(cloud.clone(), "A"));
        let mut env = Env {
            writer: Arc::new(SyncWriter::new(db.clone(), clock.clone())),
            state: SyncStateStore::new(db.clone()),
            quarantine: QuarantineStore::new(db.clone()),
            engine: Arc::new(placeholder_engine(&db)),
            events: Arc::default(),
            kill_next_commit: Arc::new(AtomicBool::new(false)),
            db,
            now,
            clock,
            cloud,
            transport,
        };
        env.engine = Arc::new(env.build_engine(None, None, None));
        env
    }

    pub fn build_engine(
        &self,
        timers: Option<SyncTimerFactory>,
        integrity: Option<Arc<dyn IntegrityRepair>>,
        transport: Option<Arc<dyn SyncTransport>>,
    ) -> SyncEngine {
        let events = self.events.clone();
        let kill = self.kill_next_commit.clone();
        SyncEngine::new(
            SyncEngineParts {
                db: self.db.clone(),
                applier: Arc::new(SyncApplier::new(
                    self.db.clone(),
                    self.clock.clone(),
                    Arc::new(SyncRowBuilder::new(self.db.clone())),
                    Some(self.quarantine.clone()),
                )),
                drain: OplogDrain::new(self.db.clone()),
                builder: Arc::new(SyncRowBuilder::new(self.db.clone())),
                state: self.state.clone(),
                integrity: integrity.unwrap_or_else(|| Arc::new(self.checker())),
                transport: transport.unwrap_or_else(|| self.transport.clone()),
            },
            SyncEngineOptions {
                on_remote_applied: Some(Arc::new(move |e: &RemoteApplied| {
                    events.lock().push(e.clone())
                })),
                timers,
                debug_before_page_commit: Some(Arc::new(move |_: &FetchPage| {
                    if kill.load(Ordering::SeqCst) {
                        return Err(Error::State("killed page commit".into()));
                    }
                    Ok(())
                })),
                ..SyncEngineOptions::default()
            },
        )
    }

    pub fn checker(&self) -> IntegrityChecker {
        IntegrityChecker::new(self.db.clone(), self.writer.clone())
    }

    /// A LOCAL practice write exactly as the SyncWriter chokepoint produces
    /// it: row + oplog entry in one transaction, stamped by the
    /// (test-driven) clock.
    pub fn save_practice(&self, row_id: &str, title: &str, depends_on_asset_id: Option<&str>) {
        self.now.add(1000); // each save is strictly later than the previous one
        let s = self.writer.stamp();
        self.db
            .transaction(|| -> Result<()> {
                self.db.upsert(&PracticeRow {
                    id: id(row_id),
                    title: title.into(),
                    created_at: s.at(),
                    updated_at: s.at(),
                    hlc_wall: s.hlc.wall_ms,
                    hlc_ctr: s.hlc.counter,
                    hlc_node: s.hlc.node.clone(),
                    hlc_packed: s.hlc.packed(),
                    ..Default::default()
                })?;
                self.writer.journal(
                    "practices",
                    &id(row_id),
                    "insert",
                    &s.hlc,
                    &format!("h:{title}"),
                    depends_on_asset_id,
                )
            })
            .unwrap();
    }

    /// An assets row stamped now, without a journal entry.
    pub fn insert_asset(&self, row_id: &str, hash: &str) {
        let s = self.writer.stamp();
        self.db
            .insert(&AssetRow {
                id: id(row_id),
                content_hash: hash.into(),
                byte_size: 3,
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                ..Default::default()
            })
            .unwrap();
    }

    pub fn mark_uploaded(&self, hash: &str) {
        self.db
            .insert(&AssetSyncRow {
                hash: hash.into(),
                uploaded_at: 1,
            })
            .unwrap();
    }

    pub fn practice_title(&self, row_id: &str) -> Option<String> {
        col(&self.db, "practices", &id(row_id), "title")
    }

    pub fn oplog_count(&self) -> i64 {
        oplog_count(&self.db)
    }

    pub fn events(&self) -> Vec<RemoteApplied> {
        self.events.lock().clone()
    }

    pub fn parked(&self) -> Vec<vocara_store::db::rows::SyncQuarantineRow> {
        self.quarantine.list().unwrap()
    }
}

fn placeholder_engine(db: &Arc<AppDatabase>) -> SyncEngine {
    let clock = Arc::new(HlcClock::new(id("NONE")));
    SyncEngine::new(
        SyncEngineParts {
            db: db.clone(),
            applier: Arc::new(SyncApplier::new(
                db.clone(),
                clock.clone(),
                Arc::new(SyncRowBuilder::new(db.clone())),
                None,
            )),
            drain: OplogDrain::new(db.clone()),
            builder: Arc::new(SyncRowBuilder::new(db.clone())),
            state: SyncStateStore::new(db.clone()),
            integrity: Arc::new(DyingChecker),
            transport: Arc::new(FakeSyncTransport::new(FakeCloud::new(), "none")),
        },
        SyncEngineOptions::default(),
    )
}

pub fn remote_practice(row_id: &str, title: &str, h: &Hlc, deleted_at: Option<i64>) -> SyncRow {
    SyncRow::new(
        "practices",
        merged(
            fields! {
                "id" => id(row_id),
                "title" => title,
                "timeline_duration_ms" => 0i64,
                "master_volume" => 1.0,
                "active_interpretation_id" => NULL,
                "separation_asset_id" => NULL,
                "reverb_json" => NULL,
                "lyrics_json" => NULL,
            },
            sync_cols(h, deleted_at),
        ),
    )
}

/// Full-row snapshot (the wire contract): the applier's missing-column gate
/// quarantines a schema_v-current record that omits any table column.
pub fn remote_track(row_id: &str, practice_id: &str, h: &Hlc, deleted_at: Option<i64>) -> SyncRow {
    SyncRow::new(
        "tracks",
        merged(
            fields! {
                "id" => id(row_id),
                "practice_id" => id(practice_id),
                "kind" => "vocal",
                "interpretation_id" => NULL,
                "display_name" => "take",
                "color_value" => NULL,
                "muted" => 0i64,
                "soloed" => 0i64,
                "volume" => 1.0,
                "pan" => 0.0,
                "bus_id" => NULL,
                "fx_json" => NULL,
                "sends_json" => NULL,
                "order_index" => 0i64,
            },
            sync_cols(h, deleted_at),
        ),
    )
}

pub fn remote_asset(row_id: &str, hash: &str, h: &Hlc) -> SyncRow {
    SyncRow::new(
        "assets",
        merged(
            fields! {
                "id" => id(row_id),
                "content_hash" => hash,
                "byte_size" => 3i64,
                "ext" => "wav",
                "kind" => "audio",
                "local_rel_path" => NULL,
                "record_sync_state" => "localOnly",
                "bytes_sync_state" => "localOnly",
                "remote_ref" => NULL,
            },
            sync_cols(h, None),
        ),
    )
}

pub fn remote_clip(
    row_id: &str,
    track_id: &str,
    asset_id: &str,
    h: &Hlc,
    deleted_at: Option<i64>,
    start_ms: i64,
) -> SyncRow {
    SyncRow::new(
        "clips",
        merged(
            fields! {
                "id" => id(row_id),
                "track_id" => id(track_id),
                "asset_id" => id(asset_id),
                "start_ms" => start_ms,
                "duration_ms" => 100i64,
                "order_index" => 0i64,
            },
            sync_cols(h, deleted_at),
        ),
    )
}

/// A wire ECHO of [real] forged to claim the HLC [h] — same (table, id)
/// identity and an internally-consistent quad, so it correlates cleanly with
/// the attempt and passes every shape check. Models a hostile ack trying to
/// re-anchor the engine's HLC comparisons; the engine must bind every
/// decision to its own attempt row instead.
fn forged_echo(real: &SyncRow, h: &Hlc) -> SyncRow {
    with(
        real,
        &[
            ("hlc_wall", h.wall_ms.into()),
            ("hlc_ctr", h.counter.into()),
            ("hlc_node", h.node.clone().into()),
            ("hlc_packed", h.packed().into()),
        ],
    )
}

fn title_of(row: &SyncRow) -> Option<&FieldValue> {
    row.field("title")
}

// --- push ------------------------------------------------------------------------

#[test]
fn push_drains_and_acks_exactly_the_drained_range_a_save_landing_mid_push_is_not_lost() {
    let env = Env::new();
    env.save_practice("P1", "one", None);
    env.transport.set_hold_pushes(true);

    let engine = env.engine.clone();
    let run = std::thread::spawn(move || engine.pump_push());
    wait_until("the first batch in flight", || {
        env.transport.held_push_count() == 1
    });

    // A save lands WHILE the push is in flight: its oplog entry has a higher
    // seq than the drained window, so the coming ack must not eat it.
    env.save_practice("P2", "two", None);

    env.transport.release_held_pushes();
    // The engine keeps draining: the mid-push save is picked up as its own
    // second batch (still held), proving the first ack was exact.
    wait_until("the second batch in flight", || {
        env.transport.held_push_count() == 1
    });
    env.transport.release_held_pushes();
    let result = run.join().unwrap();

    assert!(!result.failed(), "{result}");
    let batches = env.transport.pushed_batches();
    assert_eq!(batches.len(), 2);
    assert_eq!(
        batches[0]
            .iter()
            .map(|r| r.id().unwrap())
            .collect::<Vec<_>>(),
        [id("P1")]
    );
    assert_eq!(
        batches[1]
            .iter()
            .map(|r| r.id().unwrap())
            .collect::<Vec<_>>(),
        [id("P2")]
    );
    assert!(env.cloud.server_row_for("practices", &id("P1")).is_some());
    assert!(env.cloud.server_row_for("practices", &id("P2")).is_some());
    assert_eq!(env.oplog_count(), 0, "everything handled is acked");
}

#[test]
fn asset_gated_rows_are_held_back_without_blocking_other_rows() {
    let env = Env::new();
    // seq 1: an ordinary row. seq 2: gated on asset X (no upload marker).
    // seq 3: another ordinary row AFTER the gated one.
    env.save_practice("B1", "before", None);
    env.insert_asset("AX", "HASH_X");
    env.save_practice("G", "gated", Some(&id("AX")));
    env.save_practice("B2", "after", None);

    let first = env.engine.pump_push();
    assert!(!first.failed());
    assert_eq!(first.held_back, 1);
    // Other rows pushed — including the one AFTER the gated entry.
    assert!(env.cloud.server_row_for("practices", &id("B1")).is_some());
    assert!(env.cloud.server_row_for("practices", &id("B2")).is_some());
    assert!(env.cloud.server_row_for("practices", &id("G")).is_none());
    // The ack watermark stopped just short of the gated entry, so it (and the
    // already-pushed B2, harmlessly re-pushable under LWW) remain.
    assert_eq!(first.acked_through, 1);
    assert_eq!(env.oplog_count(), 2);

    // P2's byte uploader would write the marker; simulate the release.
    env.mark_uploaded("HASH_X");
    let second = env.engine.pump_push();
    assert!(!second.failed());
    assert_eq!(second.held_back, 0);
    assert!(env.cloud.server_row_for("practices", &id("G")).is_some());
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn deleting_a_gated_row_does_not_strand_the_ack_watermark() {
    // A tombstone references no bytes. A delete inheriting its insert's
    // dependency waits on an upload that may never come (the asset is
    // collectable once nothing references it) — and since the push loop
    // acks only up to the entry before the first gated one, that one
    // tombstone freezes the watermark for the entire ledger.
    let env = Env::new();
    env.insert_asset("AX", "HASH_X");
    env.save_practice("G", "gated", Some(&id("AX")));
    // The row is deleted before its bytes were ever uploaded.
    env.now.add(1000);
    let t = env.writer.stamp();
    env.db
        .transaction(|| -> Result<()> {
            env.db.execute(
                "UPDATE practices SET deleted_at = ? WHERE id = ?",
                rusqlite::params![t.at(), id("G")],
            )?;
            env.writer
                .journal("practices", &id("G"), "tombstone", &t.hlc, "", None)
        })
        .unwrap();
    env.save_practice("AFTER", "behind the tombstone", None);

    let run = env.engine.pump_push();
    assert!(!run.failed());
    assert_eq!(run.held_back, 0, "a delete needs no bytes");
    assert_eq!(env.oplog_count(), 0, "the whole ledger acked");
    assert!(env
        .cloud
        .server_row_for("practices", &id("AFTER"))
        .is_some());
}

#[test]
fn a_gated_row_edited_again_stays_gated_across_pumps() {
    // The partial ack must not delete the entry that recorded its
    // dependency. The insert carries the dependency; a later content-only
    // edit does not. The drain collapses them to the newest entry (keeping
    // the dependency), so the ack cut falls BELOW the insert — which used to
    // delete the only journaled record of the gate. The next pump then saw a
    // bare update and pushed a row referencing bytes the server had never
    // received.
    let env = Env::new();
    env.insert_asset("AX", "HASH_X");
    env.save_practice("B1", "before", None);
    env.save_practice("G", "gated", Some(&id("AX")));
    env.save_practice("G", "gated, renamed", None); // no dependency of its own

    let first = env.engine.pump_push();
    assert_eq!(first.held_back, 1);
    assert!(env.cloud.server_row_for("practices", &id("G")).is_none());

    let second = env.engine.pump_push();
    assert_eq!(second.held_back, 1, "the bytes are still not uploaded");
    assert!(
        env.cloud.server_row_for("practices", &id("G")).is_none(),
        "a row must never reach the server before its bytes"
    );

    // Once the uploader posts the marker the row goes, as always.
    env.mark_uploaded("HASH_X");
    let third = env.engine.pump_push();
    assert_eq!(third.held_back, 0);
    assert!(env.cloud.server_row_for("practices", &id("G")).is_some());
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn a_cas_check_against_an_older_server_row_never_conflicts() {
    // The conforming server accepts the newer push outright: an "older-row
    // conflict" does not exist in the protocol (the engine refuses one as a
    // lie; see the conflicted-ack validation group).
    let env = Env::new();
    env.cloud.accept(remote_practice(
        "P",
        "server-old",
        &remote_hlc(T0 - 5000),
        None,
    ));
    env.save_practice("P", "local-new", None);
    env.transport.inject_conflict_once("practices", &id("P"), 1);

    let run = env.engine.pump_push();

    assert!(!run.failed());
    assert_eq!(env.transport.push_calls(), 1, "accepted on the first push");
    assert_eq!(
        title_of(&env.cloud.server_row_for("practices", &id("P")).unwrap()),
        Some(&FieldValue::from("local-new"))
    );
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn a_conflict_with_a_newer_server_row_drops_ours_and_the_server_state_arrives_via_pull() {
    let env = Env::new();
    env.save_practice("P", "local-stale", None);
    // Server holds a strictly newer version (one minute ahead — well within
    // the applier's drift gate).
    let server_row = remote_practice("P", "server-new", &remote_hlc(env.now.get() + 60000), None);
    env.cloud.accept(server_row);
    env.transport.inject_conflict_once("practices", &id("P"), 1);

    let run = env.engine.pump_push();

    assert!(!run.failed());
    assert_eq!(
        env.transport.push_calls(),
        1,
        "a losing row is not re-pushed"
    );
    assert_eq!(
        title_of(&env.cloud.server_row_for("practices", &id("P")).unwrap()),
        Some(&FieldValue::from("server-new"))
    );
    assert_eq!(env.oplog_count(), 0, "the dropped row is handled");
    // Local still shows ours until the pull brings the newer server state.
    assert_eq!(env.practice_title("P").as_deref(), Some("local-stale"));
    let pull = env.engine.pump_pull();
    assert!(!pull.failed());
    assert_eq!(env.practice_title("P").as_deref(), Some("server-new"));
}

#[test]
fn a_transport_failure_leaves_the_oplog_untouched_and_the_next_pump_retries() {
    let env = Env::new();
    env.save_practice("P", "one", None);
    env.transport.set_partitioned(true);

    let failing = env.engine.pump_push();
    assert!(failing.failed());
    assert_eq!(env.oplog_count(), 1);
    assert!(env.cloud.log().is_empty());

    env.transport.set_partitioned(false);
    let retry = env.engine.pump_push();
    assert!(!retry.failed());
    assert_eq!(env.oplog_count(), 0);
    assert!(env.cloud.server_row_for("practices", &id("P")).is_some());
}

// --- push ack validation -------------------------------------------------------
// A PushAck is never trusted blind: the result set must cover EXACTLY the
// pushed rows, or the engine must fail the run BEFORE the oplog ack —
// otherwise a buggy or hostile server returning a short/wrong ack lets the
// engine delete journal entries for rows the server never stored.

fn assert_transport_failure(run: &vocara_store::sync::sync_engine::PushRun) {
    assert!(run.failed());
    assert!(
        run.failure.as_ref().unwrap().is_transport_exception(),
        "{run}"
    );
}

#[test]
fn an_ack_with_fewer_results_than_pushed_rows_fails_the_run_and_leaves_the_oplog_intact() {
    let env = Env::new();
    env.save_practice("P1", "one", None);
    env.save_practice("P2", "two", None);
    env.transport
        .set_mangle_next_ack(|_, ack| PushAck::new(ack.results[..1].to_vec()));

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(run.acked_through, 0);
    assert_eq!(env.oplog_count(), 2, "nothing acked on a bad ack");

    // The knob cleared itself: an honest server drains the retry (the
    // re-push of already-stored rows is a harmless LWW no-op server-side).
    let retry = env.engine.pump_push();
    assert!(!retry.failed());
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn an_ack_whose_result_is_for_a_different_row_fails_the_run() {
    // Same count is not enough — identity must match.
    let env = Env::new();
    env.save_practice("P1", "one", None);
    env.transport.set_mangle_next_ack(|_, _| {
        PushAck::new(vec![PushRowResult::accepted(remote_practice(
            "IMPOSTOR",
            "not what we sent",
            &remote_hlc(T0),
            None,
        ))])
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 1);
}

#[test]
fn duplicate_results_for_one_row_masking_a_missing_one_fail_the_run() {
    // The match is 1:1, not set membership.
    let env = Env::new();
    env.save_practice("P1", "one", None);
    env.save_practice("P2", "two", None);
    env.transport.set_mangle_next_ack(|_, ack| {
        PushAck::new(vec![ack.results[0].clone(), ack.results[0].clone()])
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 2);
}

// --- conflicted-ack validation -------------------------------------------------
// A conflicted result's serverRow decides retry-or-drop; trusted blind, a
// lazy lie lets the server make the engine silently drop (and ack away) an
// edit it never stored. The engine validates every conflicted serverRow —
// existence, identity, internal HLC consistency, strict newness — and fails
// the run on any violation, oplog intact.

#[test]
fn a_conflicted_result_carrying_no_server_row_fails_the_run() {
    // A conflicted push result WITHOUT a serverRow: inconsistent through
    // PushRowResult's own constructors, this models the seam a decoding
    // transport adds — a response that CLAIMS conflict while carrying no
    // server state. The engine must refuse it rather than misread it as
    // accepted.
    let env = Env::new();
    env.save_practice("P", "one", None);
    env.transport.set_mangle_next_ack(|_, ack| {
        PushAck::new(vec![PushRowResult {
            row: ack.results[0].row.clone(),
            server_row: None,
            accepted: false,
        }])
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 1, "nothing acked on a bad ack");
}

#[test]
fn a_conflicted_server_row_with_a_mismatched_identity_fails_the_run() {
    // A rejection must be justified by the row it rejects.
    let env = Env::new();
    env.save_practice("P", "one", None);
    let later = env.now.get() + 60000;
    env.transport.set_mangle_next_ack(move |_, ack| {
        PushAck::new(vec![PushRowResult::conflicted(
            ack.results[0].row.clone(),
            // Internally consistent and plausibly newer — but for another
            // row.
            remote_practice("OTHER", "x", &remote_hlc(later), None),
        )])
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 1);
}

#[test]
fn a_conflicted_server_row_whose_hlc_packed_disagrees_with_its_quad_fails_the_run() {
    // The forged-LWW-key lie, same rule as the pull door.
    let env = Env::new();
    env.save_practice("P", "one", None);
    let now = env.now.get();
    let honest = remote_practice("P", "x", &remote_hlc(now + 60000), None);
    env.transport.set_mangle_next_ack(move |_, ack| {
        PushAck::new(vec![PushRowResult::conflicted(
            ack.results[0].row.clone(),
            // The quad says +60s while the packed LWW key claims +120s:
            // exactly the inconsistency SyncRow::from_json refuses on pull,
            // enforced here through the same shared routine.
            with(
                &honest,
                &[("hlc_packed", remote_hlc(now + 120000).packed().into())],
            ),
        )])
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 1);
}

#[test]
fn a_conflicted_server_row_older_than_the_pushed_row_fails_the_run() {
    // A conforming server accepts a push it holds nothing newer than.
    let env = Env::new();
    env.save_practice("P", "local-new", None);
    env.transport.set_mangle_next_ack(|_, ack| {
        PushAck::new(vec![PushRowResult::conflicted(
            ack.results[0].row.clone(),
            // Internally consistent, same identity — but strictly OLDER than
            // the push it claims to reject: a nonsensical CAS.
            remote_practice("P", "server-old", &remote_hlc(T0 - 5000), None),
        )])
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 1, "nothing acked on a bad ack");
}

#[test]
fn a_conflicted_server_row_that_merely_echoes_the_pushed_row_fails_the_run() {
    // Equal HLC — not strictly newer.
    let env = Env::new();
    env.save_practice("P", "one", None);
    env.transport.set_mangle_next_ack(|_, ack| {
        PushAck::new(
            ack.results
                .iter()
                // The cheapest possible lie: "I hold your row, therefore you
                // conflict". An equal HLC can never legitimately CAS-reject
                // (equal is the idempotent re-push, which a conforming server
                // accepts).
                .map(|r| PushRowResult::conflicted(r.row.clone(), r.row.clone()))
                .collect(),
        )
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 1);

    // The knob cleared itself: an honest server drains the retry.
    let retry = env.engine.pump_push();
    assert!(!retry.failed());
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn a_forged_older_echo_with_a_mid_hlc_server_row_fails_the_run() {
    // Every decision binds to the ATTEMPT row, never the echo. The
    // counter-example this rule exists for: we push P@100. A hostile ack
    // echoes P@0 as "the pushed row" and pairs it with a consistent serverRow
    // P@50 — strictly newer than the ECHO, strictly older than the true
    // attempt. Validated against the echo, this passed every check; the
    // engine judged "server newer", dropped ours, and acked away the drained
    // oplog entry — the local edit silently lost to nothing but a mangled
    // ack.
    let env = Env::new();
    env.save_practice("P", "local-new", None);
    env.transport.set_mangle_next_ack(|pushed, _| {
        PushAck::new(vec![PushRowResult::conflicted(
            forged_echo(&pushed[0], &remote_hlc(T0 - 10000)),
            remote_practice("P", "server-mid", &remote_hlc(T0 - 5000), None),
        )])
    });

    let run = env.engine.pump_push();
    assert_transport_failure(&run);
    assert_eq!(env.oplog_count(), 1, "nothing acked, nothing lost");
    assert_eq!(env.practice_title("P").as_deref(), Some("local-new"));

    // The knob cleared itself: an honest server drains the retry and the
    // local edit survives to the cloud.
    let retry = env.engine.pump_push();
    assert!(!retry.failed());
    assert_eq!(
        title_of(&env.cloud.server_row_for("practices", &id("P")).unwrap()),
        Some(&FieldValue::from("local-new"))
    );
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn a_genuine_conflict_still_resolves_as_a_drop_even_when_the_echo_is_forged() {
    // The serverRow is strictly newer than the attempt; the echo is the
    // correlation key and nothing else.
    let env = Env::new();
    env.save_practice("P", "local-stale", None);
    let server_new = remote_practice("P", "server-new", &remote_hlc(env.now.get() + 60000), None);
    env.cloud.accept(server_new);
    env.transport.inject_conflict_once("practices", &id("P"), 1);
    // A conforming CAS rejection, except the echo claims an ancient HLC.
    // Judged against the attempt the serverRow is still strictly newer, so
    // the conflict is genuine and resolves exactly as with an honest echo.
    env.transport.set_mangle_next_ack(|pushed, ack| {
        PushAck::new(vec![PushRowResult::conflicted(
            forged_echo(&pushed[0], &remote_hlc(T0 - 10000)),
            ack.results[0].server_row.clone().unwrap(),
        )])
    });

    let run = env.engine.pump_push();
    assert!(!run.failed(), "{run}");
    assert_eq!(
        env.transport.push_calls(),
        1,
        "a losing row is not re-pushed"
    );
    assert_eq!(env.oplog_count(), 0, "the dropped row is handled");
    assert_eq!(
        title_of(&env.cloud.server_row_for("practices", &id("P")).unwrap()),
        Some(&FieldValue::from("server-new"))
    );
}

// --- asset record sync-state (push side) ---------------------------------------

impl Env {
    /// A LOCAL asset row + its journal entry, exactly as importFile commits
    /// them (row and oplog entry in one transaction, schema defaults for the
    /// per-device sync-state columns).
    fn save_asset(&self, row_id: &str, hash: &str) {
        self.now.add(1000);
        let s = self.writer.stamp();
        self.db
            .transaction(|| -> Result<()> {
                self.db.insert(&AssetRow {
                    id: id(row_id),
                    content_hash: hash.into(),
                    byte_size: 3,
                    created_at: s.at(),
                    updated_at: s.at(),
                    hlc_wall: s.hlc.wall_ms,
                    hlc_ctr: s.hlc.counter,
                    hlc_node: s.hlc.node.clone(),
                    hlc_packed: s.hlc.packed(),
                    ..Default::default()
                })?;
                self.writer.journal(
                    "assets",
                    &id(row_id),
                    "insert",
                    &s.hlc,
                    &format!("h:{hash}"),
                    None,
                )
            })
            .unwrap();
    }

    /// (record_sync_state, bytes_sync_state, updated_at, hlc_packed).
    fn asset_state(&self, row_id: &str) -> (String, String, i64, String) {
        self.db
            .query_row(
                "SELECT record_sync_state, bytes_sync_state, updated_at, hlc_packed \
                 FROM assets WHERE id = ?",
                [id(row_id)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }
}

#[test]
fn a_pushed_and_accepted_asset_is_marked_record_synced_as_pure_bookkeeping() {
    // No journal entry, no updated_at, no HLC change.
    let env = Env::new();
    env.save_asset("A", "HASH_A");
    let before = env.asset_state("A");
    assert_eq!(before.0, "localOnly");

    let run = env.engine.pump_push();
    assert!(!run.failed());
    assert!(env.cloud.server_row_for("assets", &id("A")).is_some());

    let after = env.asset_state("A");
    assert_eq!(
        after.0, "synced",
        "the record crossed the wire — the GC never-synced guard must now protect this row"
    );
    assert_eq!(
        after.1, "localOnly",
        "blob upload has no transport yet; its state is untouched"
    );
    assert_eq!(after.2, before.2);
    assert_eq!(after.3, before.3);
    assert_eq!(
        env.oplog_count(),
        0,
        "the marking is per-device bookkeeping and journals nothing"
    );
}

#[test]
fn an_asset_dropped_as_server_newer_counts_as_synced_too() {
    // The server holds the record either way.
    let env = Env::new();
    env.save_asset("A", "HASH_A");
    // The server already holds a strictly newer version of the record.
    env.cloud.accept(remote_asset(
        "A",
        "HASH_A",
        &remote_hlc(env.now.get() + 60000),
    ));
    env.transport.inject_conflict_once("assets", &id("A"), 1);

    let run = env.engine.pump_push();
    assert!(!run.failed());
    assert_eq!(
        env.transport.push_calls(),
        1,
        "a losing row is not re-pushed"
    );
    assert_eq!(env.asset_state("A").0, "synced");
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn asset_record_sync_marking_binds_to_the_attempt_row_too() {
    // A genuine server-newer conflict whose echo is forged still marks the
    // asset synced.
    let env = Env::new();
    env.save_asset("A", "HASH_A");
    env.cloud.accept(remote_asset(
        "A",
        "HASH_A",
        &remote_hlc(env.now.get() + 60000),
    ));
    env.transport.inject_conflict_once("assets", &id("A"), 1);
    env.transport.set_mangle_next_ack(|pushed, ack| {
        PushAck::new(vec![PushRowResult::conflicted(
            forged_echo(&pushed[0], &remote_hlc(T0 - 10000)),
            ack.results[0].server_row.clone().unwrap(),
        )])
    });

    let run = env.engine.pump_push();
    assert!(!run.failed());
    assert_eq!(
        env.asset_state("A").0,
        "synced",
        "the drop decision and the marking both read the attempt row — the mangled echo must \
         not divert either"
    );
    assert_eq!(env.oplog_count(), 0);
}

#[test]
fn a_local_asset_never_accepted_by_the_server_stays_local_only() {
    // A failed run must not pre-mark anything.
    let env = Env::new();
    env.transport.set_partitioned(true);
    env.save_asset("A", "HASH_A");

    let run = env.engine.pump_push();
    assert!(run.failed());
    assert_eq!(env.asset_state("A").0, "localOnly");
    assert_eq!(env.oplog_count(), 1, "still journaled for the retry");
}

// --- pull ------------------------------------------------------------------------

#[test]
fn pull_applies_the_page_and_advances_the_token_in_one_transaction() {
    // A killed commit moves neither.
    let env = Env::new();
    env.cloud
        .accept(remote_practice("P", "hello", &remote_hlc(T0 - 1000), None));

    env.kill_next_commit.store(true, Ordering::SeqCst);
    let failing = env.engine.pump_pull();
    assert!(failing.failed());
    assert_eq!(failing.pages, 0);
    assert_eq!(env.practice_title("P"), None, "rows rolled back");
    assert_eq!(env.state.change_token().unwrap(), None, "token unmoved");

    env.kill_next_commit.store(false, Ordering::SeqCst);
    let retry = env.engine.pump_pull();
    assert!(!retry.failed());
    assert_eq!(retry.applied, 1);
    assert_eq!(env.practice_title("P").as_deref(), Some("hello"));
    assert_eq!(env.state.change_token().unwrap().as_deref(), Some("1"));
    assert_eq!(env.oplog_count(), 0, "remote applies journal nothing");
}

#[test]
fn a_re_delivered_page_is_a_no_op() {
    // At-least-once tolerance.
    let env = Env::new();
    env.cloud
        .accept(remote_practice("P", "v1", &remote_hlc(T0 - 1000), None));
    let first = env.engine.pump_pull();
    assert_eq!(first.applied, 1);
    assert_eq!(env.events().len(), 1);

    env.transport.set_redeliver_next_fetches(1);
    let again = env.engine.pump_pull();
    assert!(!again.failed());
    assert_eq!(again.applied, 0);
    assert_eq!(again.skipped, 1, "LWW: we already hold this exact row");
    assert_eq!(env.practice_title("P").as_deref(), Some("v1"));
    assert_eq!(env.state.change_token().unwrap().as_deref(), Some("1"));
    assert_eq!(env.oplog_count(), 0);
    assert_eq!(env.events().len(), 1, "a no-op page announces nothing");
}

#[test]
fn a_run_that_dies_before_its_repair_leaves_nothing_broken_after_a_restart() {
    // The new engine repairs on its first successful run even though that
    // run applies nothing. A live track under a tombstoned practice: the page
    // applies both (the FK is satisfied — the practice row exists), and only
    // the end-of-run repair tombstones the track.
    let mut env = Env::new();
    env.cloud.accept(remote_practice(
        "P",
        "deleted",
        &remote_hlc(T0 - 5000),
        Some(T0 - 5000),
    ));
    env.cloud
        .accept(remote_track("T", "P", &remote_hlc(T0 - 4000), None));
    env.engine = Arc::new(env.build_engine(None, Some(Arc::new(DyingChecker)), None));
    let died = env.engine.pump_pull();
    assert!(died.failed());
    assert_eq!(died.pages, 1, "the page and its token committed");
    assert!(!env.checker().check().unwrap().is_empty());

    // "Restart": a fresh engine over the same durable store. The token is
    // already at head, so this run fetches and applies nothing — the debt the
    // dead process held in memory is gone with it.
    env.engine = Arc::new(env.build_engine(None, None, None));
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(run.applied, 0);
    assert!(
        env.checker().check().unwrap().is_empty(),
        "the first successful run after boot repairs regardless"
    );
}

#[test]
fn a_rows_bearing_page_with_an_unmoved_token_and_has_more_fails_the_run_loudly() {
    // The rows-bearing analogue of the empty branch's refuse-to-spin guard.
    let mut env = Env::new();
    let stuck = Arc::new(StuckTokenTransport {
        rows: vec![remote_practice(
            "P",
            "served forever",
            &remote_hlc(T0 - 1000),
            None,
        )],
        fetch_calls: AtomicUsize::new(0),
    });
    env.engine = Arc::new(env.build_engine(None, None, Some(stuck.clone())));

    let run = env.engine.pump_pull();
    // The first delivery landed normally (a None token cannot be "unmoved");
    // the second returned the identical page with the token pinned. An
    // unguarded loop refetched it forever: every record a clean LWW skip,
    // has_more true, no failure surfaced — sync (and every push queued
    // behind the engine's serialization) silently wedged in-process.
    assert!(run.failed(), "the violation must surface");
    assert!(
        run.failure
            .as_ref()
            .unwrap()
            .to_string()
            .contains("transport violation"),
        "{run}"
    );
    assert_eq!(
        stuck.fetch_calls.load(Ordering::SeqCst),
        2,
        "stop at the first unmoved token — no spin"
    );
    assert_eq!(run.applied, 1);
    assert_eq!(env.practice_title("P").as_deref(), Some("served forever"));
}

#[test]
fn a_quarantined_record_is_parked_and_the_token_advances_past_it() {
    let env = Env::new();
    env.cloud.accept(remote_practice(
        "GOOD",
        "kept",
        &remote_hlc(T0 - 1000),
        None,
    ));
    // Implausibly far in the future: beyond the 5-minute drift gate.
    env.cloud.accept(remote_practice(
        "FUT",
        "poison",
        &remote_hlc(T0 + 6 * 60 * 1000),
        None,
    ));

    let run = env.engine.pump_pull();

    assert!(!run.failed());
    assert_eq!(run.applied, 1);
    assert_eq!(run.quarantined, 1);
    assert_eq!(env.practice_title("GOOD").as_deref(), Some("kept"));
    assert_eq!(env.practice_title("FUT"), None);
    assert_eq!(
        env.state.change_token().unwrap().as_deref(),
        Some("2"),
        "sync is not stalled"
    );
    let parked = env.parked();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, id("FUT"));
    // Still parked (not silently dropped) after further pulls.
    env.engine.pump_pull();
    assert_eq!(env.parked().len(), 1);
}

#[test]
fn a_record_with_a_mistyped_hlc_quad_is_quarantined_alone() {
    // The rest of the page applies and the token advances (one bad record
    // must never brick sync). Regression: the type error out of rec.hlc used
    // to escape the applier's per-record handling, roll back the whole page,
    // and leave the engine failing "pull page failed to commit; token
    // unmoved" — the same page then retried forever, blocking every good
    // record behind it.
    let env = Env::new();
    env.cloud.accept(remote_practice(
        "GOOD",
        "kept",
        &remote_hlc(T0 - 2000),
        None,
    ));
    let bad = remote_practice("BAD", "poison", &remote_hlc(T0 - 1000), None);
    env.cloud
        .accept(with(&bad, &[("hlc_wall", "not-an-int".into())]));
    env.cloud.accept(remote_practice(
        "ALSO",
        "behind",
        &remote_hlc(T0 - 500),
        None,
    ));

    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(run.applied, 2);
    assert_eq!(run.failed_records, 1);
    assert_eq!(env.practice_title("GOOD").as_deref(), Some("kept"));
    assert_eq!(env.practice_title("ALSO").as_deref(), Some("behind"));
    assert_eq!(env.practice_title("BAD"), None);
    assert_eq!(
        env.state.change_token().unwrap().as_deref(),
        Some("3"),
        "sync is not stalled"
    );
    let parked = env.parked();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, id("BAD"));
    // The column gate names the offending column at the door, instead of
    // letting the mistyped value fail deep inside apply as a generic
    // 'apply-failed' with nothing to act on.
    assert!(parked[0].reason.starts_with("invalid-value"));
}

#[test]
fn a_record_claiming_a_below_range_schema_v_is_quarantined_alone() {
    // The rest of the page applies and the token advances. Wire schema
    // versions start at 1. A below-range claim used to slip between the
    // newer-schema gate (> current) and the full-row gate (== current) and
    // apply with missing columns accepted; now it parks.
    let env = Env::new();
    env.cloud.accept(remote_practice(
        "GOOD",
        "kept",
        &remote_hlc(T0 - 3000),
        None,
    ));
    let zero = remote_practice("Z", "poison", &remote_hlc(T0 - 2000), None);
    env.cloud.accept(with(&zero, &[("schema_v", 0i64.into())]));
    let negative = remote_practice("N", "poison", &remote_hlc(T0 - 1000), None);
    env.cloud
        .accept(with(&negative, &[("schema_v", (-1i64).into())]));

    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(run.applied, 1);
    assert_eq!(run.failed_records, 2);
    assert_eq!(env.practice_title("GOOD").as_deref(), Some("kept"));
    assert_eq!(env.practice_title("Z"), None);
    assert_eq!(env.practice_title("N"), None);
    assert_eq!(
        env.state.change_token().unwrap().as_deref(),
        Some("3"),
        "sync is not stalled"
    );
    let parked = env.parked();
    assert_eq!(parked.len(), 2);
    for q in parked {
        assert!(q.reason.starts_with("invalid-schema_v"), "{}", q.reason);
    }
}

#[test]
fn an_expired_change_token_clears_the_token_and_re_pulls_from_scratch_within_the_same_run() {
    // The server compacted its log.
    let env = Env::new();
    env.cloud
        .accept(remote_practice("P1", "one", &remote_hlc(T0 - 2000), None));
    env.cloud
        .accept(remote_practice("P2", "two", &remote_hlc(T0 - 1000), None));
    let first = env.engine.pump_pull();
    assert_eq!(first.applied, 2);
    assert_eq!(env.state.change_token().unwrap().as_deref(), Some("2"));

    // The server compacts its change log past our token: the next fetch with
    // it fails with the dead-token error. The engine must clear the token and
    // restart from the beginning of the zone — safe because re-applied rows
    // are LWW no-ops.
    env.transport.set_expire_next_fetches(1);
    let recovered = env.engine.pump_pull();
    assert!(!recovered.failed());
    assert_eq!(recovered.applied, 0);
    assert_eq!(recovered.skipped, 2, "re-applied rows settle as skips");
    assert_eq!(env.practice_title("P1").as_deref(), Some("one"));
    assert_eq!(env.practice_title("P2").as_deref(), Some("two"));
    assert_eq!(
        env.state.change_token().unwrap().as_deref(),
        Some("2"),
        "token rebuilt to head"
    );
}

#[test]
fn a_transport_that_rejects_even_a_fresh_token_degrades_to_a_normal_failed_run() {
    // Instead of spinning clear-and-refetch forever.
    let env = Env::new();
    env.cloud
        .accept(remote_practice("P", "one", &remote_hlc(T0 - 1000), None));
    env.engine.pump_pull();
    assert_eq!(env.state.change_token().unwrap().as_deref(), Some("1"));

    env.transport.set_expire_next_fetches(10);
    env.transport.set_expire_fresh_tokens_too(true);
    let before = env.transport.fetch_calls();
    let run = env.engine.pump_pull();
    assert!(run.failed());
    assert!(run.failure.as_ref().unwrap().is_token_expired(), "{run}");
    // Exactly two fetches: the expired stored token, then the one bounded
    // retry with a fresh (None) token — never a spin.
    assert_eq!(env.transport.fetch_calls() - before, 2);
    assert_eq!(
        env.state.change_token().unwrap(),
        None,
        "the stored token was already dead; cleared is the honest state"
    );
}

#[test]
fn remote_applied_carries_table_id_pairs_and_rolls_child_rows_up_to_their_practice() {
    let env = Env::new();
    let h = remote_hlc(T0 - 1000);
    env.cloud.accept(remote_practice("P", "song", &h, None));
    env.cloud.accept(remote_asset("A", "HASH_A", &h));
    env.cloud.accept(remote_track("T", "P", &h, None));
    // The clip row carries only track_id — the engine resolves the practice
    // with a post-commit lookup.
    env.cloud.accept(remote_clip("C", "T", "A", &h, None, 0));

    let run = env.engine.pump_pull();
    assert_eq!(run.applied, 4);
    let events = env.events();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    for pair in [
        ("practices", "P"),
        ("assets", "A"),
        ("tracks", "T"),
        ("clips", "C"),
    ] {
        assert!(
            e.rows.contains(&(pair.0.to_string(), id(pair.1))),
            "{pair:?}"
        );
    }
    assert_eq!(
        e.practice_ids.iter().cloned().collect::<Vec<_>>(),
        [id("P")]
    );
    assert!(e.technique_ids.is_empty());
}

// --- quarantine replay cost ----------------------------------------------------

fn costed() -> (Env, Statements) {
    let (db, statements) = traced_db();
    (Env::over(db), statements)
}

#[test]
fn a_park_still_past_the_drift_bound_is_not_re_applied_by_every_pull() {
    // No transaction, no whole-store FK sweep, no rewrite. A peer whose clock
    // runs 6 minutes fast: every row it writes parks here until real time
    // catches up — and every pull replays the lot.
    let (env, cost) = costed();
    env.cloud.accept(remote_practice(
        "P",
        "ahead",
        &remote_hlc(T0 + 6 * 60 * 1000),
        None,
    ));
    assert_eq!(env.engine.pump_pull().quarantined, 1);

    cost.reset();
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(cost.transactions(), 0, "{:?}", cost.all());
    assert_eq!(cost.matching("foreign_key_check"), 0);
    assert!(cost.writes().is_empty(), "{:?}", cost.writes());
    assert_eq!(env.parked().len(), 1, "still parked");

    env.now.add(2 * 60 * 1000); // real time catches up: the replay lands it
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(env.practice_title("P").as_deref(), Some("ahead"));
    assert!(env.parked().is_empty());
}

#[test]
fn an_fk_orphan_whose_parent_is_still_missing_costs_a_pull_nothing_and_lands_with_its_parent() {
    // Every pull applied it in a transaction, ran the whole-store FK sweep,
    // and parked it again, for as long as the parent stayed away.
    let (env, cost) = costed();
    env.cloud
        .accept(remote_track("T", "P", &remote_hlc(T0 - 1000), None));
    assert_eq!(env.engine.pump_pull().failed_records, 1);
    assert!(env.parked()[0].reason.starts_with("fk-orphan"));

    cost.reset();
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(cost.transactions(), 0, "{:?}", cost.all());
    assert_eq!(cost.matching("foreign_key_check"), 0);
    assert!(cost.writes().is_empty(), "{:?}", cost.writes());
    assert_eq!(env.parked().len(), 1, "still parked");

    env.cloud
        .accept(remote_practice("P", "parent", &remote_hlc(T0 - 2000), None));
    assert!(!env.engine.pump_pull().failed());
    assert!(env.parked().is_empty());
    assert!(
        exists(&env.db, "tracks", &id("T")),
        "replayed once its parent arrived"
    );
}

/// Parks [track] under its BARE id with the FK sweep's reason, as an older
/// build did.
fn park_under_bare_id(env: &Env, track: &SyncRow) {
    env.quarantine
        .put(&QuarantineEntry {
            table: "tracks",
            id: &id("T"),
            hlc_packed: &track.hlc().unwrap().packed(),
            schema_v: track.schema_v().unwrap(),
            payload: &track.encode().unwrap(),
            reason: &format!("fk-orphan: parent practices/{} missing", id("P")),
            quarantined_at: 1,
        })
        .unwrap();
}

#[test]
fn an_fk_orphan_an_older_build_parked_under_its_bare_id_still_replays_and_moves_to_the_sweep_slot()
{
    let (env, _) = costed();
    let track = remote_track("T", "P", &remote_hlc(T0 - 1000), None);
    park_under_bare_id(&env, &track);
    assert!(!env.engine.pump_pull().failed());
    // Left under the bare id, a drift-parked newer state of the row would
    // displace it there (see QuarantineStore::put_swept).
    let parked = env.parked();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, QuarantineStore::sweep_slot_id(&id("T")));
}

#[test]
fn an_fk_orphan_the_row_has_moved_past_settles_though_its_parent_never_arrives() {
    let (env, _) = costed();
    env.cloud
        .accept(remote_practice("P", "parent", &remote_hlc(T0 - 3000), None));
    let routed = remote_track("T", "P", &remote_hlc(T0 - 2000), None);
    env.cloud
        .accept(with(&routed, &[("bus_id", id("B").into())]));
    assert_eq!(env.engine.pump_pull().failed_records, 1);
    assert!(env.parked()[0]
        .reason
        .starts_with("fk-orphan: parent fx_buses/"));

    // The author routes the take back to master; bus B never arrives.
    env.cloud
        .accept(remote_track("T", "P", &remote_hlc(T0 - 1000), None));
    assert!(!env.engine.pump_pull().failed());
    assert!(env.parked().is_empty());
}

#[test]
fn a_replay_scans_for_fk_orphans_among_the_rows_it_writes_not_the_whole_store_per_row() {
    // A peer's fast clock parks every row it sends; once real time catches
    // up, each landed in its own batch, and each batch scanned the whole store
    // for orphans: K x N for K parked rows in a library of N.
    let (env, cost) = costed();
    for p in ["A", "B", "C"] {
        env.cloud
            .accept(remote_practice(p, p, &remote_hlc(T0 + 6 * 60 * 1000), None));
    }
    assert_eq!(env.engine.pump_pull().quarantined, 3);

    env.now.add(7 * 60 * 1000);
    cost.reset();
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(cost.matching("foreign_key_check"), 0);
    for p in ["A", "B", "C"] {
        assert_eq!(env.practice_title(p).as_deref(), Some(p));
    }
    assert!(env.parked().is_empty());
}

#[test]
fn a_replayed_row_still_dangling_after_it_lands_is_parked_again() {
    // Checked among its own rows only, the orphan an older build parked
    // under its bare id is still caught, and parked rather than lost.
    let (env, _) = costed();
    let track = remote_track("T", "P", &remote_hlc(T0 - 1000), None);
    park_under_bare_id(&env, &track);
    assert!(!env.engine.pump_pull().failed());
    let parked = env.parked();
    assert!(parked[0].reason.starts_with("fk-orphan"));
    // Parked again by the sweep (into the row's sweep slot), not left as it
    // was by a replay whose commit failed on a missed violation.
    assert_eq!(parked[0].id, QuarantineStore::sweep_slot_id(&id("T")));
    assert!(!exists(&env.db, "tracks", &id("T")));
}

#[test]
fn a_park_that_can_never_land_costs_a_pull_nothing() {
    // Its verdict is a function of the payload alone, yet every pull decoded
    // it, applied it in a transaction and re-parked it unchanged — as many
    // times as a peer had sent such rows.
    let (env, cost) = costed();
    let bad = remote_practice("BAD", "never", &remote_hlc(T0), None);
    env.cloud.accept(with(&bad, &[("schema_v", 0i64.into())]));
    assert_eq!(env.engine.pump_pull().failed_records, 1);
    assert!(env.parked()[0].reason.starts_with("invalid-schema_v"));

    cost.reset();
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(cost.transactions(), 0, "{:?}", cost.all());
    assert!(cost.writes().is_empty(), "{:?}", cost.writes());
    assert_eq!(env.parked().len(), 1, "kept for review");
}

// --- quarantine replay ---------------------------------------------------------

#[test]
fn a_fresh_device_with_a_slow_clock_parks_every_row_and_a_fixed_clock_replays_them_all() {
    // Quarantine is not a black hole. Rows stamped at "true" time by a
    // healthy remote device.
    let env = Env::new();
    env.cloud
        .accept(remote_practice("P1", "one", &remote_hlc(T0), None));
    env.cloud
        .accept(remote_practice("P2", "two", &remote_hlc(T0 + 10), None));
    // This device's wall clock is 10 minutes SLOW. Fresh install: empty DB
    // means max_wall_ever_seen is 0, so the drift gate judges against the bad
    // local `now` alone and every correctly-stamped row looks far-future.
    env.now.set(T0 - 10 * 60 * 1000);

    let bad = env.engine.pump_pull();
    assert!(!bad.failed());
    assert_eq!(bad.quarantined, 2);
    assert_eq!(env.practice_title("P1"), None);
    assert_eq!(env.parked().len(), 2);
    assert_eq!(
        env.state.change_token().unwrap().as_deref(),
        Some("2"),
        "the token is already PAST the parked rows: the server will never re-deliver them, only \
         replay can let them in"
    );

    // The clock is corrected. The cloud has nothing new (token at head).
    env.now.set(T0 + 1000);
    let recovered = env.engine.pump_pull();
    assert!(!recovered.failed());
    assert_eq!(env.practice_title("P1").as_deref(), Some("one"));
    assert_eq!(env.practice_title("P2").as_deref(), Some("two"));
    assert!(env.parked().is_empty());
    assert_eq!(env.oplog_count(), 0, "replayed applies journal nothing");
}

#[test]
fn an_apply_failure_record_replays_once_the_schema_gains_the_unknown_column() {
    let env = Env::new();
    let rec = remote_practice("P", "future schema", &remote_hlc(T0 - 1000), None);
    env.cloud
        .accept(with(&rec, &[("brand_new_column", 7i64.into())]));

    let first = env.engine.pump_pull();
    assert!(!first.failed());
    assert_eq!(first.failed_records, 1);
    assert_eq!(env.practice_title("P"), None);
    assert_eq!(env.parked().len(), 1);

    // Simulate the app upgrade that understands the new column.
    env.db
        .execute_batch("ALTER TABLE practices ADD COLUMN brand_new_column INTEGER")
        .unwrap();
    let upgraded = env.engine.pump_pull();
    assert!(!upgraded.failed());
    assert_eq!(env.practice_title("P").as_deref(), Some("future schema"));
    assert!(env.parked().is_empty());
}

#[test]
fn a_parked_row_that_lost_lww_meanwhile_replays_as_a_clean_no_op_and_its_entry_is_removed() {
    // LWW settles it even while the original failure cause persists. v1
    // carries a column from the future -> parked on arrival.
    let env = Env::new();
    let v1 = remote_practice("P", "stale v1", &remote_hlc(T0 - 2000), None);
    env.cloud
        .accept(with(&v1, &[("brand_new_column", 1i64.into())]));
    let first = env.engine.pump_pull();
    assert_eq!(first.failed_records, 1);
    assert_eq!(env.practice_title("P"), None);
    assert_eq!(env.parked().len(), 1);

    // A NEWER, well-formed edit of the same row arrives and applies. The
    // end-of-run replay then finds the parked v1 strictly older than the
    // stored row: the applier's LWW comparison short-circuits BEFORE any
    // write (the unknown column never matters again), the replay is a clean
    // no-op, and the obsolete entry is settled — never a regression of the
    // row, never an entry retried forever.
    env.cloud.accept(remote_practice(
        "P",
        "fresh v2",
        &remote_hlc(T0 - 1000),
        None,
    ));
    let second = env.engine.pump_pull();
    assert!(!second.failed());
    assert_eq!(second.applied, 1);
    assert_eq!(
        env.practice_title("P").as_deref(),
        Some("fresh v2"),
        "the stale replay lost LWW cleanly"
    );
    assert!(
        env.parked().is_empty(),
        "the obsolete entry is removed, not retried forever"
    );
}

#[test]
fn parked_parent_and_child_replay_parent_first_even_when_the_child_parked_first() {
    let env = Env::new();
    env.now.set(T0 - 10 * 60 * 1000); // slow clock: everything inbound parks
                                      // The child arrives (and parks) BEFORE its parent: parked-at order is
                                      // child-first, so replay must re-order by the FK-derived table rank.
    env.cloud
        .accept(remote_track("T", "P", &remote_hlc(T0), None));
    assert_eq!(env.engine.pump_pull().quarantined, 1);
    env.cloud
        .accept(remote_practice("P", "parent", &remote_hlc(T0 + 10), None));
    assert_eq!(env.engine.pump_pull().quarantined, 1);
    assert_eq!(env.parked().len(), 2);

    env.now.set(T0 + 1000); // clock fixed
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(env.practice_title("P").as_deref(), Some("parent"));
    assert!(
        exists(&env.db, "tracks", &id("T")),
        "the child landed after its parent"
    );
    assert!(env.parked().is_empty());
}

#[test]
fn an_fk_orphan_child_parked_beside_its_quarantined_parent_recovers_whole_after_the_upgrade() {
    // Replay lands both, parent-first. The parent is forward-incompatible —
    // an unknown column stands in for a newer build's schema — so the schema
    // gate parks it. The child is a perfectly current-schema row: it APPLIES,
    // and the FK sweep must then park it as an fk-orphan rather than
    // hard-delete it. The old sweep deleted it with the token already
    // advanced — the server never re-sent it, and the upgrade replayed the
    // parent from quarantine into a library whose child was permanently
    // gone.
    let env = Env::new();
    let parent = remote_practice("P", "parent", &remote_hlc(T0 - 2000), None);
    env.cloud
        .accept(with(&parent, &[("brand_new_column", 7i64.into())]));
    env.cloud
        .accept(remote_track("T", "P", &remote_hlc(T0 - 1000), None));

    let first = env.engine.pump_pull();
    assert!(!first.failed());
    assert_eq!(first.failed_records, 2, "parked parent + swept child");
    assert_eq!(env.practice_title("P"), None);
    let classes: std::collections::HashSet<String> = env
        .parked()
        .into_iter()
        .map(|q| q.reason.split(':').next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        classes,
        ["unknown-column".to_string(), "fk-orphan".to_string()]
            .into_iter()
            .collect()
    );

    // Simulate the app upgrade that understands the parent's shape.
    env.db
        .execute_batch("ALTER TABLE practices ADD COLUMN brand_new_column INTEGER")
        .unwrap();
    let upgraded = env.engine.pump_pull();
    assert!(!upgraded.failed());
    assert_eq!(env.practice_title("P").as_deref(), Some("parent"));
    assert!(
        exists(&env.db, "tracks", &id("T")),
        "the swept child came back with its parent"
    );
    assert!(env.parked().is_empty());
}

#[test]
fn a_replayed_fk_orphan_whose_parent_is_still_missing_is_re_parked() {
    // Never lost, never an error, no growth.
    let env = Env::new();
    env.cloud
        .accept(remote_track("T", "P", &remote_hlc(T0 - 1000), None));
    let first = env.engine.pump_pull();
    assert!(!first.failed());
    assert_eq!(first.failed_records, 1);
    // Every later pull replays (or waits on) the parked child: the sweep
    // slot's upsert REPLACES the entry — no primary-key error, no pile-up, no
    // drop.
    env.engine.pump_pull();
    env.engine.pump_pull();
    let parked = env.parked();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].tbl, "tracks");
    assert_eq!(parked[0].id, QuarantineStore::sweep_slot_id(&id("T")));
    assert!(parked[0].reason.starts_with("fk-orphan"));
    assert!(
        !exists(&env.db, "tracks", &id("T")),
        "never live while the parent is missing"
    );
}

// A drift-parked newer state never displaces the FK sweep's copy of the same
// row. T@honest is a plausible child whose parent P has not arrived: it
// applies and the FK sweep parks its only copy. T@runaway is a newer state of
// the same row from a peer whose clock runs 6 minutes ahead: the drift gate
// parks it. Both pages are behind the change token, so the parking lot is the
// only place either state still exists.

fn honest() -> Hlc {
    remote_hlc(T0 - 1000)
}

fn runaway_stamp() -> Hlc {
    remote_hlc(T0 + 6 * 60 * 1000)
}

fn runaway() -> SyncRow {
    with(
        &remote_track("T", "P", &runaway_stamp(), None),
        &[("display_name", "runaway".into())],
    )
}

fn track_name_and_stamp(env: &Env) -> Option<(String, String)> {
    env.db
        .query_opt(
            "SELECT display_name, hlc_packed FROM tracks WHERE id = ?",
            [id("T")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

/// The parent lands while the runaway clock is still ahead: the honest state
/// is due NOW. Under one shared (table, id) entry the runaway state had
/// evicted it (or filtered its park out), the replay found only a stamp the
/// drift gate still refuses, and the row stayed deleted on this device —
/// while every peer holding the parent showed T@honest — until the wall clock
/// caught up with the bad peer.
fn parent_lands_then_clock_catches_up(env: &Env) {
    env.cloud
        .accept(remote_practice("P", "parent", &remote_hlc(T0 - 2000), None));
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(
        track_name_and_stamp(env),
        Some(("take".to_string(), honest().packed())),
        "the swept honest state lands with its parent"
    );
    let parked = env.parked();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, id("T"));
    assert!(parked[0].reason.starts_with("hlc-drift"));

    // The clock reaches the runaway stamp: its own replay lands it over the
    // honest state by plain LWW, and the parking lot drains.
    env.now.set(runaway_stamp().wall_ms);
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(
        track_name_and_stamp(env),
        Some(("runaway".to_string(), runaway_stamp().packed()))
    );
    assert!(env.parked().is_empty());
}

#[test]
fn sweep_first_a_later_page_s_drift_park_leaves_the_sweep_s_park_in_place() {
    let env = Env::new();
    env.cloud.accept(remote_track("T", "P", &honest(), None));
    assert_eq!(env.engine.pump_pull().failed_records, 1);
    env.cloud.accept(runaway());
    assert_eq!(env.engine.pump_pull().quarantined, 1);
    assert!(track_name_and_stamp(&env).is_none());
    assert_eq!(env.parked().len(), 2);

    parent_lands_then_clock_catches_up(&env);
}

#[test]
fn same_page_the_door_parks_the_runaway_state_before_the_end_of_batch_sweep_parks_the_honest_one() {
    let env = Env::new();
    env.cloud.accept(remote_track("T", "P", &honest(), None));
    env.cloud.accept(runaway());
    let first = env.engine.pump_pull();
    assert_eq!(first.quarantined, 1);
    assert_eq!(first.failed_records, 1);
    assert!(track_name_and_stamp(&env).is_none());
    assert_eq!(env.parked().len(), 2);

    parent_lands_then_clock_catches_up(&env);
}

#[test]
fn a_batch_internal_orphan_whose_parent_arrives_on_the_next_page_converges_within_the_run() {
    // The sweep's original design case. A peer without the parent-first push
    // sort can upload child-first; with one row per page the child lands
    // alone on page 1.
    let env = Env::new();
    env.cloud
        .accept(remote_track("T", "P", &remote_hlc(T0 - 2000), None));
    env.cloud
        .accept(remote_practice("P", "parent", &remote_hlc(T0 - 1000), None));
    env.transport.set_page_size(1);

    let run = env.engine.pump_pull();
    assert!(!run.failed());
    // Page 1 parked the child (fk-orphan) and advanced the token past it;
    // page 2 delivered the parent. The server does NOT re-send the child (its
    // page is behind the token), so what lands it is the END-OF-RUN quarantine
    // replay: the parked child now finds its parent, applies, and its entry
    // settles.
    assert_eq!(env.practice_title("P").as_deref(), Some("parent"));
    assert!(
        exists(&env.db, "tracks", &id("T")),
        "the run itself converged the orphan"
    );
    assert!(env.parked().is_empty());
}

#[test]
fn replayed_rows_are_announced_via_remote_applied() {
    // A recovered backlog must refresh open screens, not land invisibly.
    // Regression: the replay used to apply rows without contributing to the
    // run's announcements, so the primary recovery scenario — a whole
    // first-pull backlog parked by a slow clock, then un-parked by a corrected
    // clock in a run that fetches NOTHING new — landed an entire library with
    // zero RemoteApplied events and no UI refresh.
    let env = Env::new();
    env.cloud
        .accept(remote_practice("P1", "one", &remote_hlc(T0), None));
    env.cloud
        .accept(remote_practice("P2", "two", &remote_hlc(T0 + 10), None));
    env.now.set(T0 - 10 * 60 * 1000); // slow clock: both rows park
    env.engine.pump_pull();
    assert!(
        env.events().is_empty(),
        "nothing applied, nothing announced"
    );

    env.now.set(T0 + 1000); // clock corrected; the cloud has nothing new
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(env.practice_title("P1").as_deref(), Some("one"));
    let events = env.events();
    assert_eq!(events.len(), 1);
    let ids: std::collections::HashSet<String> = events[0].practice_ids.iter().cloned().collect();
    assert_eq!(ids, [id("P1"), id("P2")].into_iter().collect());
    assert!(events[0].touches_table("practices"));

    // A pull with nothing parked and nothing fetched stays silent.
    env.events.lock().clear();
    env.engine.pump_pull();
    assert!(env.events().is_empty());
}

#[test]
fn a_still_rejected_record_keeps_its_entry_parked() {
    // Replay is a bounded retry, never a drop.
    let env = Env::new();
    env.cloud.accept(remote_practice(
        "FUT",
        "poison",
        &remote_hlc(T0 + 6 * 60 * 1000),
        None,
    ));
    env.engine.pump_pull();
    assert_eq!(env.parked().len(), 1);
    // The clock has NOT recovered: every later pull replays, re-rejects, and
    // re-parks the same single entry — no growth, no loss.
    env.engine.pump_pull();
    env.engine.pump_pull();
    let parked = env.parked();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].id, id("FUT"));
    assert_eq!(env.practice_title("FUT"), None);
}

// --- end-of-run repair vs a parked parent ----------------------------------------
// The checker never judges a row against a state the quarantine already
// supersedes. Device D deletes track T (cascading a tombstone onto its clip
// C); device B never saw that delete and, concurrently, edits C and T. Both of
// B's edits out-rank D's delete under LWW, so the converged state is T live +
// C live. Every test below delivers B's T edit in a way this device PARKS,
// while B's C edit applies live under the (still locally tombstoned) T.

fn by_d(wall: i64) -> Hlc {
    Hlc::new(wall, 0, id("DELETER"))
}

fn by_b(wall: i64) -> Hlc {
    Hlc::new(wall, 0, id("EDITOR"))
}

fn deleted_at_of(env: &Env, table: &str, row_id: &str) -> Option<i64> {
    col::<Option<i64>>(&env.db, table, &id(row_id), "deleted_at").unwrap()
}

/// Shared history, pulled: P, X, T and C created, then D's delete.
fn pull_deleted_track(env: &Env) {
    env.cloud
        .accept(remote_asset("X", "HASH", &by_d(T0 - 100000)));
    env.cloud
        .accept(remote_practice("P", "song", &by_d(T0 - 100000), None));
    env.cloud
        .accept(remote_track("T", "P", &by_d(T0 - 90000), None));
    env.cloud
        .accept(remote_clip("C", "T", "X", &by_d(T0 - 90000), None, 0));
    env.cloud
        .accept(remote_track("T", "P", &by_d(T0 - 60000), Some(T0 - 60000)));
    env.cloud.accept(remote_clip(
        "C",
        "T",
        "X",
        &by_d(T0 - 60000),
        Some(T0 - 60000),
        0,
    ));
    assert!(!env.engine.pump_pull().failed());
    assert!(deleted_at_of(env, "tracks", "T").is_some());
    assert!(deleted_at_of(env, "clips", "C").is_some());
}

/// B's clip edit has APPLIED live under the locally tombstoned T, and the
/// end-of-run repair left it alone: nothing journaled for C, and a push puts
/// no bookkeeping tombstone on the server.
fn expect_clip_deferred(env: &Env, clip_edit: &Hlc) {
    assert_eq!(
        col::<String>(&env.db, "clips", &id("C"), "hlc_packed"),
        Some(clip_edit.packed())
    );
    assert_eq!(
        deleted_at_of(env, "clips", "C"),
        None,
        "the parent's NEWER state is parked: the clip is pending, not orphaned, and must not be \
         tombstoned on a superseded verdict"
    );
    assert_eq!(env.oplog_count(), 0, "no repair was journaled");
    assert!(!env.engine.pump_push().failed());
    assert_eq!(
        env.cloud
            .server_row_for("clips", &id("C"))
            .unwrap()
            .field("deleted_at"),
        Some(&NULL),
        "the fleet must not receive a bookkeeping tombstone for C"
    );
}

/// The converged state once the parked parent has landed.
fn expect_both_live(env: &Env) {
    assert_eq!(deleted_at_of(env, "tracks", "T"), None);
    assert_eq!(
        deleted_at_of(env, "clips", "C"),
        None,
        "B's clip edit won LWW over D's delete"
    );
    assert!(env.parked().is_empty());
    assert!(env.checker().check().unwrap().is_empty());
}

#[test]
fn a_parent_edit_parked_at_the_drift_gate_defers_the_child_s_repair() {
    // Once the clock catches up both land live everywhere.
    let env = Env::new();
    pull_deleted_track(&env);
    // B's clock runs ~6 min ahead of this device: its LATER track edit is
    // past the 5-minute drift bound, its earlier clip edit is not.
    let clip_edit = by_b(T0 - 1000);
    env.cloud
        .accept(remote_track("T", "P", &by_b(T0 + 6 * 60 * 1000), None));
    env.cloud
        .accept(remote_clip("C", "T", "X", &clip_edit, None, 500));
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(run.quarantined, 1, "T@B parked at the drift gate");
    expect_clip_deferred(&env, &clip_edit);

    env.now.add(2 * 60 * 1000); // this device's clock catches up
    assert!(!env.engine.pump_pull().failed());
    expect_both_live(&env);
}

#[test]
fn a_parent_edit_parked_on_its_schema_defers_the_child_s_repair_until_the_upgrade_lands_it() {
    let env = Env::new();
    pull_deleted_track(&env);
    // An unknown column stands in for a newer build's schema (the same
    // upgrade seam the quarantine-replay tests use).
    let track_edit = remote_track("T", "P", &by_b(T0 - 2000), None);
    env.cloud
        .accept(with(&track_edit, &[("brand_new_column", 7i64.into())]));
    let clip_edit = by_b(T0 - 1000);
    env.cloud
        .accept(remote_clip("C", "T", "X", &clip_edit, None, 500));
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(run.failed_records, 1, "T@B parked on its schema");
    expect_clip_deferred(&env, &clip_edit);

    env.db
        .execute_batch("ALTER TABLE tracks ADD COLUMN brand_new_column INTEGER")
        .unwrap();
    assert!(!env.engine.pump_pull().failed());
    expect_both_live(&env);
}

#[test]
fn a_parent_edit_the_fk_sweep_parks_and_restores_to_its_tombstone_defers_the_child_s_repair() {
    // ...until the parent lands. B routed T into a new bus Y. Y is parked
    // here on its schema, so B's T edit dangles: the sweep parks it
    // (fk-orphan) and restores T to D's tombstone — the table again holds a
    // superseded parent state.
    let env = Env::new();
    pull_deleted_track(&env);
    let bus = SyncRow::new(
        "fx_buses",
        merged(
            fields! {
                "id" => id("Y"),
                "practice_id" => id("P"),
                "name" => "verb",
                "color_value" => NULL,
                "volume" => 1.0,
                "pan" => 0.0,
                "muted" => 0i64,
                "soloed" => 0i64,
                "output_bus_id" => NULL,
                "fx_json" => NULL,
                "sends_json" => NULL,
                "order_index" => 0i64,
                "brand_new_column" => 7i64,
            },
            sync_cols(&by_b(T0 - 3000), None),
        ),
    );
    env.cloud.accept(bus);
    let track_edit = remote_track("T", "P", &by_b(T0 - 2000), None);
    env.cloud
        .accept(with(&track_edit, &[("bus_id", id("Y").into())]));
    let clip_edit = by_b(T0 - 1000);
    env.cloud
        .accept(remote_clip("C", "T", "X", &clip_edit, None, 500));
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    let parked: HashMap<String, String> = env
        .parked()
        .into_iter()
        .map(|q| (q.tbl, q.reason))
        .collect();
    assert!(parked["tracks"].starts_with("fk-orphan"), "{parked:?}");
    assert!(deleted_at_of(&env, "tracks", "T").is_some());
    expect_clip_deferred(&env, &clip_edit);

    env.db
        .execute_batch("ALTER TABLE fx_buses ADD COLUMN brand_new_column INTEGER")
        .unwrap();
    assert!(!env.engine.pump_pull().failed());
    expect_both_live(&env);
    assert_eq!(
        col::<Option<String>>(&env.db, "tracks", &id("T"), "bus_id"),
        Some(Some(id("Y")))
    );
}

#[test]
fn a_parked_parent_state_that_settles_as_an_lww_skip_ends_the_deferral() {
    // That run repairs even though it applied nothing.
    let env = Env::new();
    pull_deleted_track(&env);
    let track_edit = remote_track("T", "P", &by_b(T0 - 2000), None);
    env.cloud
        .accept(with(&track_edit, &[("brand_new_column", 7i64.into())]));
    let clip_edit = by_b(T0 - 1000);
    env.cloud
        .accept(remote_clip("C", "T", "X", &clip_edit, None, 500));
    env.engine.pump_pull();
    assert_eq!(deleted_at_of(&env, "clips", "C"), None);

    // A local write supersedes the parked state while T stays tombstoned
    // (stand-in: a re-stamp of T through the writer chokepoint). The door
    // park ratcheted the clock, so the local stamp out-ranks T@B.
    let s = env.writer.stamp();
    assert!(s.hlc > by_b(T0 - 2000));
    env.db
        .transaction(|| -> Result<()> {
            env.db.execute(
                "UPDATE tracks SET updated_at = ?, hlc_wall = ?, hlc_ctr = ?, hlc_node = ?, \
                 hlc_packed = ? WHERE id = ?",
                rusqlite::params![
                    s.at(),
                    s.hlc.wall_ms,
                    s.hlc.counter,
                    s.hlc.node,
                    s.hlc.packed(),
                    id("T")
                ],
            )?;
            env.writer
                .journal("tracks", &id("T"), "tombstone", &s.hlc, "", None)
        })
        .unwrap();

    // Nothing new on the server: the run's only event is the replay settling
    // T@B as a skip. The clip's verdict is no longer deferred — its parent's
    // newest state IS the tombstone — so this run repairs.
    let run = env.engine.pump_pull();
    assert!(!run.failed());
    assert_eq!(run.applied, 0);
    assert!(env.parked().is_empty(), "T@B settled");
    assert!(
        deleted_at_of(&env, "clips", "C").is_some(),
        "the deferred verdict was re-judged when its park settled"
    );
    assert!(env.checker().check().unwrap().is_empty());
}

// --- a pointer whose target has not arrived yet is pending, not dangling -----
// The pusher uploads its backlog in drain windows and chunks, and boot
// compaction leaves each row only its latest op, so a row can reach the server
// one request before the row its FK-less pointer names. A puller that catches
// up in between must not "heal" that pointer: the derived null out-ranks the
// author's own row and would reach every device.

fn by_a(wall: i64) -> Hlc {
    Hlc::new(wall, 0, id("AUTHOR"))
}

fn practice_with_active(interp_id: &str, h: &Hlc) -> SyncRow {
    with(
        &remote_practice("P", "song", h, None),
        &[("active_interpretation_id", id(interp_id).into())],
    )
}

fn interp(row_id: &str, h: &Hlc) -> SyncRow {
    SyncRow::new(
        "interpretations",
        merged(
            fields! {
                "id" => id(row_id),
                "practice_id" => id("P"),
                "name" => "take 2",
                "order_index" => 0i64,
            },
            sync_cols(h, None),
        ),
    )
}

fn bus(row_id: &str, h: &Hlc, output_bus_id: Option<&str>) -> SyncRow {
    SyncRow::new(
        "fx_buses",
        merged(
            fields! {
                "id" => id(row_id),
                "practice_id" => id("P"),
                "name" => row_id,
                "color_value" => NULL,
                "volume" => 1.0,
                "pan" => 0.0,
                "muted" => 0i64,
                "soloed" => 0i64,
                "output_bus_id" => output_bus_id.map(id),
                "fx_json" => NULL,
                "sends_json" => NULL,
                "order_index" => 0i64,
            },
            sync_cols(h, None),
        ),
    )
}

#[test]
fn an_active_interpretation_that_lands_on_a_later_upload_keeps_the_practice_pointing_at_it() {
    // Nothing journaled, nothing to redo.
    let env = Env::new();
    env.cloud
        .accept(practice_with_active("I", &by_a(T0 - 2000)));
    assert!(!env.engine.pump_pull().failed());
    let active = || {
        col::<Option<String>>(&env.db, "practices", &id("P"), "active_interpretation_id").flatten()
    };
    assert_eq!(active(), Some(id("I")));
    assert_eq!(env.oplog_count(), 0, "no heal may be journaled");

    env.cloud.accept(interp("I", &by_a(T0 - 1000)));
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(active(), Some(id("I")));
    assert_eq!(env.oplog_count(), 0);
    assert!(env.checker().check().unwrap().is_empty());
}

#[test]
fn a_bus_route_to_a_bus_that_lands_on_a_later_upload_survives() {
    let env = Env::new();
    env.cloud
        .accept(remote_practice("P", "song", &by_a(T0 - 3000), None));
    env.cloud.accept(bus("X", &by_a(T0 - 2000), Some("Y")));
    assert!(!env.engine.pump_pull().failed());
    let route = || col::<Option<String>>(&env.db, "fx_buses", &id("X"), "output_bus_id").flatten();
    assert_eq!(route(), Some(id("Y")));
    assert_eq!(env.oplog_count(), 0, "no heal may be journaled");

    env.cloud.accept(bus("Y", &by_a(T0 - 1000), None));
    assert!(!env.engine.pump_pull().failed());
    assert_eq!(route(), Some(id("Y")));
    assert_eq!(env.oplog_count(), 0);
    assert!(env.checker().check().unwrap().is_empty());
}

// --- poke wiring -------------------------------------------------------------------

#[test]
fn pokes_fire_only_after_a_transaction_completes_and_a_rolled_back_write_drains_to_a_no_op() {
    let mut conn = Connection::open_in_memory().unwrap();
    let inserts = OplogInserts::install(&mut conn).unwrap();
    let mut env = Env::over(Arc::new(AppDatabase::from_connection(conn).unwrap()));
    let (timers, created) = fake_timers();
    env.engine = Arc::new(env.build_engine(Some(timers), None, None));
    env.engine.start(&inserts);

    // A rolled-back transaction: the notifier announces its insert only once
    // the transaction has ENDED (here: rolled back) — never from inside it.
    // The resulting poke is harmless: the journal entry rolled back with the
    // data, so the pump pushes nothing.
    let mut pokes_inside = None;
    let rolled_back = env.db.transaction(|| -> Result<()> {
        let s = env.writer.stamp();
        env.writer
            .journal("practices", &id("GONE"), "insert", &s.hlc, "h", None)?;
        pokes_inside = Some(created.lock().len());
        Err(Error::State("roll it back".into()))
    });
    assert!(matches!(rolled_back, Err(Error::State(_))));
    assert_eq!(pokes_inside, Some(0), "no poke inside an open txn");
    let fired: Vec<Arc<FakeTimer>> = created.lock().clone();
    for t in fired {
        t.fire();
    }
    assert_eq!(
        env.transport.push_calls(),
        0,
        "nothing survived the rollback"
    );
    assert!(env.cloud.log().is_empty());

    // A committed save pokes: quiet + max-wait timers are armed.
    let before = created.lock().len();
    env.save_practice("P", "one", None);
    let timers: Vec<Arc<FakeTimer>> = created.lock().clone();
    assert_eq!(timers.len(), before + 2);
    assert_eq!(timers[before].duration, Duration::from_secs(2));
    assert_eq!(timers[before + 1].duration, Duration::from_secs(30));

    // Firing the quiet timer runs the push.
    timers[before].fire();
    assert!(env.cloud.server_row_for("practices", &id("P")).is_some());
    assert_eq!(env.oplog_count(), 0);

    // After dispose nothing is scheduled any more.
    env.engine.dispose();
    let after = created.lock().len();
    env.save_practice("Q", "two", None);
    assert_eq!(
        created.lock().len(),
        after,
        "a disposed engine schedules nothing"
    );
}

#[test]
fn the_real_timer_debounces_into_a_push() {
    // The production timer factory: a poke pushes after the quiet window.
    let mut env = Env::new();
    let engine = SyncEngine::new(
        SyncEngineParts {
            db: env.db.clone(),
            applier: env.engine.applier().clone(),
            drain: OplogDrain::new(env.db.clone()),
            builder: Arc::new(SyncRowBuilder::new(env.db.clone())),
            state: env.state.clone(),
            integrity: Arc::new(env.checker()),
            transport: env.transport.clone(),
        },
        SyncEngineOptions {
            quiet_window: Duration::from_millis(20),
            max_wait: Duration::from_millis(500),
            ..SyncEngineOptions::default()
        },
    );
    env.engine = Arc::new(engine);
    env.save_practice("P", "one", None);
    env.engine.poke();
    wait_until("the debounced push", || {
        env.cloud.server_row_for("practices", &id("P")).is_some()
    });
    wait_until("the ack", || env.oplog_count() == 0);
}

// --- undecodable records -------------------------------------------------------

#[test]
fn a_record_that_never_decoded_is_parked_as_a_dead_letter_and_the_page_still_applies() {
    // Such a record has no id, table or stamp to route by, so the applier's
    // per-record quarantine cannot reach it: a transport was left choosing
    // between failing the whole page (one bad record bricks sync) and
    // dropping it silently while the token advanced past it.
    let env = Env::new();
    env.cloud
        .accept(remote_practice("GOOD", "kept", &remote_hlc(T0), None));
    env.transport
        .add_pending_reject(RejectedRecord::new("{\"not\":\"a row\"}", "missing id"));

    let run = env.engine.pump_pull();

    assert_eq!(run.applied, 1, "the good record still lands");
    assert_eq!(run.failed_records, 1);
    assert!(env.state.change_token().unwrap().is_some(), "not stalled");
    let parked = env.parked();
    assert_eq!(parked.len(), 1);
    assert!(parked[0].reason.contains("undecodable"));
    assert!(parked[0].payload.contains("not"));
    // Never re-applied: the payload is deliberately not a SyncRow envelope.
    assert!(SyncRow::decode(&parked[0].payload).is_err());
}

// --- run_exclusive ---------------------------------------------------------------

#[test]
fn run_exclusive_shares_the_pumps_serial_section() {
    let env = Env::new();
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    env.transport.set_hold_pushes(true);
    env.save_practice("P", "one", None);
    let engine = env.engine.clone();
    let push = std::thread::spawn(move || engine.pump_push());
    wait_until("the push in flight", || {
        env.transport.held_push_count() == 1
    });
    let engine = env.engine.clone();
    let o = order.clone();
    let exclusive = std::thread::spawn(move || engine.run_exclusive(|| o.lock().push("exclusive")));
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        order.lock().is_empty(),
        "the exclusive op waits for the run"
    );
    order.lock().push("push released");
    env.transport.release_held_pushes();
    assert!(!push.join().unwrap().failed());
    exclusive.join().unwrap();
    assert_eq!(*order.lock(), ["push released", "exclusive"]);
}
