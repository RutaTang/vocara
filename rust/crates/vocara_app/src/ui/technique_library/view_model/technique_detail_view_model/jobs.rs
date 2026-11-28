//! The detail view model's off-UI-thread work, what comes back from it, and
//! the few things it shares with its workers.
//!
//! A [`Job`] is one Dart `await` on I/O: the repository read or save, an
//! analysis, a file copy or delete, a recording start's pending half, an
//! export. The screen runs it on a blocking worker ([`Job::run`]) and hands
//! the [`Done`] to `TechniqueDetailViewModel::land`, where the Dart code
//! after that `await` runs. The job carries everything its worker needs and
//! nothing of the view model: what a worker must read of the view model's
//! state (Dart's `wanted` predicates) it reads from a [`Probe`] the view
//! model publishes to.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use vocara_model::models::technique_media::TechniqueMedia;
use vocara_services::app_settings_service::AppSettingsService;
use vocara_services::graph_audio_playback_service::GraphAudioPlaybackService;
use vocara_services::playback_engine::PlaybackEngine;

use crate::util::panic_message;

/// What a [`Job`] does: for the screen's diagnostics, and for the tests,
/// which hold a kind back where a Dart test parked its fake on a completer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// The technique's read (`load`, run once on creation and when a remote
    /// change finds no technique to refresh).
    Load,
    /// A freshness reload after a remote change (`_reloadFromRemote`).
    Reload,
    /// A reference example's analysis (`ensureReferenceAnalysis`).
    AnalyzeReference,
    /// A practice take's analysis; its temp folder is deleted after it.
    AnalyzePractice,
    /// A dirty example take's analysis, which decides whether it is staged
    /// (its temp folder is deleted when it is not).
    ValidateExample,
    /// A recording start's pending half: the permission prompt, the take's
    /// temp folder, the mic open and its ready handshake.
    StartRecording,
    /// An example clip's copy into the sandbox: a dropped file, or a
    /// recorded take (whose temp folder goes once it is copied).
    Import,
    /// The technique's three-way save.
    Commit,
    /// The technique's delete: its rows, then its media folder.
    Delete,
    /// An example's export: the save panel and the copy.
    Export,
    /// An example file's delete, its failure logged.
    DeleteMedia,
    /// A take's temp folder's delete.
    DeleteTake,
    /// The page engine's dispose.
    DisposeEngine,
}

type Work = Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>;

/// A job's value as it lands: the work's, or why there is none.
pub(super) type Value = Result<Box<dyn Any + Send>, String>;

/// Work for a blocking worker thread (see the module docs).
pub struct Job {
    pub(super) id: u64,
    pub(super) kind: JobKind,
    pub(super) work: Work,
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Job {
    /// What the job does.
    pub fn kind(&self) -> JobKind {
        self.kind
    }

    /// Runs the work on this thread (it blocks) and returns what to land. A
    /// panic in the work comes back as the job's failure, never as a stuck
    /// view model: each landing takes it the way the Dart code took a throw.
    pub fn run(self) -> Done {
        let value = catch_unwind(AssertUnwindSafe(self.work))
            .map_err(|payload| panic_message(payload.as_ref()));
        Done {
            id: self.id,
            kind: self.kind,
            value: Arc::new(Mutex::new(Some(value))),
        }
    }

    /// What the job reports when its worker never came back (the runtime
    /// lost it): its failure, so the flow waiting on it does not stay busy.
    /// Take it before moving the job onto the worker.
    pub fn lost(&self, why: &str) -> Done {
        Done {
            id: self.id,
            kind: self.kind,
            value: Arc::new(Mutex::new(Some(Err(why.to_string())))),
        }
    }

    /// Runs the job on a thread of its own and drops what it reports: for
    /// the clean-up a page that is gone hands over (see
    /// `TechniqueDetailViewModel::dispose`), which nothing waits for.
    pub fn detach(self) {
        let spawned = std::thread::Builder::new()
            .name("technique-cleanup".into())
            .spawn(move || drop(self.run()));
        if let Err(e) = spawned {
            vocara_model::diagnostics::DiagnosticsLog::instance()
                .log("technique", &format!("cleanup thread failed: {e}"));
        }
    }
}

/// What a [`Job`] reported. Cloneable (the screen's messages are), but
/// landed once: a second landing of the same report finds nothing waiting.
#[derive(Clone)]
pub struct Done {
    pub(super) id: u64,
    kind: JobKind,
    value: Arc<Mutex<Option<Value>>>,
}

impl fmt::Debug for Done {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Done")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Done {
    /// What the job did.
    pub fn kind(&self) -> JobKind {
        self.kind
    }

    /// The work's value, or why there is none (it panicked, or was lost).
    pub(super) fn take(&self) -> Value {
        self.value
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| Err("already landed".to_string()))
    }
}

