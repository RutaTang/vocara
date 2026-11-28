//! Splitting a full song into an instrumental and an isolated vocal stem
//! (port of `lib/data/services/source_separation_service.dart`).
//!
//! An engine-agnostic abstraction: the UI and view models never depend on
//! *how* separation happens. The prototype runs a Python sidecar as a
//! subprocess ([`ProcessSourceSeparationService`]); a shipped build swaps in a
//! bundled, self-contained engine behind the same trait so end users never
//! install anything. Prod builds wire [`NullSourceSeparationService`]; tests
//! use [`FakeSourceSeparationService`].

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fancy_regex::Regex;
use md5::{Digest as _, Md5};
use sha2::Sha256;

use crate::local_storage_service::LocalStorageService;

/// The bundled sidecar, copied out to app support on first use.
const SEPARATE_PY: &str = include_str!("../assets/separation/separate.py");

/// The default separation model. MUST mirror `separate.py`'s DEFAULT_MODEL —
/// it only feeds the cache key (so a model change invalidates stems), since
/// the sidecar applies its own default when `--model` is omitted.
pub const DEFAULT_SEPARATION_MODEL: &str = "mel_band_roformer_instrumental_becruily.ckpt";

/// The two stems produced by separating a full song: the instrumental (to
/// sing over) and the isolated vocal (a reference to A/B against).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparationResult {
    pub instrumental_path: PathBuf,
    pub vocal_path: PathBuf,
}

/// Why a separation did not produce stems.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeparationError {
    /// The user cancelled (or the app is quitting): a benign stop, not an
    /// error (Dart's `SeparationCancelled`).
    #[error("Separation cancelled")]
    Cancelled,
    /// The run failed, with the message the UI shows (Dart's
    /// `FormatException` messages, verbatim).
    #[error("{0}")]
    Failed(String),
    /// No engine ships in this build (Dart's `UnsupportedError`).
    #[error("{0}")]
    Unsupported(String),
    /// A file operation failed outside the sidecar (Dart let the
    /// `FileSystemException` through).
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for SeparationError {
    fn from(e: std::io::Error) -> Self {
        SeparationError::Io(e.to_string())
    }
}

/// A progress callback, called with best-effort progress in `[0, 1]`.
pub type Progress<'a> = Option<&'a (dyn Fn(f64) + Send + Sync)>;

/// A run registered by [`SourceSeparationService::begin`], to pass to
/// [`SourceSeparationService::separate_with`].
///
/// Dart registered a run synchronously at the call, so a cancel issued right
/// after `separate()` always reached it. Here the blocking work runs on a
/// worker thread, so registration is a separate step the caller takes on its
/// own thread first.
pub struct SeparationTicket {
    run: Option<Arc<Run>>,
    prior: Option<Arc<Run>>,
    /// Lists the run as running with the quit guard until
    /// [`SourceSeparationService::separate_with`] returns, or the ticket is
    /// dropped unused (see [`SeparationQuitGuard::has_active`]).
    _running: Option<Running>,
}

/// Splits a full song into an instrumental and an isolated vocal stem.
pub trait SourceSeparationService: Send + Sync {
    /// Whether the engine can run on this machine (e.g. the interpreter
    /// exists). May block (it can probe interpreters).
    fn is_available(&self) -> bool;

    /// Registers a new run, cancelling one still in flight (see
    /// [`SeparationTicket`]). Never blocks.
    fn begin(&self) -> SeparationTicket;

    /// Separates [source] into two stems on the run [ticket] registered.
    /// [on_progress] reports best-effort progress in `[0, 1]`. Returns the
    /// two stem file paths (16-bit PCM WAV, which is what the analysis
    /// pipeline accepts). Blocking: run it off the UI thread.
    fn separate_with(
        &self,
        ticket: SeparationTicket,
        source: &Path,
        on_progress: Progress<'_>,
    ) -> Result<SeparationResult, SeparationError>;

    /// [`begin`] then [`separate_with`], on the caller's thread.
    ///
    /// [`begin`]: SourceSeparationService::begin
    /// [`separate_with`]: SourceSeparationService::separate_with
    fn separate(
        &self,
        source: &Path,
        on_progress: Progress<'_>,
    ) -> Result<SeparationResult, SeparationError> {
        let ticket = self.begin();
        self.separate_with(ticket, source, on_progress)
    }

    /// Aborts an in-flight run (best-effort); a no-op when idle.
    fn cancel(&self);
}

// -------------------------------------------------------------------------
// The quit guard
// -------------------------------------------------------------------------

type KillHook = Arc<dyn Fn() + Send + Sync>;

