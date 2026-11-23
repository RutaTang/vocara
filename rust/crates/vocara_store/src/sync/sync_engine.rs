//! The sync orchestrator: push and pull cycles, change-token cursors, the
//! quarantine replay and the debounced push scheduling (port of
//! `lib/data/sync/sync_engine.dart`).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use indexmap::IndexSet;
use parking_lot::{Condvar, FairMutex, Mutex};
use rusqlite::hooks::Action;
use rusqlite::Connection;
use vocara_model::diagnostics::DiagnosticsLog;

use crate::dart;
use crate::db::rows::SyncQuarantineRow;
use crate::db::schema::ALL_TABLES;
use crate::db::AppDatabase;
use crate::error::{Error, Result};
use crate::sync::integrity_checker::{IntegrityChecker, IntegrityViolation};
use crate::sync::oplog_drain::{OplogDrain, PendingOplogEntry};
use crate::sync::quarantine_store::QuarantineStore;
use crate::sync::sync_applier::{ApplyOptions, SyncApplier};
use crate::sync::sync_row::{FieldValue, SyncRow, SyncRowBuilder};
use crate::sync::sync_state_store::SyncStateStore;
use crate::sync::sync_transport::{
    FetchPage, PushAck, PushRowResult, SyncTransport, SyncTransportException, TransportError,
};

/// One "remote changes landed" notification, fired AFTER the page's
/// transaction committed (never from inside an open transaction) and after
/// the pull run's end-of-run integrity repair ran, so listeners always
/// reload an already-consistent view.
///
/// This event exists for FRESHNESS only, never for safety: three-way
/// reconcile (base hashes) already guarantees that an editor which misses
/// (or skips) a reload cannot revert or tombstone the synced-in rows on its
/// next save. View models therefore listen with tiny, heavily-guarded
/// handlers and simply skip the reload whenever the user is mid-anything.
/// (That is also why a run that aborts mid-backlog may drop the events of
/// its committed pages: the next successful run announces its own pages and
/// repairs everything; nothing is lost but a reload nudge.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteApplied {
    /// The (table, id) pairs the applied page carried, deduped, arrival
    /// order.
    pub rows: Vec<(String, String)>,
    /// Ids of every practice the page touched — directly (a practices row)
    /// or through a child row (tracks/interpretations/fx_buses carry
    /// practice_id on the wire; clips/notes/markers are resolved via their
    /// track_id with one local read AFTER the commit). A child whose track
    /// is unknown locally (quarantined / swept) resolves to nothing and is
    /// simply omitted.
    pub practice_ids: IndexSet<String>,
    /// Ids of every technique the page touched (techniques rows and
    /// technique_media child rows).
    pub technique_ids: IndexSet<String>,
}

impl RemoteApplied {
    /// Whether the event carries a row of [table].
    pub fn touches_table(&self, table: &str) -> bool {
        self.rows.iter().any(|(t, _)| t == table)
    }
}

/// A scheduled callback the engine can cancel (Dart's `Timer`).
pub trait SyncTimer: Send {
    /// Cancels the timer; a fired or cancelled timer ignores it.
    fn cancel(&self);
}

/// How a timer is minted — injectable so tests never arm a real timer.
/// [run] must be called at most once, after [delay], unless the timer is
/// cancelled first, and NEVER from inside the factory call itself.
pub type SyncTimerFactory =
    Arc<dyn Fn(Duration, Box<dyn FnOnce() + Send>) -> Box<dyn SyncTimer> + Send + Sync>;

/// The production timer: a thread that sleeps for the delay (waking early
/// only to exit when cancelled), then runs the callback on itself.
struct ThreadTimer {
    cancelled: Arc<(Mutex<bool>, Condvar)>,
}

impl SyncTimer for ThreadTimer {
    fn cancel(&self) {
        let (flag, wake) = &*self.cancelled;
        *flag.lock() = true;
        wake.notify_all();
    }
}

fn real_timer(delay: Duration, run: Box<dyn FnOnce() + Send>) -> Box<dyn SyncTimer> {
    let cancelled = Arc::new((Mutex::new(false), Condvar::new()));
    let state = cancelled.clone();
    let deadline = Instant::now() + delay;
    // A failed spawn leaves the timer inert: the journal entries it would
    // have pushed stay in the oplog for the next poke.
    let _ = std::thread::Builder::new()
        .name("vocara-sync-timer".to_string())
        .spawn(move || {
            let (flag, wake) = &*state;
            let mut done = flag.lock();
            while !*done {
                if wake.wait_until(&mut done, deadline).timed_out() {
                    break;
                }
            }
            if !*done {
                drop(done);
                run();
            }
        });
    Box::new(ThreadTimer { cancelled })
}

/// Why a pump run aborted (Dart's `Object? failure`): a transport failure
/// (or a response the engine refused as a protocol violation), or a store
/// failure (a database error, a page that failed to commit, a transport
/// that broke the token contract).
#[derive(Debug, thiserror::Error)]
pub enum SyncFailure {
    /// The transport failed or was refused.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The store failed, or a run-level invariant broke.
    #[error(transparent)]
    Store(#[from] Error),
}

impl SyncFailure {
    /// Whether this is a [`SyncTransportException`].
    pub fn is_transport_exception(&self) -> bool {
        matches!(self, SyncFailure::Transport(e) if e.is_transport_exception())
    }

    /// Whether this is a dead change token.
    pub fn is_token_expired(&self) -> bool {
        matches!(self, SyncFailure::Transport(e) if e.is_token_expired())
    }
}

impl From<SyncTransportException> for SyncFailure {
    fn from(e: SyncTransportException) -> Self {
        SyncFailure::Transport(TransportError::Transport(e))
    }
}

/// Diagnostics of one [`SyncEngine::pump_push`] run.
#[derive(Debug, Default)]
pub struct PushRun {
    /// Rows sent to (and accepted by) the transport across the whole run.
    pub pushed: usize,
    /// Asset-gated entries left in the oplog for a later run (see the
    /// engine docs on the asset gate).
    pub held_back: usize,
    /// The highest oplog sequence acknowledged (0 = nothing was acked).
    pub acked_through: i64,
    /// Set when the run aborted (transport failure, unresolved conflict):
    /// every unacked oplog entry is still in the ledger and re-pushes
    /// later.
    pub failure: Option<SyncFailure>,
}

impl PushRun {
    /// Whether the run aborted.
    pub fn failed(&self) -> bool {
        self.failure.is_some()
    }
}

impl fmt::Display for PushRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PushRun(pushed: {}, heldBack: {}, ackedThrough: {}, failure: {})",
            self.pushed,
            self.held_back,
            self.acked_through,
            self.failure
                .as_ref()
                .map_or_else(|| "null".to_string(), ToString::to_string)
        )
    }
}

/// Diagnostics of one [`SyncEngine::pump_pull`] run.
#[derive(Debug, Default)]
pub struct PullRun {
    /// Pages whose transaction (records + change token) committed.
    pub pages: usize,
    /// Records applied across the committed pages.
    pub applied: usize,
    /// Records that lost LWW across the committed pages.
    pub skipped: usize,
    /// Records parked at the drift gate across the committed pages.
    pub quarantined: usize,
    /// Per-record failures inside committed pages (quarantined to the
    /// store).
    pub failed_records: usize,
    /// Set when the run aborted before catching up (transport failure or a
    /// rolled-back page). The change token was NOT advanced past anything
    /// that failed to commit; the next run resumes exactly there.
    pub failure: Option<SyncFailure>,
}

impl PullRun {
    /// Whether the run aborted.
    pub fn failed(&self) -> bool {
        self.failure.is_some()
    }
}

impl fmt::Display for PullRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PullRun(pages: {}, applied: {}, skipped: {}, quarantined: {}, failedRecords: {}, \
             failure: {})",
            self.pages,
            self.applied,
            self.skipped,
            self.quarantined,
            self.failed_records,
            self.failure
                .as_ref()
                .map_or_else(|| "null".to_string(), ToString::to_string)
        )
    }
}