/// Something the screen does for the view model.
pub enum Effect {
    /// Run the job on a blocking worker ([`Job::run`]) and land its [`Done`].
    Run(Job),
    /// Every example player pauses (Dart's `micArmed`): a take started
    /// arming. They play outside the engine, so its echo canceller has no
    /// reference to subtract: a clip left playing on speakers was recorded
    /// into the very take it is compared against.
    PauseExamples,
    /// The analyzed example's player seeks here (Dart's `analysisScrub`): the
    /// user scrubbed the analysis. Every request is one, an equal target
    /// included — a repeat scrub (back to a time the player has since played
    /// past) must still move the player.
    SeekAnalyzed(Duration),
}

impl fmt::Debug for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Effect::Run(job) => f.debug_tuple("Run").field(job).finish(),
            Effect::PauseExamples => f.write_str("PauseExamples"),
            Effect::SeekAnalyzed(to) => f.debug_tuple("SeekAnalyzed").field(to).finish(),
        }
    }
}

/// What a Dart method that returned a value answered, once it had: the
/// screen acts on these as the Dart screen acted on the awaited result.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// `startPractice`: the message to toast, or `None` (started, or nothing
    /// to say).
    PracticeStart(Option<String>),
    /// `startExampleRecording`: the message to toast, or `None`.
    ExampleStart(Option<String>),
    /// `stopExampleRecording`: the clip it staged, or `None` (not recording,
    /// the take failed, or the draft was abandoned mid-import).
    ExampleStop(Option<TechniqueMedia>),
    /// `save`: whether it persisted (the screen leaves edit mode on `true`,
    /// and says "Couldn't save" on `false`).
    Saved(bool),
    /// `discardMediaDraft`: whether it ran (a save in flight holds it off,
    /// and the edit must then stay open).
    DraftDiscarded(bool),
    /// `delete`: whether it persisted (the screen navigates back on `true`).
    Deleted(bool),
}

/// What a call on the view model asks of the screen.
#[must_use = "the effects must be run, and the answers acted on"]
#[derive(Debug, Default)]
pub struct Next {
    /// To run or do, in order.
    pub effects: Vec<Effect>,
    /// The values Dart's methods returned, in the order they did.
    pub answers: Vec<Answer>,
}

impl Next {
    /// Appends what `other` asks.
    pub fn extend(&mut self, other: Next) {
        self.effects.extend(other.effects);
        self.answers.extend(other.answers);
    }

    /// The jobs to run, the other effects left out.
    pub fn jobs(self) -> Vec<Job> {
        self.effects
            .into_iter()
            .filter_map(|e| match e {
                Effect::Run(job) => Some(job),
                _ => None,
            })
            .collect()
    }
}

/// Guards a flow holds until it settles — a quit hold, a quit's answer — and
/// that each of its jobs holds while it runs: a page gone mid-flow drops its
/// landings, and the work in flight still keeps the guard until it is done.
#[derive(Clone, Default)]
pub(super) struct Keep(Vec<Arc<dyn Any + Send + Sync>>);

impl Keep {
    pub(super) fn with(mut self, guard: impl Any + Send + Sync) -> Keep {
        self.0.push(Arc::new(guard));
        self
    }
}

/// A one-shot gate a thread can wait on.
#[derive(Default)]
pub(super) struct Latch {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Latch {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn open(&self) {
        *self.lock() = true;
        self.opened.notify_all();
    }