/// Process-wide registry of in-flight separation kill hooks, so the quit
/// paths can tear down the sidecar subprocess before the app exits (mirrors
/// the recording finalizer: the quit path can't reach the services graph, so
/// a service registers itself here). Without this, quitting mid-separation
/// orphans a heavy torch process that keeps burning CPU/RAM for minutes after
/// the app is gone — child processes are reparented on macOS, not killed with
/// the parent.
#[derive(Default)]
pub struct SeparationQuitGuard {
    kills: Mutex<Vec<(u64, KillHook)>>,
    next: AtomicU64,
    /// The separations running now (see [`SeparationQuitGuard::has_active`]).
    running: AtomicUsize,
}

/// A separation listed as running with its quit guard, until dropped
/// ([`SeparationQuitGuard::begin_run`]).
pub struct Running(Arc<SeparationQuitGuard>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SeparationQuitGuard {
    /// A guard of its own (tests: the global one is shared by every test in
    /// the process, and one test's quit must not cancel another's run).
    pub fn new() -> Self {
        Self::default()
    }

    /// The single app-wide instance.
    pub fn instance() -> Arc<SeparationQuitGuard> {
        static INSTANCE: OnceLock<Arc<SeparationQuitGuard>> = OnceLock::new();
        INSTANCE
            .get_or_init(|| Arc::new(SeparationQuitGuard::new()))
            .clone()
    }

    /// Registers [kill]; the id unregisters it.
    pub fn register(&self, kill: KillHook) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.kills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, kill));
        id
    }

    pub fn unregister(&self, id: u64) {
        self.kills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(k, _)| *k != id);
    }

    /// Lists a separation as running until the returned guard is dropped:
    /// from the run's registration until it has wound down.
    pub fn begin_run(self: &Arc<Self>) -> Running {
        self.running.fetch_add(1, Ordering::SeqCst);
        Running(Arc::clone(self))
    }

    /// Whether a separation is running: what [`SeparationQuitGuard::kill_all`]
    /// would cut short, minutes of work lost (an update's relaunch waits for
    /// it, `self_update::work_in_flight`).
    pub fn has_active(&self) -> bool {
        self.running.load(Ordering::SeqCst) > 0
    }

    /// Kills every in-flight separation (best-effort; one failure never
    /// blocks the rest or the quit). Never waits: it only sends kill signals
    /// — the app is exiting, so nothing waits for the processes to be reaped.
    pub fn kill_all(&self) {
        let kills: Vec<KillHook> = self
            .kills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(_, k)| k.clone())
            .collect();
        for kill in kills {
            let _ = catch_unwind(AssertUnwindSafe(|| kill()));
        }
    }
}

// -------------------------------------------------------------------------
// The null and fake engines
// -------------------------------------------------------------------------

/// The engine wired in PROD builds, where no separation engine ships yet: it
/// reports unavailable WITHOUT touching the host machine — never spawning a
/// process or probing for Python interpreters the way
/// [`ProcessSourceSeparationService::is_available`] does. The UI hides the
/// separation action when it is unavailable, so separating is only a guard
/// against a call that slipped past the gate.
pub struct NullSourceSeparationService;

impl SourceSeparationService for NullSourceSeparationService {
    fn is_available(&self) -> bool {
        false
    }

    fn begin(&self) -> SeparationTicket {
        SeparationTicket {
            run: None,
            prior: None,
            _running: None,
        }
    }

    fn separate_with(
        &self,
        _ticket: SeparationTicket,
        _source: &Path,
        _on_progress: Progress<'_>,
    ) -> Result<SeparationResult, SeparationError> {
        Err(SeparationError::Unsupported(
            "Source separation is not available in this version.".into(),
        ))
    }

    fn cancel(&self) {
        // nothing can be in flight
    }
}

/// A no-op engine for tests/dev: it doesn't actually separate, it just copies
/// the source to both stem paths so the surrounding flow can be exercised.
pub struct FakeSourceSeparationService {
    pub available: bool,
}

impl Default for FakeSourceSeparationService {
    fn default() -> Self {
        FakeSourceSeparationService { available: true }
    }
}

impl SourceSeparationService for FakeSourceSeparationService {
    fn is_available(&self) -> bool {
        self.available
    }

    fn begin(&self) -> SeparationTicket {
        SeparationTicket {
            run: None,
            prior: None,
            _running: None,
        }
    }

    fn separate_with(
        &self,
        _ticket: SeparationTicket,
        source: &Path,
        on_progress: Progress<'_>,
    ) -> Result<SeparationResult, SeparationError> {
        if let Some(p) = on_progress {
            p(0.2);
        }
        let dir = std::env::temp_dir().join(format!(
            "vocara_sep_fake{}",
            vocara_model::util::id::new_id()
        ));
        fs::create_dir_all(&dir)?;
        let inst = dir.join("instrumental.wav");
        let voc = dir.join("vocal.wav");
        if source.exists() {
            fs::copy(source, &inst)?;
            fs::copy(source, &voc)?;
        } else {
            fs::File::create(&inst)?;
            fs::File::create(&voc)?;
        }
        if let Some(p) = on_progress {
            p(1.0);
        }
        Ok(SeparationResult {
            instrumental_path: inst,
            vocal_path: voc,
        })
    }

