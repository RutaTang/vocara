//! The device-to-server seam of the sync engine, and the in-memory cloud the
//! tests sync through (port of `lib/data/sync/sync_transport.dart` and
//! `test/support/fake_sync_transport.dart`).

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::dart;
use crate::sync::sync_row::SyncRow;

/// A transient transport failure (offline, server unreachable, request timed
/// out). The `SyncEngine` treats any failure of a transport call as "try
/// again later": the oplog and the change token are left exactly as they
/// were, so no data is ever lost to a flaky network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncTransportException {
    /// What failed.
    pub message: String,
    /// The underlying failure, rendered, when there was one.
    pub cause: Option<String>,
}

impl SyncTransportException {
    /// An exception without a cause.
    pub fn new(message: impl Into<String>) -> Self {
        SyncTransportException {
            message: message.into(),
            cause: None,
        }
    }

    /// An exception caused by [cause].
    pub fn with_cause(message: impl Into<String>, cause: impl fmt::Display) -> Self {
        SyncTransportException {
            message: message.into(),
            cause: Some(cause.to_string()),
        }
    }
}

impl fmt::Display for SyncTransportException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.cause {
            None => write!(f, "SyncTransportException({})", self.message),
            Some(c) => write!(f, "SyncTransportException({}, cause: {c})", self.message),
        }
    }
}

impl std::error::Error for SyncTransportException {}

/// Raised by [`SyncTransport::fetch_changes`] when the server can no longer
/// honor the resume token it was given — the canonical case is HTTP 410 Gone
/// from a server that compacted its change log past the token. Typed, unlike
/// the generic retry-later [`SyncTransportException`], because retrying a
/// dead token retries forever; the recovery is a POLICY decision that lives
/// ABOVE the transport: the `SyncEngine` clears the persisted token and
/// re-pulls the zone from scratch, which is safe because the applier is
/// idempotent entity-level LWW. A transport must only raise this — never
/// clear the token itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncTokenExpiredException {
    /// Why the token is dead.
    pub message: String,
}

impl SyncTokenExpiredException {
    /// The exception.
    pub fn new(message: impl Into<String>) -> Self {
        SyncTokenExpiredException {
            message: message.into(),
        }
    }
}

impl fmt::Display for SyncTokenExpiredException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SyncTokenExpiredException({})", self.message)
    }
}

impl std::error::Error for SyncTokenExpiredException {}