/// The repair the engine runs at the end of a pull run (the
/// [`IntegrityChecker`]'s `repair`, as a seam so a test can stand in for a
/// process that dies before it — Dart subclassed the checker).
pub trait IntegrityRepair: Send + Sync {
    /// Detects and repairs the store's invariants; returns what could not
    /// be repaired.
    fn repair(&self) -> Result<Vec<IntegrityViolation>>;
}

impl IntegrityRepair for IntegrityChecker {
    fn repair(&self) -> Result<Vec<IntegrityViolation>> {
        IntegrityChecker::repair(self)
    }
}

type Listener = Arc<dyn Fn() + Send + Sync>;

/// The Rust form of drift's `tableUpdates` stream, narrowed to what the
/// engine subscribes to: INSERTs into the oplog, announced once per
/// transaction when that transaction ENDS — at its COMMIT or its ROLLBACK,
/// never from inside it. Like drift's notification, a rolled-back write
/// still announces (harmlessly: its journal entry rolled back with the
/// data, so the push it pokes drains nothing).
///
/// Installed on the library's connection BEFORE it becomes an
/// [`AppDatabase`] (SQLite's update/commit/rollback hooks need the
/// connection exclusively): `OplogInserts::install(&mut conn)` then
/// `AppDatabase::from_connection(conn)`. Listeners run inside SQLite's hook,
/// so they must not touch the database — the engine's only arms its
/// debounce timers.
pub struct OplogInserts {
    pending: AtomicBool,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next_id: AtomicU64,
}

impl fmt::Debug for OplogInserts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OplogInserts")
            .field("listeners", &self.listeners.lock().len())
            .finish()
    }
}

impl OplogInserts {
    /// Installs the notifier's hooks on [conn] (replacing any update,
    /// commit or rollback hook it had).
    pub fn install(conn: &mut Connection) -> Result<Arc<OplogInserts>> {
        let this = Arc::new(OplogInserts {
            pending: AtomicBool::new(false),
            listeners: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        });
        let on_update = Arc::downgrade(&this);
        conn.update_hook(Some(
            move |action: Action, _db: &str, table: &str, _rowid: i64| {
                if action == Action::SQLITE_INSERT && table == "oplog" {
                    if let Some(this) = on_update.upgrade() {
                        this.pending.store(true, Ordering::SeqCst);
                    }
                }
            },
        ))?;
        let on_commit = Arc::downgrade(&this);
        conn.commit_hook(Some(move || {
            if let Some(this) = on_commit.upgrade() {
                this.transaction_ended();
            }
            false // never veto the commit
        }))?;
        let on_rollback = Arc::downgrade(&this);
        conn.rollback_hook(Some(move || {
            if let Some(this) = on_rollback.upgrade() {
                this.transaction_ended();
            }
        }))?;
        Ok(this)
    }

    fn transaction_ended(&self) {
        if !self.pending.swap(false, Ordering::SeqCst) {
            return;
        }
        let listeners: Vec<Listener> = self
            .listeners
            .lock()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for listener in listeners {
            listener();
        }
    }

    /// Registers [listener]; returns the id to unsubscribe with.
    pub fn subscribe(&self, listener: impl Fn() + Send + Sync + 'static) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.listeners.lock().push((id, Arc::new(listener)));
        id
    }

    /// Removes the listener [id] (a no-op for an unknown id).
    pub fn unsubscribe(&self, id: u64) {
        self.listeners.lock().retain(|(i, _)| *i != id);
    }
}

/// Called after each committed page that actually changed rows
/// (post-commit, post-repair).
pub type RemoteAppliedSink = Arc<dyn Fn(&RemoteApplied) + Send + Sync>;

/// Test-only seam: runs INSIDE each page's transaction, after the token
/// write. A failing hook kills the transaction, proving atomicity.
pub type PageCommitHook = Arc<dyn Fn(&FetchPage) -> Result<()> + Send + Sync>;

/// The collaborators of a [`SyncEngine`] (Dart's required constructor
/// arguments).
pub struct SyncEngineParts {
    /// The library database.
    pub db: Arc<AppDatabase>,
    /// The remote door pulled pages go through.
    pub applier: Arc<SyncApplier>,
    /// The oplog reader/acker.
    pub drain: OplogDrain,
    /// The outbound row projection.
    pub builder: Arc<SyncRowBuilder>,
    /// The change token and friends.
    pub state: SyncStateStore,
    /// The end-of-run repair.
    pub integrity: Arc<dyn IntegrityRepair>,
    /// The server seam.
    pub transport: Arc<dyn SyncTransport>,
}

/// The tunables of a [`SyncEngine`] (Dart's optional constructor
/// arguments).
#[derive(Clone)]
pub struct SyncEngineOptions {
    /// Fired after each committed page that actually changed rows
    /// (post-commit, post-repair). Wired by Persistence to its broadcast
    /// stream.
    pub on_remote_applied: Option<RemoteAppliedSink>,
    /// Debounce: a push fires after this long with no further local
    /// writes...
    pub quiet_window: Duration,
    /// ...but never later than this after the first unflushed write.
    pub max_wait: Duration,
    /// Transport batch cap (a conservative per-request row limit any backend
    /// can honor).
    pub push_batch_limit: usize,
    /// Raw oplog rows read per drain (see [`OplogDrain::read_pending`]).
    pub drain_limit: i64,
    /// How timers are minted (None: real timer threads).
    pub timers: Option<SyncTimerFactory>,
    /// Test-only seam: runs INSIDE each page's transaction, after the token
    /// write.
    pub debug_before_page_commit: Option<PageCommitHook>,
}

impl Default for SyncEngineOptions {
    fn default() -> Self {
        SyncEngineOptions {
            on_remote_applied: None,
            quiet_window: Duration::from_secs(2),
            max_wait: Duration::from_secs(30),
            push_batch_limit: 400,
            drain_limit: 500,
            timers: None,
            debug_before_page_commit: None,
        }
    }
}

/// What only a pump run reads and writes, held under the engine's serial
/// lock.
struct RunState {
    /// Once-per-instance durable marker that a transport exists (see
    /// [`SyncStateStore::TRANSPORT_SEEN_KEY`]); the asset GC's
    /// pending-journal guard keys off it. Cached so steady-state pumps cost
    /// no extra query.
    transport_seen_marked: bool,
    /// Set when a run that had already committed applied pages (or settled
    /// parked entries) aborted before its end-of-run repair: the next
    /// successful run repairs even if it applies nothing itself.
    ///
    /// Starts TRUE, so every engine lifetime repairs on its first successful
    /// run. The pages and their change token are durable the moment they
    /// commit, but this flag is not: a crash or quit between a page commit
    /// and the repair transaction lost the debt with the process, the next
    /// run (token already past those pages) applied nothing and skipped the
    /// repair, and the violations those pages delivered (a live child under
    /// a tombstoned parent, a dangling pointer, a bus cycle the native
    /// engine rejects) stood until some later pull happened to apply a row.
    /// Repairing once per boot settles whatever such a run left behind
    /// without persisting anything: it covers every case a durable debt
    /// marker would (and needs no write per page to keep one), and the
    /// repair is idempotent — on a consistent store it is one scan that
    /// writes nothing.
    repair_debt: bool,
}

/// The debounce timers and the oplog subscription.
#[derive(Default)]
struct Schedule {
    quiet: Option<Box<dyn SyncTimer>>,
    max: Option<Box<dyn SyncTimer>>,
    subscription: Option<(Arc<OplogInserts>, u64)>,
}

struct Inner {
    db: Arc<AppDatabase>,
    applier: Arc<SyncApplier>,
    drain: OplogDrain,
    builder: Arc<SyncRowBuilder>,
    state: SyncStateStore,
    integrity: Arc<dyn IntegrityRepair>,
    transport: Arc<dyn SyncTransport>,
    timers: SyncTimerFactory,
    debug_before_page_commit: Option<PageCommitHook>,
    on_remote_applied: Option<RemoteAppliedSink>,
    quiet_window: Duration,
    max_wait: Duration,
    push_batch_limit: usize,
    drain_limit: i64,
    /// Serializes push and pull: the two must never interleave (a pull's
    /// integrity repair journals, which pokes a push — that push must queue
    /// behind the pull, not race it). FIFO-fair, like Dart's chained
    /// futures: runs proceed in the order they were requested.
    serial: FairMutex<RunState>,
    schedule: Mutex<Schedule>,
    disposed: AtomicBool,
}