    fn cancel(&self) {
        // the fake completes instantly; nothing to abort
    }
}

// -------------------------------------------------------------------------
// The process engine
// -------------------------------------------------------------------------

/// Per-run mutable state. Runs can overlap in time (a cancelled run is still
/// draining while the next screen starts a fresh one), so the process handle
/// and the cancel verdict live here, where a stale run can't clobber a newer
/// run's state.
struct Run {
    /// The sidecar's pid, once started (none during the hashing/cache phase).
    pid: Mutex<Option<i32>>,
    /// Set by cancel/quit teardown; the run reports
    /// [`SeparationError::Cancelled`] instead of a failure once its process
    /// exits.
    cancelled: AtomicBool,
    /// Set when the run has fully wound down (process gone, temp files
    /// cleaned, unregistered).
    done: Mutex<bool>,
    done_cv: Condvar,
}

impl Run {
    fn new() -> Self {
        Run {
            pid: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn is_done(&self) -> bool {
        *self.done.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mark_done(&self) {
        *self.done.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.done_cv.notify_all();
    }

    fn wait_done(&self) {
        let mut d = self.done.lock().unwrap_or_else(|e| e.into_inner());
        while !*d {
            d = self.done_cv.wait(d).unwrap_or_else(|e| e.into_inner());
        }
    }
}

struct Inner {
    storage: Arc<LocalStorageService>,
    /// The interpreter that runs the sidecar. In a shipped build this is
    /// swapped for a bundled, self-contained binary.
    python_executable: String,
    /// Optional override for the separation model the sidecar loads.
    model_name: Option<String>,
    /// The currently-registered run. The service is app-lifetime while a run
    /// can outlive the view model that started it, so a stale run draining
    /// after a cancel must never clobber a newer run's registration, cancel
    /// verdict, or temp files.
    active: Mutex<Option<Arc<Run>>>,
    /// Folded into each run's .tmp names so overlapping runs (old one
    /// draining, new one writing) can never share a temp path.
    run_seq: AtomicU64,
    /// Latched by the quit kill: the app is exiting, so no new sidecar may be
    /// spawned and any cancel becomes an immediate SIGKILL (the escalation
    /// timer would die with the process).
    quitting: AtomicBool,
    /// Every spawned sidecar whose exit has not been observed yet. The
    /// registered run alone cannot reach all of them (see `kill_for_quit`);
    /// membership ends when the process is reaped, so the quit sweep never
    /// signals a stale pid.
    live: Mutex<HashSet<i32>>,
    /// The resolved interpreter that actually has audio-separator, discovered
    /// once and cached (outer `Some` = resolved). A macOS app gets a minimal
    /// PATH (no conda/homebrew), so bare `python3` resolves to the system
    /// Python that lacks the engine — common locations are probed.
    resolved: Mutex<Option<Option<String>>>,
    sweep_done: Mutex<bool>,
    sweep_cv: Condvar,
    /// The quit guard the service is registered with, where its runs are
    /// listed as running.
    guard: Arc<SeparationQuitGuard>,
}

/// Runs the Python separation sidecar (`assets/separation/separate.py`) as a
/// subprocess. Results are cached on disk keyed by a content hash of the
/// source song (and the model), so re-separating the same file is instant.
pub struct ProcessSourceSeparationService {
    inner: Arc<Inner>,
}

/// A tqdm progress bar ("Separating:  3%|▎    | 1/38 [00:02<00:12, 1.7it/s]"),
/// with the "desc:" label that may lead it. A bar redraws with \r and no line
/// break, so text printed while one was on screen (a native abort
/// mid-inference) follows it on the same line: only the bar is cut off, not
/// the line. The bar between its "|"s may hold only tqdm's own glyphs, and
/// any label must end in tqdm's ":", so an error that merely mentions "100%|"
/// keeps its words. A bar cut off anywhere — inside its body, its count, or
/// its brackets — goes whole, while an abort that lands inside its brackets
/// or right after its count keeps its words.
///
/// The Dart pattern, verbatim but for `\d` and `\w`, written as their ASCII
/// classes because Dart's (JavaScript) regexes read them as ASCII.
fn progress_bar() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"^(?:[^%|]{0,60}?:)?\s*[0-9]{1,3}%\|[ #0-9▏▎▍▌▋▊▉█]*",
            // Its count ("17/38", "12.3M/45.6M", "0/?"), or one cut off at
            // the line's end; a count's unit is taken only before its "/" or
            // the line's end, never as the first letter of what follows.
            // Then its brackets: whole, or cut off with nothing after them
            // but tqdm's own characters and part of a rate's unit ("1.7it",
            // "6.1MiB/"). Or nothing at all, when the bar itself was cut off
            // inside its body.
            r"(?:\|\s*",
            r"(?:(?:[0-9.]+[kMGTPEZY]?/(?:[0-9.]+[kMGTPEZY]?(?![A-Za-z0-9_.])|\?)?",
            r"|[0-9.]+[kMGTPEZY]?$)",
            r"(?:\s*\[(?:[^\]]*\]|[0-9:<>,.?\s]*[kMGTPEZYiBts/]{0,6}$))?)?",
            r"|$)",
        ))
        .expect("the progress-bar pattern compiles")
    })
}