/// Everything a transport call can fail with (a Dart transport could throw
/// anything; these are the cases the engine tells apart, plus the rest).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// A transient failure, or a response the engine refused as a protocol
    /// violation ([`SyncTransportException`]).
    #[error(transparent)]
    Transport(#[from] SyncTransportException),
    /// The resume token is dead ([`SyncTokenExpiredException`]).
    #[error(transparent)]
    TokenExpired(#[from] SyncTokenExpiredException),
    /// Any other failure a transport raised (Dart: whatever else it threw,
    /// e.g. `UnimplementedError`), rendered.
    #[error("{0}")]
    Other(String),
}

impl TransportError {
    /// Whether this is a [`SyncTransportException`].
    pub fn is_transport_exception(&self) -> bool {
        matches!(self, TransportError::Transport(_))
    }

    /// Whether this is a [`SyncTokenExpiredException`].
    pub fn is_token_expired(&self) -> bool {
        matches!(self, TransportError::TokenExpired(_))
    }
}

/// The per-row outcome of a [`SyncTransport::push_rows`] call.
///
/// [`PushRowResult::accepted`] means the server durably stored the row.
/// Otherwise the push was rejected because the server already holds a
/// STRICTLY NEWER version of the record by hlcPacked — the only comparison
/// the server ever makes (docs/sync-protocol.md) — and
/// [`PushRowResult::server_row`] carries that current server state so the
/// caller can resolve. Resolution is the ENGINE's job, not the transport's,
/// and the engine VALIDATES the claim before acting on it — see
/// [`SyncTransport::push_rows`].
///
/// [`PushRowResult::row`] is a wire ECHO: whatever the transport decoded
/// from the response, nominally the row as pushed. The engine uses it ONLY
/// to correlate the result with the matching row of its own push array (a
/// 1:1 identity match on (table, id)); every decision — conflict validation,
/// retry-or-drop, asset marking — then reads that attempt row, never the
/// echo. This holds even for a future production transport that GUARANTEES
/// the echo is taken verbatim from the request array: the client
/// deliberately does not rely on any such guarantee, because the echo
/// crosses the trust boundary either way and a forged echo would otherwise
/// re-anchor the engine's HLC comparisons.
///
/// The fields are public, so a decoding transport can represent what a
/// response claims even when it is inconsistent (a conflict with no server
/// row): the engine refuses such a result rather than misreading it. The
/// two constructors build the consistent shapes.
#[derive(Debug, Clone, PartialEq)]
pub struct PushRowResult {
    /// The wire echo of the pushed row — correlation key ONLY.
    pub row: SyncRow,
    /// The server's current record when the push conflicted; None when
    /// accepted.
    pub server_row: Option<SyncRow>,
    /// Whether the server stored the row (Dart: `serverRow == null`).
    pub accepted: bool,
}

impl PushRowResult {
    /// The server stored [row].
    pub fn accepted(row: SyncRow) -> Self {
        PushRowResult {
            row,
            server_row: None,
            accepted: true,
        }
    }

    /// The server holds [server_row], strictly newer than [row].
    pub fn conflicted(row: SyncRow, server_row: SyncRow) -> Self {
        PushRowResult {
            row,
            server_row: Some(server_row),
            accepted: false,
        }
    }
}

/// The result of one [`SyncTransport::push_rows`] call: one
/// [`PushRowResult`] per pushed row, in the SAME order as the input list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PushAck {
    /// The per-row results.
    pub results: Vec<PushRowResult>,
}

impl PushAck {
    /// An ack carrying [results].
    pub fn new(results: Vec<PushRowResult>) -> Self {
        PushAck { results }
    }

    /// Whether every row was accepted.
    pub fn all_accepted(&self) -> bool {
        self.results.iter().all(|r| r.accepted)
    }

    /// The conflicted results.
    pub fn conflicts(&self) -> impl Iterator<Item = &PushRowResult> {
        self.results.iter().filter(|r| !r.accepted)
    }
}

/// A record of a page that could not be DECODED into a [`SyncRow`] — its
/// raw payload plus why. Kept separate from [`FetchPage::rows`] because such
/// a record has no id, table or stamp to route by: it never became a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRecord {
    /// The record exactly as it arrived, so the parking lot holds the
    /// evidence.
    pub payload: String,
    /// Why decoding refused it (a format error's message).
    pub reason: String,
}

impl RejectedRecord {
    /// A rejected record.
    pub fn new(payload: impl Into<String>, reason: impl Into<String>) -> Self {
        RejectedRecord {
            payload: payload.into(),
            reason: reason.into(),
        }
    }
}

/// One page of remote changes from [`SyncTransport::fetch_changes`].
#[derive(Debug, Clone, PartialEq)]
pub struct FetchPage {
    /// The changed rows in SERVER order (a parent row is always uploaded
    /// before its children by the pushing device, so server order preserves
    /// parent-before-child across page boundaries).
    pub rows: Vec<SyncRow>,
    /// Records of this page that failed to DECODE. A transport decodes per
    /// record and puts the failures here rather than choosing between
    /// failing the whole page (one bad record bricks sync, the exact outcome
    /// the applier's per-record quarantine exists to prevent) and dropping
    /// them silently (the change token advances past a record nothing
    /// recorded). The engine parks them as dead letters — unreplayable by
    /// construction, but reviewable, and accounted for.
    pub rejects: Vec<RejectedRecord>,
    /// The opaque token to persist ATOMICALLY with applying [rows]; passing
    /// it to the next [`SyncTransport::fetch_changes`] resumes after this
    /// page.
    pub next_token: String,
    /// Whether more pages are immediately available after [next_token].
    pub has_more: bool,
}

