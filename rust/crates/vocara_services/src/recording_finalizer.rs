//! The quit's wait on in-flight recordings and saves (port of
//! `lib/data/services/recording_finalizer.dart`).
//!
//! Process-wide registry of in-flight recordings so the app can FINALIZE them
//! (stop + analyze + save the captured take) before the process is torn down
//! on quit. Without this, quitting mid-record loses the take as a 0-length
//! row + a header-broken WAV — the finalize only runs on the normal stop path.
//!
//! Quit paths covered (both wired in the app's quit guard, `vocara_app::app`):
//!  * red close button — the window's close request is deferred to the quit
//!    guard, which runs [`RecordingFinalizer::finalize_all`] before the window
//!    closes;
//!  * Cmd+Q / app-menu Quit / Dock quit / logout — these route through
//!    `NSApplication.terminate:`, which arrives as a cancellable exit request;
//!    the guard runs [`RecordingFinalizer::finalize_all`] before allowing the
//!    exit.
//!
//! NOT interceptable: force-quit / `kill -9`, where the process dies without
//! notice. That path is covered differently: the engine keeps the streaming
//! WAV's header valid (periodic in-place patch), and the next boot recovers
//! the staged file as a new take (`Persistence::recover_staged_recordings`)
//! before the staging sweep.
//!
//! A recording controller registers a [`Finalizer`] when it starts and
//! removes it when it stops or is disposed. A save in flight holds the quit
//! the same way for exactly its own lifetime ([`RecordingFinalizer::hold`]).
//!
//! Dart's futures become blocking calls here, as everywhere in the services:
//! a [`Finalizer`] blocks until its work has settled, and the quit runs each
//! one on a thread of its own and waits on them together, within its budgets
//! — a finalizer the quit stops waiting on keeps its thread, as Dart's
//! abandoned future kept running. The state Dart kept on its one isolate sits
//! behind one mutex, and every wait Dart awaited is a condition variable.
//! [`RecordingFinalizer::begin_hold`] and [`RecordingFinalizer::begin_landing`]
//! are the guard forms of [`RecordingFinalizer::hold`] and
//! [`RecordingFinalizer::landing`], for work that runs on another thread than
//! the one that starts it: Dart registered from the synchronous start of the
//! work, so a caller takes the guard before it hands the work off, and moves
//! it along with the work.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::util::export_copy::export_bytes_written;
use vocara_store::db::AssetStore;

/// Work the quit runs before the app exits (Dart's `Future<void> Function()`).
///
/// Calling it blocks until its work has settled, whatever the outcome: the
/// quit is best-effort, so a finalizer reports its own failure (a panic is
/// absorbed). It may be called again while an earlier call still runs (each
/// quit run calls every finalizer still pending), so it must be safe to.
///
/// Identity is the allocation, as a Dart tear-off's was: clones are the same
/// finalizer (register and unregister match), two [`Finalizer::new`] calls
/// are two finalizers even over the same closure.
#[derive(Clone)]
pub struct Finalizer(Arc<dyn Fn() + Send + Sync>);

impl Finalizer {
    pub fn new(finalize: impl Fn() + Send + Sync + 'static) -> Self {
        Finalizer(Arc::new(finalize))
    }

    fn key(&self) -> *const () {
        Arc::as_ptr(&self.0) as *const ()
    }

    fn call(&self) {
        (self.0)()
    }
}

impl PartialEq for Finalizer {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Finalizer {}

impl Hash for Finalizer {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.key(), state);
    }
}

impl fmt::Debug for Finalizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Finalizer").field(&self.key()).finish()
    }
}

/// A reading that moves while finalizers do real work (see
/// [`RecordingFinalizer::with_progress`]). Only a change between readings
/// means anything; a panic counts as no reading.
pub type Progress = Arc<dyn Fn() -> i64 + Send + Sync>;

/// How long the quit waits on its finalizers, which run together, with
/// nothing being copied. A legitimate finalize is a hardware stop, one
/// waveform extraction and one database write — a second or two even for a
/// long take on a busy disk. Past that it is wedged, which is reachable
/// without any bug: a save retries behind the busy gate for as long as it
/// takes, because dropping it would be worse.
const DEFAULT_PER_FINALIZER: Duration = Duration::from_secs(5);

