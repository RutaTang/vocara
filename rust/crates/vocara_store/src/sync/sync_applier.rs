//! The remote door: applies records pulled from sync into the local store
//! (port of `lib/data/sync/sync_applier.dart`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use vocara_model::util::dart_json;

use crate::dart;
use crate::db::rows::OplogRow;
use crate::db::AppDatabase;
use crate::error::{Error, Result};
use crate::sync::hlc::{Hlc, HlcClock};
use crate::sync::quarantine_store::{QuarantineEntry, QuarantineStore};
use crate::sync::sync_row::{
    check_column_types, inbound_per_device_overrides, per_device_columns, synced_table, FieldValue,
    Fields, SyncRow, SyncRowBuilder, CURRENT_RECORD_SCHEMA_V,
};
use crate::sync::sync_state_store::SyncStateStore;
use crate::sync::sync_transport::RejectedRecord;
use crate::sync::sync_writer::op;

/// A row pulled from the cloud, ready to apply: the table name plus every
/// synced column (including the HLC quad, deleted_at and schema_v) as a
/// value map. Unified with the wire format: the applier accepts any
/// [`SyncRow`] straight off the transport.
pub type RemoteRecord = SyncRow;

/// The counters of one [`SyncApplier::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// Records that won LWW and were written.
    pub applied: usize,
    /// Records that lost the LWW comparison (we already hold something
    /// newer).
    pub skipped: usize,
    /// Records with an implausibly-future clock; not applied.
    pub quarantined: usize,
    /// Rejected while applying: an unknown table/column, a newer schema_v,
    /// or a failure inside the record's savepoint (malformed /
    /// forward-incompatible row).
    pub failed: usize,
    /// Whether the batch's OUTER transaction committed. False means the
    /// whole batch rolled back (nothing was applied, quarantined-to-store,
    /// or written by the `before_commit` hook) and the caller must retry the
    /// page — this is how the sync engine knows its change token did NOT
    /// advance.
    pub committed: bool,
}

impl Default for ApplyResult {
    fn default() -> Self {
        ApplyResult {
            applied: 0,
            skipped: 0,
            quarantined: 0,
            failed: 0,
            committed: true,
        }
    }
}

impl std::fmt::Display for ApplyResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ApplyResult(applied: {}, skipped: {}, quarantined: {}, failed: {}, committed: {})",
            self.applied, self.skipped, self.quarantined, self.failed, self.committed
        )
    }
}

/// The optional arguments of [`SyncApplier::apply_with`] (Dart's named
/// parameters).
#[derive(Default)]
pub struct ApplyOptions<'a> {
    /// Runs INSIDE the batch's outer transaction as its very last step
    /// (after every record and the FK sweep), so whatever it writes commits
    /// atomically with the batch. This is the sync engine's change-token
    /// hook: the token it covers advances IFF the batch committed. A failing
    /// hook rolls the WHOLE batch back ([`ApplyResult::committed`] false) —
    /// deliberately, since applying records whose token write failed would
    /// re-apply the page forever.
    pub before_commit: Option<&'a dyn Fn() -> Result<()>>,
    /// Records of the same page that never DECODED into a row (see
    /// [`RejectedRecord`]). They are parked as dead letters inside this
    /// batch's transaction, which is what makes them atomic with the
    /// caller's change-token write: parking them afterwards would lose them
    /// to a crash in between, and the server never re-delivers a record the
    /// token passed.
    pub rejects: &'a [RejectedRecord],
    /// Limits the pre-commit FK sweep to the rows this batch wrote, instead
    /// of the whole store. The quarantine replay applies one parked row per
    /// batch, and a whole-store scan for each made a replay of K rows into a
    /// store of N cost K x N (a thousand rows parked by a slow clock: a
    /// thousand scans, growing with the library). Pull pages keep the
    /// whole-store scan, which also catches a dangling row no batch wrote.
    pub own_rows_only: bool,
}

/// A per-record failure inside [`SyncApplier::apply`].
enum RecordError {
    /// A record failed to PARK (Dart's `_QuarantinePutException`): unlike
    /// every other per-record failure (which quarantines that record and
    /// continues), this one must fail the WHOLE batch — re-parking through
    /// the same failing store cannot succeed, and committing would advance
    /// the change token past a record that was never parked.
    QuarantinePut(Error),
    /// Any other per-record failure.
    Failed(Error),
}

impl From<Error> for RecordError {
    fn from(e: Error) -> Self {
        RecordError::Failed(e)
    }
}

impl RecordError {
    /// The batch-level error a park failure becomes.
    fn into_batch_error(self) -> Error {
        match self {
            RecordError::QuarantinePut(e) => Error::State(format!("quarantine put failed: {e}")),
            RecordError::Failed(e) => e,
        }
    }
}

/// One occupant a slot resolution tombstoned, held as an atomic revert
/// PAIR: the oplog rows exactly THIS resolution journaled, and the
/// occupant's verbatim restore point. The revert either applies both halves
/// (journal deleted + snapshot restored) or neither (the occupant stays
/// tombstoned WITH its journal rows — an accounted tombstone peers still
/// learn) — never the ghost in between, a tombstone whose ledger entry was
/// deleted and that therefore replicates nowhere.
#[derive(Debug, Clone)]
struct OccupantRevert {
    /// The occupant's full wire snapshot captured BEFORE the resolution
    /// tombstoned it: the verbatim restore point, its own prior HLC
    /// included.
    snapshot: SyncRow,
    /// The derived stamp the resolution wrote INTO the occupant's row; the
    /// pair is restorable only while the row still carries it.
    stamp_packed: String,
    /// Oplog seqs of exactly this resolution's journal rows.
    oplog_seqs: Vec<i64>,
}

/// The side effects ONE row's slot resolution(s) wrote during a batch,
/// remembered by [`SyncApplier::apply`] per (table, id) so the FK sweep can
/// revert them if the record that triggered them is later parked — the
/// resolution ran BEFORE the record's own application, so "quarantined at
/// the door" must undo it too (delete its oplog entries, restore the
/// tombstoned occupant). Several records for the same id can each trigger a
/// resolution within one batch; their effects are merged via
/// [`SlotResolution::absorb`], which APPENDS — occupant entries are never
/// folded into one another, because each is an independent revert PAIR the
/// sweep must decide on its own. (Folding used to keep the FIRST occupant's
/// snapshot under the LAST occupant's derived stamp, so a two-occupant
/// revert restored NEITHER occupant while deleting BOTH journal entries —
/// two tracks left tombstoned with their ledger gone, a permanent
/// unaccounted loss.)
#[derive(Debug, Clone)]
struct SlotResolution {
    /// The (latest) incoming record that triggered a resolution for this row
    /// — the parked payload when the row itself holds the resolution's
    /// tombstoned-at-the-door state (see `parkable_snapshot`).
    record: SyncRow,
    /// The derived stamp the (latest) resolution wrote into the INCOMING
    /// record's own row (incoming-loses case): identifies the
    /// tombstoned-at-the-door state so the sweep can park the original
    /// record instead.
    self_stamp_packed: Option<String>,
    /// Oplog seqs of the at-the-door tombstone entries the incoming-loses
    /// resolution(s) journaled for the record's OWN row. Deleted
    /// unconditionally with the park: the sweep just deleted/restored the
    /// row itself and parks the ORIGINAL record in place of the derived
    /// state, so these entries would journal a decision minted for a write
    /// that never landed.
    self_oplog_seqs: Vec<i64>,
    /// One revert pair per occupant this row's resolution(s) tombstoned, in
    /// resolution order (earliest first).
    occupants: Vec<OccupantRevert>,
}

impl SlotResolution {
    fn new(record: SyncRow) -> Self {
        SlotResolution {
            record,
            self_stamp_packed: None,
            self_oplog_seqs: Vec::new(),
            occupants: Vec::new(),
        }
    }

    /// Merges a later resolution's effects for the same row into this one.
    fn absorb(&mut self, other: SlotResolution) {
        self.record = other.record;
        if other.self_stamp_packed.is_some() {
            self.self_stamp_packed = other.self_stamp_packed;
        }
        self.self_oplog_seqs.extend(other.self_oplog_seqs);
        self.occupants.extend(other.occupants);
    }
}

type RowKey = (String, String);

/// What one batch tracks across its records, for the FK sweep and the
/// post-commit clock advance.
#[derive(Default)]
struct Batch {
    /// (table, id) pairs INSERTED by this batch — the only rows the FK sweep
    /// is allowed to park-and-delete.
    inserted: IndexSet<RowKey>,
    /// (table, id) -> the PRE-BATCH wire snapshot of every pre-existing row
    /// this batch updated, captured just before the first update overwrote
    /// it. The FK sweep restores from these when the update itself planted a
    /// dangling pointer. Costs one extra row read per applied update — the
    /// price of never nulling a reference that a parked parent could still
    /// satisfy.
    updated: IndexMap<RowKey, SyncRow>,
    /// (table, id) -> the slot-resolution side effects the record(s) for
    /// that row triggered while applying. The FK sweep REVERTS these when it
    /// parks the row: a record that is quarantined at the door must not
    /// leave behind a tombstone (and its oplog entry) minted on its behalf.
    slot_resolutions: IndexMap<RowKey, SlotResolution>,
    /// Per-batch live-schema cache for the column gate.
    live_columns: HashMap<String, IndexSet<String>>,
    /// Stamps this batch ACCEPTED: records whose savepoint settled as
    /// applied or skipped, plus parked records (door gates, replayable
    /// savepoint failures) whose plausible stamp was noted. Their
    /// state-changing merge runs only once the outer transaction has
    /// committed — an advance for records the outer commit then rolled back
    /// would be a side effect surviving the rollback.
    accepted_hlcs: Vec<Hlc>,
}

/// One `PRAGMA foreign_key_check` row.
struct Violation {
    table: String,
    rowid: i64,
    parent: String,
    fkid: Option<i64>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Drift's `getSingleOrNull`: None for no row, the row for one, an error
/// for more.
fn single_or_none<T>(mut rows: Vec<T>) -> Result<Option<T>> {
    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.pop()),
        _ => Err(Error::State("Too many elements".to_string())),
    }
}