impl FetchPage {
    /// A page without rejects.
    pub fn new(rows: Vec<SyncRow>, next_token: impl Into<String>, has_more: bool) -> Self {
        FetchPage {
            rows,
            rejects: Vec::new(),
            next_token: next_token.into(),
            has_more,
        }
    }
}

/// The device-to-server seam of the sync engine — the interface a real
/// backend client implements in Phase 1 (an HTTP client against the Vocara
/// sync server; the wire contract lives in docs/sync-protocol.md). P0 ships
/// only the in-memory fake used by tests ([`FakeSyncTransport`]).
///
/// DELIVERY CONTRACT: everything here is AT-LEAST-ONCE, never exactly-once.
/// A crash between a successful push and the oplog ack re-pushes the same
/// rows; a crash between applying a fetched page and persisting its token
/// re-fetches (and re-applies) the same page; the server itself may
/// re-deliver a page. All of that is safe by construction: rows are full row
/// states keyed by (table, id) and resolved by entity-level HLC
/// Last-Writer-Wins, so a duplicate delivery is a no-op. Implementations
/// must therefore make each call atomic-ish but need NOT dedupe.
///
/// TOKENS are opaque strings minted by the transport
/// ([`FetchPage::next_token`]). The engine persists them verbatim and never
/// inspects them. A transport must advance the token whenever it returns
/// rows; returning an unchanged token WITH an empty row list is the "caught
/// up" signal. A transport that can no longer honor a token (e.g. the
/// server compacted its change log past it) must fail with
/// [`SyncTokenExpiredException`] — clearing the token and re-pulling from
/// scratch is a policy decision that lives above the transport (the
/// `SyncEngine`'s pull loop implements it).
///
/// Calls block (Dart awaited them); the engine makes them from whichever
/// thread runs a pump.
pub trait SyncTransport: Send + Sync {
    /// Uploads full row states. Returns one result per row, in input order.
    ///
    /// A row the server already holds in a STRICTLY NEWER version comes
    /// back as [`PushRowResult::conflicted`] with the server's current
    /// record — a conforming server ACCEPTS a push it holds nothing newer
    /// than (an older or equal stored row never conflicts; equal is the
    /// idempotent re-push). The ENGINE resolves, after validating the claim
    /// (same identity, internally consistent HLC, strictly newer — anything
    /// else fails the run as a transport violation): a valid conflict means
    /// the server row wins, so the local push is dropped and the newer state
    /// arrives via [`SyncTransport::fetch_changes`]. (The LWW re-push path
    /// for a local-newer conflict is retained in the engine but is
    /// unreachable against a conforming server.)
    ///
    /// Fails (e.g. [`SyncTransportException`]) on transport failure; the
    /// caller must then leave its ledger untouched and retry later.
    fn push_rows(&self, rows: &[SyncRow]) -> Result<PushAck, TransportError>;

    /// Fetches changes after [since_token] (None = from the beginning of the
    /// zone). See the trait docs for the token contract.
    fn fetch_changes(&self, since_token: Option<&str>) -> Result<FetchPage, TransportError>;

    /// P2 (asset bytes sync) — uploads the file at [file_path] keyed by its
    /// content hash. Defined now so the engine's asset gate has its release
    /// counterpart on the seam; NOT implemented by the P0 fake.
    fn push_asset_bytes(&self, content_hash: &str, file_path: &Path) -> Result<(), TransportError>;

    /// P2 (asset bytes sync) — downloads the bytes for [content_hash] into
    /// [dest_path]. Defined now for the same reason as
    /// [`SyncTransport::push_asset_bytes`]; NOT implemented by the P0 fake.
    fn fetch_asset_bytes(&self, content_hash: &str, dest_path: &Path)
        -> Result<(), TransportError>;
}