/// What one replay pass did.
struct Replay {
    /// Rows that landed.
    count: usize,
    /// Entries that SETTLED — landed or skipped.
    settled: usize,
    /// The landed rows, for the announcement.
    rows: Vec<SyncRow>,
}

/// The running totals of one pull run.
#[derive(Default)]
struct PullTotals {
    pages: usize,
    applied: usize,
    skipped: usize,
    quarantined: usize,
    failed_records: usize,
    /// Rows the quarantine replay landed this run (they count toward the
    /// repair gate, not toward the page counters — they are not page rows).
    replayed: usize,
    /// Parked entries the replay SETTLED this run — landed or LWW-skipped. A
    /// skip changes no row, but it ends the deferral of any verdict the
    /// checker held back for that parked state, so it gates the repair too.
    settled: usize,
}

/// The running totals of one push run.
#[derive(Default)]
struct PushTotals {
    pushed: usize,
    held_back: usize,
    acked_through: i64,
}

/// Drift's `getSingleOrNull`.
fn single_or_none<T>(mut rows: Vec<T>) -> Result<Option<T>> {
    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.pop()),
        _ => Err(Error::State("Too many elements".to_string())),
    }
}

/// The Dart runtime type a store error would have had, for diagnostics.
fn error_kind(e: &Error) -> &'static str {
    match e {
        Error::Sqlite(_) => "SqliteException",
        Error::Io(_) => "FileSystemException",
        Error::InvalidData(_) => "InvalidDataException",
        Error::InvalidArgument(_) => "ArgumentError",
        Error::State(_) => "StateError",
        Error::Format(_) => "FormatException",
        Error::Migration { .. } => "MigrationException",
        Error::HlcDrift(_) => "HlcDriftException",
    }
}

/// The FK sweep's park reason, `fk-orphan: parent <table>/<id> missing`
/// (`^fk-orphan: parent ([a-z_]+)/(.+) missing$`, `.` excluding line
/// terminators): the parent's table and id.
fn parse_orphan_reason(reason: &str) -> Option<(&str, &str)> {
    let rest = reason.strip_prefix("fk-orphan: parent ")?;
    let slash = rest.find('/')?;
    let table = &rest[..slash];
    if table.is_empty() || !table.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        return None;
    }
    let id = rest[slash + 1..].strip_suffix(" missing")?;
    if id.is_empty() || id.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some((table, id))
}

/// The sync orchestrator: drains the local oplog into
/// [`SyncTransport::push_rows`] and pulls [`SyncTransport::fetch_changes`]
/// pages through the [`SyncApplier`].
///
/// HARD INVARIANTS (each has a test):
///  * The change token advances IFF the page it covers committed — the
///    token write runs inside the applier's outer transaction via
///    `before_commit`.
///  * Applying remote changes journals nothing (the applier's contract), so
///    pull can never echo rows back into push. (The applier's one carve-out
///    — the single-slot resolution tombstone — is a new derived local
///    decision, bounded to one write per losing row, never an echo.)
///  * At-least-once tolerant: re-delivered rows/pages are LWW no-ops.
///  * Listeners are only notified AFTER transactions close
///    (`on_remote_applied` fires post-commit; the oplog poke fires when its
///    transaction ends — a rollback's spurious poke drains to a no-op, see
///    [`SyncEngine::start`]).
///  * Data safety over progress: any failure leaves the oplog entries
///    unacked and the token unmoved; the next pump simply retries.
///  * Quarantine is a parking lot, never a black hole: the change token
///    advances past a quarantined record's page when its batch commits, and
///    the server only re-delivers seq > token — so nothing would ever
///    deliver a parked row again. Every pull run therefore REPLAYS the
///    parking lot through the applier with fresh drift/schema checks
///    (`replay_quarantine`); a recovered clock or an upgraded schema lets
///    the rows in, and a row that lost LWW meanwhile is settled and dropped.
///
/// ASSET GATE: an oplog entry carrying
/// [`PendingOplogEntry::depends_on_asset_id`] whose asset bytes have no
/// `asset_sync` upload marker is HELD BACK — the row must never reach a peer
/// before the bytes it references are fetchable. Held entries stay in the
/// oplog (the ack watermark stops just short of the first held sequence), so
/// rows after them may be re-pushed on the next run — harmless under LWW.
/// P2's byte uploader writes the marker and thereby releases the gate; until
/// then nothing in production journals a dependency, so the gate is
/// exercised only by tests.
///
/// SCHEDULING: tests drive [`SyncEngine::pump_push`]/[`SyncEngine::pump_pull`]
/// directly — no timers exist unless [`SyncEngine::start`] is called (or
/// [`SyncEngine::poke`]). start subscribes to oplog inserts (announced only
/// when the owning transaction ends) and debounces into a push: the quiet
/// window of no further writes, capped at max wait from the first unflushed
/// write. The timer factory is injectable so even the debounce is testable
/// without real time.
///
/// Every pump blocks its caller (Dart awaited them); runs from different
/// threads queue on one FIFO lock, so push and pull never interleave.
pub struct SyncEngine {
    inner: Arc<Inner>,
}

impl fmt::Debug for SyncEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncEngine").finish_non_exhaustive()
    }
}

impl SyncEngine {
    /// Bounded conflict resolution: how many times one
    /// [`SyncEngine::pump_push`] run re-pushes rows whose local HLC beat the
    /// server row before giving up (leaving the oplog unacked for the next
    /// run).
    pub const MAX_CONFLICT_RETRIES: usize = 3;

    /// An engine over [parts], tuned by [options].
    pub fn new(parts: SyncEngineParts, options: SyncEngineOptions) -> Self {
        SyncEngine {
            inner: Arc::new(Inner {
                db: parts.db,
                applier: parts.applier,
                drain: parts.drain,
                builder: parts.builder,
                state: parts.state,
                integrity: parts.integrity,
                transport: parts.transport,
                timers: options.timers.unwrap_or_else(|| Arc::new(real_timer)),
                debug_before_page_commit: options.debug_before_page_commit,
                on_remote_applied: options.on_remote_applied,
                quiet_window: options.quiet_window,
                max_wait: options.max_wait,
                push_batch_limit: options.push_batch_limit,
                drain_limit: options.drain_limit,
                serial: FairMutex::new(RunState {
                    transport_seen_marked: false,
                    repair_debt: true,
                }),
                schedule: Mutex::new(Schedule::default()),
                disposed: AtomicBool::new(false),
            }),
        }
    }

    /// The remote door the engine pulls through.
    pub fn applier(&self) -> &Arc<SyncApplier> {
        &self.inner.applier
    }

    /// Runs [op] in the same serial section as [`SyncEngine::pump_push`] and
    /// [`SyncEngine::pump_pull`], so it can never interleave with a pull
    /// run's pages. For maintenance that judges the whole store —
    /// Persistence.repairLibrary's integrity pass — which must not see a
    /// run's half-delivered aggregate (see [`SyncEngine::pump_pull`] for why
    /// repairing one manufactures intent). [op] must not pump this engine.
    pub fn run_exclusive<T>(&self, op: impl FnOnce() -> T) -> T {
        let _serial = self.inner.serial.lock();
        op()
    }

    // --- Automatic mode (production wiring; optional) ----------------------