/// The message of a format error (Dart's `FormatException.message`).
fn message_of(e: &Error) -> String {
    match e {
        Error::Format(m) => m.clone(),
        other => other.to_string(),
    }
}

/// The "remote door": applies records pulled from sync into the local store.
///
/// Distinct from local writes (which go through SyncWriter): a remote change
/// ADOPTS the remote HLC (it does not tick), wins only by entity-level
/// Last-Writer-Wins on the HLC, and is NEVER journaled — so an applied
/// remote change can't be re-pushed into an infinite ping-pong. (The single
/// carve-out is the single-slot RESOLUTION tombstone, which is a new derived
/// local decision — not an echo of the applied record — and must replicate
/// to converge; it is bounded to one write per losing row, see
/// `resolve_slot`. The carve-out is REVERTIBLE: if the FK sweep parks the
/// very record whose application minted it, the tombstone and its journal
/// entry are undone with the park, so no track dies for a record that never
/// landed — see `revert_slot_resolution`.)
/// The clock is advanced so subsequent LOCAL writes stay causally after
/// everything we've seen; a record whose clock is implausibly far in the
/// future is quarantined instead of merged (one bad peer can't poison the
/// fleet). The gate and the advance are split deliberately:
/// [`HlcClock::assert_plausible`] (check-only, same drift rule) runs with
/// the other door gates, and the state-changing
/// [`HlcClock::merge_verified`] calls run only AFTER the batch's outer
/// transaction has committed, for the records whose savepoint settled as
/// applied or skipped AND for the plausible stamps of records the door gates
/// PARKED, or that a savepoint failure parked REPLAYABLE (see
/// `note_parked_stamp`: a parked snapshot stays eligible to win LWW at
/// replay, so later local writes must stamp above it) — a record that fails
/// inside its savepoint deterministically, and equally a whole batch whose
/// outer commit fails (an FK-sweep abort, a failing before_commit hook),
/// must leave NO side effect behind, and an in-memory clock advanced for
/// writes the database rolled back is a side effect. The post-commit
/// advance is merge_verified rather than [`HlcClock::receive`] because
/// re-running the drift check after the commit could fail for stamps that
/// are already durable (see the bottom of [`SyncApplier::apply_with`]).
///
/// Apply is order-tolerant and per-record resilient: defer_foreign_keys lets
/// a child arrive before its parent within the batch, and each record
/// applies inside its own SAVEPOINT so one malformed / forward-incompatible
/// record is quarantined ([`ApplyResult::failed`]) rather than rolling back
/// every other record. Because defer_foreign_keys pushes FK enforcement to
/// the OUTER commit (outside every savepoint), a record whose parent never
/// arrives in the batch would fail the whole commit — so before committing
/// we sweep foreign_key_check (`sweep_fk_orphans`), healing by provenance: a
/// row INSERTED by this batch is PARKED to the quarantine store and then
/// deleted (the parent may be locally quarantined — e.g. a newer-schema row
/// the schema gate rejected — in which case the server never re-delivers
/// the child, and only the quarantine replay can land it once the parent
/// exists); a pre-existing row UPDATED by this batch is parked the same way
/// and then RESTORED verbatim to its pre-batch snapshot (the update was
/// effectively quarantined at the door); a dangling pre-existing row this
/// batch never touched only has its dangling pointer column nulled. The
/// outer transaction is wrapped so a commit failure degrades to a failed
/// batch instead of failing out of apply().
///
/// Thread-safe: a batch holds the database for its whole outer transaction
/// (see `AppDatabase::transaction`), so two batches never interleave.
#[derive(Debug)]
pub struct SyncApplier {
    db: Arc<AppDatabase>,
    clock: Arc<HlcClock>,
    /// Where the durable HLC high-water mark is kept. Written inside the
    /// apply transaction so a restart can restore the monotonicity the
    /// in-memory clock gained from stamps whose records never became rows.
    state: SyncStateStore,
    /// Projects a row back into its full wire snapshot so the FK sweep can
    /// PARK an orphan instead of losing it, and so apply can capture the
    /// pre-update restore point of every row it updates. Deliberately
    /// REQUIRED (unlike the quarantine): an absent projection would silently
    /// degrade the sweep to the old hard-delete, and a locally quarantined
    /// parent then makes that deletion a permanent row loss.
    builder: Arc<SyncRowBuilder>,
    /// Optional collaborator: when present, a rejected record (HLC drift or
    /// a per-record apply failure) is PERSISTED to the quarantine parking
    /// lot inside the same outer transaction, so nothing is silently
    /// dropped. Kept optional so wiring (and tests) without persistence keep
    /// the counter-only behavior unchanged.
    quarantine: Option<QuarantineStore>,
}

impl SyncApplier {
    /// The sentinel table undecodable payloads park under. Not a real synced
    /// table on purpose: the record never told us which table it belonged
    /// to, and inventing one would put a review artifact in a live table's
    /// space.
    pub const DEAD_LETTER_TABLE: &'static str = "!undecoded";

    /// An applier writing into [db], advancing [clock], projecting rows with
    /// [builder], and parking rejects in [quarantine] when one is given.
    pub fn new(
        db: Arc<AppDatabase>,
        clock: Arc<HlcClock>,
        builder: Arc<SyncRowBuilder>,
        quarantine: Option<QuarantineStore>,
    ) -> Self {
        SyncApplier {
            state: SyncStateStore::new(db.clone()),
            db,
            clock,
            builder,
            quarantine,
        }
    }

    /// The device clock.
    pub fn clock(&self) -> &Arc<HlcClock> {
        &self.clock
    }

    /// The quarantine parking lot, when wired.
    pub fn quarantine(&self) -> Option<&QuarantineStore> {
        self.quarantine.as_ref()
    }

    /// The database it writes into.
    pub fn db(&self) -> &Arc<AppDatabase> {
        &self.db
    }

    /// Applies one batch of remote records in ONE outer transaction (see
    /// [`SyncApplier::apply_with`] for the options).
    pub fn apply(&self, records: &[SyncRow]) -> ApplyResult {
        self.apply_with(records, ApplyOptions::default())
    }

    /// Applies one batch of remote records in ONE outer transaction; see
    /// [`ApplyOptions`] for the before-commit hook, the page's undecodable
    /// rejects and the own-rows-only FK sweep. Never fails: a batch that
    /// could not commit reports [`ApplyResult::committed`] false.
    pub fn apply_with(&self, records: &[SyncRow], options: ApplyOptions<'_>) -> ApplyResult {
        let mut result = ApplyResult::default();
        let mut batch = Batch::default();
        let outcome: Result<()> = self.db.transaction(|| {
            self.db.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            for rec in records {
                // EVERYTHING a single record does is wrapped so that ANY
                // per-record failure — not just the enumerated gates below —
                // quarantines that record and lets the loop continue. Before
                // this, only the drift error was caught around the clock
                // receive; anything else (a mistyped HLC field, a DB error)
                // escaped the loop, failed the whole page, and the engine
                // retried the SAME page forever ("token unmoved") — one bad
                // record bricked sync and blocked every good record behind
                // it.
                match self.apply_record(rec, &mut result, &mut batch) {
                    Ok(()) => {}
                    // A record failed to PARK: the loop's usual answer to a
                    // per-record failure — quarantine it and move on — is
                    // exactly the thing that just failed. Fail so the outer
                    // transaction rolls back (token unmoved, page retried)
                    // instead of committing a batch that silently lost a
                    // rejected record (see `quarantine_record`).
                    Err(e @ RecordError::QuarantinePut(_)) => return Err(e.into_batch_error()),
                    Err(RecordError::Failed(e)) => {
                        // Any other per-record failure. If the record was
                        // inside its savepoint, the savepoint rolled back
                        // BEFORE this point, so by the time this quarantine
                        // write runs the record's partial writes are gone
                        // while the quarantine row (written in the OUTER
                        // transaction) survives with the rest of the batch.
                        result.failed += 1;
                        let reason = Self::savepoint_failure_reason(&e);
                        self.quarantine_record(rec, &reason)
                            .map_err(RecordError::into_batch_error)?;
                        // The savepoint rolled the record's writes back, but
                        // a REPLAYABLE park (apply-failed) still leaves its
                        // snapshot behind, eligible to win LWW at replay — so
                        // its stamp ratchets the clock and the durable
                        // high-water mark exactly like a door park's (see
                        // `note_parked_stamp`). A deterministic failure
                        // (invalid-value) can never land, so it keeps the
                        // class rule that a savepoint failure leaves no side
                        // effect.
                        if QuarantineStore::is_replayable(&reason) {
                            self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
                        }
                    }
                }
            }
            for reject in options.rejects {
                result.failed += 1;
                self.park_undecodable(reject)
                    .map_err(RecordError::into_batch_error)?;
            }
            // foreign_key_check scans the WHOLE store, but only a row this
            // batch wrote can dangle: every earlier commit left the store
            // FK-clean (foreign_keys is ON, and deferred checks still run at
            // commit). So a batch that only parked or skipped — every
            // quarantine replay that re-parks — skips the scan.
            // result.applied counts exactly the records whose savepoint wrote
            // (slot resolutions ride inside them). The quarantine replay
            // narrows the scan to those rows (own_rows_only).
            if result.applied > 0 {
                self.sweep_fk_orphans(&mut result, &mut batch, options.own_rows_only)?;
            }
            // Durably record what the in-memory clock is about to merge,
            // inside this transaction so the mark can never claim stamps a
            // rollback threw away. The advance itself still happens only
            // post-commit; this is the part that has to survive a restart,
            // because a stamp whose record the FK sweep parked leaves no
            // hlc_packed anywhere for the boot seed to find (see
            // SyncStateStore::HLC_HIGH_WATER_KEY).
            let highest = batch.accepted_hlcs.iter().map(Hlc::packed).reduce(|a, b| {
                if dart::compare_strings(&a, &b).is_ge() {
                    a
                } else {
                    b
                }
            });
            if let Some(highest) = highest {
                self.state.raise_hlc_high_water(&highest)?;
            }
            if let Some(hook) = options.before_commit {
                hook()?;
            }
            Ok(())
        });
        if outcome.is_err() {
            // The outer commit failed despite the per-record savepoints and
            // the FK sweep (e.g. a deferred constraint we can't attribute to a
            // single row, or a failing before_commit hook). The whole
            // transaction rolled back — including any persisted quarantine
            // rows, which is correct: the retry of this page re-quarantines
            // them atomically with whatever does commit. Report the batch as
            // failed rather than failing out of apply(), which would lose the
            // ApplyResult and stall sync on an endless retry of this page.
            result.applied = 0;
            result.skipped = 0;
            result.failed = records.len() - result.quarantined;
            result.committed = false;
        }
        if result.committed {
            // The batch is durably committed: only now advance the clock past
            // every accepted record's stamp, so subsequent local writes stay
            // causally after everything the database actually holds.
            // merge_verified, NOT receive: every stamp here already passed
            // assert_plausible at its gate, and re-running the drift check
            // now could FAIL after the data and the change token are durable
            // — a wall clock rolled back between the gate and this line
            // shrinks the max(now, max_wall_ever_seen) reference
            // (assert_plausible never raises it), turning a committed page
            // into a reported failure. Clamping the stamp instead was
            // rejected: it would corrupt causal order. Stamps of records the
            // FK-orphan sweep later parked-and-deleted merge too,
            // deliberately: HLC semantics advance on any RECEIVED message;
            // row survival is a different layer — and door-parked records,
            // like replayable savepoint parks, join for the same reason (see
            // `note_parked_stamp`). Deferring the advance also means the drift
            // gate judged every record of this batch against the PRE-batch
            // clock reference — that is deliberate and conservative: a record
            // plausible only relative to an earlier in-batch stamp
            // quarantines now and is un-parked by the sync engine's
            // end-of-run quarantine replay, once this commit's merges have
            // raised the reference.
            for hlc in &batch.accepted_hlcs {
                self.clock.merge_verified(hlc);
            }
        }
        result
    }