/// One accepted row in the fake cloud's ordered log.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeCloudEntry {
    /// Monotonically increasing server sequence (1-based). The change token
    /// is simply the highest seq a fetch has covered, as a string.
    pub seq: usize,
    /// The accepted row.
    pub row: SyncRow,
}

#[derive(Debug, Default)]
struct CloudState {
    log: Vec<FakeCloudEntry>,
    latest: HashMap<(String, String), SyncRow>,
}

/// The shared in-memory "cloud" N [`FakeSyncTransport`]s (one per simulated
/// device) push to and pull from: an append-only, seq-ordered log of
/// accepted rows plus the latest accepted record per (table, id) for
/// conflict checks.
///
/// DETERMINISTIC by construction: no wall clock, no randomness — the only
/// state transitions are the calls the tests (and the engines under test)
/// make, in the order they make them.
#[derive(Debug, Default)]
pub struct FakeCloud {
    state: Mutex<CloudState>,
}

/// The (table, id) key of a row whose id is a string ("" otherwise: the fake
/// keys whatever it is handed, like a server keying by the raw field).
fn row_key(row: &SyncRow) -> (String, String) {
    (
        row.table().to_string(),
        row.id().map(str::to_string).unwrap_or_default(),
    )
}

impl FakeCloud {
    /// An empty cloud.
    pub fn new() -> Arc<FakeCloud> {
        Arc::new(FakeCloud::default())
    }

    /// Accepts one row: appends to the log and replaces the current record.
    /// Also used by tests to seed server state directly.
    pub fn accept(&self, row: SyncRow) -> FakeCloudEntry {
        let mut s = self.state.lock();
        let entry = FakeCloudEntry {
            seq: s.log.len() + 1,
            row: row.clone(),
        };
        s.log.push(entry.clone());
        s.latest.insert(row_key(&row), row);
        entry
    }

    /// The server's current record per (table, id): the LAST ACCEPTED push,
    /// like a real sync server — it knows change tags, not HLCs, so it never
    /// resolves conflicts itself.
    pub fn server_row_for(&self, table: &str, id: &str) -> Option<SyncRow> {
        self.state
            .lock()
            .latest
            .get(&(table.to_string(), id.to_string()))
            .cloned()
    }

    /// The whole accepted log, in seq order.
    pub fn log(&self) -> Vec<FakeCloudEntry> {
        self.state.lock().log.clone()
    }

    /// The highest seq accepted so far.
    pub fn latest_seq(&self) -> usize {
        self.state.lock().log.len()
    }
}

/// A push parked by [`FakeSyncTransport::set_hold_pushes`], waiting for the
/// test to release it.
struct HeldPush {
    rows: Vec<SyncRow>,
    ack: Mutex<Option<PushAck>>,
    landed: Condvar,
}

/// Transforms the next push's ack (see [`FakeSyncTransport::set_mangle_next_ack`]).
pub type AckMangler = Box<dyn FnOnce(&[SyncRow], PushAck) -> PushAck + Send>;