    /// Subscribes the debounced push to oplog inserts ([inserts], installed
    /// on this library's connection). The notifier announces an insert only
    /// once the transaction that made it ENDS (commit or rollback), so a
    /// poke can never observe an open transaction. A rolled-back save still
    /// announces, but that poke is harmless by construction: the journal
    /// entry rolled back with the data, so the pump drains nothing (tested).
    /// The ack's deletes are not inserts and announce nothing. A second call
    /// (or one after [`SyncEngine::dispose`]) is a no-op.
    pub fn start(&self, inserts: &Arc<OplogInserts>) {
        if self.inner.disposed.load(Ordering::SeqCst) {
            return;
        }
        let mut schedule = self.inner.schedule.lock();
        if schedule.subscription.is_some() {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let id = inserts.subscribe(move || {
            if let Some(inner) = weak.upgrade() {
                Inner::poke(&inner);
            }
        });
        schedule.subscription = Some((inserts.clone(), id));
    }

    /// Nudges the engine: schedules a debounced [`SyncEngine::pump_push`].
    /// Called by the oplog subscription after every committed local write;
    /// safe to call manually.
    pub fn poke(&self) {
        Inner::poke(&self.inner);
    }

    /// Cancels the subscription and any pending timers. Idempotent. In-flight
    /// pumps finish on their own (they are already crash-tolerant, so there
    /// is nothing to interrupt safely — we just stop scheduling new ones).
    pub fn dispose(&self) {
        self.inner.disposed.store(true, Ordering::SeqCst);
        let subscription = {
            let mut schedule = self.inner.schedule.lock();
            if let Some(t) = schedule.quiet.take() {
                t.cancel();
            }
            if let Some(t) = schedule.max.take() {
                t.cancel();
            }
            schedule.subscription.take()
        };
        if let Some((inserts, id)) = subscription {
            inserts.unsubscribe(id);
        }
    }

    // --- Push ----------------------------------------------------------------

    /// Drains pending oplog entries, pushes the CURRENT state of their rows,
    /// and acks exactly what was handled. Loops while it makes full
    /// progress, so a large backlog drains in one call. Any failure aborts
    /// with the ledger intact (minus already-acked prefixes, which the
    /// server durably accepted).
    pub fn pump_push(&self) -> PushRun {
        self.inner.pump_push()
    }

    // --- Pull ----------------------------------------------------------------

    /// Fetches remote pages and applies each one atomically WITH its change
    /// token (one transaction per page, via the applier's `before_commit`
    /// hook). The quarantine parking lot is replayed at the START of the run
    /// and again once caught up (see `replay_quarantine` for why pull owns
    /// this). After the run has caught up: ONE integrity repair in its own
    /// transaction, then the buffered `on_remote_applied` events. Stops at
    /// the first failure with the token exactly at the last committed page.
    ///
    /// The repair deliberately runs at the END of the run, never between
    /// pages: mid-run the local store may hold a HALF-DELIVERED aggregate (a
    /// practice row whose interpretation is on the next page), and repairing
    /// that transient state manufactures intent — e.g. the active-interp
    /// scalar heal nulls the pointer with an HLC above the row's that can
    /// later beat a legitimate tombstone and resurrect a deleted practice
    /// fleet-wide (found by the convergence simulation, seeds 0xc1c2b7 and
    /// 0x5eed0009). Once the run is caught up, the delivered prefix is
    /// causally complete (the pusher uploads parents before children and
    /// whole transactions in order) — but the APPLIED prefix need not be:
    /// the applier may have PARKED a row's newer state (drift gate, newer
    /// schema, an FK-sweep park-and-restore) while applying its children, and
    /// the table then still holds a state the quarantine already supersedes.
    /// The checker therefore defers every verdict that depends on a row with
    /// a newer replayable parked state (see the IntegrityChecker's
    /// parked-newer guard); the replay that later lands or settles that state
    /// opens the run's repair gate, so the deferred verdict is re-judged in
    /// that same run. What the checker repairs is thus only genuinely broken.
    /// Residual exposure: a pusher whose SINGLE transaction exceeds one push
    /// chunk (400 rows) can still be observed half-uploaded by a caught-up
    /// puller; P0 aggregates are far smaller, and the derived repair stamps
    /// cap the blast radius to bookkeeping that any later real write
    /// out-ranks.
    pub fn pump_pull(&self) -> PullRun {
        self.inner.pump_pull()
    }
}

impl Drop for SyncEngine {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl Inner {
    fn poke(this: &Arc<Inner>) {
        if this.disposed.load(Ordering::SeqCst) {
            return;
        }
        let mut schedule = this.schedule.lock();
        if let Some(t) = schedule.quiet.take() {
            t.cancel();
        }
        schedule.quiet = Some((this.timers)(this.quiet_window, Self::flusher(this)));
        if schedule.max.is_none() {
            schedule.max = Some((this.timers)(this.max_wait, Self::flusher(this)));
        }
    }