/// A Python logging line at DEBUG or INFO ("... - INFO - separator - ..."):
/// the library narrating its run, not why it failed.
fn library_log() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r" - (?:DEBUG|INFO) - ").expect("compiles"))
}

/// The cause a failed run's stderr names: its LAST non-empty line, with a
/// progress bar cut off it, that is not the library's own INFO/DEBUG
/// narration. The sidecar (and a Python traceback) print the real reason
/// last, after audio-separator's own logs.
fn failure_cause(detail: &str) -> String {
    detail
        .split(['\r', '\n'])
        .map(|l| progress_bar().replace(l, "").trim().to_string())
        .rfind(|l| !l.is_empty() && !library_log().is_match(l).unwrap_or(false))
        .unwrap_or_default()
}

/// Sends [signal] to [pid] if it is still a live sidecar of this service.
fn signal_live(inner: &Inner, pid: i32, signal: i32) {
    let live = inner.live.lock().unwrap_or_else(|e| e.into_inner());
    if live.contains(&pid) {
        // SAFETY: kill(2) on a pid we spawned and have not reaped yet (the
        // set is only cleared after `wait` returns, under the same lock).
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

/// A .tmp in the stems cache older than this is an orphan of a dead run — no
/// separation takes anywhere near an hour, so anything older cannot be a
/// live sidecar's output.
const STALE_TMP_AGE: Duration = Duration::from_secs(60 * 60);

/// Upper bound on the persistent stems cache; oldest pairs are evicted first.
const STEMS_CACHE_MAX_BYTES: u64 = 3 * 1024 * 1024 * 1024;

impl ProcessSourceSeparationService {
    /// The service, registered with the app-wide quit guard.
    pub fn new(
        storage: Arc<LocalStorageService>,
        python_executable: &str,
        model_name: Option<&str>,
    ) -> Self {
        Self::with_guard(
            storage,
            python_executable,
            model_name,
            SeparationQuitGuard::instance(),
        )
    }

    /// The service, registered with [guard].
    pub fn with_guard(
        storage: Arc<LocalStorageService>,
        python_executable: &str,
        model_name: Option<&str>,
        guard: Arc<SeparationQuitGuard>,
    ) -> Self {
        let inner = Arc::new(Inner {
            storage,
            python_executable: python_executable.to_string(),
            model_name: model_name.map(str::to_string),
            active: Mutex::new(None),
            run_seq: AtomicU64::new(0),
            quitting: AtomicBool::new(false),
            live: Mutex::new(HashSet::new()),
            resolved: Mutex::new(None),
            sweep_done: Mutex::new(false),
            sweep_cv: Condvar::new(),
            guard: Arc::clone(&guard),
        });
        // Reclaim .tmp orphans left by runs that never got to clean up (hard
        // crash, force-quit, or the quit-path SIGKILL). Done here rather than
        // in the cache cap because the cap only evicts once the 3 GB budget
        // trips — orphans below the budget would otherwise live forever, and
        // the cap's .wav filter would skip them anyway. Age-gated so another
        // app instance's live run is never hit. Best-effort, in the
        // background; it never fails.
        let sweeper = inner.clone();
        std::thread::spawn(move || {
            sweep_stale_tmps(&sweeper);
            *sweeper.sweep_done.lock().unwrap_or_else(|e| e.into_inner()) = true;
            sweeper.sweep_cv.notify_all();
        });
        // Let the quit path kill an in-flight sidecar before exit. The
        // service is app-lifetime, so it never unregisters; the hook holds it
        // weakly so a dropped service (tests) is not kept alive.
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        guard.register(Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                kill_for_quit(&inner);
            }
        }));
        ProcessSourceSeparationService { inner }
    }

    /// Test hook: returns once the construction-time .tmp sweep has finished.
    pub fn wait_stale_tmp_sweep(&self) {
        let mut d = self
            .inner
            .sweep_done
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while !*d {
            d = self
                .inner
                .sweep_cv
                .wait(d)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    fn python_candidates(&self) -> Vec<String> {
        let mut c = vec![
            self.inner.python_executable.clone(), // explicit override / 'python3'
            "python3".to_string(),
            "/opt/miniconda3/bin/python3".to_string(),
            "/opt/anaconda3/bin/python3".to_string(),
            "/opt/homebrew/bin/python3".to_string(),
            "/usr/local/bin/python3".to_string(),
        ];
        if let Ok(home) = std::env::var("HOME") {
            c.push(format!("{home}/miniconda3/bin/python3"));
            c.push(format!("{home}/anaconda3/bin/python3"));
        }
        c
    }

    /// Finds (once, cached) a Python interpreter that has audio-separator.
    fn resolve_python(&self) -> Option<String> {
        if let Some(r) = self
            .inner
            .resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return r;
        }
        let mut tried = HashSet::new();
        let mut found = None;
        for candidate in self.python_candidates() {
            if candidate.is_empty() || !tried.insert(candidate.clone()) {
                continue;
            }
            if has_engine(&candidate) {
                found = Some(candidate);
                break;
            }
        }
        *self
            .inner
            .resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(found.clone());
        found
    }

    /// The effective model identity: explicit override wins, else the env
    /// var, else mirrors the sidecar's own default. Folded into the cache key
    /// so a model change invalidates prior stems.
    fn effective_model(&self) -> String {
        self.inner
            .model_name
            .clone()
            .or_else(|| std::env::var("VOCARA_SEP_MODEL").ok())
            .unwrap_or_else(|| DEFAULT_SEPARATION_MODEL.to_string())
    }

    /// Copies the bundled sidecar to a stable path the interpreter can run,
    /// rewriting only when the script changes.
    fn ensure_script(&self) -> Result<PathBuf, SeparationError> {
        let dir = self.inner.storage.asset_dir("separation")?;
        let dest = dir.join("separate.py");
        let current = fs::read_to_string(&dest).ok();
        if current.as_deref() != Some(SEPARATE_PY) {
            fs::write(&dest, SEPARATE_PY)?;
        }
        Ok(dest)
    }

    fn separate_run(
        &self,
        run: &Arc<Run>,
        source: &Path,
        on_progress: Progress<'_>,
    ) -> Result<SeparationResult, SeparationError> {
        let inner = &self.inner;
        let progress = |v: f64| {
            if let Some(p) = on_progress {
                p(v);
            }
        };
        progress(0.0);
        let cache_dir = inner.storage.asset_dir("stems_cache")?;
        let model = self.effective_model();
        // Key by source content AND model, so switching models doesn't return
        // stale stems for the same song.
        let model_tag = &hex::encode(Md5::digest(model.as_bytes()))[..8];
        let key = format!("{}.{model_tag}", hash_file(source)?);
        let inst = cache_dir.join(format!("{key}.instrumental.wav"));
        let voc = cache_dir.join(format!("{key}.vocal.wav"));

        // Cache hit — separation is expensive, so reuse prior stems verbatim
        // (but still honor a cancel that landed during hashing).
        if inst.exists() && voc.exists() {
            if run.is_cancelled() {
                return Err(SeparationError::Cancelled);
            }
            progress(1.0);
            return Ok(SeparationResult {
                instrumental_path: inst,
                vocal_path: voc,
            });
        }

        let Some(python) = self.resolve_python() else {
            return Err(SeparationError::Failed(
                "Separation engine not found. The audio-separator tool is not installed in \
                 any known Python."
                    .into(),
            ));
        };

        // Write to temp paths and atomically rename in only on success, so the
        // cache never holds a partial/old pair from a failed run. The temp
        // names carry a per-run nonce: a deterministic '<cachepath>.tmp' would
        // collide across overlapping runs (an old run's cleanup deleting the
        // very files a new run's sidecar is writing). The final rename target
        // stays the deterministic cache path.
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        let nonce = format!("{micros}.{}", inner.run_seq.fetch_add(1, Ordering::SeqCst));
        let inst_tmp = PathBuf::from(format!("{}.{nonce}.tmp", inst.display()));
        let voc_tmp = PathBuf::from(format!("{}.{nonce}.tmp", voc.display()));
        let script = self.ensure_script()?;
        // The checkpoint (hundreds of MB) lives in app support:
        // audio-separator's default is /tmp, which macOS clears at reboot, so
        // the first separation after each one re-downloaded the model.
        let model_dir = inner
            .storage
            .asset_dir(&Path::new("separation").join("models").to_string_lossy())?;
        let args: Vec<String> = vec![
            script.to_string_lossy().into_owned(),
            "--input".into(),
            source.to_string_lossy().into_owned(),
            "--instrumental".into(),
            inst_tmp.to_string_lossy().into_owned(),
            "--vocal".into(),
            voc_tmp.to_string_lossy().into_owned(),
            "--model".into(),
            model.clone(),
            "--model-dir".into(),
            model_dir.to_string_lossy().into_owned(),
        ];

        // A quit (or cancel) that arrived during the hashing/resolution phase
        // must not spawn a sidecar at all: at quit there is no process left
        // to run the SIGTERM->SIGKILL escalation, so a spawned-then-SIGTERMed
        // torch process could ignore the signal and orphan. `quitting` also
        // covers a FRESH run entered after the quit kill (its own cancelled
        // flag is still false).
        if run.is_cancelled() || inner.quitting.load(Ordering::SeqCst) {
            return Err(SeparationError::Cancelled);
        }
        let mut child = Command::new(&python)
            .args(&args)
            .env("PATH", augmented_path(&python))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| {
                SeparationError::Failed(
                    "Could not start the separation engine. Is Python installed?".into(),
                )
            })?;
        let pid = child.id() as i32;
        inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid);
        *run.pid.lock().unwrap_or_else(|e| e.into_inner()) = Some(pid);
        // A cancel that raced the spawn still aborts the just-started
        // process. During a quit, SIGKILL directly (the escalation timer dies
        // with the exiting app); otherwise the normal SIGTERM->SIGKILL dance.
        if run.is_cancelled() {
            if inner.quitting.load(Ordering::SeqCst) {
                signal_live(inner, pid, libc::SIGKILL);
            } else {
                cancel_run(inner, run);
            }
        }

        // Set once the stems are renamed into the cache; until then the .tmp
        // files are orphans the cleanup below must remove on ANY exit.
        let mut renamed_in = false;
        let mut reaped = false;
        let result = (|| -> Result<SeparationResult, SeparationError> {
            // The sidecar's coarse stdout markers bracket the phases; during
            // the long inference phase, audio-separator prints a tqdm bar to
            // stderr ("45%|…"), mapped into the 0.15–0.85 band so the bar
            // actually moves instead of sticking at 15%. tqdm is only trusted
            // once inference has started (so a first-run model-download bar
            // doesn't drive it backwards).
            let in_inference = Arc::new(AtomicBool::new(false));
            let stderr = child.stderr.take();
            let stdout = child.stdout.take();
            let err_bytes = std::thread::scope(|scope| {
                let flag = in_inference.clone();
                let err_reader = scope.spawn(move || {
                    let mut buf = Vec::new();
                    let Some(mut stderr) = stderr else {
                        return buf;
                    };
                    let tqdm = tqdm_percent();
                    let mut chunk = [0u8; 8192];
                    loop {
                        match stderr.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if !flag.load(Ordering::SeqCst) {
                                    continue;
                                }
                                let text = String::from_utf8_lossy(&chunk[..n]);
                                if let Some(pct) = tqdm
                                    .captures_iter(text.as_ref())
                                    .filter_map(|c| c.ok())
                                    .filter_map(|c| {
                                        c.get(1).and_then(|m| m.as_str().parse::<i64>().ok())
                                    })
                                    .last()
                                {
                                    progress(
                                        (0.15 + pct.clamp(0, 100) as f64 / 100.0 * 0.70)
                                            .clamp(0.0, 1.0),
                                    );
                                }
                            }
                        }
                    }
                    buf
                });
                if let Some(stdout) = stdout {
                    let marker = progress_marker();
                    for line in BufReader::new(stdout).split(b'\n') {
                        let Ok(line) = line else { break };
                        let line = String::from_utf8_lossy(&line);
                        if let Ok(Some(m)) = marker.captures(line.trim()) {
                            if let Some(v) = m.get(1).and_then(|g| g.as_str().parse::<f64>().ok()) {
                                in_inference.store((0.15..0.85).contains(&v), Ordering::SeqCst);
                                progress(v.clamp(0.0, 1.0));
                            }
                        }
                    }
                }
                err_reader.join().unwrap_or_default()
            });
            let status = child.wait()?;
            inner
                .live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&pid);
            reaped = true;
            let code = exit_code(&status);
            let detail = String::from_utf8_lossy(&err_bytes).trim().to_string();

            if run.is_cancelled() {
                delete_quietly(&inst_tmp);
                delete_quietly(&voc_tmp);
                return Err(SeparationError::Cancelled);
            }
            // Existence is not enough: an empty/edge-case model output
            // re-encodes to a structurally-valid 0-frame WAV (44-byte header,
            // exit 0), which would be committed and imported as a silent
            // zero-duration take. Require real audio frames.
            let inst_ok = fs::metadata(&inst_tmp)
                .map(|m| m.len() > 44)
                .unwrap_or(false);
            let voc_ok = fs::metadata(&voc_tmp)
                .map(|m| m.len() > 44)
                .unwrap_or(false);
            if code != 0 || !inst_ok || !voc_ok {
                delete_quietly(&inst_tmp);
                delete_quietly(&voc_tmp);
                let cause = failure_cause(&detail);
                // Ended by a signal (negative on POSIX): say which, with what
                // it printed. A SIGKILL comes from outside the run, whatever
                // it printed last, and is most often the system ending a
                // model run that ran out of memory.
                if code == -9 {
                    return Err(SeparationError::Failed(
                        "Separation stopped: the system ended the separation engine \
                         (signal 9), possibly for lack of memory."
                            .into(),
                    ));
                }
                if code < 0 {
                    return Err(SeparationError::Failed(if cause.is_empty() {
                        format!("Separation failed (signal {}).", -code)
                    } else {
                        format!("Separation failed (signal {}): {cause}", -code)
                    }));
                }
                return Err(SeparationError::Failed(if !cause.is_empty() {
                    format!("Separation failed: {cause}")
                } else if code != 0 {
                    format!("Separation failed (exit {code}).")
                } else {
                    "Separation produced empty stems.".to_string()
                }));
            }
            fs::rename(&inst_tmp, &inst)?;
            fs::rename(&voc_tmp, &voc)?;
            renamed_in = true;
            // Keep the (deliberately persistent) stems cache bounded — each
            // pair is tens of MB and nothing else reclaims it; the kept
            // instrumental already lives content-addressed in the asset pool.
            cap_stems_cache(&cache_dir, STEMS_CACHE_MAX_BYTES, Some(&key));
            progress(1.0);
            // Re-check last: a Cancel landing in the commit (the bar already
            // near 100%) must still win — the caller would otherwise import
            // stems the user cancelled. The committed pair stays cached; a
            // retry is an instant cache hit.
            if run.is_cancelled() {
                return Err(SeparationError::Cancelled);
            }
            Ok(SeparationResult {
                instrumental_path: inst.clone(),
                vocal_path: voc.clone(),
            })
        })();
        // Ensure the subprocess is never left running: on an unexpected
        // stream error before its exit was observed, tear the (heavy torch)
        // subprocess down instead of orphaning it, and reap it.
        if !reaped {
            signal_live(inner, pid, libc::SIGKILL);
            let _ = child.wait();
            inner
                .live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&pid);
        }
        // Clean up orphan .tmp files on any path that didn't rename them in:
        // the stems-cache cap only reclaims .wav files, so a stray .tmp would
        // accumulate unbounded across failed runs.
        if !renamed_in {
            delete_quietly(&inst_tmp);
            delete_quietly(&voc_tmp);
        }
        result
    }
}