/// Ceiling on that wait since the quit began, across both quit paths' runs
/// (see [`State::quit_clock`]), so a quit macOS would end on its own terms
/// never happens without bytes still moving.
const DEFAULT_TOTAL: Duration = Duration::from_secs(8);

/// How long a quit keeps waiting, all told, on a finalizer that is still
/// making progress past its budget (see [`RecordingFinalizer::finalize_all`]):
/// a save pooling a large file — a 1 GB technique video copies for well over
/// 5 s — is working, not wedged, and cutting it off lost the user's Save
/// outright.
const DEFAULT_PROGRESS_CEILING: Duration = Duration::from_secs(60);

/// The production progress reading: pool imports and exports alike.
fn bytes_moving() -> i64 {
    AssetStore::bytes_being_copied().wrapping_add(export_bytes_written() as i64)
}

/// The registry's state, Dart's fields on its one isolate.
#[derive(Default)]
struct State {
    /// The registered finalizers, in registration order (Dart's linked set).
    pending: Vec<Finalizer>,

    /// The registered finalizers marked `starts_work` (see
    /// [`RecordingFinalizer::register`]).
    starts_work: HashSet<Finalizer>,

    /// The finalizers this quit has called since each was registered. Every
    /// run calls every pending finalizer again — each is safe to: a take's
    /// stops only a take still recording, a stop's tail and a save's hold
    /// wait afresh, so a run sees work registered under the same finalizer
    /// since (a take re-armed on the same screen, a second stop's tail). But
    /// only a `starts_work` one not in here gets a budget of its own; the
    /// rest are on the quit's clock, or a take's own stop-and-save —
    /// registering its tail and holds as it goes — would earn the exit
    /// request's run a fresh budget every time.
    invoked: HashSet<Finalizer>,

    /// Set when a `starts_work` finalizer this quit never called was
    /// unregistered (see [`RecordingFinalizer::unregister`]): the next run
    /// gets a budget of its own.
    uncounted_work: bool,

    /// When the quit began, kept across runs while one cut a finalizer off:
    /// the close button runs [`RecordingFinalizer::finalize_all`], and the
    /// exit request after it runs it again for whatever is still pending —
    /// timed afresh, a wedged save held the quit for twice every limit. A
    /// run whose finalizers all settled clears it, and so does a later run
    /// finding it older than any quit lasts (one the app survived: a close
    /// that failed).
    quit_clock: Option<Instant>,

    /// The in-flight [`RecordingFinalizer::finalize_all`] run, shared by
    /// concurrent callers: opened when it ends.
    in_flight: Option<Arc<Latch>>,

    /// Stopped takes still landing, per practice id (see
    /// [`RecordingFinalizer::landing`]): each tail by its token.
    landing: HashMap<String, Vec<u64>>,

    /// The token the next landing tail takes.
    next_tail: u64,
}

/// The registry (see the module doc).
pub struct RecordingFinalizer {
    per_finalizer_budget: Duration,
    total_budget: Duration,
    progress_ceiling: Duration,

    /// A reading that moves while finalizers do real work (the asset store's
    /// import counter: bytes copied, plus a tick per finished landing step —
    /// flush, rename, commit; and the bytes exports in flight have written):
    /// the quit's evidence that a finalizer past its budget is still working.
    /// Only a change between readings means anything.
    progress: Progress,

    state: Mutex<State>,

    /// Signalled whenever a landing tail settles (see
    /// [`RecordingFinalizer::landed`]).
    landing_changed: Condvar,
}

impl fmt::Debug for RecordingFinalizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordingFinalizer")
            .field("per_finalizer_budget", &self.per_finalizer_budget)
            .field("total_budget", &self.total_budget)
            .field("progress_ceiling", &self.progress_ceiling)
            .finish_non_exhaustive()
    }
}

impl RecordingFinalizer {
    /// How long one progress reading may take (see
    /// [`RecordingFinalizer::finalize_all`]).
    pub const READING_LIMIT: Duration = Duration::from_secs(2);