    pub(super) fn wait(&self) {
        let mut open = self.lock();
        while !*open {
            open = self
                .opened
                .wait(open)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A quit's call to the view model's `quitFinalize`, answered when dropped:
/// the finalizer thread that asked waits until then.
pub(super) struct QuitAnswer(Arc<Latch>);

impl Drop for QuitAnswer {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// The quit finalizer's way into the view model. The recording controller
/// calls its quit finalize on a finalizer thread, and Dart's `quitFinalize`
/// ran on the UI isolate: the call is queued here and waits, the screen's
/// tick hands the queue to the view model, which answers each once its quit
/// path has settled.
#[derive(Default)]
pub(super) struct QuitRequests {
    state: Mutex<QuitState>,
}

#[derive(Default)]
struct QuitState {
    closed: bool,
    pending: Vec<QuitAnswer>,
}

impl QuitRequests {
    fn lock(&self) -> MutexGuard<'_, QuitState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a quit's call and waits until the view model answers it (at
    /// once once the page is gone).
    pub(super) fn request_and_wait(&self) {
        let latch = Arc::new(Latch::default());
        {
            let mut st = self.lock();
            if st.closed {
                return;
            }
            st.pending.push(QuitAnswer(Arc::clone(&latch)));
        }
        latch.wait();
    }

    /// Whether a quit's call waits for the view model.
    pub(super) fn pending(&self) -> bool {
        !self.lock().pending.is_empty()
    }

    /// The calls waiting, each answered when its guard drops.
    pub(super) fn drain(&self) -> Vec<QuitAnswer> {
        std::mem::take(&mut self.lock().pending)
    }

    /// The page is gone: every call waiting is answered, and later ones are
    /// at once.
    pub(super) fn close(&self) {
        let pending = {
            let mut st = self.lock();
            st.closed = true;
            std::mem::take(&mut st.pending)
        };
        drop(pending);
    }
}

/// What a worker reads of the view model's state: Dart's `wanted`
/// predicates read the view model live; here the view model publishes what
/// they read on every notify.
#[derive(Default)]
pub(super) struct Probe {
    disposed: AtomicBool,
    practice_generation: AtomicU64,
    reference: Mutex<ReferenceView>,
}

#[derive(Default)]
struct ReferenceView {
    /// The shown reference example's path.
    path: Option<String>,
    /// In-flight analyses a commit moved: old path -> where they land.
    moves: HashMap<String, String>,
}

impl Probe {
    fn reference(&self) -> MutexGuard<'_, ReferenceView> {
        self.reference
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn dispose(&self) {
        self.disposed.store(true, Ordering::SeqCst);
    }

    pub(super) fn set_practice_generation(&self, generation: u64) {
        self.practice_generation.store(generation, Ordering::SeqCst);
    }

    pub(super) fn set_reference(&self, path: Option<String>, moves: &HashMap<String, String>) {
        let mut view = self.reference();
        view.path = path;
        if view.moves != *moves {
            view.moves = moves.clone();
        }
    }

    /// `ensureReferenceAnalysis`'s `wanted`: the page is open and the clip
    /// analyzed at [path] (or where a commit moved it) is still the shown
    /// reference.
    pub(super) fn reference_wanted(&self, path: &str) -> bool {
        if self.disposed.load(Ordering::SeqCst) {
            return false;
        }
        let view = self.reference();
        let landed = view.moves.get(path).map_or(path, String::as_str);
        view.path.as_deref() == Some(landed)
    }

    /// `stopPractice`'s `ours`: the page is open and the take was not dropped
    /// since [generation].
    pub(super) fn practice_wanted(&self, generation: u64) -> bool {
        !self.disposed.load(Ordering::SeqCst)
            && self.practice_generation.load(Ordering::SeqCst) == generation
    }
}

/// Builds the page's engine (Dart's `createEngine`; a test injects its own).
pub type CreateEngine = Arc<dyn Fn() -> Arc<GraphAudioPlaybackService> + Send + Sync>;

/// The page's own engine, built on its first take (see
/// `TechniqueDetailViewModel::acquire_engine`). Shared with the recording
/// controller, which borrows it on the thread that runs a start.
pub(super) struct EngineSlot {
    engine: Mutex<Option<Arc<GraphAudioPlaybackService>>>,
    create: CreateEngine,
    settings: Option<Arc<AppSettingsService>>,
}

impl EngineSlot {
    pub(super) fn new(create: CreateEngine, settings: Option<Arc<AppSettingsService>>) -> Self {
        EngineSlot {
            engine: Mutex::new(None),
            create,
            settings,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Arc<GraphAudioPlaybackService>>> {
        self.engine.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The engine for the next take: this page's own, built on the first
    /// take. One that failed to open (no output device then) never opens by
    /// itself, and kept, every take failed until the page was reopened: it
    /// is replaced, so connecting a device and pressing Record again works.
    /// So is one whose microphone did not deliver on the last take (even
    /// after the recording controller's reopen): its audio host and mic unit
    /// may be wedged in a way no reopen on it clears, and this page plays
    /// nothing through its engine, so a fresh one costs nothing but the build.
    /// The old one is disposed first, here on the start's worker, so its mic
    /// unit is gone before the new engine opens one.
    pub(super) fn acquire(&self) -> Arc<GraphAudioPlaybackService> {
        let engine = {
            let mut slot = self.lock();
            if let Some(dud) = slot.take_if(|e| !e.is_available() || e.input_failed()) {
                PlaybackEngine::dispose(&*dud);
            }
            Arc::clone(slot.get_or_insert_with(|| (self.create)()))
        };
        // Record from the input the user chose in Settings, device and
        // channel (this view model owns its own engine, so without this it
        // would capture from the OS default and silently contradict the
        // global selection). Read per take, so a choice changed between
        // takes applies to the next. No-op when the engine is unavailable.
        if let Some(settings) = &self.settings {
            settings.apply_input_to(&engine);
        }
        engine
    }

    /// The engine built so far, taken out for its dispose.
    pub(super) fn take(&self) -> Option<Arc<GraphAudioPlaybackService>> {
        self.lock().take()
    }
}