impl SourceSeparationService for ProcessSourceSeparationService {
    fn is_available(&self) -> bool {
        self.resolve_python().is_some()
    }

    /// Same-screen re-entry is guarded upstream, but a run can outlive its
    /// view model: navigating away cancels it, yet the service is
    /// app-lifetime and the sidecar takes up to seconds to die — reopening
    /// the song can start a separation while the old run is still draining.
    /// The old run is cancelled here and waited out in [`separate_with`]
    /// rather than refused: the new request is the one the user actually
    /// wants. Two live sidecars would also fight over CPU/RAM.
    ///
    /// [`separate_with`]: SourceSeparationService::separate_with
    fn begin(&self) -> SeparationTicket {
        let run = Arc::new(Run::new());
        let prior = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(run.clone());
        if let Some(p) = &prior {
            if !p.is_done() {
                cancel_run(&self.inner, p);
            }
        }
        SeparationTicket {
            run: Some(run),
            prior,
            _running: Some(self.inner.guard.begin_run()),
        }
    }

    fn separate_with(
        &self,
        ticket: SeparationTicket,
        source: &Path,
        on_progress: Progress<'_>,
    ) -> Result<SeparationResult, SeparationError> {
        let Some(run) = ticket.run else {
            return Err(SeparationError::Cancelled);
        };
        if let Some(prior) = &ticket.prior {
            prior.wait_done();
        }
        let result = if run.is_cancelled() {
            Err(SeparationError::Cancelled)
        } else {
            self.separate_run(&run, source, on_progress)
        };
        // Unregister ONLY if this run is still the registered one — never
        // clobber a newer run's registration (that would leave the newer run
        // uncancellable).
        {
            let mut active = self.inner.active.lock().unwrap_or_else(|e| e.into_inner());
            if active.as_ref().is_some_and(|a| Arc::ptr_eq(a, &run)) {
                *active = None;
            }
        }
        run.mark_done();
        result
    }