    fn with_progress_reading(progress: Progress) -> Self {
        RecordingFinalizer {
            per_finalizer_budget: DEFAULT_PER_FINALIZER,
            total_budget: DEFAULT_TOTAL,
            progress_ceiling: DEFAULT_PROGRESS_CEILING,
            progress,
            state: Mutex::new(State::default()),
            landing_changed: Condvar::new(),
        }
    }

    /// The single app-wide instance (recordings are bound to the one main
    /// window), reading the asset store's import progress and the exports'.
    pub fn instance() -> Arc<RecordingFinalizer> {
        static INSTANCE: OnceLock<Arc<RecordingFinalizer>> = OnceLock::new();
        INSTANCE
            .get_or_init(|| {
                Arc::new(RecordingFinalizer::with_progress_reading(Arc::new(
                    bytes_moving,
                )))
            })
            .clone()
    }

    /// An isolated instance (tests, and an owner that wants a private
    /// registry): the production budgets, and a progress reading that never
    /// moves until [`RecordingFinalizer::with_progress`] gives one. The
    /// `with_*` setters make budgets small enough to drive the expiry paths
    /// in real time.
    pub fn for_testing() -> Self {
        Self::with_progress_reading(Arc::new(|| 0))
    }

    /// See [`RecordingFinalizer::per_finalizer_budget`].
    pub fn with_per_finalizer_budget(mut self, budget: Duration) -> Self {
        self.per_finalizer_budget = budget;
        self
    }

    /// See [`RecordingFinalizer::total_budget`].
    pub fn with_total_budget(mut self, budget: Duration) -> Self {
        self.total_budget = budget;
        self
    }

    /// See [`RecordingFinalizer::progress_ceiling`].
    pub fn with_progress_ceiling(mut self, ceiling: Duration) -> Self {
        self.progress_ceiling = ceiling;
        self
    }

    /// Replaces the progress reading (see [`Progress`]).
    pub fn with_progress(mut self, progress: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.progress = Arc::new(progress);
        self
    }

    /// How long the quit waits on its finalizers with nothing being copied
    /// (5 s in production).
    pub fn per_finalizer_budget(&self) -> Duration {
        self.per_finalizer_budget
    }

    /// The ceiling on that wait since the quit began (8 s in production).
    pub fn total_budget(&self) -> Duration {
        self.total_budget
    }