/// In-memory [`SyncTransport`] over a shared [`FakeCloud`] — one instance
/// per simulated device. Fault-injection knobs (all OFF by default, all
/// driven explicitly by tests — nothing is time- or randomness-based):
///
///  * partitioned — every call fails with [`SyncTransportException`]
///    (device offline).
///  * hold pushes — push_rows parks instead of completing; the test calls
///    [`FakeSyncTransport::release_held_pushes`] to land the rows and
///    complete the acks (models delivery delay; lets a test interleave work
///    "during" a push, the push running on another thread).
///  * page size — fetch_changes page size (has_more paging).
///  * [`FakeSyncTransport::inject_conflict_once`] — the next push of that
///    (table, id) is CAS-checked against the server's CURRENT row and
///    rejected with it iff that row is strictly newer by hlcPacked, exactly
///    the one conflict a conforming server may raise
///    (docs/sync-protocol.md); against an older-or-equal held row (or no row
///    at all) the injection is consumed but the push is accepted. A
///    NON-conforming conflict (older/equal/mismatched serverRow) is modeled
///    with the ack mangler instead — the engine must refuse it.
///  * redeliver next fetches — the next N fetch_changes calls re-serve the
///    previously served page verbatim (same rows, same next token): the
///    at-least-once duplicate-delivery case.
///  * expire next fetches — the next N fetch_changes calls fail with
///    [`SyncTokenExpiredException`] (the server compacted its change log
///    past the token, HTTP 410). By default only NON-null tokens expire — a
///    from-scratch re-pull is honored, like a real server; set "expire fresh
///    tokens too" to reject null tokens as well (exercises the engine's
///    refuse-to-spin guard).
pub struct FakeSyncTransport {
    cloud: Arc<FakeCloud>,
    name: String,
    partitioned: AtomicBool,
    page_size: AtomicUsize,
    hold_pushes: AtomicBool,
    redeliver_next_fetches: AtomicUsize,
    expire_next_fetches: AtomicUsize,
    expire_fresh_tokens_too: AtomicBool,
    mangle_next_ack: Mutex<Option<AckMangler>>,
    held_pushes: Mutex<Vec<Arc<HeldPush>>>,
    conflict_once: Mutex<HashMap<(String, String), usize>>,
    last_served_page: Mutex<Option<FetchPage>>,
    pending_rejects: Mutex<Vec<RejectedRecord>>,
    push_calls: AtomicUsize,
    fetch_calls: AtomicUsize,
    pushed_batches: Mutex<Vec<Vec<SyncRow>>>,
}