    fn cancel(&self) {
        // Flags the CURRENT registered run; a stale run draining in the
        // background already carries its own verdict and is not re-targeted.
        let run = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(run) = run {
            cancel_run(&self.inner, &run);
        }
    }
}

/// Flags [run] cancelled and terminates its process. Safe before the process
/// exists (hashing/cache phase): the run re-routes through here right after
/// spawning when it finds the flag already set.
fn cancel_run(inner: &Arc<Inner>, run: &Arc<Run>) {
    run.cancelled.store(true, Ordering::SeqCst);
    let Some(pid) = *run.pid.lock().unwrap_or_else(|e| e.into_inner()) else {
        return;
    };
    signal_live(inner, pid, libc::SIGTERM);
    // A native torch/ML process may ignore SIGTERM while in a long compute
    // call, which would hang the teardown (and the overlay). Escalate to
    // SIGKILL if it hasn't exited shortly — SIGKILL can't be ignored, so
    // stdout closes and the exit resolves. The run being done is its own
    // signal that the process was reaped and no escalation is needed.
    let inner = inner.clone();
    let run = run.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        if !run.is_done() {
            signal_live(&inner, pid, libc::SIGKILL);
        }
    });
}

/// Quit-path teardown (see [`SeparationQuitGuard`]): SIGKILL immediately —
/// the app is exiting, so there is no time for the escalation dance. The
/// orphaned .tmp output is reclaimed by the next launch's sweep.
fn kill_for_quit(inner: &Arc<Inner>) {
    inner.quitting.store(true, Ordering::SeqCst);
    if let Some(run) = inner
        .active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        run.cancelled.store(true, Ordering::SeqCst);
    }
    // Sweep EVERY live sidecar, not just the registered run's process: while
    // a new run drains its superseded predecessor, the predecessor's sidecar
    // is alive but no longer reachable through the registration (whose own
    // process does not exist yet) — and its escalation timer dies with the
    // exiting app, so a SIGTERM-ignoring compute phase would orphan it.
    let pids: Vec<i32> = inner
        .live
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .collect();
    for pid in pids {
        signal_live(inner, pid, libc::SIGKILL);
    }
}