    /// One record of [`SyncApplier::apply_with`]'s loop: the door gates,
    /// then the savepoint.
    fn apply_record(
        &self,
        rec: &SyncRow,
        result: &mut ApplyResult,
        batch: &mut Batch,
    ) -> std::result::Result<(), RecordError> {
        // STRUCTURAL GATE — before any SQL is built from the record.
        // insert/update (and the LWW SELECT) interpolate rec.table and its
        // field names into SQL text, so both must be vetted against the
        // schema, never trusted off the wire.
        let Some(table_spec) = synced_table(rec.table()) else {
            result.failed += 1;
            self.quarantine_record(rec, &format!("unknown-table: \"{}\"", rec.table()))?;
            self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
            return Ok(());
        };
        let table = table_spec.name;
        // LWW settle — deliberately AHEAD of the schema_v/column gates (the
        // table name above is the only thing this SELECT interpolates, and it
        // is vetted): a record that is not strictly newer than the local row
        // is a clean skip, and since no write will happen its (possibly
        // forward-incompatible) shape never matters. The quarantine replay
        // relies on this order — a parked row that lost LWW meanwhile must
        // SETTLE as a skip (its entry is then removed) instead of being
        // re-parked on its shape forever.
        let held = self.held_stamp(table, rec.id()?)?;
        if let Some(held) = held {
            if dart::compare_strings(rec.hlc_packed()?, &held).is_le() {
                result.skipped += 1;
                return Ok(());
            }
        }
        // Wire schema versions start at 1 — no build has ever written a
        // lower CURRENT_RECORD_SCHEMA_V — so schema_v 0 or negative is never
        // an "older peer": it is a malformed or non-conforming record, and it
        // MUST be stopped here because the two schema gates below bracket it
        // without catching it (the newer-schema gate matches only
        // v > current, the full-row gate only v == current), so a
        // below-range record used to walk between them and apply with
        // missing columns silently accepted. This is an applier gate rather
        // than a stricter SyncRow::from_json: from_json runs at the
        // transport/page layer (a real transport decodes the WHOLE page, so
        // one failure there fails every record on it) and on
        // quarantine-replay decode (where a failure keeps the entry parked
        // forever instead of letting LWW settle it) — only this per-record
        // path can park the one bad record and keep the
        // one-bad-record-never-bricks-the-page invariant.
        let schema_v = rec.schema_v()?;
        if schema_v < 1 {
            result.failed += 1;
            self.quarantine_record(
                rec,
                &format!(
                    "invalid-schema_v: {schema_v} is below 1, the lowest version any build \
                     ever wrote"
                ),
            )?;
            self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
            return Ok(());
        }
        // Entity ids become filesystem path components once applied
        // (`audio/<practiceId>/`, `technique_media/<techniqueId>/`, and the
        // recursive deletes of both), so a hostile id carrying `../` or a
        // separator is a write/delete primitive OUTSIDE the media tree.
        // Parked per record, like every other malformed shape — and like
        // them, deliberately here rather than in from_json, which fails whole
        // pages.
        if let Err(e) = SyncRow::check_id_shapes(table, rec.fields()) {
            result.failed += 1;
            self.quarantine_record(rec, &format!("invalid-id: {}", message_of(&e)))?;
            self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
            return Ok(());
        }
        // A record from a NEWER record-schema version is quarantined before
        // the column gate, keeping the pre-gate behavior (and its
        // apply-failed reason) for genuinely newer peers: its shape is by
        // definition not fully understood here, and the quarantine replay
        // un-parks it after the app upgrade that is.
        if schema_v > CURRENT_RECORD_SCHEMA_V {
            result.failed += 1;
            self.quarantine_record(
                rec,
                &format!(
                    "apply-failed: schema_v {schema_v} is newer than this build understands \
                     ({CURRENT_RECORD_SCHEMA_V})"
                ),
            )?;
            self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
            return Ok(());
        }
        // At an accepted schema_v an unknown column is never legitimate:
        // quarantine it at the door instead of letting the name reach the SQL
        // text below.
        let known = self.live_columns(table, &mut batch.live_columns)?;
        let unknown: Vec<&String> = rec
            .fields()
            .keys()
            .filter(|k| !known.contains(k.as_str()))
            .collect();
        if !unknown.is_empty() {
            result.failed += 1;
            self.quarantine_record(
                rec,
                &format!(
                    "unknown-column: {} not in \"{table}\"",
                    unknown
                        .iter()
                        .map(|c| format!("\"{c}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )?;
            self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
            return Ok(());
        }
        // The mirror gate: at the CURRENT record schema version a record must
        // carry EVERY column of the table (minus the per-device set, which
        // never rides the wire) — the wire contract is full-row snapshots
        // (docs/sync-protocol.md). A partial row that won LWW would advance
        // hlc_packed while keeping STALE local content for its missing
        // columns, and a later genuine full row with an intermediate HLC
        // then loses LWW forever — the row is poisoned, not just briefly
        // wrong. Quarantined like an unknown column: the sender is
        // non-conforming, and the parking lot (not a silent partial write) is
        // where a non-conforming record belongs.
        //
        // Deliberately NOT enforced for schema_v < current (which, past the
        // below-range gate above, means valid older versions only —
        // 1 <= schema_v < current): an older peer legitimately lacks columns
        // this build added, and its full-row snapshot of the OLD shape is
        // still the safest thing to apply. A per-version column registry
        // (required set keyed by schema_v) is the future-proof answer once a
        // v2 exists; until then the live schema — the same source the
        // unknown-column gate reads — IS the v-current column set.
        if schema_v == CURRENT_RECORD_SCHEMA_V {
            let per_device = per_device_columns(table);
            let missing: Vec<&String> = known
                .iter()
                .filter(|c| !per_device.contains(&c.as_str()) && !rec.fields().contains_key(*c))
                .collect();
            if !missing.is_empty() {
                result.failed += 1;
                self.quarantine_record(
                    rec,
                    &format!(
                        "missing-column: {} absent from a schema_v {CURRENT_RECORD_SCHEMA_V} \
                         record for \"{table}\" (the wire contract is full-row snapshots)",
                        missing
                            .iter()
                            .map(|c| format!("\"{c}\""))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )?;
                self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
                return Ok(());
            }
        }
        // Every value must BIND to its column. SQLite's dynamic typing would
        // take a string into an INTEGER column without complaint, and the
        // failure then surfaced far away — the row hydrating and failing
        // inside a view model — instead of here, where the record can still
        // be parked with its evidence. Outside the schema_v == current branch
        // on purpose: an older peer's columns must bind too.
        if let Err(e) = check_column_types(table_spec, rec.fields()) {
            result.failed += 1;
            self.quarantine_record(rec, &format!("invalid-value: {}", message_of(&e)))?;
            self.note_parked_stamp(rec, &mut batch.accepted_hlcs);
            return Ok(());
        }
        // Check-only: the drift gate must not advance the clock, or a record
        // that then fails/quarantines inside its savepoint would still have
        // moved it. The advancing merge_verified() runs only after the outer
        // transaction commits (bottom of apply_with).
        let rec_hlc = rec.hlc()?;
        if let Err(drift) = self.clock.assert_plausible(&rec_hlc) {
            result.quarantined += 1;
            self.quarantine_record(rec, &format!("hlc-drift: {drift}"))?;
            return Ok(());
        }
        // Apply each record inside a nested transaction (SQLite SAVEPOINT) so
        // a single malformed record — a CHECK/NOT NULL violation, a
        // constraint the gates above cannot see — rolls back ONLY itself and
        // is quarantined, instead of failing out of the batch and discarding
        // every other (good) record. Counters are updated OUTSIDE the
        // savepoint so a rollback can't desync them.
        let mut did_insert = false;
        let mut prior_snapshot: Option<SyncRow> = None;
        let mut slot_effects: Option<SlotResolution> = None;
        let applied = self.db.transaction(|| -> Result<bool> {
            let existing = self.held_stamp(table, rec.id()?)?;
            let wins = match &existing {
                None => true,
                Some(held) => dart::compare_strings(rec.hlc_packed()?, held).is_gt(),
            };
            if !wins {
                return Ok(false);
            }
            // Clear any single-slot conflict BEFORE writing. The single-slot
            // track kinds (one live accompaniment/reference per practice)
            // carry a partial UNIQUE index; inserting a second LIVE row for an
            // occupied slot raises a UNIQUE violation. Resolving here (and the
            // whole write) is in this record's savepoint, so a failure rolls
            // back the occupant tombstone too.
            let (columns, effects) = self.resolve_slot(rec)?;
            slot_effects = effects;
            let columns = Self::localize_per_device(table, columns);
            if existing.is_none() {
                self.insert(table, &columns)?;
                did_insert = true;
            } else {
                // Capture the row's pre-update wire snapshot BEFORE the
                // write, so the FK sweep can restore the exact prior state if
                // this very update turns out to have planted a dangling
                // pointer (see sweep_fk_orphans and Batch::updated for the
                // one-extra-read cost note).
                prior_snapshot = self.builder.build(table, rec.id()?)?;
                self.update(table, &columns)?;
            }
            Ok(true)
        })?;
        // The record settled (applied, or cleanly lost LWW): remember its
        // stamp for the post-commit clock advance. NOT merged here — this
        // line still runs inside the outer transaction, and an FK-sweep
        // abort, a failing before_commit hook, or a failed outer commit would
        // roll the database back while the in-memory clock kept the advance.
        batch.accepted_hlcs.push(rec_hlc);
        if applied {
            result.applied += 1;
            let key: RowKey = (table.to_string(), rec.id()?.to_string());
            // Record any slot-resolution side effects this record's savepoint
            // committed (a rolled-back savepoint discards slot_effects with
            // everything else). Accumulated per row: several records for the
            // same id can each trigger a resolution, and the sweep must be
            // able to walk every one of their revert pairs when the row is
            // parked.
            if let Some(effects) = slot_effects {
                match batch.slot_resolutions.get_mut(&key) {
                    Some(acc) => acc.absorb(effects),
                    None => {
                        batch.slot_resolutions.insert(key.clone(), effects);
                    }
                }
            }
            if did_insert {
                batch.inserted.insert(key);
            } else if let Some(prior) = prior_snapshot {
                // A row inserted EARLIER IN THIS BATCH and updated now is
                // already covered by the park-and-delete path; its mid-batch
                // snapshot is not a restore point. First write wins: with two
                // updates to the same pre-existing row in one batch, the
                // restore point must stay the PRE-BATCH state, not the state
                // between the two updates.
                if !batch.inserted.contains(&key) {
                    batch.updated.entry(key).or_insert(prior);
                }
            }
        } else {
            result.skipped += 1;
        }
        Ok(())
    }

    /// The stored hlc_packed of row [id] of [table] (a vetted table name),
    /// if the row exists.
    fn held_stamp(&self, table: &str, id: &str) -> Result<Option<String>> {
        single_or_none(self.db.query(
            &format!("SELECT hlc_packed FROM {table} WHERE id = ?"),
            [id],
            |r| r.get::<_, String>("hlc_packed"),
        )?)
    }

    /// The column-name set of [table], read from the LIVE schema
    /// (PRAGMA table_info) rather than the compiled spec, and cached only
    /// per apply call (the caller passes [cache]). Both choices are
    /// deliberate: the live schema is the ground truth the INSERT/UPDATE
    /// below actually runs against, and re-reading it per batch keeps the
    /// quarantine-replay contract intact — a schema upgrade must un-park a
    /// previously forward-incompatible row on the NEXT replay, not after a
    /// process restart (see the engine's quarantine replay). [table] is
    /// already vetted against the registry before this is called.
    fn live_columns<'c>(
        &self,
        table: &str,
        cache: &'c mut HashMap<String, IndexSet<String>>,
    ) -> Result<&'c IndexSet<String>> {
        if !cache.contains_key(table) {
            let names: IndexSet<String> = self
                .db
                .query(&format!("PRAGMA table_info(\"{table}\")"), [], |r| {
                    r.get::<_, String>("name")
                })?
                .into_iter()
                .collect();
            cache.insert(table.to_string(), names);
        }
        cache
            .get(table)
            .ok_or_else(|| Error::State(format!("live columns of {table}")))
    }

    /// Records a DOOR-PARKED record's stamp for the post-commit clock
    /// advance and the durable high-water mark, exactly like a settled
    /// record's — and the stamp of a record whose savepoint failure was
    /// parked REPLAYABLE (apply-failed): the savepoint rolled its write back,
    /// but its snapshot survives in the store just the same.
    ///
    /// A door park is not a rollback: the parked snapshot survives the batch
    /// and stays eligible to win LWW at replay — the newer-schema park is the
    /// DESIGNED honest path for a mixed-version fleet ("the quarantine replay
    /// un-parks it after the app upgrade"). Skipping its stamp let a later
    /// local edit stamp BELOW the parked record, and the replay then silently
    /// reverted the user's newer edit under LWW — the exact failure
    /// SyncStateStore::HLC_HIGH_WATER_KEY documents. The clock's invariant is
    /// to advance past everything the device has SEEN, row survival being a
    /// different layer (same rationale as the FK-orphan merges, see the
    /// bottom of apply_with).
    ///
    /// Two stamps are still excluded, deliberately: a malformed one (no
    /// well-formed stamp exists to advance past — the park itself keys on
    /// whatever hlc_packed the record carried) and an implausible one (the
    /// drift discipline holds at EVERY gate: a suspect stamp must never
    /// ratchet the clock, which is why the drift gate's own handler never
    /// calls this). And without a quarantine store (counter-only test wiring)
    /// nothing is parked at all — no surviving snapshot can out-rank a later
    /// local edit, and the mark must not claim stamps nothing holds.
    fn note_parked_stamp(&self, rec: &SyncRow, accepted: &mut Vec<Hlc>) {
        if self.quarantine.is_none() {
            return;
        }
        let Ok(hlc) = rec.hlc() else {
            return;
        };
        if self.clock.assert_plausible(&hlc).is_err() {
            return;
        }
        accepted.push(hlc);
    }

    /// Classifies a savepoint failure into the quarantine's replay classes.
    /// The default, 'apply-failed', is REPLAYABLE — IO pressure or a racing
    /// writer may clear on the next cycle, and the newer-schema park relies
    /// on an app upgrade un-parking it. A CHECK or NOT NULL violation is
    /// neither: the verdict is a pure function of the payload against a
    /// schema constraint that never changes at runtime, so its replay
    /// re-parks forever — and a forever-failing entry sitting in a replayable
    /// class could displace a replayable park of the same row, the only copy
    /// of a state the change token already passed (see
    /// [`QuarantineStore::put`]'s retention rule). Park those under the
    /// unreplayable 'invalid-value:' class instead.
    fn savepoint_failure_reason(e: &Error) -> String {
        // SQLite extended result codes: (275) SQLITE_CONSTRAINT_CHECK,
        // (1299) SQLITE_CONSTRAINT_NOTNULL. Other constraint codes stay
        // replayable on purpose: UNIQUE/FK verdicts depend on DB state a
        // later batch can change.
        const CONSTRAINT_CHECK: i32 = 275;
        const CONSTRAINT_NOT_NULL: i32 = 1299;
        // (Dart unwrapped the background isolate's DriftRemoteException
        // here; the Rust store runs on the caller's thread, so the SQLite
        // error arrives as is, on disk and in memory alike.)
        if let Error::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) = e {
            if failure.extended_code == CONSTRAINT_CHECK
                || failure.extended_code == CONSTRAINT_NOT_NULL
            {
                return format!("invalid-value: {e}");
            }
        }
        format!("apply-failed: {e}")
    }

    /// Persists a rejected record to the quarantine parking lot (latest state
    /// per row) INSIDE the current outer apply transaction.
    ///
    /// A STORAGE failure here fails the WHOLE batch, aligned with the FK
    /// sweep's park: the put is the only thing standing between a rejected
    /// record and silent permanent loss — the change token advances past
    /// this page when the batch commits, so a record whose park was dropped
    /// would be forever behind the token (while its dependents may have
    /// parked fine), unreachable by any replay. The failure comes back as
    /// [`RecordError::QuarantinePut`] so the per-record handler in apply
    /// fails the batch (re-parking through the same failing store cannot
    /// succeed) and the outer transaction rolls back: token unmoved, a
    /// retried page over a silent loss.
    ///
    /// A record with no usable id, or whose payload cannot be serialized, is
    /// parked as a DEAD LETTER (see `dead_letter`) instead: it can never
    /// replay, but the raw evidence survives for review.
    ///
    /// The [reason] prefixes written here are part of the store's retention
    /// contract: [`QuarantineStore::put`] classifies parks as replayable or
    /// permanently dead by them, so a renamed gate reason must be reflected
    /// there.
    fn quarantine_record(
        &self,
        rec: &SyncRow,
        reason: &str,
    ) -> std::result::Result<(), RecordError> {
        let Some(store) = &self.quarantine else {
            return Ok(());
        };
        let (park_id, payload, park_reason) = match rec.field("id") {
            Some(FieldValue::Text(id)) if !id.is_empty() => match rec.encode() {
                Ok(payload) => (id.clone(), payload, reason.to_string()),
                // Un-encodable (a non-finite double, a mistyped identity —
                // wire rows are well-formed by SyncRow::from_json): park a
                // DEAD LETTER instead of dropping silently. It can never
                // replay, but the raw evidence survives for review — silent
                // loss is exactly what quarantine exists to prevent.
                Err(_) => {
                    let (id, payload) = Self::dead_letter(rec);
                    (id, payload, format!("dead-letter({reason})"))
                }
            },
            // No addressable id: same dead-letter treatment under a
            // synthesized content-hash key.
            _ => {
                let (id, payload) = Self::dead_letter(rec);
                (id, payload, format!("dead-letter({reason})"))
            }
        };
        let hlc_packed = match rec.field("hlc_packed") {
            Some(FieldValue::Text(s)) => s.as_str(),
            _ => "",
        };
        let schema_v = match rec.field("schema_v") {
            Some(FieldValue::Int(v)) => *v,
            _ => 0,
        };
        store
            .put(&QuarantineEntry {
                table: rec.table(),
                id: &park_id,
                hlc_packed,
                schema_v,
                payload: &payload,
                reason: &park_reason,
                quarantined_at: now_ms(),
            })
            .map_err(RecordError::QuarantinePut)
    }

    /// Parks a record that never decoded into a row. It has no table, id or
    /// stamp to route by, so it goes to the same dead-letter space as an
    /// unparkable record: content-keyed, deliberately not a valid
    /// [`SyncRow`] envelope, so the replay's decode refuses it and it stays
    /// for review.
    fn park_undecodable(&self, reject: &RejectedRecord) -> std::result::Result<(), RecordError> {
        let Some(store) = &self.quarantine else {
            return Ok(());
        };
        let mut obj = Map::new();
        obj.insert("deadLetter".into(), Value::Bool(true));
        obj.insert("undecodable".into(), Value::Bool(true));
        obj.insert("raw".into(), Value::String(reject.payload.clone()));
        let payload = dart_json::encode(&Value::Object(obj))
            .map_err(|e| RecordError::QuarantinePut(Error::InvalidArgument(e.to_string())))?;
        let digest = hex::encode(Sha256::digest(payload.as_bytes()));
        store
            .put(&QuarantineEntry {
                table: Self::DEAD_LETTER_TABLE,
                id: &format!("deadletter{}", &digest[..32]),
                hlc_packed: "",
                schema_v: 0,
                payload: &payload,
                reason: &format!("dead-letter(undecodable: {})", reject.reason),
                quarantined_at: now_ms(),
            })
            .map_err(RecordError::QuarantinePut)
    }

    /// A best-effort, always-encodable snapshot of an otherwise unparkable
    /// record, keyed by a content hash so distinct dead letters get distinct
    /// rows. The payload is deliberately NOT a valid [`SyncRow`] envelope:
    /// the quarantine replay's decode fails and keeps it parked — a dead
    /// letter is for human review, never re-application.
    fn dead_letter(rec: &SyncRow) -> (String, String) {
        let mut safe = Map::new();
        for (k, v) in rec.fields() {
            // A non-finite double is spelled out: the encoder refuses it, and
            // it is exactly what makes such a record un-encodable in the
            // first place (see check_column_types).
            let value = v
                .to_json()
                .unwrap_or_else(|| Value::String(v.dart_to_string()));
            safe.insert(k.clone(), value);
        }
        let mut obj = Map::new();
        obj.insert("deadLetter".into(), Value::Bool(true));
        obj.insert("table".into(), Value::String(rec.table().to_string()));
        obj.insert("fields".into(), Value::Object(safe));
        let payload = dart_json::encode(&Value::Object(obj)).unwrap_or_default();
        // A content digest of the payload: deterministic across runs, so the
        // same dead letter re-parks onto the same row. Wide on purpose — the
        // previous 32-bit FNV collided in a space small enough to reach, and
        // a collision is SILENT DATA LOSS here: two distinct dead letters
        // share a (table, id), and the quarantine upsert compares
        // hlc_packed, which is usually '' for a dead letter, so the second
        // is dropped rather than stored.
        let digest = hex::encode(Sha256::digest(payload.as_bytes()));
        (format!("deadletter{}", &digest[..32]), payload)
    }

    /// Pre-commit sweep for rows that STILL violate a foreign key once the
    /// whole batch is in (defer_foreign_keys postponed enforcement to the
    /// outer commit, so order-tolerance already absorbed child-before-parent
    /// within the batch).
    ///
    /// foreign_key_check scans the WHOLE database, so a flagged row is not
    /// necessarily one this batch created: a remote UPDATE can plant a
    /// dangling pointer into a row that existed long before.
    /// ([own_rows_only] checks only the rows this batch inserted or updated,
    /// which is every row a batch can leave dangling; the untouched-row case
    /// below is then the next pull page's to find.) Healing is by
    /// provenance:
    ///
    ///  * A row INSERTED by this batch is PARKED, then deleted. The delete is
    ///    non-negotiable — the deferred FK still fails the outer commit while
    ///    the row exists — but deleting ALONE is not enough: "it re-arrives
    ///    with its parent on a later page" is false when the parent is
    ///    locally QUARANTINED (e.g. a newer-schema row parked by the schema
    ///    gate) while the child applied — the child's page is then already
    ///    behind the change token, the server never re-delivers it, and the
    ///    row would be lost forever. So its full wire snapshot (reconstructed
    ///    via [`SyncRowBuilder::build`], carrying the row's OWN HLC so a
    ///    later LWW settle still works) goes to the quarantine store first,
    ///    with the distinct `fk-orphan` reason. The pull run's quarantine
    ///    replay then converges it: a replayed orphan whose parent now exists
    ///    applies normally (and its entry settles), one whose parent is STILL
    ///    missing is re-swept here and re-parked (the store's upsert replaces
    ///    the entry) — never lost, never a hot loop. Unlike the door's
    ///    parks, after the delete a failed park IS a lost row, so a storage
    ///    error aborts the batch (a retried page over silent loss). The park
    ///    goes to the row's own SWEEP slot ([`QuarantineStore::put_swept`]),
    ///    so no door park of the same row — a drift-parked or newer-schema
    ///    newer state, a dead payload — can displace it or filter it out:
    ///    such a state waits on a different gate, and letting it hold the
    ///    only slot left the row deleted (or stale) after its parent landed.
    ///    Within the slot it is only ever filtered by a strictly newer sweep
    ///    park — a state that likewise passed every gate and waits only on a
    ///    parent. Without a quarantine store (counter-only test wiring) the
    ///    old hard-delete stands.
    ///  * A PRE-EXISTING row UPDATED by this batch is PARKED, then RESTORED —
    ///    never deleted. It satisfied every FK when the batch began (SQLite
    ///    enforced them at every earlier commit), so the dangling pointer was
    ///    planted by this batch's own update, and the record is treated
    ///    exactly like the inserted case: it was effectively quarantined at
    ///    the door. Its CURRENT (post-update, remote-HLC) snapshot goes to
    ///    the quarantine store under the same fk-orphan reason, and the row
    ///    is then reverted VERBATIM to the pre-batch snapshot apply captured
    ///    just before the update — the exact prior state including its HLC,
    ///    deliberately NOT a derived stamp (a derived stamp mints new state
    ///    that can out-rank a peer's legitimate write — the resurrection
    ///    class of bug docs/sync-protocol.md warns about — while a verbatim
    ///    revert re-asserts state that already existed, so nothing new
    ///    enters the fleet). One carve-out: a live single-slot snapshot whose
    ///    slot a LATER in-page winner now holds is restored TOMBSTONED under
    ///    a derived stamp and journaled instead — a verbatim live restore
    ///    would trip ux_track_slot and brick the page (see
    ///    `restore_prior_snapshot` for the full invariant). Convergence: the
    ///    parked snapshot is strictly newer than the restored row, so once
    ///    the missing parent lands the quarantine replay applies it by plain
    ///    LWW; and a re-delivered IDENTICAL update is likewise strictly newer
    ///    than the restored row, so it re-applies (and is re-parked here
    ///    while the parent is still missing) — under the old
    ///    null-the-pointer heal that re-delivery was LWW-equal to the healed
    ///    row and skipped, leaving the reference nulled FOREVER unless some
    ///    strictly-newer update happened to arrive. Because the restore heals
    ///    every repointed column at once, this path covers dangling NOT NULL
    ///    columns too: the abort below is unreachable for updated-this-batch
    ///    rows (with a store wired), turning the old retry-forever page into
    ///    a committed batch plus a parked, replayable update.
    ///  * A dangling PRE-EXISTING row this batch NEVER touched (and, without
    ///    a quarantine store — counter-only test wiring — any dangling
    ///    pre-existing row, since parking is impossible) keeps the old heal:
    ///    only the dangling column is nulled (the IntegrityChecker-style
    ///    scalar self-heal), deliberately un-journaled like every remote
    ///    apply. No remote apply can CREATE this case any more — every
    ///    dangling pre-existing row is one this batch just updated — so it
    ///    guards states that arose outside the applier's control. Should a
    ///    NOT NULL column dangle on such a row, we still abort so the WHOLE
    ///    batch rolls back — a retried page over silently deleting
    ///    pre-existing data (deleting the row — and then its subtree via the
    ///    fixpoint loop — would destroy local data with no tombstone and no
    ///    oplog).
    ///
    /// Loops to a fixpoint because deleting an orphan can strand its own
    /// batch-inserted children. Counters: a deleted batch-insert and a
    /// restored batch-update each move from applied to failed exactly once
    /// (duplicate violations for a multi-FK row are skipped once the row is
    /// deleted or restored); a nulled untouched row changes no counter.
    ///
    /// Slot-resolution revert: parking a row is "quarantined at the door", so
    /// on BOTH park paths any slot-resolution side effects the parked record
    /// triggered are reverted with it — per occupant, as atomic pairs:
    /// exactly that resolution's oplog entries are deleted and that occupant
    /// restored to its pre-resolution snapshot, or (when the stamp was
    /// superseded or a later winner holds the slot) the WHOLE pair is kept —
    /// tombstone and journal together, never a ghost (see
    /// `revert_slot_resolution`). The revert runs AFTER the parked row itself
    /// is deleted/restored, deliberately: a restored occupant comes back
    /// LIVE, and restoring it while the parked record still held the slot
    /// live would trip the ux_track_slot unique index. A parked record that
    /// LOST the slot was stored tombstoned at the door under a derived stamp;
    /// parking that derived state would replay an UN-journaled tombstone (the
    /// revert just deleted its journal entry), so the parked snapshot is
    /// substituted with the ORIGINAL incoming record — the replay then
    /// re-runs the resolution from scratch, re-deriving and re-journaling
    /// whatever it still decides. Counter-only wiring (no store) keeps
    /// today's behavior on every path, revert included.
    fn sweep_fk_orphans(
        &self,
        result: &mut ApplyResult,
        batch: &mut Batch,
        own_rows_only: bool,
    ) -> Result<()> {
        // Rows already park-and-restored by THIS sweep. A restored row can
        // never be flagged by a LATER fixpoint pass (its pre-batch state
        // satisfied every FK when the batch began, and the sweep only ever
        // deletes rows this batch inserted — which a pre-batch snapshot
        // cannot reference), but WITHIN the pass that restored it,
        // foreign_key_check may have reported it once per violated
        // constraint and the later entries are stale.
        let mut restored: HashSet<RowKey> = HashSet::new();
        loop {
            let violations = if own_rows_only {
                let mut own: IndexSet<RowKey> = batch.inserted.clone();
                own.extend(batch.updated.keys().cloned());
                self.violations_of(&own)?
            } else {
                self.db.query("PRAGMA foreign_key_check", [], |r| {
                    Ok(Violation {
                        table: r.get("table")?,
                        rowid: r.get("rowid")?,
                        parent: r.get("parent")?,
                        fkid: r.get("fkid")?,
                    })
                })?
            };
            if violations.is_empty() {
                break;
            }
            for v in violations {
                let table = v.table.as_str();
                // A row with several dangling FKs is reported once per
                // violated constraint; after the DELETE below the id lookup
                // misses, so the duplicate entries are skipped (no double
                // accounting).
                let id: Option<String> = single_or_none(self.db.query(
                    &format!("SELECT id FROM {table} WHERE rowid = ?"),
                    [v.rowid],
                    |r| r.get::<_, String>("id"),
                )?)?;
                let Some(id) = id else {
                    continue;
                };
                let key: RowKey = (table.to_string(), id.clone());
                if batch.inserted.contains(&key) {
                    if let Some(store) = &self.quarantine {
                        // Park BEFORE deleting (same outer transaction, so the
                        // entry and the delete commit or roll back together).
                        // The snapshot is the shared outbound projection —
                        // never a hand-written second one — and reads the
                        // row's own adopted HLC straight from the table (or
                        // the ORIGINAL incoming record when the row holds a
                        // slot-resolution tombstone the revert below is about
                        // to disown — see parkable_snapshot).
                        let snapshot = Self::parkable_snapshot(
                            self.builder.build(table, &id)?,
                            batch.slot_resolutions.get(&key),
                        );
                        if let Some(snapshot) = snapshot {
                            self.park_orphan(store, &v, table, &id, &snapshot)?;
                        }
                    }
                    self.db
                        .execute(&format!("DELETE FROM {table} WHERE rowid = ?"), [v.rowid])?;
                    if self.quarantine.is_some() {
                        // The record is quarantined at the door: side effects
                        // minted on its behalf must not outlive it. After the
                        // delete, so a restored occupant can never transiently
                        // share the live slot with the parked row.
                        let resolution = batch.slot_resolutions.shift_remove(&key);
                        self.revert_slot_resolution(resolution)?;
                    }
                    // The insert was counted as applied above; it did not
                    // stick.
                    result.applied = result.applied.saturating_sub(1);
                    result.failed += 1;
                    continue;
                }
                // Pre-existing row UPDATED by this batch: the update itself
                // planted the dangling pointer, so park the post-update
                // snapshot and restore the pre-batch state verbatim (see the
                // doc comment). Without a store the old null-the-pointer heal
                // below stands, like the inserted case's hard-delete
                // fallback.
                let prior = batch.updated.get(&key).cloned();
                if let (Some(prior), Some(store)) = (prior, &self.quarantine) {
                    if !restored.insert(key.clone()) {
                        // A later violation entry of the SAME pass for a row
                        // already restored (several FK columns repointed at
                        // once): the verbatim restore healed every column, so
                        // the entry is stale.
                        continue;
                    }
                    // Park the CURRENT (post-update, remote-HLC) snapshot
                    // BEFORE the restore overwrites it — the shared outbound
                    // projection and the same reason format as the inserted
                    // case (with the same original-record substitution when
                    // the row holds a slot-resolution tombstone, see
                    // parkable_snapshot). Strictly newer than the restored
                    // row, so the quarantine replay lands it by plain LWW
                    // once the parent exists. Like the inserted case's park,
                    // a storage failure here aborts the batch rather than
                    // losing the update.
                    let snapshot = Self::parkable_snapshot(
                        self.builder.build(table, &id)?,
                        batch.slot_resolutions.get(&key),
                    );
                    if let Some(snapshot) = snapshot {
                        self.park_orphan(store, &v, table, &id, &snapshot)?;
                        // Restore the prior snapshot — verbatim in the normal
                        // case, tombstoned under a derived stamp when a later
                        // in-page winner now holds its single-track slot live
                        // (see restore_prior_snapshot for the invariant).
                        // After this the row has no dangling pointer, so the
                        // nullable and NOT NULL cases converge on this same
                        // path.
                        self.restore_prior_snapshot(&prior)?;
                        // Quarantined at the door, like the inserted case:
                        // revert any slot resolution the parked record
                        // triggered — after the restore above put the row back
                        // on its (pre-batch, slot-free or slot-consistent)
                        // state, so the returning occupant cannot transiently
                        // double-occupy the live slot.
                        let resolution = batch.slot_resolutions.shift_remove(&key);
                        self.revert_slot_resolution(resolution)?;
                        // The update was counted as applied above; it did not
                        // stick.
                        result.applied = result.applied.saturating_sub(1);
                        result.failed += 1;
                        continue;
                    }
                    // The violation proves the row exists, so a None snapshot
                    // cannot happen; if it ever does, fall through to the
                    // scalar heal below rather than crash the sweep.
                    restored.remove(&key);
                }
                // Untouched pre-existing row (or counter-only wiring): null
                // just the dangling column. Which column comes from the
                // violation's fkid resolved against foreign_key_list.
                let column = match v.fkid {
                    Some(fkid) => self.fk_column(table, fkid)?,
                    None => None,
                };
                let healable = match &column {
                    Some(c) => self.is_nullable(table, c)?,
                    None => false,
                };
                let Some(column) = column.filter(|_| healable) else {
                    // Un-heal-able (see doc comment): fail the batch, keep the
                    // data. Residual by construction — an updated-this-batch
                    // row (with a store wired) took the park-and-restore path
                    // above instead.
                    return Err(Error::State(format!(
                        "FK sweep: pre-existing {table}/{id} has a dangling NOT NULL foreign \
                         key (fkid: {}); failing the batch to preserve it",
                        v.fkid.map_or_else(|| "null".to_string(), |f| f.to_string())
                    )));
                };
                self.db.execute(
                    &format!("UPDATE {table} SET {column} = NULL WHERE rowid = ?"),
                    [v.rowid],
                )?;
            }
        }
        Ok(())
    }

    /// Parks the FK sweep's [snapshot] of row [id] of [table] in the row's
    /// sweep slot, naming the missing parent [v] points at.
    fn park_orphan(
        &self,
        store: &QuarantineStore,
        v: &Violation,
        table: &str,
        id: &str,
        snapshot: &SyncRow,
    ) -> Result<()> {
        let column = match v.fkid {
            Some(fkid) => self.fk_column(table, fkid)?,
            None => None,
        };
        let parent_id = column
            .as_deref()
            .and_then(|c| snapshot.field(c))
            .filter(|value| !value.is_null())
            .map_or_else(|| "unknown".to_string(), FieldValue::dart_to_string);
        store.put_swept(&QuarantineEntry {
            table,
            id,
            hlc_packed: snapshot.hlc_packed()?,
            schema_v: snapshot.schema_v()?,
            payload: &snapshot.encode()?,
            reason: &format!("fk-orphan: parent {}/{parent_id} missing", v.parent),
            quarantined_at: now_ms(),
        })
    }

    /// Restores an updated-this-batch row the sweep just parked to its
    /// pre-batch [snapshot] — verbatim in the normal case: a pure revert to
    /// the exact prior state including its HLC, never a derived stamp (a
    /// derived stamp mints new state that can out-rank a peer's legitimate
    /// write — the resurrection class of bug docs/sync-protocol.md warns
    /// about — while a verbatim revert re-asserts state that already
    /// existed).
    ///
    /// The one exception is a live single-slot track whose slot a LATER
    /// in-page winner now holds: restoring the snapshot live would trip the
    /// ux_track_slot partial unique index OUTSIDE any savepoint, fail the
    /// outer commit and retry the same page forever (token unmoved). Skipping
    /// the restore is no answer either — the post-update row still dangles,
    /// so the sweep's fixpoint would spin — and nulling just the pointer
    /// leaves the table LWW-equal to the parked snapshot, which then never
    /// replays (the permanent-null regression the park-and-restore path
    /// exists to fix). So the collision is resolved the way the applier
    /// resolves every slot conflict: the snapshot is restored TOMBSTONED
    /// under a derived stamp (node-derived from its own prior HLC, dated to
    /// the winner's wall like every slot tombstone) and the tombstone is
    /// JOURNALED.
    ///
    /// INVARIANT (pinned by test): the page stays committable — the
    /// tombstoned row escapes the partial index — and every state left
    /// behind is accounted. The journaled tombstone matches the row the
    /// table actually holds and replicates to peers, out-ranking the
    /// pre-batch state they still hold — exactly the resolution they would
    /// have derived had they applied the winner themselves. And the PARKED
    /// post-update snapshot still out-ranks the derived stamp (it beat the
    /// prior state by LWW, and a derived stamp stays below any later real
    /// edit — see derived_hlc), so the quarantine replay lands it once the
    /// missing parent arrives and re-runs the slot resolution from scratch.
    fn restore_prior_snapshot(&self, snapshot: &SyncRow) -> Result<()> {
        let Some(winner_wall) = self.slot_winner_wall(snapshot)? else {
            return self.update(snapshot.table(), snapshot.fields());
        };
        let stamp = self.derived_hlc(&snapshot.hlc()?);
        let mut columns = snapshot.fields().clone();
        columns.insert("deleted_at".into(), FieldValue::Int(winner_wall));
        columns.insert("updated_at".into(), FieldValue::Int(winner_wall));
        columns.insert("hlc_wall".into(), FieldValue::Int(stamp.wall_ms));
        columns.insert("hlc_ctr".into(), FieldValue::Int(stamp.counter));
        columns.insert("hlc_node".into(), FieldValue::Text(stamp.node.clone()));
        columns.insert("hlc_packed".into(), FieldValue::Text(stamp.packed()));
        self.update(snapshot.table(), &columns)?;
        self.journal_slot_tombstone(snapshot.id()?, &stamp)?;
        Ok(())
    }

    /// `PRAGMA foreign_key_check`'s rows (table, rowid, parent, fkid) for the
    /// [rows] given by (table, id) only: each foreign key of each row whose
    /// value names no parent row. What the sweep's own-rows-only mode reads
    /// in place of the whole-store check. A row the sweep deleted is simply
    /// not found again, as with the whole-store check.
    fn violations_of(&self, rows: &IndexSet<RowKey>) -> Result<Vec<Violation>> {
        let mut ids_by_table: IndexMap<&str, Vec<&str>> = IndexMap::new();
        for (table, id) in rows {
            ids_by_table.entry(table.as_str()).or_default().push(id);
        }
        let mut found = Vec::new();
        for (table, ids) in ids_by_table {
            let fks: Vec<(String, String, Option<String>, i64)> =
                self.db
                    .query(&format!("PRAGMA foreign_key_list(\"{table}\")"), [], |r| {
                        Ok((r.get("table")?, r.get("from")?, r.get("to")?, r.get("id")?))
                    })?;
            for (parent, from, to, fkid) in fks {
                let to = to.unwrap_or_else(|| "id".to_string());
                for chunk in ids.chunks(500) {
                    let placeholders = vec!["?"; chunk.len()].join(", ");
                    let sql = format!(
                        "SELECT ? AS \"table\", c.rowid AS rowid, ? AS parent, ? AS fkid \
                         FROM \"{table}\" c WHERE c.id IN ({placeholders}) \
                         AND c.\"{from}\" IS NOT NULL AND NOT EXISTS \
                         (SELECT 1 FROM \"{parent}\" p WHERE p.\"{to}\" = c.\"{from}\")"
                    );
                    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&table, &parent, &fkid];
                    for id in chunk {
                        params.push(id);
                    }
                    found.extend(self.db.query(&sql, &*params, |r| {
                        Ok(Violation {
                            table: r.get("table")?,
                            rowid: r.get("rowid")?,
                            parent: r.get("parent")?,
                            fkid: r.get("fkid")?,
                        })
                    })?);
                }
            }
        }
        Ok(found)
    }

    /// The child column of foreign key [fkid] on [table] (our FKs are all
    /// single-column, so the first component is the whole key).
    fn fk_column(&self, table: &str, fkid: i64) -> Result<Option<String>> {
        let fks: Vec<(i64, String)> =
            self.db
                .query(&format!("PRAGMA foreign_key_list({table})"), [], |r| {
                    Ok((r.get("id")?, r.get("from")?))
                })?;
        Ok(fks
            .into_iter()
            .find(|(id, _)| *id == fkid)
            .map(|(_, from)| from))
    }

    fn is_nullable(&self, table: &str, column: &str) -> Result<bool> {
        let cols: Vec<(String, i64)> =
            self.db
                .query(&format!("PRAGMA table_info({table})"), [], |r| {
                    Ok((r.get("name")?, r.get("notnull")?))
                })?;
        Ok(cols
            .into_iter()
            .find(|(name, _)| name == column)
            .is_some_and(|(_, notnull)| notnull == 0))
    }

    /// Returns the column map to write for [rec] after resolving any
    /// single-slot conflict by entity-level LWW: if [rec] is a LIVE
    /// accompaniment/reference whose (practice, kind) slot is held by a
    /// DIFFERENT live row, the higher hlc_packed keeps the slot live and the
    /// loser is tombstoned. When [rec] is the loser, the returned map is
    /// tombstoned so the row is still stored (a later sync can update it)
    /// without occupying the live slot.
    ///
    /// Alongside the columns it returns the resolution's side effects (a
    /// per-occupant revert pair — pre-tombstone snapshot, derived stamp,
    /// journal seq — or the record's own at-the-door stamp and seq) so apply
    /// can remember them per row and the FK sweep can REVERT them if the
    /// very record that triggered the resolution is later parked — without
    /// the revert, a record "quarantined at the door" would leave a
    /// tombstoned track and a journaled resolution behind for a write that
    /// never landed (see revert_slot_resolution).
    ///
    /// The resolution REPLICATES: the loser's tombstone is stamped by a
    /// derived stamp over the loser's own stamp (under OUR node — see
    /// derived_hlc) and journaled, because whether a device even observes
    /// the conflict depends on its local apply order (the loser's creator
    /// meets the winner arriving; a device that received the winner's later
    /// tombstone first never sees two live rows at once). An un-journaled,
    /// un-bumped local tombstone therefore diverged permanently: same
    /// hlc_packed, live on one device and dead on another (convergence
    /// simulation, seeds 0x5eed0009 / 0xabd782). With the bump + journal the
    /// resolution wins LWW on every peer, and two devices resolving the same
    /// pair independently produce identical rows up to the node tiebreak.
    /// The tombstone TIME is the winner's wall clock (the moment the slot was
    /// claimed) — the same value on every resolver. This is the ONE local
    /// write the applier journals; it is a new derived decision, never an
    /// echo of the applied record, so it cannot ping-pong (see the type
    /// docs).
    fn resolve_slot(&self, rec: &SyncRow) -> Result<(Fields, Option<SlotResolution>)> {
        let unresolved = || Ok((rec.fields().clone(), None));
        if rec.table() != "tracks" || rec.field("deleted_at").is_some_and(|v| !v.is_null()) {
            return unresolved();
        }
        let kind = match rec.field("kind") {
            Some(FieldValue::Text(k)) if k == "accompaniment" || k == "reference" => k.clone(),
            _ => return unresolved(),
        };
        let Some(FieldValue::Text(practice_id)) = rec.field("practice_id") else {
            return unresolved();
        };
        let occupant: Option<(String, String, i64)> = single_or_none(self.db.query(
            "SELECT id, hlc_packed, hlc_wall FROM tracks WHERE practice_id = ? \
             AND kind = ? AND deleted_at IS NULL AND id <> ?",
            [practice_id.as_str(), kind.as_str(), rec.id()?],
            |r| Ok((r.get("id")?, r.get("hlc_packed")?, r.get("hlc_wall")?)),
        )?)?;
        let Some((loser_id, occupant_packed, occupant_wall)) = occupant else {
            return unresolved();
        };
        let mut effects = SlotResolution::new(rec.clone());
        if dart::compare_strings(rec.hlc_packed()?, &occupant_packed).is_gt() {
            // Incoming wins → free the slot by tombstoning the current
            // occupant (the loser), under a stamp derived from the occupant's
            // own HLC. The occupant's pre-tombstone snapshot is captured FIRST
            // — the revert's restore point if this record is later parked.
            let snapshot = self.builder.build("tracks", &loser_id)?;
            let stamp = self.derived_hlc(&Hlc::parse(&occupant_packed)?);
            let tombstone_at = rec.field("hlc_wall").cloned().unwrap_or(FieldValue::Null);
            self.db.execute(
                "UPDATE tracks SET deleted_at = ?, updated_at = ?, hlc_wall = ?, \
                 hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?",
                rusqlite::params![
                    tombstone_at,
                    tombstone_at,
                    stamp.wall_ms,
                    stamp.counter,
                    stamp.node,
                    stamp.packed(),
                    loser_id,
                ],
            )?;
            let seq = self.journal_slot_tombstone(&loser_id, &stamp)?;
            // The occupant was selected just above, so build() returning None
            // is a never-expected race; without a restore point the pair is
            // simply not recorded, and on a revert the tombstone then stays
            // WITH its journal row — still an accounted state, never a ghost.
            if let Some(snapshot) = snapshot {
                effects.occupants.push(OccupantRevert {
                    snapshot,
                    stamp_packed: stamp.packed(),
                    oplog_seqs: vec![seq],
                });
            }
            return Ok((rec.fields().clone(), Some(effects)));
        }
        // Incoming loses → store it tombstoned (under a stamp derived from its
        // own HLC, dated to the occupant/winner's wall) so it never occupies
        // the live slot and the resolution replicates to peers still holding
        // it live.
        let stamp = self.derived_hlc(&rec.hlc()?);
        effects.self_stamp_packed = Some(stamp.packed());
        effects
            .self_oplog_seqs
            .push(self.journal_slot_tombstone(rec.id()?, &stamp)?);
        let mut columns = rec.fields().clone();
        columns.insert("deleted_at".into(), FieldValue::Int(occupant_wall));
        columns.insert("updated_at".into(), FieldValue::Int(occupant_wall));
        columns.insert("hlc_wall".into(), FieldValue::Int(stamp.wall_ms));
        columns.insert("hlc_ctr".into(), FieldValue::Int(stamp.counter));
        columns.insert("hlc_node".into(), FieldValue::Text(stamp.node.clone()));
        columns.insert("hlc_packed".into(), FieldValue::Text(stamp.packed()));
        Ok((columns, Some(effects)))
    }

    /// The snapshot the FK sweep should PARK for a row it is about to
    /// quarantine at the door: normally the row's own [snapshot] as built
    /// from the table — except when the row currently holds the
    /// tombstoned-at-the-door state a slot [resolution] minted for it (the
    /// incoming record LOST the slot and was stored under a derived stamp).
    /// The revert is about to delete that resolution's journal entry, so
    /// parking the derived state would later replay an UN-journaled
    /// tombstone that no peer ever learns about; the ORIGINAL incoming
    /// record is parked instead, and the replay re-runs the slot resolution
    /// from scratch (re-deriving and re-journaling if it still loses).
    /// Identified by the row's stamp: any later record for the same id moved
    /// hlc_packed off the derived stamp, and its genuine remote state must
    /// then be parked as-is.
    fn parkable_snapshot(
        snapshot: Option<SyncRow>,
        resolution: Option<&SlotResolution>,
    ) -> Option<SyncRow> {
        let (Some(snapshot), Some(resolution)) = (&snapshot, resolution) else {
            return snapshot;
        };
        let derived = resolution.self_stamp_packed.as_deref();
        if derived.is_none() || derived != snapshot.hlc_packed().ok() {
            return Some(snapshot.clone());
        }
        Some(resolution.record.clone())
    }

    /// Reverts the side effects of a slot [resolution] whose triggering
    /// record the FK sweep just parked: the record was "quarantined at the
    /// door", so a track must not stay tombstoned — nor a resolution
    /// journaled — on behalf of a write that never landed. Rides the same
    /// outer transaction as the park, like every sweep write. A None
    /// [resolution] (no resolution recorded for the parked row) is a no-op.
    ///
    /// Occupants are reverted per entry as atomic PAIRS: exactly this
    /// resolution's journal rows are deleted and the occupant is restored
    /// VERBATIM to its pre-resolution snapshot — its own prior HLC, never a
    /// derived stamp — together, or the WHOLE pair is skipped. A pair is
    /// restorable only while
    ///
    ///  * the occupant row still carries this resolution's derived tombstone
    ///    stamp — if a later record legitimately overwrote the tombstone by
    ///    LWW (or the occupant row was itself swept away), that state must
    ///    stand, reverting over it would resurrect old state — AND
    ///  * no OTHER live row currently holds the occupant's single-track slot,
    ///    checked with the same predicate the ux_track_slot partial unique
    ///    index enforces (slot_winner_wall): a later in-page winner may have
    ///    claimed the slot after the parked record did, and restoring the
    ///    occupant live under it would trip the index OUTSIDE any savepoint,
    ///    fail the outer commit and retry the same page forever.
    ///
    /// A skipped pair leaves the occupant tombstoned WITH its journal rows —
    /// an accounted tombstone peers still learn — never the ghost of a kept
    /// tombstone whose ledger entry was deleted, and never a double-occupied
    /// live slot.
    ///
    /// Entries are walked LATEST-FIRST, deliberately: successive occupants
    /// of one slot were each tombstoned so the next could hold it, and
    /// restoring the latest restorable one (the earlier entries then skip on
    /// its occupancy) reproduces the state a peer that never saw the parked
    /// record derives from its own resolutions — the latest occupant live,
    /// every earlier one tombstoned under the same derived stamps. Walking
    /// earliest-first would instead restore the OLDEST occupant live, an
    /// un-journaled state no peer can converge to.
    fn revert_slot_resolution(&self, resolution: Option<SlotResolution>) -> Result<()> {
        let Some(resolution) = resolution else {
            return Ok(());
        };
        // The record's own at-the-door tombstone entries go with the park
        // unconditionally: the sweep just deleted/restored the row itself,
        // and the parked payload substitutes the ORIGINAL record for the
        // derived state (see parkable_snapshot).
        self.delete_oplog_seqs(&resolution.self_oplog_seqs)?;
        for entry in resolution.occupants.iter().rev() {
            let snapshot = &entry.snapshot;
            let current = self.held_stamp(snapshot.table(), snapshot.id()?)?;
            if current.as_deref() != Some(entry.stamp_packed.as_str()) {
                continue; // superseded by a later LWW winner: skip the whole pair
            }
            if self.slot_winner_wall(snapshot)?.is_some() {
                continue; // the slot is held by a live later winner: skip the pair
            }
            self.delete_oplog_seqs(&entry.oplog_seqs)?;
            self.update(snapshot.table(), snapshot.fields())?;
        }
        Ok(())
    }

    fn delete_oplog_seqs(&self, seqs: &[i64]) -> Result<()> {
        if seqs.is_empty() {
            return Ok(());
        }
        let placeholders = vec!["?"; seqs.len()].join(", ");
        self.db.execute(
            &format!("DELETE FROM oplog WHERE seq IN ({placeholders})"),
            rusqlite::params_from_iter(seqs.iter()),
        )?;
        Ok(())
    }

    /// The hlc_wall of the LIVE row — other than [snapshot]'s own id —
    /// currently holding [snapshot]'s single-track slot, or None when the
    /// slot is free or [snapshot] would not enter a slot at all (not a live
    /// accompaniment/reference track). The occupancy predicate mirrors the
    /// ux_track_slot partial unique index EXACTLY: this is the pre-flight
    /// check for every sweep-time restore, whose UPDATE runs outside any
    /// savepoint and would otherwise trip the index and fail the outer
    /// commit. The returned wall doubles as the tombstone time when the
    /// caller resolves the collision by restoring tombstoned
    /// (restore_prior_snapshot), matching resolve_slot's convention that a
    /// slot tombstone is dated to the winner's wall clock.
    fn slot_winner_wall(&self, snapshot: &SyncRow) -> Result<Option<i64>> {
        if snapshot.table() != "tracks" {
            return Ok(None);
        }
        if snapshot.field("deleted_at").is_some_and(|v| !v.is_null()) {
            return Ok(None);
        }
        let kind = match snapshot.field("kind") {
            Some(FieldValue::Text(k)) if k == "accompaniment" || k == "reference" => k,
            _ => return Ok(None),
        };
        let Some(FieldValue::Text(practice_id)) = snapshot.field("practice_id") else {
            return Ok(None);
        };
        single_or_none(self.db.query(
            "SELECT hlc_wall FROM tracks WHERE practice_id = ? AND kind = ? \
             AND deleted_at IS NULL AND id <> ?",
            [practice_id.as_str(), kind.as_str(), snapshot.id()?],
            |r| r.get::<_, i64>("hlc_wall"),
        )?)
    }

    /// A DERIVED stamp for a local resolution write, via
    /// [`HlcClock::derived`]: same (wall, counter) as [base], node extended
    /// by `'!' + device`. The extension out-ranks [base] (the write
    /// replicates) yet sorts below EVERY stamp above base — including the
    /// parked update that beat base by node alone, whose quarantine replay
    /// must land over this tombstone once its parent arrives. Deriving in
    /// node space (never counter space) is what guarantees that; see
    /// [`HlcClock::derived`]. Same derivation as the IntegrityChecker's
    /// repair stamps.
    fn derived_hlc(&self, base: &Hlc) -> Hlc {
        HlcClock::derived(base, self.clock.node())
    }

    /// Journals the slot-resolution tombstone so the resolution replicates,
    /// and returns the inserted entry's seq (the oplog's AUTOINCREMENT rowid
    /// — race-free inside the enclosing transaction). The seq lets the FK
    /// sweep DELETE exactly this entry if the record that triggered the
    /// resolution is later parked (see revert_slot_resolution). Runs inside
    /// the record's savepoint: the row write and its ledger entry commit (or
    /// roll back) together, like every local write.
    fn journal_slot_tombstone(&self, id: &str, hlc: &Hlc) -> Result<i64> {
        let entry = OplogRow {
            seq: 0,
            entity_table: "tracks".to_string(),
            entity_id: id.to_string(),
            op: op::TOMBSTONE.to_string(),
            hlc_packed: hlc.packed(),
            payload_hash: String::new(),
            synced: false,
            depends_on_asset_id: None,
            created_at: hlc.wall_ms,
        };
        self.db.with_conn(|c| entry.insert_new(c))
    }

    /// Localizes an inbound record's per-device bookkeeping before writing:
    /// drops the per-device columns and then force-sets the inbound
    /// overrides.
    ///
    /// The strip: the builder already omits per-device columns from the
    /// outbound projection, but a peer on an older build (or any
    /// non-conforming pusher) may still send them — applying them verbatim
    /// would overwrite THIS device's local bookkeeping (e.g. clobbering
    /// assets.local_rel_path so resolve() misses bytes that exist here).
    /// Stripped, they are ordinary missing columns: an UPDATE leaves the
    /// local values untouched and an INSERT takes the schema defaults.
    ///
    /// The overrides: some per-device state is DEDUCIBLE from the arrival
    /// itself — an assets row that came over the wire is by definition
    /// record-synced, and leaving an inserted row on the 'localOnly' schema
    /// default would hand the asset-GC's never-synced guard exactly the rows
    /// it must protect (see INBOUND_PER_DEVICE_OVERRIDES for the full
    /// rationale).
    fn localize_per_device(table: &str, columns: Fields) -> Fields {
        let per_device = per_device_columns(table);
        let overrides = inbound_per_device_overrides(table);
        if per_device.is_empty() && overrides.is_none() {
            return columns;
        }
        let mut out: Fields = columns
            .into_iter()
            .filter(|(k, _)| !per_device.contains(&k.as_str()))
            .collect();
        for (k, v) in overrides.unwrap_or_default() {
            out.insert((*k).to_string(), FieldValue::Text((*v).to_string()));
        }
        out
    }

    fn insert(&self, table: &str, columns: &Fields) -> Result<()> {
        let cols: Vec<&str> = columns.keys().map(String::as_str).collect();
        let placeholders = vec!["?"; cols.len()].join(", ");
        self.db.execute(
            &format!(
                "INSERT INTO {table} ({}) VALUES ({placeholders})",
                cols.join(", ")
            ),
            rusqlite::params_from_iter(columns.values()),
        )?;
        Ok(())
    }

    fn update(&self, table: &str, columns: &Fields) -> Result<()> {
        let cols: Vec<&str> = columns
            .keys()
            .map(String::as_str)
            .filter(|c| *c != "id")
            .collect();
        let set_clause = cols
            .iter()
            .map(|c| format!("{c} = ?"))
            .collect::<Vec<_>>()
            .join(", ");
        let null = FieldValue::Null;
        let mut params: Vec<&FieldValue> = cols.iter().filter_map(|c| columns.get(*c)).collect();
        params.push(columns.get("id").unwrap_or(&null));
        self.db.execute(
            &format!("UPDATE {table} SET {set_clause} WHERE id = ?"),
            rusqlite::params_from_iter(params),
        )?;
        Ok(())
    }
}