impl fmt::Debug for FakeSyncTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeSyncTransport")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl FakeSyncTransport {
    /// A transport named [name] (for assertion messages) over [cloud].
    pub fn new(cloud: Arc<FakeCloud>, name: impl Into<String>) -> Self {
        FakeSyncTransport {
            cloud,
            name: name.into(),
            partitioned: AtomicBool::new(false),
            page_size: AtomicUsize::new(200),
            hold_pushes: AtomicBool::new(false),
            redeliver_next_fetches: AtomicUsize::new(0),
            expire_next_fetches: AtomicUsize::new(0),
            expire_fresh_tokens_too: AtomicBool::new(false),
            mangle_next_ack: Mutex::new(None),
            held_pushes: Mutex::new(Vec::new()),
            conflict_once: Mutex::new(HashMap::new()),
            last_served_page: Mutex::new(None),
            pending_rejects: Mutex::new(Vec::new()),
            push_calls: AtomicUsize::new(0),
            fetch_calls: AtomicUsize::new(0),
            pushed_batches: Mutex::new(Vec::new()),
        }
    }

    /// The shared cloud.
    pub fn cloud(&self) -> &Arc<FakeCloud> {
        &self.cloud
    }

    /// The label.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Takes the device offline (every call fails) or back online.
    pub fn set_partitioned(&self, partitioned: bool) {
        self.partitioned.store(partitioned, Ordering::SeqCst);
    }

    /// Sets the fetch page size (default 200).
    pub fn set_page_size(&self, page_size: usize) {
        self.page_size.store(page_size, Ordering::SeqCst);
    }

    /// Parks every following push until [`FakeSyncTransport::release_held_pushes`].
    pub fn set_hold_pushes(&self, hold: bool) {
        self.hold_pushes.store(hold, Ordering::SeqCst);
    }

    /// Re-serves the previously served page on the next [n] fetches.
    pub fn set_redeliver_next_fetches(&self, n: usize) {
        self.redeliver_next_fetches.store(n, Ordering::SeqCst);
    }

    /// Fails the next [n] fetches with a dead-token error.
    pub fn set_expire_next_fetches(&self, n: usize) {
        self.expire_next_fetches.store(n, Ordering::SeqCst);
    }

    /// Whether a fresh (None) token expires too.
    pub fn set_expire_fresh_tokens_too(&self, expire: bool) {
        self.expire_fresh_tokens_too.store(expire, Ordering::SeqCst);
    }

    /// The NEXT push's ack is passed through [mangle] before it is returned,
    /// then the knob clears itself. Models a buggy or hostile server whose
    /// ack does not match the pushed rows (short, wrong-row, duplicated
    /// results) — the engine must refuse to ack its oplog on it.
    pub fn set_mangle_next_ack(
        &self,
        mangle: impl FnOnce(&[SyncRow], PushAck) -> PushAck + Send + 'static,
    ) {
        *self.mangle_next_ack.lock() = Some(Box::new(mangle));
    }

    /// Queues a record to report as undecodable on the NEXT page (drained
    /// when served).
    pub fn add_pending_reject(&self, reject: RejectedRecord) {
        self.pending_rejects.lock().push(reject);
    }

    /// How many push_rows calls were made.
    pub fn push_calls(&self) -> usize {
        self.push_calls.load(Ordering::SeqCst)
    }

    /// How many fetch_changes calls were made.
    pub fn fetch_calls(&self) -> usize {
        self.fetch_calls.load(Ordering::SeqCst)
    }

    /// Every pushed batch, in call order.
    pub fn pushed_batches(&self) -> Vec<Vec<SyncRow>> {
        self.pushed_batches.lock().clone()
    }

    /// Number of pushes currently parked by the hold knob.
    pub fn held_push_count(&self) -> usize {
        self.held_pushes.lock().len()
    }

    /// Arms a CAS check for the next [times] pushes of ([table], [id]): each
    /// is rejected with the server's current row iff that row is strictly
    /// newer (see the type docs — the fake stays protocol-conforming).
    pub fn inject_conflict_once(&self, table: &str, id: &str, times: usize) {
        self.conflict_once
            .lock()
            .insert((table.to_string(), id.to_string()), times);
    }

    /// Lands every parked push (in arrival order) and completes its ack.
    pub fn release_held_pushes(&self) {
        let parked: Vec<Arc<HeldPush>> = std::mem::take(&mut *self.held_pushes.lock());
        for p in parked {
            let ack = self.accept_rows(&p.rows);
            *p.ack.lock() = Some(ack);
            p.landed.notify_all();
        }
    }

    fn accept_rows(&self, rows: &[SyncRow]) -> PushAck {
        let mut results = Vec::with_capacity(rows.len());
        for row in rows {
            let key = row_key(row);
            let server_row = self.cloud.server_row_for(&key.0, &key.1);
            let armed = {
                let mut conflicts = self.conflict_once.lock();
                let remaining = conflicts.get(&key).copied().unwrap_or(0);
                if remaining > 0 && server_row.is_some() {
                    conflicts.insert(key.clone(), remaining - 1);
                    true
                } else {
                    false
                }
            };
            if armed {
                // Conforming CAS: reject ONLY when the held row is strictly
                // newer (docs/sync-protocol.md). An older-or-equal held row
                // falls through to acceptance — a real server never
                // conflicts on those, and the engine refuses such an ack as a
                // protocol violation.
                if let Some(server) = &server_row {
                    let newer = match (server.hlc_packed(), row.hlc_packed()) {
                        (Ok(s), Ok(r)) => dart::compare_strings(s, r).is_gt(),
                        _ => false,
                    };
                    if newer {
                        results.push(PushRowResult::conflicted(row.clone(), server.clone()));
                        continue;
                    }
                }
            }
            self.cloud.accept(row.clone());
            results.push(PushRowResult::accepted(row.clone()));
        }
        let ack = PushAck::new(results);
        let mangle = self.mangle_next_ack.lock().take();
        match mangle {
            Some(mangle) => mangle(rows, ack),
            None => ack,
        }
    }
}