fn tqdm_percent() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([0-9]{1,3})%\|").expect("compiles"))
}

fn progress_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^PROGRESS\s+([0-9.]+)").expect("compiles"))
}

/// Dart's `Process.exitCode`: the exit status, or the negated signal number
/// for a process a signal ended (POSIX).
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -sig;
        }
    }
    -1
}

/// `find_spec` verifies audio-separator is importable without the heavy
/// `import audio_separator` (which would pull in torch).
fn has_engine(python: &str) -> bool {
    Command::new(python)
        .args([
            "-c",
            "import importlib.util,sys;sys.exit(0 if importlib.util.find_spec(\"audio_separator\") else 1)",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false) // interpreter not found / not runnable
}

/// PATH augmented with common tool locations. A macOS app gets a minimal
/// PATH (no homebrew/conda), and audio-separator shells out to ffmpeg to
/// write the stems — so without this the separation fails near the very end
/// with an "ffmpeg not found" error.
fn augmented_path(python: &str) -> String {
    // Dart's p.dirname of a bare name is ".".
    let bin = match Path::new(python).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    };
    let current = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    [
        bin.as_str(),
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/opt/miniconda3/bin",
        current.as_str(),
    ]
    .join(":")
}

fn hash_file(path: &Path) -> Result<String, SeparationError> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