    fn flusher(this: &Arc<Inner>) -> Box<dyn FnOnce() + Send> {
        let weak: Weak<Inner> = Arc::downgrade(this);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.flush();
            }
        })
    }

    fn flush(&self) {
        {
            let mut schedule = self.schedule.lock();
            if let Some(t) = schedule.quiet.take() {
                t.cancel();
            }
            if let Some(t) = schedule.max.take() {
                t.cancel();
            }
        }
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        // Dart: unawaited(pumpPush()). The timer's own thread runs it; the
        // run reports through the oplog it leaves behind.
        let _ = self.pump_push();
    }

    fn ensure_transport_seen(&self, rs: &mut RunState) -> Result<()> {
        if rs.transport_seen_marked {
            return Ok(());
        }
        self.state.mark_transport_seen()?;
        rs.transport_seen_marked = true;
        Ok(())
    }

    fn pump_push(&self) -> PushRun {
        let mut rs = self.serial.lock();
        let mut totals = PushTotals::default();
        let outcome = self.push_run(&mut rs, &mut totals);
        // On failure (transport failure / exhausted conflict retries):
        // nothing past the last durable ack is lost — it is all still
        // journaled and re-pushes later.
        PushRun {
            pushed: totals.pushed,
            held_back: totals.held_back,
            acked_through: totals.acked_through,
            failure: outcome.err(),
        }
    }

    fn push_run(
        &self,
        rs: &mut RunState,
        totals: &mut PushTotals,
    ) -> std::result::Result<(), SyncFailure> {
        // Durable BEFORE the first wire call of this library's life: once a
        // push may have reached a server, the asset GC's pending-journal
        // guard must engage — a crash mid-push must never leave a server-held
        // row in a library that still claims no transport ever existed.
        self.ensure_transport_seen(rs)?;
        let mut last_max_seq = 0;
        loop {
            let batch = self.drain.read_pending(self.drain_limit)?;
            if batch.is_empty() {
                break;
            }
            // No forward progress is possible when the same window comes back
            // (its head is asset-gated); stop rather than spin.
            if batch.max_seq == last_max_seq {
                break;
            }
            last_max_seq = batch.max_seq;

            // Partition on the asset gate (entries are seq-ascending).
            let mut ready: Vec<&PendingOplogEntry> = Vec::new();
            let mut first_held_seq: Option<i64> = None;
            let mut held = 0;
            for e in &batch.entries {
                let gated = match &e.depends_on_asset_id {
                    Some(asset_id) => !self.asset_bytes_uploaded(asset_id)?,
                    None => false,
                };
                if gated {
                    held += 1;
                    first_held_seq.get_or_insert(e.seq);
                } else {
                    ready.push(e);
                }
            }
            totals.held_back += held;

            // Push CURRENT row state (the drain already collapsed to latest
            // per row; the builder re-reads, so a save that landed after the
            // drain simply makes the pushed state even fresher — and its own
            // oplog entry, with a higher seq, survives the ack below).
            let mut built: Vec<SyncRow> = Vec::new();
            for e in ready {
                // A vanished row (hard-deleted before it ever synced, e.g. an
                // FK-swept draft) has nothing to push; its entry is still
                // acked.
                if let Some(row) = self.builder.build(&e.table, &e.id)? {
                    built.push(row);
                }
            }
            // Upload parents before children — the FetchPage ordering
            // contract. The drain's latest-op order is CHILD-first for a
            // cascade delete (children are journaled before their parent),
            // and a receiver that has never seen the aggregate applies those
            // rows as fresh inserts: child-first across a page boundary would
            // strand a child whose parent hasn't arrived yet, and the
            // applier's FK sweep would drop it with the change token already
            // advanced — a permanently lost row (convergence simulation, seed
            // 0x5eed0001). A stable (rank, journal-order) sort fixes the
            // order; the ack cover below is seq-based and unaffected. Rows
            // are full LWW states, so reordering within one push never
            // changes any outcome, only arrival order.
            let ranks = if built.len() > 1 {
                Some(self.builder.table_ranks()?)
            } else {
                None
            };
            let rank_of = |table: &str| {
                ranks
                    .as_ref()
                    .and_then(|r| r.get(table).copied())
                    .unwrap_or(0)
            };
            built.sort_by_key(|row| rank_of(row.table()));
            for chunk in built.chunks(self.push_batch_limit.max(1)) {
                totals.pushed += self.push_chunk_resolving_conflicts(chunk)?;
            }

            // Ack exactly what this read covered — held back at the first
            // gated entry so it is never deleted un-pushed. Everything acked
            // was either accepted by the server or deliberately dropped
            // (server newer).
            let ack_to = first_held_seq.map_or(batch.max_seq, |s| s - 1);
            if ack_to > 0 {
                self.drain.ack(ack_to)?;
                totals.acked_through = ack_to;
            }
            if ack_to < batch.max_seq {
                break; // gated head: later entries wait
            }
        }
        Ok(())
    }

    /// Pushes one chunk, resolving conflicted rows per the transport
    /// contract: local HLC strictly greater than the server row's → re-push
    /// (bounded by [`SyncEngine::MAX_CONFLICT_RETRIES`]); otherwise drop
    /// ours — the newer server state arrives via pull, and LWW makes
    /// dropping safe. Returns rows accepted.
    ///
    /// Every ack is VALIDATED against the rows just pushed before anything
    /// acts on it (see validate_ack), and every conflicted result's
    /// serverRow is validated before it may drive the retry-or-drop decision
    /// (see validate_conflict) — each retry round validates its own attempt
    /// list.
    ///
    /// Every DECISION binds to the ATTEMPT row — the row from our own push
    /// array — never to the echoed [`PushRowResult::row`], which is wire data
    /// a hostile server controls and is used ONLY to find which attempt a
    /// result answers. Trusted for anything more, a forged OLDER echo paired
    /// with an internally-consistent serverRow stamped between the echo and
    /// the true attempt passes every per-result check, the engine judges
    /// "server newer", drops the local edit, and acks away its oplog entry —
    /// silent data loss on nothing but a mangled ack.
    ///
    /// Handled assets rows are marked record-synced here, inside the round
    /// and therefore strictly BEFORE the caller's oplog ack (see
    /// mark_asset_records_synced for why that order matters).
    fn push_chunk_resolving_conflicts(
        &self,
        chunk: &[SyncRow],
    ) -> std::result::Result<usize, SyncFailure> {
        let mut accepted = 0;
        let mut attempt: Vec<SyncRow> = chunk.to_vec();
        let mut tries = 0;
        while !attempt.is_empty() {
            let ack = self.transport.push_rows(&attempt)?;
            Self::validate_ack(&attempt, &ack)?;
            // Correlate each result back to the attempt's OWN row. Keys are
            // unique here — the drain collapses to one entry per (table, id),
            // and the retry list below inherits that — so the map can never
            // conflate two attempts; refuse loudly if that invariant ever
            // breaks rather than resolve results ambiguously.
            let mut attempt_by_key: HashMap<(String, String), usize> = HashMap::new();
            for (i, row) in attempt.iter().enumerate() {
                let key = (row.table().to_string(), row.id()?.to_string());
                if attempt_by_key.insert(key, i).is_some() {
                    return Err(Error::State(format!(
                        "push attempt carries duplicate rows for {}/{}; ack results cannot be \
                         correlated unambiguously",
                        row.table(),
                        row.id()?
                    ))
                    .into());
                }
            }
            let ours_of = |r: &PushRowResult| -> std::result::Result<&SyncRow, SyncFailure> {
                let key = (r.row.table().to_string(), r.row.id()?.to_string());
                // validate_ack proved the 1:1 identity cover, so the lookup
                // cannot miss.
                let index = attempt_by_key.get(&key).copied().ok_or_else(|| {
                    Error::State(format!(
                        "push ack result for {}/{} has no attempt",
                        key.0, key.1
                    ))
                })?;
                Ok(&attempt[index])
            };
            // Same discipline as validate_ack: the WHOLE round is vetted
            // before anything in it takes effect, so an invalid conflict
            // claim can't land after part of the round was already
            // counted/marked.
            for r in &ack.results {
                if !r.accepted {
                    Self::validate_conflict(ours_of(r)?, r)?;
                }
            }
            let mut retry: Vec<SyncRow> = Vec::new();
            // Assets rows the server now durably HOLDS after this round —
            // accepted ones, and ones dropped because the server's version is
            // newer (the record has crossed the wire either way).
            let mut synced_asset_ids: Vec<String> = Vec::new();
            for r in &ack.results {
                // From here on the echo is never read again: identity, HLC
                // and asset marking all come from the attempt row.
                let ours = ours_of(r)?;
                if r.accepted {
                    accepted += 1;
                    if ours.table() == "assets" {
                        synced_asset_ids.push(ours.id()?.to_string());
                    }
                    continue;
                }
                // validate_conflict proved the server row exists.
                let server = r.server_row.as_ref().ok_or_else(|| {
                    Error::State("conflicted push result without a serverRow".to_string())
                })?;
                if dart::compare_strings(ours.hlc_packed()?, server.hlc_packed()?).is_gt() {
                    // Unreachable against a validated ack (validate_conflict
                    // requires the server row STRICTLY newer than the
                    // attempt); kept as the documented resolution policy so
                    // that a deliberate future relaxation of the conflict
                    // contract makes "local wins" re-push instead of silently
                    // dropping.
                    retry.push(ours.clone());
                } else if ours.table() == "assets" {
                    // Server holds something newer — drop ours. For an assets
                    // row that still counts as synced: the server holds the
                    // record, so another device may reference the asset.
                    synced_asset_ids.push(ours.id()?.to_string());
                }
            }
            self.mark_asset_records_synced(&synced_asset_ids)?;
            if retry.is_empty() {
                break;
            }
            if tries + 1 >= SyncEngine::MAX_CONFLICT_RETRIES {
                // Give up this run WITHOUT acking: the rows stay journaled and
                // the next run retries from scratch (data safety over
                // progress).
                return Err(SyncTransportException::new(format!(
                    "push conflicts unresolved after {} attempts ({} rows)",
                    SyncEngine::MAX_CONFLICT_RETRIES,
                    retry.len()
                ))
                .into());
            }
            attempt = retry;
            tries += 1;
        }
        Ok(accepted)
    }

    /// Refuses a [`PushAck`] whose result set is not EXACTLY the rows that
    /// were pushed: same count, and a 1:1 identity match on (table, id) —
    /// order may differ, duplicates are invalid. Without this the ack was
    /// trusted blind: a buggy (or hostile) server returning an empty or
    /// wrong result list still let the caller ack — i.e. DELETE — the whole
    /// drained oplog range, silently losing every row the server never
    /// actually stored.
    ///
    /// The failure is a [`SyncTransportException`], so pump_push handles it
    /// like any transport failure: the run reports failed, the oplog ack for
    /// this window never runs, and every unacked entry re-pushes on a later
    /// run — verified to hold, since the ack in push_run sits after all
    /// chunk pushes and this failure escapes them.
    fn validate_ack(pushed: &[SyncRow], ack: &PushAck) -> std::result::Result<(), SyncFailure> {
        if ack.results.len() != pushed.len() {
            return Err(SyncTransportException::new(format!(
                "push ack has {} results for {} pushed rows",
                ack.results.len(),
                pushed.len()
            ))
            .into());
        }
        // Multiset match: every result must consume exactly one pushed row.
        // The drain collapses to one entry per row, so pushed keys are unique
        // in practice — but counting keeps the check exact even if that
        // changes.
        let mut expected: HashMap<(String, String), usize> = HashMap::new();
        for row in pushed {
            *expected
                .entry((row.table().to_string(), row.id()?.to_string()))
                .or_insert(0) += 1;
        }
        for r in &ack.results {
            let key = (r.row.table().to_string(), r.row.id()?.to_string());
            let n = expected.get(&key).copied().unwrap_or(0);
            if n == 0 {
                return Err(SyncTransportException::new(format!(
                    "push ack carries a result for {}/{}, which was not in the pushed rows (or \
                     is a duplicate result)",
                    key.0, key.1
                ))
                .into());
            }
            expected.insert(key, n - 1);
        }
        Ok(())
    }

    /// Refuses a `conflicted` [`PushRowResult`] whose serverRow cannot be an
    /// honest CAS rejection of [attempt] — the row from OUR push array this
    /// result was correlated to, never the echoed `r.row` (which a hostile
    /// server controls; see push_chunk_resolving_conflicts). The serverRow
    /// decides retry-or-drop; trusted blind, a lazy lie could make the
    /// engine silently drop — and then ack away — a local edit the server
    /// never stored. Four checks, all cheap:
    ///
    ///  * it must EXIST — a conflict claim with no server state is
    ///    meaningless (inconsistent through [`PushRowResult`]'s own
    ///    constructors, but a decoding transport seam must not be able to
    ///    smuggle one through);
    ///  * it must carry the SAME (table, id) as the attempt row — checked on
    ///    the raw field map, so a mistyped id fails as a transport violation
    ///    rather than a type error;
    ///  * it must be internally CONSISTENT: its HLC quad must re-pack to its
    ///    hlc_packed — the exact rule [`SyncRow::from_json`] enforces at the
    ///    pull door, applied via the shared
    ///    [`SyncRow::check_hlc_consistency`] so the two doors can never
    ///    diverge;
    ///  * its hlcPacked must be STRICTLY greater than the ATTEMPT row's: a
    ///    conforming server accepts a push it holds nothing newer than
    ///    (docs/sync-protocol.md), so a CAS reject by an older-or-equal row is
    ///    nonsensical — and the canonical low-cost lie is echoing the pushed
    ///    row straight back as "the server's". Comparing against the echo
    ///    instead would let a forged OLDER echo smuggle a mid-HLC serverRow
    ///    through this very check.
    ///
    /// Any violation fails with [`SyncTransportException`]: the run fails,
    /// nothing is acked, and every journaled entry re-pushes on a later run.
    /// This closes every low-cost lie; a server that fabricates an
    /// internally-consistent, strictly NEWER row is indistinguishable from
    /// one legitimately holding it, and is therefore outside the client's
    /// defensible model — the documented trust boundary
    /// (docs/sync-protocol.md "Trust model"), equivalent to the server
    /// withholding rows on pull.
    fn validate_conflict(
        attempt: &SyncRow,
        r: &PushRowResult,
    ) -> std::result::Result<(), SyncFailure> {
        let attempt_id = attempt.id()?;
        let Some(server) = &r.server_row else {
            return Err(SyncTransportException::new(format!(
                "conflicted push result for {}/{attempt_id} carries no serverRow",
                attempt.table()
            ))
            .into());
        };
        if server.table() != attempt.table()
            || server.field("id") != Some(&FieldValue::Text(attempt_id.to_string()))
        {
            return Err(SyncTransportException::new(format!(
                "conflicted push result for {}/{attempt_id} carries a serverRow for {}/{}",
                attempt.table(),
                server.table(),
                server
                    .field("id")
                    .map_or_else(|| "null".to_string(), FieldValue::dart_to_string)
            ))
            .into());
        }
        if let Err(e) = SyncRow::check_hlc_consistency(server.fields())
            .and_then(|()| SyncRow::check_id_shapes(server.table(), server.fields()))
        {
            return Err(SyncTransportException::with_cause(
                format!(
                    "conflicted serverRow for {}/{attempt_id} is internally inconsistent",
                    attempt.table()
                ),
                e,
            )
            .into());
        }
        // check_hlc_consistency proved hlc_packed is a String, so the getter
        // below cannot fail.
        let server_packed = server.hlc_packed()?;
        let attempt_packed = attempt.hlc_packed()?;
        if dart::compare_strings(server_packed, attempt_packed).is_le() {
            return Err(SyncTransportException::new(format!(
                "conflicted serverRow for {}/{attempt_id} is not strictly newer than the pushed \
                 row (\"{server_packed}\" <= \"{attempt_packed}\"); a conforming server accepts \
                 a push it holds nothing newer than",
                attempt.table()
            ))
            .into());
        }
        Ok(())
    }

    /// Marks the given assets rows' record_sync_state 'synced' — per-device
    /// bookkeeping ONLY, so deliberately a direct update touching nothing but
    /// that one column: never through SyncWriter (marking must not journal —
    /// an oplog echo would re-push forever) and never touching updated_at or
    /// the HLC columns (bookkeeping must not look like an edit or shift
    /// LWW).
    ///
    /// Called BEFORE the batch's oplog ack so a crash between the two errs
    /// toward 'synced': the asset-GC's never-synced guard then merely
    /// refuses to sweep a row the server may not hold — conservative —
    /// instead of sweeping (and fleet-wide tombstoning) a row the server
    /// DOES hold.
    fn mark_asset_records_synced(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        self.db.execute(
            &format!(
                "UPDATE \"assets\" SET \"record_sync_state\" = 'synced' WHERE \"id\" IN \
                 ({placeholders})"
            ),
            rusqlite::params_from_iter(ids.iter()),
        )?;
        Ok(())
    }

    /// Whether [asset_id]'s BYTES are durably uploaded: its content hash has
    /// an asset_sync marker. A missing asset row counts as not uploaded
    /// (held back — the safe side).
    fn asset_bytes_uploaded(&self, asset_id: &str) -> Result<bool> {
        let row = single_or_none(self.db.query(
            "SELECT 1 AS ok FROM asset_sync s JOIN assets a ON a.content_hash = s.hash \
             WHERE a.id = ?",
            [asset_id],
            |r| r.get::<_, i64>("ok"),
        )?)?;
        Ok(row.is_some())
    }

    fn pump_pull(&self) -> PullRun {
        let mut rs = self.serial.lock();
        let mut totals = PullTotals::default();
        // Events buffered until the end-of-run repair has run (listeners must
        // always reload an already-consistent view).
        let mut announcements: Vec<RemoteApplied> = Vec::new();
        let outcome = self.pull_run(&mut rs, &mut totals, &mut announcements);
        if outcome.is_err() && (totals.applied > 0 || totals.replayed > 0 || totals.settled > 0) {
            // Aborted mid-backlog: committed pages exist without their repair
            // (and without their announcements — freshness only, see
            // RemoteApplied). Carry the repair forward so the next successful
            // run settles it.
            rs.repair_debt = true;
        }
        PullRun {
            pages: totals.pages,
            applied: totals.applied,
            skipped: totals.skipped,
            quarantined: totals.quarantined,
            failed_records: totals.failed_records,
            failure: outcome.err(),
        }
    }

    fn pull_run(
        &self,
        rs: &mut RunState,
        totals: &mut PullTotals,
        announcements: &mut Vec<RemoteApplied>,
    ) -> std::result::Result<(), SyncFailure> {
        // A pull proves a transport exists just as much as a push does (see
        // push_run): record it before the first wire call.
        self.ensure_transport_seen(rs)?;
        // Replay the quarantine parking lot FIRST: the token is already past
        // those rows, so no fetch will ever re-deliver them — a recovered
        // clock or an upgraded schema must be able to let them in even when
        // this run fetches nothing new (the fresh-device-with-a-slow-clock
        // case parks EVERY first-pull row while the token marches to head).
        let first = self.replay_quarantine()?;
        totals.replayed = first.count;
        totals.settled = first.settled;
        // Replayed rows are announced like page rows: the primary recovery
        // scenario (a whole first-pull backlog parked, then un-parked by a
        // corrected clock) can land an entire library in a run that fetches
        // nothing — without an event, open screens would never refresh.
        if !first.rows.is_empty() {
            announcements.push(self.describe_rows(&first.rows)?);
        }
        let mut token = self.state.change_token()?;
        // One token reset per run: a transport signalling an EXPIRED change
        // token (e.g. HTTP 410 from a server that compacted its change log
        // past it) is recovered by clearing the persisted token and
        // restarting the pull from the beginning of the zone, in this same
        // run. Safe by construction: the applier is idempotent entity-level
        // LWW, so re-applying the whole zone is a stream of clean skips for
        // rows we already hold and normal applies for the rest. Bounded to ONE
        // reset — a transport that rejects even a fresh (None) token is
        // treated as an ordinary failed run instead of spinning
        // clear-and-refetch forever.
        let mut token_reset_used = false;
        loop {
            let page = match self.transport.fetch_changes(token.as_deref()) {
                Ok(page) => page,
                Err(TransportError::TokenExpired(e)) => {
                    if token.is_none() || token_reset_used {
                        return Err(TransportError::TokenExpired(e).into());
                    }
                    token_reset_used = true;
                    self.state.clear_change_token()?;
                    token = None;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let unmoved = token.as_deref() == Some(page.next_token.as_str());
            if page.rows.is_empty() && page.rejects.is_empty() {
                // Caught up. Persist a moved empty-page token (a server may
                // advance the token without changes); there is no batch to be
                // atomic with.
                if !unmoved {
                    self.state.set_change_token(&page.next_token)?;
                }
                if !page.has_more {
                    break;
                }
                if unmoved {
                    break; // defensive: refuse to spin
                }
                token = Some(page.next_token);
                continue;
            }
            let before_commit = || -> Result<()> {
                self.state.set_change_token(&page.next_token)?;
                if let Some(hook) = &self.debug_before_page_commit {
                    hook(&page)?;
                }
                Ok(())
            };
            let result = self.applier.apply_with(
                &page.rows,
                ApplyOptions {
                    before_commit: Some(&before_commit),
                    // Parked inside the page's own transaction, so a record
                    // that never decoded is atomic with the token that moves
                    // past it — the server only re-delivers seq > token, so
                    // anything the token passes and nothing recorded is gone
                    // for good.
                    rejects: &page.rejects,
                    own_rows_only: false,
                },
            );
            if !result.committed {
                // The page (and its token) rolled back in full: stop, retry
                // later.
                return Err(
                    Error::State("pull page failed to commit; token unmoved".into()).into(),
                );
            }
            totals.pages += 1;
            totals.applied += result.applied;
            totals.skipped += result.skipped;
            totals.quarantined += result.quarantined;
            totals.failed_records += result.failed;
            if result.applied > 0 {
                announcements.push(self.describe_rows(&page.rows)?);
            }
            // An unmoved token WITHOUT has_more is the legal at-least-once
            // redelivery (the page just re-applied as clean LWW skips, and the
            // token write was a same-value no-op): done, like any final page.
            if !page.has_more {
                break;
            }
            // TRUST MODEL (sync_transport): a page bearing rows or rejects
            // MUST advance the token. Looping on an unmoved one would refetch
            // this identical page forever — every record a clean skip,
            // has_more true, no failure surfaced — silently wedging sync (and
            // every push queued behind the serial lock) in an invisible hot
            // loop. The empty branch above already refuses to spin; this is
            // the rows-bearing analogue, and like every other
            // cheaply-verifiable transport violation it fails the run loudly.
            if unmoved {
                return Err(Error::State(format!(
                    "pull transport violation: a page with {} row(s) and {} reject(s) returned \
                     an unmoved change token with hasMore set",
                    page.rows.len(),
                    page.rejects.len()
                ))
                .into());
            }
            token = Some(page.next_token);
        }
        // Replay once more after catching up: this run's successful receives
        // may have raised the clock's max-wall-ever-seen (or delivered a
        // missing parent), unlocking rows parked just moments ago.
        let second = self.replay_quarantine()?;
        totals.replayed += second.count;
        totals.settled += second.settled;
        if !second.rows.is_empty() {
            announcements.push(self.describe_rows(&second.rows)?);
        }
        if totals.applied > 0 || totals.replayed > 0 || totals.settled > 0 || rs.repair_debt {
            // Caught up: repair the invariants closure ONCE, in one
            // transaction so each repair write commits with its journal
            // entry, then announce.
            self.db.transaction(|| self.integrity.repair())?;
            rs.repair_debt = false;
        }
        for event in announcements.drain(..) {
            self.fire_remote_applied(&event);
        }
        Ok(())
    }

    /// Re-attempts every quarantined record through the NORMAL applier path
    /// and returns how many actually applied. Bounded and idempotent — safe
    /// to run every cycle: one single-record apply per parked row, nothing
    /// spins.
    ///
    /// Each replay gets a FRESH gate pass inside the applier: the HLC drift
    /// check runs against the clock's CURRENT state (a corrected local
    /// clock, or honest stamps received since parking raising
    /// max-wall-ever-seen, un-park drift rejects) and the write re-runs
    /// against the CURRENT schema (an app upgrade un-parks
    /// forward-incompatible rows).
    ///
    /// Ordering: parent tables before child tables (the same parent-first
    /// contract as push, via [`SyncRowBuilder::table_ranks`], so a parked
    /// child lands after its parked parent within one pass), original parked
    /// order within a rank (the quarantine store lists parked-at ascending).
    ///
    /// Interaction with rows applied since parking is safe by construction —
    /// row-level LWW makes a stale replay lose cleanly: the applier compares
    /// the parked hlc_packed against the CURRENT row and skips the write
    /// when the parked state is older. Outcomes per row:
    ///  * applied — the record landed; its parking-lot entry is deleted.
    ///  * skipped — lost LWW to something newer applied since parking; the
    ///    parked state is obsolete, so its entry is deleted too. (The delete
    ///    runs after the apply's commit; a crash in between just re-replays
    ///    it as another clean skip next run.)
    ///  * quarantined / failed — still rejected; the applier itself re-parked
    ///    the latest state, so the entry stays for a later cycle. A stamp
    ///    still past the drift bound is not even applied: it could only
    ///    re-park.
    ///  * settled but unremovable — the apply committed and only the
    ///    bookkeeping delete failed; the entry stays parked and the run
    ///    continues, because the replay is idempotent and next cycle settles
    ///    it as a clean skip.
    ///
    /// An undecodable payload is kept parked for manual review (it can never
    /// re-apply, but dropping it silently is exactly what quarantine exists
    /// to prevent).
    ///
    /// Returns how many rows landed (with the rows themselves for the
    /// announcement) and how many entries SETTLED — landed or skipped —
    /// which the caller's repair gate needs: either outcome ends a deferral
    /// the IntegrityChecker held for that parked state.
    fn replay_quarantine(&self) -> Result<Replay> {
        let nothing = || Replay {
            count: 0,
            settled: 0,
            rows: Vec::new(),
        };
        let Some(store) = self.applier.quarantine() else {
            return Ok(nothing());
        };
        // Only what can land: a park whose verdict is a function of its
        // payload alone was decoded, applied in a transaction and re-parked
        // unchanged by every pull, however many a peer had left there.
        let parked = store.list_replayable()?;
        if parked.is_empty() {
            return Ok(nothing());
        }
        let ranks = if parked.len() > 1 {
            Some(self.builder.table_ranks()?)
        } else {
            None
        };
        let rank_of = |table: &str| {
            ranks
                .as_ref()
                .and_then(|r| r.get(table).copied())
                .unwrap_or(0)
        };
        let mut ordered = parked;
        // Stable: the parked-at order survives within a rank.
        ordered.sort_by_key(|q| rank_of(&q.tbl));
        let mut applied = 0;
        let mut settled = 0;
        // Rows that actually landed, for the caller's RemoteApplied
        // announcement (skips changed nothing locally, so they are not
        // announced).
        let mut landed: Vec<SyncRow> = Vec::new();
        for q in ordered {
            let Ok(row) = SyncRow::decode(&q.payload) else {
                continue; // undecodable: keep parked (see doc comment)
            };
            // Still past the drift bound: the apply could only park it again,
            // so spare it the transaction (and the rewrite). It cannot have
            // lost LWW meanwhile either — the drift reference covers every
            // stamp the tables hold (the clock merges each one it stores, and
            // boot seeds from them), so a stamp beyond it out-ranks whatever
            // is held for its row.
            let Ok(hlc) = row.hlc() else {
                continue;
            };
            if self.applier.clock().assert_plausible(&hlc).is_err() {
                continue;
            }
            // An FK orphan whose parent is still absent can only park again:
            // spare it the transaction and its FK sweep, which every pull
            // otherwise paid per orphan for as long as the parent stayed
            // away. Parents replay first (the rank order above), so one
            // landing earlier in this pass is already there.
            if self.orphan_waits_on_parent(&q, &row)? {
                continue;
            }
            let result = self.applier.apply_with(
                std::slice::from_ref(&row),
                ApplyOptions {
                    own_rows_only: true,
                    ..ApplyOptions::default()
                },
            );
            if !result.committed {
                break; // storage trouble — retry next run
            }
            if result.applied > 0 || result.skipped > 0 {
                // Conditional on the exact state we REPLAYED: a different
                // state for this (table, id) can be parked while the apply
                // above ran, and an unconditional delete would drop it
                // unreplayed (TOCTOU).
                //
                // A failure here is deliberately swallowed. The apply has
                // already committed, so the settle is pure bookkeeping:
                // leaving the entry parked costs one more clean LWW skip next
                // cycle. Letting it escape would abort the pull run BEFORE the
                // change token is even fetched — this replay runs first — so a
                // single undeletable row would stall every future pull, and
                // the rows that DID land would lose their announcement with
                // it. The opposite of the applier's own park failure, which
                // must fail its batch: there a dropped write LOSES a record,
                // here it only delays one.
                if let Err(e) =
                    store.delete_if_unchanged(&q.tbl, &q.id, &q.remote_hlc_packed, &q.payload)
                {
                    DiagnosticsLog::instance().log(
                        "sync",
                        &format!(
                            "quarantine settle failed ({}); the entry replays next cycle",
                            error_kind(&e)
                        ),
                    );
                }
                applied += result.applied;
                settled += 1;
                if result.applied > 0 {
                    landed.push(row);
                }
            }
        }
        Ok(Replay {
            count: applied,
            settled,
            rows: landed,
        })
    }

    /// Whether parked entry [q], holding [row], is an FK orphan that can
    /// only park again, so its replay can wait. All of:
    ///   * it sits in the row's sweep slot. An fk-orphan entry an older build
    ///     parked under the bare row id moves there on its first replay (see
    ///     [`QuarantineStore::put_swept`]), and must still get that replay;
    ///   * the local row is not already at or past its stamp. One the row
    ///     has moved past (the author pointed it elsewhere since) replays as
    ///     a clean LWW skip and settles, whatever its parent;
    ///   * the parent its reason names (the FK sweep's wording, see
    ///     SyncApplier) still has no row, tombstones included: the
    ///     constraint checks existence, not liveness.
    ///
    /// False whenever that cannot be told, so the entry replays as before.
    fn orphan_waits_on_parent(&self, q: &SyncQuarantineRow, row: &SyncRow) -> Result<bool> {
        if q.id != QuarantineStore::sweep_slot_id(row.id()?) {
            return Ok(false);
        }
        let Some((parent_table, parent_id)) = parse_orphan_reason(&q.reason) else {
            return Ok(false);
        };
        if parent_id == "unknown" {
            return Ok(false);
        }
        if !ALL_TABLES.contains(&parent_table) || !ALL_TABLES.contains(&row.table()) {
            return Ok(false);
        }
        let caught_up = single_or_none(self.db.query(
            &format!(
                "SELECT 1 FROM {} WHERE id = ? AND hlc_packed >= ?",
                row.table()
            ),
            [row.id()?, q.remote_hlc_packed.as_str()],
            |r| r.get::<_, i64>(0),
        )?)?;
        if caught_up.is_some() {
            return Ok(false);
        }
        let parent = single_or_none(self.db.query(
            &format!("SELECT 1 FROM {parent_table} WHERE id = ?"),
            [parent_id],
            |r| r.get::<_, i64>(0),
        )?)?;
        Ok(parent.is_none())
    }

    fn fire_remote_applied(&self, event: &RemoteApplied) {
        let Some(sink) = &self.on_remote_applied else {
            return;
        };
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        sink(event);
    }

    /// Builds the [`RemoteApplied`] event for committed rows (a fetched page
    /// or a batch of replayed quarantine rows): the (table, id) pairs plus
    /// the practice/technique ids they roll up to. Runs AFTER the commit;
    /// the one extra read (track_id -> practice_id) is over ids only.
    fn describe_rows(&self, rows: &[SyncRow]) -> Result<RemoteApplied> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut practice_ids: IndexSet<String> = IndexSet::new();
        let mut technique_ids: IndexSet<String> = IndexSet::new();
        let mut track_ids: IndexSet<String> = IndexSet::new();
        let text = |row: &SyncRow, column: &str| match row.field(column) {
            Some(FieldValue::Text(s)) => Some(s.clone()),
            _ => None,
        };
        for row in rows {
            let pair = (row.table().to_string(), row.id()?.to_string());
            if seen.insert(pair.clone()) {
                pairs.push(pair);
            }
            match row.table() {
                "practices" => {
                    practice_ids.insert(row.id()?.to_string());
                }
                "tracks" | "interpretations" | "fx_buses" => {
                    if let Some(pid) = text(row, "practice_id") {
                        practice_ids.insert(pid);
                    }
                }
                "clips" | "practice_notes" | "technique_markers" => {
                    if let Some(tid) = text(row, "track_id") {
                        track_ids.insert(tid);
                    }
                }
                "techniques" => {
                    technique_ids.insert(row.id()?.to_string());
                }
                "technique_media" => {
                    if let Some(tid) = text(row, "technique_id") {
                        technique_ids.insert(tid);
                    }
                }
                _ => {}
            }
        }
        // Chunked like every other id-set read: one IN over a big page's
        // tracks passed SQLite's variable limit, and this runs after the
        // commit — the run failed and the screens never heard of what it had
        // applied.
        let track_ids: Vec<String> = track_ids.into_iter().collect();
        for chunk in in_chunks(&track_ids) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let parents: Vec<String> = self.db.query(
                &format!("SELECT practice_id FROM tracks WHERE id IN ({placeholders})"),
                rusqlite::params_from_iter(chunk.iter()),
                |r| r.get::<_, String>("practice_id"),
            )?;
            practice_ids.extend(parents);
        }
        Ok(RemoteApplied {
            rows: pairs,
            practice_ids,
            technique_ids,
        })
    }
}