    /// How long a quit keeps waiting, all told, while bytes still move (60 s
    /// in production).
    pub fn progress_ceiling(&self) -> Duration {
        self.progress_ceiling
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing that can panic runs under this lock: a poisoned lock only
        // means a panic elsewhere, and the state is still consistent.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True while at least one recording is in flight.
    pub fn has_active(&self) -> bool {
        !self.lock().pending.is_empty()
    }

    /// Lists [finalize] for the next quit. [starts_work] marks one that
    /// begins work no quit run has budgeted yet — an armed take's
    /// stop-and-save — as opposed to a wait on work already under way (a
    /// save's [`hold`], a stop's tail): a quit run that finds one registered
    /// since its last call gives it a budget of its own (see
    /// [`finalize_all`]).
    ///
    /// [`hold`]: RecordingFinalizer::hold
    /// [`finalize_all`]: RecordingFinalizer::finalize_all
    pub fn register(&self, finalize: &Finalizer, starts_work: bool) {
        let mut s = self.lock();
        if !s.pending.contains(finalize) {
            s.pending.push(finalize.clone());
        }
        s.invoked.remove(finalize);
        if starts_work {
            s.starts_work.insert(finalize.clone());
        } else {
            s.starts_work.remove(finalize);
        }
    }

    /// Takes [finalize] off the next quit's list.
    pub fn unregister(&self, finalize: &Finalizer) {
        let mut s = self.lock();
        // A take armed during a quit's wait and stopped by the user before the
        // next run: its stop-and-save carries on under waits that are not marked
        // starts_work, and with the take's own finalizer gone that run found the
        // budget spent and exited mid-save. Its budget outlives the Stop.
        if s.quit_clock.is_some()
            && s.starts_work.contains(finalize)
            && !s.invoked.contains(finalize)
        {
            s.uncounted_work = true;
        }
        s.pending.retain(|f| f != finalize);
        s.invoked.remove(finalize);
        s.starts_work.remove(finalize);
    }

    /// Runs [work] — a save the user already asked for — while holding the
    /// quit: registered from the start of [work] until it settles, so a quit
    /// landing mid-save waits for it (under the same budgets as any
    /// finalizer) instead of exiting under it. Without this a Save followed
    /// by Cmd+Q exited before the commit, and the edit was lost with nothing
    /// left to recover. [work]'s outcome is returned to the caller unchanged
    /// (a panic too); the quit only waits for it.
    pub fn hold<T>(&self, work: impl FnOnce() -> T) -> T {
        let held = self.start_hold();
        let _release = HoldRelease {
            finalizer: self,
            held,
        };
        work()
    }

    /// [`RecordingFinalizer::hold`] for work that runs elsewhere: the quit is
    /// held from this call until the returned guard is dropped. Take it
    /// before handing the save off to another thread, and drop it once the
    /// save has settled.
    pub fn begin_hold(self: &Arc<Self>) -> Hold {
        Hold {
            held: self.start_hold(),
            finalizer: Arc::clone(self),
        }
    }

    fn start_hold(&self) -> Held {
        let settled = Arc::new(Latch::default());
        let wait_for_it = Finalizer::new({
            let settled = Arc::clone(&settled);
            move || settled.wait()
        });
        self.register(&wait_for_it, false);
        Held {
            wait_for_it,
            settled,
        }
    }

    fn end_hold(&self, held: &Held) {
        // Off the list first, then settled: a quit waiting on it returns with
        // the hold already gone, as Dart's finally ran before the quit's wait
        // on the same future resumed.
        self.unregister(&held.wait_for_it);
        held.settled.open();
    }

    /// Runs [work] — the post-stop finalize of a take of [practice_id] (its
    /// analysis, then the save that commits it) — listed for
    /// [`RecordingFinalizer::landed`] until it settles, and returns its
    /// outcome (a panic too) to the caller. The finalize outlives the screen
    /// that stopped the take (Stop, then Back), and a screen reopened on the
    /// practice meanwhile read it without the take.
    pub fn landing<T>(&self, practice_id: &str, work: impl FnOnce() -> T) -> T {
        let token = self.start_landing(practice_id);
        let _settle = LandingRelease {
            finalizer: self,
            practice_id,
            token,
        };
        work()
    }

    /// [`RecordingFinalizer::landing`] for work that runs elsewhere:
    /// [practice_id] is landing from this call until the returned guard is
    /// dropped.
    pub fn begin_landing(self: &Arc<Self>, practice_id: &str) -> Landing {
        Landing {
            token: self.start_landing(practice_id),
            practice_id: practice_id.to_string(),
            finalizer: Arc::clone(self),
        }
    }

    fn start_landing(&self, practice_id: &str) -> u64 {
        let mut s = self.lock();
        let token = s.next_tail;
        s.next_tail += 1;
        s.landing
            .entry(practice_id.to_string())
            .or_default()
            .push(token);
        token
    }

    fn end_landing(&self, practice_id: &str, token: u64) {
        let mut s = self.lock();
        if let Some(tails) = s.landing.get_mut(practice_id) {
            tails.retain(|t| *t != token);
            if tails.is_empty() {
                s.landing.remove(practice_id);
            }
        }
        drop(s);
        self.landing_changed.notify_all();
    }

    /// Whether anything of [practice_id] is landing right now (see
    /// [`RecordingFinalizer::landing`]).
    pub fn is_landing(&self, practice_id: &str) -> bool {
        self.lock().landing.contains_key(practice_id)
    }

    /// Whether anything of any practice is landing right now: a stopped
    /// take's finalize, a queued save, or an edit behind a song page's busy
    /// gate (an import being copied, transcoded and saved among them), which
    /// the gate lists as landing for as long as it is held.
    pub fn any_landing(&self) -> bool {
        !self.lock().landing.is_empty()
    }

    /// Returns once every take of [practice_id] landing now has settled,
    /// whatever its outcome (the finalize reports its own) — or after
    /// [`RecordingFinalizer::per_finalizer_budget`], like a quit's wait: a
    /// wedged finalize then costs the reopened screen only the take's
    /// visibility, never the screen. A take listed while this waits is not
    /// waited on.
    pub fn landed(&self, practice_id: &str) {
        let started = Instant::now();
        let mut s = self.lock();
        let Some(tails) = s.landing.get(practice_id).cloned() else {
            return;
        };
        loop {
            let still_landing = s
                .landing
                .get(practice_id)
                .is_some_and(|now| tails.iter().any(|t| now.contains(t)));
            if !still_landing {
                return;
            }
            let left = self.per_finalizer_budget.saturating_sub(started.elapsed());
            if left.is_zero() {
                return;
            }
            s = self
                .landing_changed
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Returns once nothing of [practice_id] is landing: every tail landing
    /// now, and any listed while this waits, has settled. No budget — what a
    /// reopened screen waits on before it takes edits, since its edits merge
    /// row by row against what it read, and a save landing underneath them
    /// would win or lose against them outright.
    pub fn settled(&self, practice_id: &str) {
        let mut s = self.lock();
        while s.landing.contains_key(practice_id) {
            s = self
                .landing_changed
                .wait(s)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// [`RecordingFinalizer::settled`], given at most [limit] (Dart's
    /// `settled(id).timeout(limit)`): whether nothing of [practice_id] is
    /// landing by then.
    pub fn settled_within(&self, practice_id: &str, limit: Duration) -> bool {
        let started = Instant::now();
        let mut s = self.lock();
        loop {
            if !s.landing.contains_key(practice_id) {
                return true;
            }
            let left = limit.saturating_sub(started.elapsed());
            if left.is_zero() {
                return false;
            }
            s = self
                .landing_changed
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Runs every registered finalizer (best-effort; one failure never blocks
    /// the rest or the quit). Iterates a snapshot, so a finalizer that
    /// unregisters itself mid-run is safe. Concurrent calls (e.g. a close
    /// click racing a Cmd+Q exit request) wait on the SAME in-flight run
    /// instead of double-running finalizers; once a run completes, a later
    /// call starts a fresh one.
    ///
    /// The finalizers run TOGETHER, and the quit waits for all of them at
    /// once. One after another, a save queued behind a long one (a mute
    /// behind a large import) found the budget spent and was never waited
    /// on. And a copy that kept the quit waiting could belong to a save it
    /// was not waiting on yet.
    ///
    /// BOUNDED: the quit waits [`per_finalizer_budget`], never past
    /// [`total_budget`] since the quit began, and on past that only while
    /// bytes are still being copied (the progress reading moved during the
    /// last window), never past [`progress_ceiling`]. Both quit paths gate
    /// the app's exit on this call, and an exit macOS ends on its own terms
    /// is worse than a skipped save — a take left behind is recovered on the
    /// next boot, a force-killed process finalizes nothing at all.
    ///
    /// [`per_finalizer_budget`]: RecordingFinalizer::per_finalizer_budget
    /// [`total_budget`]: RecordingFinalizer::total_budget
    /// [`progress_ceiling`]: RecordingFinalizer::progress_ceiling
    pub fn finalize_all(&self) {
        let (run, ours) = {
            let mut s = self.lock();
            match &s.in_flight {
                Some(run) => (Arc::clone(run), false),
                None => {
                    let run = Arc::new(Latch::default());
                    s.in_flight = Some(Arc::clone(&run));
                    (run, true)
                }
            }
        };
        if !ours {
            run.wait();
            return;
        }
        let _done = RunDone {
            finalizer: self,
            run,
        };
        self.run_all();
    }

    /// One [`RecordingFinalizer::finalize_all`] run (Dart's `_finalizeAll`).
    fn run_all(&self) {
        let per = self.per_finalizer_budget;
        let (pending, clock, window) = {
            let mut s = self.lock();
            let pending = s.pending.clone();
            if pending.is_empty() {
                return;
            }
            let forgotten = self
                .progress_ceiling
                .saturating_add(self.per_finalizer_budget);
            if s.quit_clock.is_some_and(|c| c.elapsed() > forgotten) {
                s.quit_clock = None;
                s.invoked.clear();
                s.uncounted_work = false;
            }
            let clock = *s.quit_clock.get_or_insert_with(Instant::now);
            // The uncounted take's budget only while it left work behind (a song
            // take's stop-and-save registers it): a practice take stopped with
            // nothing to save earned the exit request's run a full wait on nothing.
            let fresh = (s.uncounted_work && pending.iter().any(|f| !s.invoked.contains(f)))
                || pending
                    .iter()
                    .any(|f| s.starts_work.contains(f) && !s.invoked.contains(f));
            s.uncounted_work = false;
            let window = if fresh {
                per
            } else {
                per.min(self.total_budget.saturating_sub(clock.elapsed()))
            };
            if window.is_zero() {
                drop(s);
                DiagnosticsLog::instance().log(
                    "quit",
                    &format!("finalize budget spent; {} still running", pending.len()),
                );
                return;
            }
            s.invoked.extend(pending.iter().cloned());
            (pending, clock, window)
        };
        // Taken as the finalizers start, and only waited on once their window
        // runs out: waited on first, a reading stuck for its whole limit held
        // every save back that long (and the quit, once they were done).
        let mut mark = self.read();
        let running = Arc::new(Running::new(pending.len()));
        for finalize in &pending {
            let (finalize, settled) = (finalize.clone(), Arc::clone(&running));
            let spawned = thread::Builder::new()
                .name("vocara-quit-finalizer".into())
                .spawn(move || {
                    // Best-effort by contract: one failure never blocks the rest.
                    let _ = catch_unwind(AssertUnwindSafe(|| finalize.call()));
                    settled.settle_one();
                });
            if spawned.is_err() {
                // Never started: a failure like any other, absorbed.
                running.settle_one();
            }
        }
        let mut window = window;
        loop {
            if running.wait(window) {
                let mut s = self.lock();
                s.quit_clock = None;
                s.invoked.clear();
                return;
            }
            // Any change counts as movement. The production reading grows with
            // every byte copied and every landing step finished (an export's part
            // drops back when it ends, which is movement too); an injected one
            // may move either way.
            let before = mark.get();
            let now = self.read().get();
            window = per.min(self.progress_ceiling.saturating_sub(clock.elapsed()));
            if now.is_none() || now == before || window.is_zero() {
                break;
            }
            mark = Reading::Ready(now);
            DiagnosticsLog::instance().log("quit", "still copying past the budget; waiting on");
        }
        // Quitting past a wedged finalize loses strictly LESS than being
        // force-killed while still waiting: the streaming WAV's header is patched
        // in place about once a second, so the staged take is recovered on the
        // next boot either way (see the force-quit note above). The finalizers'
        // own failures are absorbed above, so none escapes into the exiting
        // process.
        DiagnosticsLog::instance().log(
            "quit",
            &format!(
                "{} of {} finalizers still running after {} ms; left for boot recovery",
                running.left(),
                pending.len(),
                clock.elapsed().as_millis()
            ),
        );
    }

    /// The progress reading, given at most [`READING_LIMIT`] from now, or
    /// `None`: a reading stuck or failing counts as no movement, instead of
    /// holding the quit, and every save waiting behind it. A backstop: the
    /// export part bounds each file itself (see `export_bytes_written`), so a
    /// stuck share doesn't hide the pool imports still moving beside it.
    ///
    /// [`READING_LIMIT`]: RecordingFinalizer::READING_LIMIT
    fn read(&self) -> Reading {
        let (tx, rx) = mpsc::channel();
        let progress = Arc::clone(&self.progress);
        let started = Instant::now();
        let spawned = thread::Builder::new()
            .name("vocara-quit-progress".into())
            .spawn(move || {
                if let Ok(v) = catch_unwind(AssertUnwindSafe(|| progress())) {
                    let _ = tx.send(v);
                }
            });
        match spawned {
            Ok(_) => Reading::Pending { rx, started },
            Err(_) => Reading::Ready(None),
        }
    }
}

/// A progress reading under way, or its outcome.
enum Reading {
    Pending {
        rx: mpsc::Receiver<i64>,
        started: Instant,
    },
    Ready(Option<i64>),
}

impl Reading {
    /// The reading, waiting for what is left of its limit.
    fn get(self) -> Option<i64> {
        match self {
            Reading::Ready(v) => v,
            Reading::Pending { rx, started } => rx
                .recv_timeout(RecordingFinalizer::READING_LIMIT.saturating_sub(started.elapsed()))
                .ok(),
        }
    }
}

/// A one-shot signal: [`Latch::wait`] returns once [`Latch::open`] ran.
#[derive(Default)]
struct Latch {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Latch {
    fn open(&self) {
        *self.open.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            open = self.changed.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// The finalizers of one run still running (Dart's `running` and the
/// `Future.wait` over them).
struct Running {
    left: Mutex<usize>,
    changed: Condvar,
}

impl Running {
    fn new(count: usize) -> Self {
        Running {
            left: Mutex::new(count),
            changed: Condvar::new(),
        }
    }

    fn settle_one(&self) {
        let mut left = self.left.lock().unwrap_or_else(|e| e.into_inner());
        *left = left.saturating_sub(1);
        drop(left);
        self.changed.notify_all();
    }

    fn left(&self) -> usize {
        *self.left.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether every finalizer settled within [window].
    fn wait(&self, window: Duration) -> bool {
        let started = Instant::now();
        let mut left = self.left.lock().unwrap_or_else(|e| e.into_inner());
        while *left > 0 {
            let rest = window.saturating_sub(started.elapsed());
            if rest.is_zero() {
                return false;
            }
            left = self
                .changed
                .wait_timeout(left, rest)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

/// A save's place on the quit's list (Dart's `waitForIt`), and its outcome.
struct Held {
    wait_for_it: Finalizer,
    settled: Arc<Latch>,
}

/// Ends a [`RecordingFinalizer::hold`] however its work ends.
struct HoldRelease<'a> {
    finalizer: &'a RecordingFinalizer,
    held: Held,
}

impl Drop for HoldRelease<'_> {
    fn drop(&mut self) {
        self.finalizer.end_hold(&self.held);
    }
}

/// The quit held for work running elsewhere (see
/// [`RecordingFinalizer::begin_hold`]); released when dropped.
#[must_use = "the quit is held only while this guard lives"]
pub struct Hold {
    finalizer: Arc<RecordingFinalizer>,
    held: Held,
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.finalizer.end_hold(&self.held);
    }
}

impl fmt::Debug for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hold").finish_non_exhaustive()
    }
}

/// Ends a [`RecordingFinalizer::landing`] however its work ends.
struct LandingRelease<'a> {
    finalizer: &'a RecordingFinalizer,
    practice_id: &'a str,
    token: u64,
}

impl Drop for LandingRelease<'_> {
    fn drop(&mut self) {
        self.finalizer.end_landing(self.practice_id, self.token);
    }
}

/// A take of a practice landing elsewhere (see
/// [`RecordingFinalizer::begin_landing`]); settled when dropped.
#[must_use = "the take is listed as landing only while this guard lives"]
pub struct Landing {
    finalizer: Arc<RecordingFinalizer>,
    practice_id: String,
    token: u64,
}

impl Drop for Landing {
    fn drop(&mut self) {
        self.finalizer.end_landing(&self.practice_id, self.token);
    }
}

impl fmt::Debug for Landing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Landing")
            .field("practice_id", &self.practice_id)
            .finish_non_exhaustive()
    }
}

/// Clears the in-flight run and releases the callers waiting on it, however
/// the run ends (Dart's `whenComplete(() => _inFlight = null)`).
struct RunDone<'a> {
    finalizer: &'a RecordingFinalizer,
    run: Arc<Latch>,
}

impl Drop for RunDone<'_> {
    fn drop(&mut self) {
        self.finalizer.lock().in_flight = None;
        self.run.open();
    }
}