fn delete_quietly(path: &Path) {
    // best-effort cleanup
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

fn sweep_stale_tmps(inner: &Inner) {
    // Best-effort: a failed sweep must never break separation itself.
    let Ok(dir) = inner.storage.asset_dir("stems_cache") else {
        return;
    };
    let Some(cutoff) = SystemTime::now().checked_sub(STALE_TMP_AGE) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_file() || !path.to_string_lossy().ends_with(".tmp") {
            continue;
        }
        // A vanished/locked file is fine; the next launch retries.
        if let Ok(modified) = e.metadata().and_then(|m| m.modified()) {
            if modified < cutoff {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// Evicts the oldest stem PAIRS until the cache directory is under
/// [max_bytes]. Best-effort: never fails. [keep] is the cache key of the run
/// that just wrote (its pair is the return value), which is NEVER evicted
/// even if that single pair exceeds [max_bytes] — otherwise the loop would
/// delete the files about to be returned.
///
/// Eviction is by cache key (the `<hash>.<model>` prefix shared by a song's
/// instrumental and vocal stems), not by file: a cache hit needs BOTH files,
/// so deleting only one would leave a useless orphan that still counts toward
/// the budget and forces a needless re-separation. A pre-existing orphan (its
/// own group) is reclaimed too.
fn cap_stems_cache(dir: &Path, max_bytes: u64, keep: Option<&str>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut group_bytes: HashMap<String, u64> = HashMap::new();
    let mut group_mtime: HashMap<String, SystemTime> = HashMap::new();
    let mut total = 0u64;
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if !path.is_file() || !name.ends_with(".wav") {
            continue;
        }
        let key = name
            .strip_suffix(".instrumental.wav")
            .or_else(|| name.strip_suffix(".vocal.wav"))
            .unwrap_or(&name) // an unexpected stray file is its own group
            .to_string();
        let Ok(meta) = e.metadata() else { continue };
        let len = meta.len();
        let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
        groups.entry(key.clone()).or_default().push(path);
        *group_bytes.entry(key.clone()).or_default() += len;
        total += len;
        // A pair's age is its most-recently-modified file.
        let cur = group_mtime.entry(key).or_insert(mtime);
        if mtime > *cur {
            *cur = mtime;
        }
    }
    if total <= max_bytes {
        return;
    }
    let mut keys: Vec<String> = groups.keys().cloned().collect();
    keys.sort_by_key(|k| group_mtime[k]);
    for key in keys {
        if total <= max_bytes {
            break;
        }
        // Never evict the run that just wrote; if it alone exceeds the cap
        // the cache transiently stays over max_bytes — far better than
        // returning paths to files just deleted.
        if Some(key.as_str()) == keep {
            continue;
        }
        for f in &groups[&key] {
            let _ = fs::remove_file(f);
        }
        total -= group_bytes[&key];
    }
}