/// Bound on the ids one `IN (...)` query binds, far below SQLite's
/// host-parameter ceiling (a private copy of `AssetStore.inQueryChunk`).
const IN_QUERY_CHUNK: usize = 500;

/// [items] in consecutive slices of at most [`IN_QUERY_CHUNK`], for set
/// reads (a private copy of `AssetStore.inChunks`).
fn in_chunks<T>(items: &[T]) -> std::slice::Chunks<'_, T> {
    items.chunks(IN_QUERY_CHUNK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_reasons_parse_like_the_dart_regex() {
        assert_eq!(
            parse_orphan_reason("fk-orphan: parent practices/ABC missing"),
            Some(("practices", "ABC"))
        );
        assert_eq!(
            parse_orphan_reason("fk-orphan: parent fx_buses/a/b missing missing"),
            Some(("fx_buses", "a/b missing"))
        );
        assert_eq!(
            parse_orphan_reason("fk-orphan: parent Practices/X missing"),
            None
        );
        assert_eq!(
            parse_orphan_reason("fk-orphan: parent practices/ missing"),
            None
        );
        assert_eq!(parse_orphan_reason("fk-orphan: parent /X missing"), None);
        assert_eq!(
            parse_orphan_reason("fk-orphan: parent practices/X\nY missing"),
            None
        );
        assert_eq!(
            parse_orphan_reason("fk-orphan: parent practices/X missing!"),
            None
        );
        assert_eq!(parse_orphan_reason("hlc-drift: x"), None);
    }

    #[test]
    fn in_chunks_slices_at_the_bound() {
        let items: Vec<usize> = (0..1001).collect();
        let sizes: Vec<usize> = in_chunks(&items).map(<[usize]>::len).collect();
        assert_eq!(sizes, vec![500, 500, 1]);
        assert_eq!(in_chunks::<usize>(&[]).count(), 0);
    }
}