impl SyncTransport for FakeSyncTransport {
    fn push_rows(&self, rows: &[SyncRow]) -> Result<PushAck, TransportError> {
        self.push_calls.fetch_add(1, Ordering::SeqCst);
        self.pushed_batches.lock().push(rows.to_vec());
        if self.partitioned.load(Ordering::SeqCst) {
            return Err(SyncTransportException::new(format!(
                "{} is partitioned (push)",
                self.name
            ))
            .into());
        }
        if self.hold_pushes.load(Ordering::SeqCst) {
            let held = Arc::new(HeldPush {
                rows: rows.to_vec(),
                ack: Mutex::new(None),
                landed: Condvar::new(),
            });
            self.held_pushes.lock().push(held.clone());
            let mut ack = held.ack.lock();
            while ack.is_none() {
                held.landed.wait(&mut ack);
            }
            return Ok(ack.take().unwrap_or_default());
        }
        Ok(self.accept_rows(rows))
    }

    fn fetch_changes(&self, since_token: Option<&str>) -> Result<FetchPage, TransportError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        if self.partitioned.load(Ordering::SeqCst) {
            return Err(SyncTransportException::new(format!(
                "{} is partitioned (fetch)",
                self.name
            ))
            .into());
        }
        let expire = self.expire_next_fetches.load(Ordering::SeqCst);
        if expire > 0
            && (since_token.is_some() || self.expire_fresh_tokens_too.load(Ordering::SeqCst))
        {
            self.expire_next_fetches.store(expire - 1, Ordering::SeqCst);
            return Err(SyncTokenExpiredException::new(format!(
                "{}: change token \"{}\" is no longer honorable",
                self.name,
                since_token.unwrap_or("null")
            ))
            .into());
        }
        let redeliver = self.redeliver_next_fetches.load(Ordering::SeqCst);
        if redeliver > 0 {
            if let Some(page) = self.last_served_page.lock().clone() {
                self.redeliver_next_fetches
                    .store(redeliver - 1, Ordering::SeqCst);
                return Ok(page);
            }
        }
        let from = match since_token {
            None => 0,
            Some(t) => match dart::parse_int(t) {
                Some(v) => usize::try_from(v).unwrap_or(0),
                None => {
                    return Err(TransportError::Other(format!(
                        "FormatException: Invalid radix-10 number: \"{t}\""
                    )))
                }
            },
        };
        let page_size = self.page_size.load(Ordering::SeqCst);
        let mut slice: Vec<FakeCloudEntry> = Vec::new();
        for e in self.cloud.log() {
            if e.seq <= from {
                continue;
            }
            slice.push(e);
            if slice.len() >= page_size {
                break;
            }
        }
        let last_seq = slice.last().map_or(from, |e| e.seq);
        let served = std::mem::take(&mut *self.pending_rejects.lock());
        let page = FetchPage {
            rows: slice.into_iter().map(|e| e.row).collect(),
            next_token: last_seq.to_string(),
            has_more: last_seq < self.cloud.latest_seq(),
            // A real transport decodes per record and reports the failures
            // here; the fake serves whatever a test staged for the next page.
            rejects: served,
        };
        *self.last_served_page.lock() = Some(page.clone());
        Ok(page)
    }

    fn push_asset_bytes(
        &self,
        _content_hash: &str,
        _file_path: &Path,
    ) -> Result<(), TransportError> {
        Err(TransportError::Other(
            "UnimplementedError: asset bytes sync is P2".to_string(),
        ))
    }

    fn fetch_asset_bytes(
        &self,
        _content_hash: &str,
        _dest_path: &Path,
    ) -> Result<(), TransportError> {
        Err(TransportError::Other(
            "UnimplementedError: asset bytes sync is P2".to_string(),
        ))
    }
}
