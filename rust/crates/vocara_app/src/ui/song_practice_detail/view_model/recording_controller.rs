//! Every consumer of the engine's INPUT side for the song detail screen (port
//! of `ui/song_practice_detail/view_model/recording_controller.dart`), in one
//! place: take recording (start/stop/finalize) on the shared
//! [`MicRecordingController`] core, input monitoring (headphone toggle + MIC
//! meter), the 1 s device watch (output-lost reopen, input-lost teardown, the
//! macOS-VPIO input-stall watchdog), the record/input meters and the live
//! recording waveform/clock.
//!
//! It borrows the engine (never creates or disposes it) and edits the model
//! only through the host ([`RecordingHost`]) — take staging (arm target,
//! empty take, append + select the recorded clip, save) stays view-model
//! logic invoked from here in the recording order.
//!
//! # How the view model drives it
//!
//! Dart's constructor closures onto the view model become a
//! [`RecordingHost`] handed to each call and read at the moment of the call.
//! What Dart did on its sibling controllers and through its async closures
//! comes back as [`RecordingEffect`]s, in order: the view model applies each
//! right after the call returns (a sibling call: the live view off, the
//! record clock's playhead write, the loop re-arm, the analysis refresh) or
//! runs it off the UI thread and hands its outcome back to the method the
//! effect names (the resync, the saves, the empty take, the file work, the
//! mic's start). Applied right after the call, a sibling call lands before
//! any other event, where Dart made it inline. Every method that can produce
//! work returns its effects (`#[must_use]`). `notifyListeners` bumps
//! [`RecordingController::revision`]; the meters and the live waveform, which
//! Dart kept in `ValueNotifier`s, bump [`RecordingController::meter_revision`]
//! and [`RecordingController::peaks_revision`].
//!
//! # The take lifecycle
//!
//! - **Start** ([`RecordingController::start_recording`]): Dart's
//!   `MicRecordingController.start` is the mic's two-phase start: the arm
//!   window opens on the UI thread ([`MicRecorder::begin_start`]) and the
//!   rest runs on a worker ([`RecordingEffect::Start`], a [`StartJob`]). The
//!   take setup Dart ran inside the destination's `create` — after the
//!   permission gate, so a denial never creates an empty take — runs on the
//!   UI thread: the job hands its [`TakeSetupRequest`] to the screen, which
//!   gives it to [`RecordingController::take_setup`], and the worker waits
//!   for the answer before it creates the WAV. The rest of Dart's `_armTake`
//!   runs in [`RecordingController::start_done`] and
//!   [`RecordingController::resynced`], with the same take-generation guards.
//! - **Stop** ([`RecordingController::stop_recording`]): stops the mic at
//!   once, holds the quit and lists the take as landing for the whole tail
//!   (the mic's stop-finalize hold and the finalizer's landing, moved along
//!   with the work), places the take, then runs the finalize under the busy
//!   gate (retried every 50 ms while an edit holds it), stages the take and
//!   saves it, again when a save did not keep it.
//! - **Quit**: the finalizers a quit runs are called on the quit's threads,
//!   and what they do is UI-thread work. They post a request and wait: the
//!   screen serves it with [`RecordingController::serve_quit_requests`] when
//!   the waker set by [`RecordingController::set_quit_waker`] fires, and the
//!   finalizer returns once what Dart awaited has settled.
//! - **Leaving** ([`RecordingController::stop_before_leaving`]): the page
//!   leaves on [`RecordingEffect::Leave`].
//!
//! The finalize of a stopped take outlives the screen (Stop, then Back): as
//! Dart's did after dispose, the controller keeps taking the outcomes of its
//! effects after [`RecordingController::dispose`], and the view model keeps
//! answering them until [`RecordingController::is_stopping`] reads false.
//!
//! # Timers
//!
//! The screen drives the controller's timers by calling it while each is
//! active: the mic's level tick ([`RecordingController::mic_tick`], while
//! [`RecordingController::next_mic_tick_due`] is `Some`), the monitor meter
//! ([`RecordingController::monitor_meter_tick`] every
//! [`RecordingController::MONITOR_METER_PERIOD`] while
//! [`RecordingController::monitor_meter_active`]) and the device watch
//! ([`RecordingController::device_watch_tick`] every
//! [`RecordingController::DEVICE_WATCH_PERIOD`] while
//! [`RecordingController::device_watch_active`]).

#[cfg(test)]
mod tests;

use std::error::Error as StdError;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_num::{clamp_f64, round_to_i64};
use vocara_model::util::id::new_id;
use vocara_services::app_settings_service::AppSettingsService;
use vocara_services::audio_edit_service::trim_wav_head_in_place;
use vocara_services::audio_file_service::{probe_wav, AudioFiles};
use vocara_services::engine::media::MAX_PLAYBACK_DURATION;
use vocara_services::graph_audio_playback_service::{GraphAudioPlaybackService, SyncError};
use vocara_services::mic_permission;
use vocara_services::mic_recording_controller::{
    DestinationError, MicEvent, MicRecorder, MicRecordingController, MicRecordingFailure,
    MicRecordingOptions, MicRecordingStart, MicRecordingStop, PendingStart, RecordingPathStrategy,
    RequestPermission, StopFinalizeGate, StopFinalizeHold, AEC_BYPASSED_NOTICE,
    INPUT_FELL_BACK_NOTICE,
};
use vocara_services::playback_engine::PlaybackEngine;
use vocara_services::recording_finalizer::{Finalizer, Landing, RecordingFinalizer};
use vocara_services::waveform_service::{self, WaveformResult, DEFAULT_TARGET_BUCKETS};

use super::playback_controller::PlaybackController;
use crate::util::busy_gate::{BusyGate, BusyOutcome, BusyRun};
use crate::util::{panic_message, ActionError};

// --- Seams --------------------------------------------------------------------

/// What the controller reads and edits on the view model that owns it
/// (Dart's constructor closures). Model and selection stay the view model's.
pub trait RecordingHost {
    /// The live practice model.
    fn practice(&self) -> Option<&SongPractice>;

    /// The selected lane, if any.
    fn selected_track_id(&self) -> Option<&str>;

    /// A lane by id, as the model holds it now.
    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>>;

    /// Replaces lane `id` with `update` of it (in the model; nothing is
    /// saved).
    fn update_track(&mut self, id: &str, update: &dyn Fn(&AudioTrack) -> AudioTrack);

    /// Puts a recording into the model as a new take at `start_ms` (take
    /// naming is the view model's: its numbering reads the active
    /// interpretation); returns the lane it landed on.
    fn append_vocal(&mut self, path: &str, analysis: &WaveformResult, start_ms: i64) -> String;

    /// Selects the lane (and, when `Some`, the clip) the recording landed on.
    /// `None` leaves the host's clip selection untouched (the new-take case).
    fn select_recorded_clip(&mut self, track_id: &str, clip_id: Option<&str>);

    /// The shared playhead ([`PlaybackController::position`]): where a take
    /// starts.
    fn position(&self) -> Duration;

    /// The screen's ONE busy gate: it guards the Stop→Record re-arm race and
    /// runs the stop's finalize.
    fn busy_gate(&mut self) -> &mut BusyGate;

    /// Whether the analysis cache's live view is on (it pulls the same input
    /// frames, so the device watch's stall check covers it).
    fn live_monitor(&self) -> bool;

    fn toast(&mut self, message: &str);

    /// The user-facing text of an error (the view model's `_friendlyError`).
    fn friendly_error(&self, error: &str) -> String;
}

/// What the controller needs of the app settings (Dart's
/// `AppSettingsService`, which implements it).
pub trait RecordingSettings: Send + Sync {
    /// See [`AppSettingsService::sync_input_channel`].
    fn sync_input_channel(&self);
    /// See [`AppSettingsService::bluetooth_in_out_conflict_now`].
    fn bluetooth_in_out_conflict_now(&self) -> bool;
    /// See [`AppSettingsService::refresh_output_devices`].
    fn refresh_output_devices(&self) -> bool;
    /// See [`AppSettingsService::reconcile_monitor_safety`].
    fn reconcile_monitor_safety(&self);
}

impl RecordingSettings for AppSettingsService {
    fn sync_input_channel(&self) {
        AppSettingsService::sync_input_channel(self);
    }

    fn bluetooth_in_out_conflict_now(&self) -> bool {
        AppSettingsService::bluetooth_in_out_conflict_now(self)
    }

    fn refresh_output_devices(&self) -> bool {
        AppSettingsService::refresh_output_devices(self)
    }

    fn reconcile_monitor_safety(&self) {
        AppSettingsService::reconcile_monitor_safety(self);
    }
}

/// A take's waveform analysis (Dart's `WaveformService.analyze`): blocking,
/// so it runs on a worker. The error is its message.
pub type TakeAnalyzer = Arc<dyn Fn(&str) -> Result<WaveformResult, String> + Send + Sync>;

/// The time source of the record clock (`Instant::now`; a test's clock).
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The controller's optional parts (Dart's optional and test-only
/// constructor parameters).
#[derive(Default)]
pub struct RecordingControllerOptions {
    /// A recorder to drive instead of the real [`MicRecordingController`]
    /// (Dart's `mic` test seam, which the view model passes through so the
    /// screen-level flows are testable too).
    pub mic: Option<Arc<dyn MicRecorder>>,
    /// The waveform analysis (default:
    /// [`waveform_service::analyze`] at its default bucket count).
    pub analyze: Option<TakeAnalyzer>,
    /// The real recorder's permission prompt (default:
    /// [`mic_permission::request`]).
    pub request_permission: Option<RequestPermission>,
    /// The record clock's time source (default: [`Instant::now`]).
    pub now: Option<Clock>,
}

// --- Effects and their answers ------------------------------------------------

/// One operation in flight: a start (its setup, arm, resync and empty-take
/// drop) or a stop (its placement, finalize, saves and resync). Tags the
/// effects it asks for and their answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RecordingOp(u64);

/// Work the view model does for the controller, in the order given.
pub enum RecordingEffect {
    /// Turn the analysis cache's live view off
    /// (`AnalysisCache::set_live_monitor(false, ..)`) and run its effects:
    /// the live view and the take can't share the engine capture.
    StopLiveView,
    /// Run the mic's start on a blocking worker ([`StartJob::run`]) and hand
    /// its outcome to [`RecordingController::start_done`].
    Start(StartJob),
    /// Create the empty take REC records into when nothing is armed (the
    /// view model's `create_empty_vocal_take`) and hand its id — `None` when
    /// its save did not land — to [`RecordingController::empty_take_created`].
    CreateEmptyTake(RecordingOp),
    /// Remove the empty take a start that never armed created for itself
    /// (the view model's `_discardEmptyTake`, which leaves a take that has
    /// clips alone), then call [`RecordingController::empty_take_discarded`].
    DiscardEmptyTake(RecordingOp, String),
    /// Resync the engine with the model (`MixerController::resync_playback`,
    /// queued at once), wait for it off the UI thread and hand its outcome to
    /// [`RecordingController::resynced`]. On a start it is what mutes the
    /// punched take; after a take that never landed it unmutes it.
    Resync(RecordingOp),
    /// The record clock's playhead write
    /// ([`PlaybackController::set_recording_position`]).
    SetRecordingPosition(Duration),
    /// Re-arm the loop wrap the take disabled
    /// ([`PlaybackController::ensure_loop_range`]).
    EnsureLoopRange,
    /// Run on a worker ([`RecordingJob::run`]) and hand its result to
    /// [`RecordingController::job_done`].
    Run(RecordingJob),
    /// The busy gate's guarded finalize: run it on a worker
    /// ([`BusyRun::into_task`]) and hand its outcome to
    /// [`RecordingController::finalize_ran`].
    Finalize(RecordingOp, BusyRun<String>),
    /// An edit holds the busy gate: call
    /// [`RecordingController::finalize_retry_due`] after `after`.
    RetryFinalize { op: RecordingOp, after: Duration },
    /// Persist the model (the view model's save, reporting whether THIS
    /// write landed) and hand the answer to [`RecordingController::saved`].
    Save(RecordingOp),
    /// The take landed on lane `0`: refresh the analysis views that show it
    /// (`AnalysisCache::on_track_audio_changed`).
    TrackAudioChanged(String),
    /// The busy gate's guarded finalize failed: surface it as the gate's
    /// `onError` does (a toast of its friendly text).
    GateError(ActionError<String>),
    /// Ask for the microphone ([`mic_permission::request`], blocking: off the
    /// UI thread) and hand the answer to
    /// [`RecordingController::monitor_permission_answered`].
    RequestMicPermission,
    /// The take in flight has ended: the Back that asked
    /// ([`RecordingController::stop_before_leaving`]) may leave now.
    Leave,
}

impl fmt::Debug for RecordingEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordingEffect::StopLiveView => f.write_str("StopLiveView"),
            RecordingEffect::Start(job) => f.debug_tuple("Start").field(&job.op).finish(),
            RecordingEffect::CreateEmptyTake(op) => {
                f.debug_tuple("CreateEmptyTake").field(op).finish()
            }
            RecordingEffect::DiscardEmptyTake(op, id) => f
                .debug_tuple("DiscardEmptyTake")
                .field(op)
                .field(id)
                .finish(),
            RecordingEffect::Resync(op) => f.debug_tuple("Resync").field(op).finish(),
            RecordingEffect::SetRecordingPosition(d) => {
                f.debug_tuple("SetRecordingPosition").field(d).finish()
            }
            RecordingEffect::EnsureLoopRange => f.write_str("EnsureLoopRange"),
            RecordingEffect::Run(job) => f.debug_tuple("Run").field(&job.op).finish(),
            RecordingEffect::Finalize(op, _) => f.debug_tuple("Finalize").field(op).finish(),
            RecordingEffect::RetryFinalize { op, after } => f
                .debug_struct("RetryFinalize")
                .field("op", op)
                .field("after", after)
                .finish(),
            RecordingEffect::Save(op) => f.debug_tuple("Save").field(op).finish(),
            RecordingEffect::TrackAudioChanged(id) => {
                f.debug_tuple("TrackAudioChanged").field(id).finish()
            }
            RecordingEffect::GateError(e) => f.debug_tuple("GateError").field(e).finish(),
            RecordingEffect::RequestMicPermission => f.write_str("RequestMicPermission"),
            RecordingEffect::Leave => f.write_str("Leave"),
        }
    }
}

/// Thrown from the take setup when the empty take REC was to record into
/// could not be saved. Aborts the start silently: the failed save has
/// already told the user.
#[derive(Debug)]
struct TakeSetupFailed;

impl fmt::Display for TakeSetupFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the take setup failed")
    }
}

impl StdError for TakeSetupFailed {}

/// Where the take records: its lane and its start (what the WAV's name
/// holds).
struct TakeSetup {
    take_id: String,
    start_ms: i64,
}

type SetupReply = Result<TakeSetup, DestinationError>;

/// A start that reached its take setup (Dart's destination `create`, after
/// the permission gate): hand it to [`RecordingController::take_setup`] on
/// the UI thread. The start waits for the answer; a request dropped
/// unanswered fails the start silently. (Clone, for a message type that
/// must be: the controller answers the copy it is given, so keep no other.)
#[derive(Clone)]
pub struct TakeSetupRequest {
    op: RecordingOp,
    reply: mpsc::Sender<SetupReply>,
}

impl fmt::Debug for TakeSetupRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TakeSetupRequest").field(&self.op).finish()
    }
}

type SetupCallback = Box<dyn FnOnce(TakeSetupRequest) + Send>;

/// Where a running start finds the screen's way to the UI thread.
type SetupSlot = Arc<Mutex<Option<SetupCallback>>>;

/// The mic's start, to run on a blocking worker.
pub struct StartJob {
    op: RecordingOp,
    pending: PendingStart,
    slot: SetupSlot,
}

impl fmt::Debug for StartJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StartJob").field(&self.op).finish()
    }
}

impl StartJob {
    pub fn op(&self) -> RecordingOp {
        self.op
    }

    /// Runs the start (it blocks on the permission prompt, the take setup,
    /// the WAV's creation and the mic's ready handshake). When it reaches its
    /// take setup, `on_setup` gets the request on this thread: hand it to the
    /// UI thread ([`RecordingController::take_setup`]).
    pub fn run(self, on_setup: impl FnOnce(TakeSetupRequest) + Send + 'static) -> StartDone {
        *lock(&self.slot) = Some(Box::new(on_setup));
        let pending = self.pending;
        let outcome = match catch_unwind(AssertUnwindSafe(|| pending.run())) {
            Ok(Ok(start)) => StartOutcome::Settled(start),
            Ok(Err(e)) => StartOutcome::Threw {
                setup_failed: e.is::<TakeSetupFailed>(),
                message: e.to_string(),
            },
            Err(panic) => StartOutcome::Threw {
                setup_failed: false,
                message: panic_message(panic.as_ref()),
            },
        };
        lock(&self.slot).take();
        StartDone {
            op: self.op,
            outcome,
        }
    }
}

/// How a start ended (Dart's `await starting`: its result, or what it
/// threw).
#[derive(Clone, Debug)]
pub enum StartOutcome {
    Settled(MicRecordingStart),
    /// The take setup or the WAV's creation failed (or the start panicked).
    /// `setup_failed`: the empty take could not be saved, which said so
    /// already.
    Threw {
        setup_failed: bool,
        message: String,
    },
}

/// A [`StartJob`]'s outcome.
#[derive(Clone, Debug)]
pub struct StartDone {
    pub op: RecordingOp,
    pub outcome: StartOutcome,
}

/// File work for a worker.
pub struct RecordingJob {
    op: Option<RecordingOp>,
    work: Box<dyn FnOnce() + Send>,
}

impl RecordingJob {
    /// Runs the work (it absorbs its own failures, as Dart's best-effort
    /// file work did).
    pub fn run(self) -> JobDone {
        let _ = catch_unwind(AssertUnwindSafe(self.work));
        JobDone { op: self.op }
    }
}

/// A [`RecordingJob`] ran.
#[derive(Clone, Debug)]
pub struct JobDone {
    op: Option<RecordingOp>,
}

// --- Quit ---------------------------------------------------------------------

/// A one-shot signal: [`Latch::wait`] returns once [`Latch::open`] ran.
#[derive(Default)]
struct Latch {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Latch {
    fn open(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        while !*open {
            open = self
                .changed
                .wait(open)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// What a quit's finalizer asks of the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuitKind {
    /// Dart's `cancelForQuit`: stop (or cancel) the take, then wait for the
    /// start in flight if it reached its take setup.
    CancelStart,
    /// The mic's `onQuitFinalize`: `if (isRecording) await stopRecording()`.
    StopTake,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct QuitQueue {
    closed: bool,
    asked: Vec<(QuitKind, Arc<Latch>)>,
    waker: Option<Waker>,
}

/// Where the quit's threads leave their requests for the UI thread.
#[derive(Default)]
struct QuitBridge {
    queue: Mutex<QuitQueue>,
}

impl QuitBridge {
    fn queue(&self) -> MutexGuard<'_, QuitQueue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Called on a quit's thread: asks, then waits until it is served.
    fn ask(&self, kind: QuitKind) {
        let latch = Arc::new(Latch::default());
        let waker = {
            let mut q = self.queue();
            if q.closed {
                return;
            }
            q.asked.push((kind, Arc::clone(&latch)));
            q.waker.clone()
        };
        if let Some(wake) = waker {
            wake();
        }
        latch.wait();
    }

    /// The controller is gone: nothing will serve a request again.
    fn close(&self) {
        let asked = {
            let mut q = self.queue();
            q.closed = true;
            q.waker = None;
            std::mem::take(&mut q.asked)
        };
        for (_, latch) in asked {
            latch.open();
        }
    }
}

/// Someone waiting for the take in flight to end.
enum Waiter {
    /// A quit's [`QuitKind::CancelStart`]: the stop it made, then (decided
    /// when that settles) the start it captured.
    QuitCancel {
        latch: Arc<Latch>,
        stop: Option<RecordingOp>,
        start: Option<RecordingOp>,
        /// `Some(start)` once the stop settled and the start must be waited
        /// for; `None` before.
        then_start: Option<Option<RecordingOp>>,
    },
    /// A quit's [`QuitKind::StopTake`].
    QuitStop {
        latch: Arc<Latch>,
        stop: RecordingOp,
    },
    /// [`RecordingController::stop_before_leaving`].
    Leave {
        stop: Option<RecordingOp>,
        start: Option<RecordingOp>,
    },
}

// --- In-flight state ----------------------------------------------------------

/// Where a start (Dart's `_armTake`) waits.
enum StartStage {
    /// The mic's start runs on a worker.
    Arming,
    /// Armed: waiting for the resync that mutes the punched take.
    Resyncing,
    /// Never armed: dropping the empty take it created.
    Discarding,
}

/// The latest start past its guard, until it armed, failed or unwound (its
/// empty take dropped) — Dart's `_startInFlight`.
struct StartFlight {
    op: RecordingOp,
    /// Registered with the finalizer for this start (Dart's
    /// `cancelForQuit`).
    cancel_for_quit: Finalizer,
    stage: StartStage,
    /// Whether the mic's start has not come back yet (see
    /// [`RecordingController::is_starting_take`]).
    awaiting_mic: bool,
    /// A take setup parked on the empty take's creation: its answer.
    setup: Option<mpsc::Sender<SetupReply>>,
    /// The take generation the armed take captured; -1 = never armed.
    take_gen: i64,
    /// The armed take's WAV.
    path: Option<String>,
}

/// A save attempt of the finalize (see [`RecordingController::saved`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveAttempt {
    First,
    Retry,
    Last,
}

/// What the finalize read of the take (on the gate's worker).
enum TakeRead {
    /// The writer finalized cleanly.
    Clean(WaveformResult),
    /// The writer hit an IO error; the partial WAV is readable.
    Partial(WaveformResult),
    /// The writer hit an IO error; the partial WAV was gone or not a
    /// recording, and is deleted.
    Unreadable,
    /// The writer hit an IO error; the partial WAV is the recorder's shape
    /// but could not be read now. It is left where it is, for boot recovery
    /// to try again.
    Held,
}

type ReadSlot = Arc<Mutex<Option<TakeRead>>>;

/// Where a stop (Dart's `_finishStop`) waits.
enum StopStage {
    /// The aligned start is being noted and the head cut.
    Placing,
    /// An edit holds the busy gate: retrying in 50 ms.
    AwaitingGate,
    /// The gate's action reads the take.
    Reading(ReadSlot),
    /// A save is in flight.
    Saving(SaveAttempt),
    /// The finalize's `finally` resyncs the engine.
    Resyncing,
}

/// The lane (and, dropped onto an armed take, the clip) a take landed on.
#[derive(Clone)]
struct Staged {
    lane: String,
    clip: Option<String>,
}

/// A stopped take landing.
struct StopFlight {
    op: RecordingOp,
    stopped: MicRecordingStop,
    stage: StopStage,
    target_id: Option<String>,
    start_ms: i64,
    analysis: Option<WaveformResult>,
    staged: Option<Staged>,
    /// Whether the finalize reached the host's save, whose resync is what
    /// makes the punched take audible again.
    save_ran: bool,
    /// Listed as landing on the practice, then the quit held: released in
    /// that order once the tail settles.
    landing: Option<Landing>,
    hold: Option<StopFinalizeHold>,
}

/// Locks a mutex whose holder cannot leave it inconsistent.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A whole-millisecond `Duration` (`Duration(milliseconds: ms)`); the
/// positions it is made from are never negative.
fn millis(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

/// The inert recorder of a screen without the native engine: nothing records
/// there (the start returns before it reaches the mic).
struct NoMic {
    gate: StopFinalizeGate,
}

impl MicRecorder for NoMic {
    fn level(&self) -> f64 {
        0.0
    }

    fn revision(&self) -> u64 {
        0
    }

    fn is_recording(&self) -> bool {
        false
    }

    fn is_starting(&self) -> bool {
        false
    }

    fn feeding_monitor(&self) -> bool {
        false
    }

    fn invalidate_pending_start(&self) {}

    fn begin_start(&self, _destination: RecordingPathStrategy) -> PendingStart {
        PendingStart::from_fn(|| Ok(MicRecordingStart::Invalidated))
    }

    fn tick(&self, _on_event: &mut dyn FnMut(MicEvent)) {}

    fn next_tick_due(&self) -> Option<Instant> {
        None
    }

    fn stop(&self) -> Option<MicRecordingStop> {
        None
    }

    fn abort(&self) -> Result<(), DestinationError> {
        Ok(())
    }

    fn begin_stop_finalize(&self) -> StopFinalizeHold {
        self.gate.begin()
    }

    fn dispose(&self) {}
}

// --- The controller -----------------------------------------------------------

/// The song screen's recording, monitoring and device watch (see the module
/// docs).
pub struct RecordingController {
    /// The native engine, when it's the active backend (only it supports
    /// device I/O + monitoring). Borrowed — the view model owns its
    /// lifecycle.
    graph_engine: Option<Arc<GraphAudioPlaybackService>>,

    /// The transport (the playback controller's engine): recording seeks,
    /// plays, pauses and disables the loop wrap through the SAME engine.
    transport: Arc<dyn PlaybackEngine>,

    audio_files: Arc<dyn AudioFiles>,
    settings: Arc<dyn RecordingSettings>,
    practice_id: String,
    finalizer: Arc<RecordingFinalizer>,
    analyze: TakeAnalyzer,
    now: Clock,

    /// The shared guarded start → pump → stop lifecycle. Its monitor stays
    /// `None`: the song screen never feeds the monitor while recording (the
    /// live view is forced off before a take). It does not watch the input
    /// device: the device poll's input bits stay this controller's own 1 s
    /// device watch's.
    mic: Arc<dyn MicRecorder>,

    /// Where the quit's finalizers leave their requests.
    quit: Arc<QuitBridge>,

    /// Set in [`RecordingController::dispose`]. Guards timer ticks and
    /// continuations racing the tear-down (the route can pop mid-start).
    disposed: bool,

    revision: u64,
    meter_revision: u64,
    peaks_revision: u64,
    next_op: u64,

    /// Every start past its guard that has not yet armed, failed or unwound.
    /// Several can be in flight: take A's continuation can still be parked
    /// on its resync after A stopped and B started.
    starts: Vec<StartFlight>,
    /// The latest start past its guard (Dart's `_startInFlight`); in flight
    /// while it is in `starts`.
    latest_start: Option<RecordingOp>,
    stop: Option<StopFlight>,
    waiters: Vec<Waiter>,

    /// The empty take the start in flight created to record into, until the
    /// take arms. A start that never arms (cancelled by Stop, denied the mic,
    /// failed to open it or its file) left that empty "Take N" lane behind.
    created_empty_take: Option<String>,

    /// Whether a start has taken its take's start position
    /// (`recording_start_ms`) and is still arming: the mic, once ready, seeks
    /// the backing back to that position, so a transport move now would be
    /// undone (see [`PlaybackController`]). Not before: at the permission
    /// prompt or the empty take's save the position is still the playhead's
    /// to set.
    start_position_held: bool,

    /// Set once a start reaches its take setup (which may save an empty
    /// take), until that start returns: before it, a cancelled start sets up
    /// nothing (the mic bails at the permission prompt), so a quit has
    /// nothing to wait for.
    take_setup_started: bool,

    /// Set when Stop cancels a start still arming, until that start returns:
    /// its take setup (the empty take's save) may still be finishing, and a
    /// fast re-arm's setup interleaving with it would bump the take
    /// generation under the new take and move its start position.
    cancelling_start: bool,

    /// Set while a start that never armed drops the empty take it created:
    /// the drop is a structural edit holding the busy gate, so a REC in that
    /// moment would be refused without a sign of why.
    discarding_take: bool,

    /// Synchronous Stop latch: set at [`RecordingController::stop_recording`]'s
    /// entry and cleared when the finalize completes. Meanwhile
    /// `is_recording` is already false and the busy gate not yet held, so
    /// without this a fast Stop→REC would arm take #2 interleaved with take
    /// #1's finalize. Checked by the start's guard.
    stopping: bool,

    /// A [`RecordingController::stop_before_leaving`] in progress.
    leaving: bool,

    /// Live input level (0..1) while recording. Drives the meter only.
    record_level: f64,

    /// Live input peak (0..1) while input monitoring is on (or recording).
    /// Drives the monitor meter.
    input_level: f64,

    recording_start_ms: i64,

    /// Monotonic per-take token, bumped when a take's destination is created.
    /// The start's continuation captures it and re-checks after every wait: a
    /// plain `is_recording` bool is ABA-vulnerable — take A's continuation
    /// can park across A's stop AND take B's arm, resume, see `is_recording
    /// == true` (B's!), and drive B's transport with A's stale
    /// seek/play/pre-roll.
    take_generation: i64,

    /// Milliseconds of audio recorded BEFORE the backing playback actually
    /// started (mic arm → play), measured from the engine's input-frame
    /// counter. The WAV's content sung along to the backing begins this far
    /// into the file, so take placement subtracts it — without this,
    /// everything between arm and play (graph resync, decode, seek) lands as
    /// unbounded skew.
    recording_pre_roll_ms: f64,

    /// Whether this take's backing has started (its pre-roll is known): the
    /// playhead waits at REC until then.
    backing_rolling: bool,

    /// [`RecordingController::taken_ms`] as the backing started: the
    /// playhead runs from it.
    head_zero_ms: f64,

    /// The vocal take the in-flight recording is punching into (frozen at
    /// start).
    recording_target_id: Option<String>,

    /// Live, growing waveform peaks of the in-progress recording (one per
    /// level interval).
    rec_peaks: Vec<f64>,
    /// Whether the peaks are Dart's `const []` (setting it again does not
    /// notify).
    peaks_const_empty: bool,

    /// When the armed take started rolling (Dart's `_takeClock`, reset and
    /// started at the arm): real time, which the peaks (and so the record
    /// clock) keep up with. The clock used to count level ticks, and a
    /// periodic timer skips the ticks a stalled isolate overran (a long
    /// frame, a GC pause) instead of delivering them late: the playhead, the
    /// clock and the forming waveform fell behind the backing for the rest of
    /// the take, and a lyric stamped or a note added at the playhead mid-take
    /// saved that late time.
    take_clock: Option<Instant>,

    // --- Input monitoring ---
    input_monitor_on: bool,
    /// Synchronous re-entrancy guard for the monitor toggle (its ON branch
    /// waits for a permission round-trip, during which a second tap must not
    /// arm monitoring twice).
    toggling_monitor: bool,
    /// The 50 ms monitor-meter timer runs.
    input_level_timer: bool,
    /// Peak-hold state for the monitor meter so brief/low vocal peaks stay
    /// visible (decays smoothly).
    held_input_level: f64,

    // --- Device watch ---
    // Input-stall watchdog: macOS VPIO raises no device-loss event, so the
    // device watch also treats a non-advancing input frame counter (while
    // capturing) as a lost input.
    last_input_frames: u64,
    last_stream_xruns: u64,
    input_stall_ticks: u32,
    /// Whether the input has delivered since capture last went live. Until
    /// it has, a silent mic is still warming up — a Bluetooth headset
    /// switching to its headset profile delivers nothing for a second or two
    /// — and the engine's own warm-up allowance reports one that never
    /// starts (input lost). Counted from the first silent tick, the watchdog
    /// stopped monitoring or the live view as "disconnected" one to two
    /// seconds after it was turned on, before that allowance ran out.
    input_delivered: bool,

    effects: Vec<RecordingEffect>,
}

impl fmt::Debug for RecordingController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordingController")
            .field("is_recording", &self.is_recording())
            .field("is_starting_take", &self.is_starting_take())
            .field("stopping", &self.stopping)
            .field("input_monitor_on", &self.input_monitor_on)
            .finish_non_exhaustive()
    }
}

impl RecordingController {
    /// One level interval: the live waveform resolution and the recording
    /// clock derive from it, so it must equal the mic controller's cadence
    /// (aliased, so it can't drift).
    const RECORD_INTERVAL_MS: i64 = MicRecordingController::LEVEL_INTERVAL_MS as i64;

    /// The device watch's period.
    pub const DEVICE_WATCH_PERIOD: Duration = Duration::from_secs(1);

    /// How long a take may run before the device watch stops it: a few
    /// seconds short of [`MAX_PLAYBACK_DURATION`], the longest audio the
    /// take's analysis (and every later decode of it) accepts. Nothing else
    /// stopped a take short of the engine's six-hour WAV limit, so a take past
    /// an hour was recorded, then failed its analysis at Stop: nothing was
    /// staged, and boot recovery, which analyzes the same way, failed it on
    /// every launch. The margin covers a watch tick (one a second) and the
    /// stop's tail.
    const TAKE_LENGTH_LIMIT: Duration =
        MAX_PLAYBACK_DURATION.saturating_sub(Duration::from_secs(3));

    /// The monitor meter's period.
    pub const MONITOR_METER_PERIOD: Duration = Duration::from_millis(50);

    /// How often the screen should call [`RecordingController::mic_tick`]
    /// while [`RecordingController::next_mic_tick_due`] is `Some`.
    pub const MIC_TICK_PERIOD: Duration = MicRecordingController::TICK_PERIOD;

    /// How long the finalize waits before trying the busy gate again.
    const GATE_RETRY: Duration = Duration::from_millis(50);

    /// A controller over the view model's engines, files, settings and
    /// finalizer.
    pub fn new(
        transport: Arc<dyn PlaybackEngine>,
        graph_engine: Option<Arc<GraphAudioPlaybackService>>,
        audio_files: Arc<dyn AudioFiles>,
        settings: Arc<dyn RecordingSettings>,
        practice_id: impl Into<String>,
        finalizer: Arc<RecordingFinalizer>,
    ) -> Self {
        Self::with_options(
            transport,
            graph_engine,
            audio_files,
            settings,
            practice_id,
            finalizer,
            RecordingControllerOptions::default(),
        )
    }

    /// [`RecordingController::new`] with its optional parts set.
    pub fn with_options(
        transport: Arc<dyn PlaybackEngine>,
        graph_engine: Option<Arc<GraphAudioPlaybackService>>,
        audio_files: Arc<dyn AudioFiles>,
        settings: Arc<dyn RecordingSettings>,
        practice_id: impl Into<String>,
        finalizer: Arc<RecordingFinalizer>,
        options: RecordingControllerOptions,
    ) -> Self {
        let quit = Arc::new(QuitBridge::default());
        let mic: Arc<dyn MicRecorder> = match (options.mic, &graph_engine) {
            (Some(mic), _) => mic,
            (None, Some(engine)) => {
                let engine = Arc::clone(engine);
                let on_quit = Arc::clone(&quit);
                let mut mic_options = MicRecordingOptions {
                    monitor: None,
                    // Registered for exactly the armed window: finalize the
                    // in-flight take if the app quits mid-record.
                    on_quit_finalize: Some(Arc::new(move || on_quit.ask(QuitKind::StopTake))),
                    // The device poll's input bits stay the 1 s device
                    // watch's (see the field's doc).
                    watch_input_device: false,
                    level_ticks: true,
                    finalizer: Some(Arc::clone(&finalizer)),
                    ..MicRecordingOptions::default()
                };
                if let Some(request) = options.request_permission {
                    mic_options.request_permission = request;
                }
                Arc::new(MicRecordingController::new(
                    move || Arc::clone(&engine),
                    mic_options,
                ))
            }
            (None, None) => Arc::new(NoMic {
                gate: StopFinalizeGate::new(Arc::clone(&finalizer)),
            }),
        };
        RecordingController {
            graph_engine,
            transport,
            audio_files,
            settings,
            practice_id: practice_id.into(),
            finalizer,
            analyze: options.analyze.unwrap_or_else(|| {
                Arc::new(|path: &str| {
                    waveform_service::analyze(path, DEFAULT_TARGET_BUCKETS).map_err(|e| e.0)
                })
            }),
            now: options.now.unwrap_or_else(|| Arc::new(Instant::now)),
            mic,
            quit,
            disposed: false,
            revision: 0,
            meter_revision: 0,
            peaks_revision: 0,
            next_op: 0,
            starts: Vec::new(),
            latest_start: None,
            stop: None,
            waiters: Vec::new(),
            created_empty_take: None,
            start_position_held: false,
            take_setup_started: false,
            cancelling_start: false,
            discarding_take: false,
            stopping: false,
            leaving: false,
            record_level: 0.0,
            input_level: 0.0,
            recording_start_ms: 0,
            take_generation: 0,
            recording_pre_roll_ms: 0.0,
            backing_rolling: false,
            head_zero_ms: 0.0,
            recording_target_id: None,
            rec_peaks: Vec::new(),
            peaks_const_empty: true,
            take_clock: None,
            input_monitor_on: false,
            toggling_monitor: false,
            input_level_timer: false,
            held_input_level: 0.0,
            last_input_frames: 0,
            last_stream_xruns: 0,
            input_stall_ticks: 0,
            input_delivered: false,
            effects: Vec::new(),
        }
    }

    // --- Notifications ----------------------------------------------------------

    /// Bumped on every notification of this controller (Dart's
    /// `notifyListeners`); never after [`RecordingController::dispose`].
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn notify(&mut self) {
        if self.disposed {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Bumped whenever [`RecordingController::record_level`] or
    /// [`RecordingController::input_level`] changes (Dart's two
    /// `ValueNotifier`s).
    pub fn meter_revision(&self) -> u64 {
        self.meter_revision
    }

    /// Bumped whenever the live waveform is set (Dart's `recordingPeaks`
    /// notifier).
    pub fn peaks_revision(&self) -> u64 {
        self.peaks_revision
    }

    fn set_record_level(&mut self, v: f64) {
        if self.record_level != v {
            self.record_level = v;
            self.meter_changed();
        }
    }

    fn set_input_level(&mut self, v: f64) {
        if self.input_level != v {
            self.input_level = v;
            self.meter_changed();
        }
    }

    fn meter_changed(&mut self) {
        if !self.disposed {
            self.meter_revision = self.meter_revision.wrapping_add(1);
        }
    }

    /// `recordingPeaks.value = const []`.
    fn clear_peaks_view(&mut self) {
        if !self.peaks_const_empty {
            self.peaks_const_empty = true;
            self.peaks_changed();
        }
    }

    fn peaks_changed(&mut self) {
        if !self.disposed {
            self.peaks_revision = self.peaks_revision.wrapping_add(1);
        }
    }

    fn op(&mut self) -> RecordingOp {
        self.next_op += 1;
        RecordingOp(self.next_op)
    }

    fn finish(&mut self) -> Vec<RecordingEffect> {
        std::mem::take(&mut self.effects)
    }

    // --- State --------------------------------------------------------------------

    /// True while a take is armed (WAV being written, pumps running).
    pub fn is_recording(&self) -> bool {
        self.mic.is_recording()
    }

    /// True during REC's arm window — after the user tapped Record but
    /// before the permission/file waits resolve and
    /// [`RecordingController::is_recording`] flips true. The live view
    /// treats it like recording: the capture already belongs to the incoming
    /// take. So does the transport: a Stop here cancels the take (see
    /// [`RecordingController::stop_recording`]), and the window lasts until a
    /// cancelled start unwound. It also covers the moment between the mic's
    /// start settling on its worker and its outcome reaching
    /// [`RecordingController::start_done`], which Dart's continuation ran in
    /// without a gap.
    pub fn is_starting_take(&self) -> bool {
        self.mic.is_starting()
            || self.starts.iter().any(|s| s.awaiting_mic)
            || self.cancelling_start
            || self.discarding_take
    }

    /// Within [`RecordingController::is_starting_take`], the part with
    /// nothing left to stop: a start Stop cancelled, or one that failed,
    /// unwinding (its empty take dropped).
    pub fn is_cancelling_start(&self) -> bool {
        self.cancelling_start || self.discarding_take
    }

    /// True from Stop until the stopped take's finalize has committed — the
    /// window where [`RecordingController::is_recording`] already reads
    /// false but the take is not in the model yet. The host's structural
    /// edits refuse it like a take in flight: one holding the busy gate here
    /// makes the finalize wait.
    pub fn is_stopping(&self) -> bool {
        self.stopping
    }

    /// Whether a Back is already ending the take on the way out. The take's
    /// finalize (analysis and save) can take seconds after
    /// [`RecordingController::is_recording`] reads false, and a second Back
    /// in that time must not leave on its own.
    pub fn is_leaving(&self) -> bool {
        self.leaving
    }

    /// Whether a start is past its guard and has not yet armed, failed or
    /// unwound (Dart's `_armTake` futures, still pending).
    pub fn is_start_in_flight(&self) -> bool {
        !self.starts.is_empty()
    }

    fn start_flight(&self, op: RecordingOp) -> Option<&StartFlight> {
        self.starts.iter().find(|s| s.op == op)
    }

    fn start_flight_mut(&mut self, op: RecordingOp) -> Option<&mut StartFlight> {
        self.starts.iter_mut().find(|s| s.op == op)
    }

    /// Whether a start has taken its take's start position and is still
    /// arming (see the field's doc): the transport moves are refused then.
    pub fn holds_start_position(&self) -> bool {
        self.start_position_held
    }

    /// Live input level (0..1) while recording. Drives the meter only.
    /// NOTE: no widget on this screen reads it (kept for parity, as in
    /// Dart).
    pub fn record_level(&self) -> f64 {
        self.record_level
    }

    /// Live input peak (0..1) while input monitoring is on. Drives the
    /// monitor meter.
    pub fn input_level(&self) -> f64 {
        self.input_level
    }

    pub fn recording_start_ms(&self) -> i64 {
        self.recording_start_ms
    }

    /// Where the forming take is drawn from: the timeline point its first
    /// frame plays against, a pre-roll before REC once the backing has
    /// started. Drawn from REC, it ran that far ahead of the playhead.
    pub fn live_take_start_ms(&self) -> i64 {
        self.recording_start_ms
            - if self.backing_rolling {
                round_to_i64(self.recording_pre_roll_ms)
            } else {
                0
            }
    }

    /// Live, growing waveform peaks of the in-progress recording (one per
    /// level interval).
    pub fn recording_peaks(&self) -> &[f64] {
        &self.rec_peaks
    }

    /// The record clock.
    pub fn recording_duration_ms(&self) -> i64 {
        self.rec_peaks.len() as i64 * Self::RECORD_INTERVAL_MS
    }

    /// Whether the singer hears themselves through the engine (live input
    /// monitoring).
    pub fn input_monitor_enabled(&self) -> bool {
        self.input_monitor_on
    }

    /// The take REC will record into: the selected lane if it's a recordable
    /// vocal take in the active interpretation, else `None` (→ REC creates a
    /// new take). Drives the "armed" record-dot; frozen while a recording is
    /// in flight.
    pub fn record_target_id(&self, host: &dyn RecordingHost) -> Option<String> {
        if self.mic.is_recording() {
            return self.recording_target_id.clone();
        }
        let id = host.selected_track_id()?;
        let t = host.track_by_id(id)?;
        if t.kind != AudioTrackKind::Vocal {
            return None;
        }
        // The rule `SongPractice::active_vocal_tracks` lists lanes by: a take
        // on screen is armable. Comparing the raw ids left a visible take
        // un-armable whenever the active id, or the take's own, did not
        // resolve.
        if let Some(practice) = host.practice() {
            if !practice.lists_take(&t) {
                return None;
            }
        }
        Some(id.to_string())
    }

    /// Map a linear input peak (0..1) to a perceptual meter fill (0..1) on a
    /// dB scale (−50 dBFS → empty, 0 dBFS → full), so normal vocal levels
    /// read clearly instead of as a tiny linear sliver.
    fn meter_from_peak(peak: f64) -> f64 {
        if peak <= 1e-6 {
            return 0.0;
        }
        let db = 20.0 * (peak.ln() / std::f64::consts::LN_10);
        clamp_f64((db + 50.0) / 50.0, 0.0, 1.0)
    }

    /// The record clock's elapsed whole milliseconds (Dart's
    /// `Stopwatch.elapsedMilliseconds`).
    fn take_clock_ms(&self) -> i64 {
        self.take_clock.map_or(0, |started| {
            i64::try_from((self.now)().saturating_duration_since(started).as_millis())
                .unwrap_or(i64::MAX)
        })
    }

    /// How long this take has been taking input, as the playhead measures
    /// it: the frames it has taken (kept or dropped from a full ring), or the
    /// clock while the input stalls (none arriving) and the backing goes on.
    /// The clock alone trailed: it starts only once the mic reports ready
    /// and steps a tick at a time.
    fn taken_ms(&self) -> f64 {
        let clock_ms = self.take_clock_ms() as f64;
        let Some(engine) = &self.graph_engine else {
            return clock_ms;
        };
        let frames = engine
            .record_written_frames()
            .saturating_add(engine.record_dropped_frames());
        clock_ms.max(frames as f64 * 1000.0 / 48000.0)
    }

    // --- Timers ---------------------------------------------------------------------

    /// When the mic's next level tick is due, while a take is armed.
    pub fn next_mic_tick_due(&self) -> Option<Instant> {
        self.mic.next_tick_due()
    }

    /// Runs the mic's timers that are due: each level tick drives the meters,
    /// the live waveform and the recording clock.
    #[must_use]
    pub fn mic_tick(&mut self) -> Vec<RecordingEffect> {
        let mut peaks = Vec::new();
        self.mic.tick(&mut |event| {
            if let MicEvent::LevelTick(peak) = event {
                peaks.push(peak);
            }
        });
        for peak in peaks {
            self.on_level_tick(peak);
        }
        self.finish()
    }

    /// Drives the meters + live waveform + recording clock off the engine's
    /// raw input peak, once per level tick while a take is armed. Ignores the
    /// mic's own (linear-clamped) level — these meters are dB-mapped.
    fn on_level_tick(&mut self, peak: f64) {
        // Meter: dB-perceptual (reads clearly). Live waveform: LINEAR, so it
        // matches the final take waveform (rendered from the WAV in linear
        // amplitude) instead of looking fuller than it.
        self.set_record_level(Self::meter_from_peak(peak));
        // Also drive the mixer's MIC meter while recording (it shares this
        // level with monitoring).
        self.set_input_level(Self::meter_from_peak(peak));
        // One peak per level interval of real time: the intervals a skipped
        // tick missed take this tick's peak.
        let due = self.take_clock_ms() / Self::RECORD_INTERVAL_MS;
        loop {
            self.rec_peaks.push(peak);
            if self.rec_peaks.len() as i64 >= due {
                break;
            }
        }
        self.peaks_const_empty = false;
        self.peaks_changed();
        // The head runs with the backing, which starts a pre-roll after the
        // take does: run from the arm, it led what was heard by that much,
        // and a lyric stamped or a note added at it mid-take saved that late
        // a time.
        let heard = if self.backing_rolling {
            self.taken_ms() - self.head_zero_ms
        } else {
            0.0
        };
        self.effects
            .push(RecordingEffect::SetRecordingPosition(millis(
                self.recording_start_ms + round_to_i64(heard).max(0),
            )));
    }

    /// Whether the 50 ms monitor meter runs.
    pub fn monitor_meter_active(&self) -> bool {
        self.input_level_timer && !self.disposed
    }

    /// One tick of the monitor meter.
    pub fn monitor_meter_tick(&mut self) {
        if !self.monitor_meter_active() {
            return;
        }
        let Some(engine) = self.graph_engine.clone() else {
            return;
        };
        // While a take is recording, the record-level tick owns the shared
        // input meter (it writes the instantaneous peak at the same 50 ms
        // cadence). Yield so the two timers don't fight and flicker the
        // mixer MIC meter; this timer keeps running and reclaims the meter
        // the moment recording stops (the stop only clears it when
        // monitoring is off).
        if self.is_recording() {
            return;
        }
        let meter = Self::meter_from_peak(engine.input_level());
        // Peak-hold with decay: hold the recent peak so brief vocal hits stay
        // visible, then ease back instead of snapping to black between
        // syllables.
        self.held_input_level = meter.max(self.held_input_level - 0.05);
        self.set_input_level(self.held_input_level);
    }

    /// Whether the 1 s device watch runs: from construction, with the native
    /// engine, until dispose.
    pub fn device_watch_active(&self) -> bool {
        self.graph_engine.is_some() && !self.disposed
    }

    // --- Recording --------------------------------------------------------------------

    /// REC.
    #[must_use]
    pub fn start_recording(&mut self, host: &mut dyn RecordingHost) -> Vec<RecordingEffect> {
        // `stopping || busy` closes the re-arm race: the stop clears
        // is_recording FIRST, then runs its finalize inside the busy gate —
        // `stopping` covers the whole window (it is set synchronously at the
        // stop's entry), `busy` the gated tail. Without them a fast
        // Stop→Record would start a second take whose empty take and resync
        // interleave with the still-running finalize's save (lost update +
        // graph thrash). is_starting_take is the synchronous re-entrancy
        // guard for a fast double-tap (and keeps a cancelled start's setup
        // from interleaving).
        if self.mic.is_recording() || self.is_starting_take() || self.stopping {
            return self.finish();
        }
        if host.busy_gate().busy() {
            // An import, split, merge or a reopened screen's catch-up is
            // running. Said, as every structural edit refused in that window
            // says it: a REC press that did nothing at all read as a broken
            // button.
            host.toast("Wait for the current edit to finish.");
            return self.finish();
        }
        // Held from here until the start settles. The mic lists the take's
        // own quit work only once it arms, so a quit in the arm window (the
        // mic's open can take up to its ready timeout) exited at once, and the
        // empty "Take N" REC had saved to record into stayed in the practice
        // for good. The quit cancels the start, as Back does, and waits for
        // it to unwind (which drops that take); a take that armed meanwhile
        // stops and saves. A start still at the permission prompt set nothing
        // up, so the quit leaves at once rather than wait out the prompt for
        // its whole budget. It begins work no running quit has budgeted (REC
        // pressed during a quit that had spent its own), so it gets a budget
        // of its own.
        let cancel_for_quit = {
            let quit = Arc::clone(&self.quit);
            Finalizer::new(move || quit.ask(QuitKind::CancelStart))
        };
        self.finalizer.register(&cancel_for_quit, true);
        let op = self.op();
        self.starts.push(StartFlight {
            op,
            cancel_for_quit,
            stage: StartStage::Arming,
            awaiting_mic: false,
            setup: None,
            take_gen: -1,
            path: None,
        });
        self.latest_start = Some(op);
        self.arm_take(op);
        self.finish()
    }

    /// The body of [`RecordingController::start_recording`] past its re-arm
    /// guard, up to the mic's start.
    fn arm_take(&mut self, op: RecordingOp) {
        // The live view and the take can't share the engine capture.
        self.effects.push(RecordingEffect::StopLiveView);
        if self.graph_engine.is_none() {
            self.finally_arm(op);
            return;
        }
        // The devices are polled only while Settings is open: the take
        // records on the input-channel choice of the device capturing now (an
        // interface plugged in, or a default that moved, since the screen
        // opened).
        self.settings.sync_input_channel();
        let slot: SetupSlot = Arc::new(Mutex::new(None));
        let pending = self.mic.begin_start(self.take_destination(op, &slot));
        if let Some(start) = self.start_flight_mut(op) {
            start.awaiting_mic = true;
        }
        // The arm window is open: show it, so Stop reaches stop_recording.
        self.notify();
        self.effects
            .push(RecordingEffect::Start(StartJob { op, pending, slot }));
    }

    /// Where a take records to: its WAV, created (on the start's worker)
    /// once the take setup ran on the UI thread — after the permission gate,
    /// so a permission denial never creates an empty take.
    fn take_destination(&self, op: RecordingOp, slot: &SetupSlot) -> RecordingPathStrategy {
        let slot = Arc::clone(slot);
        let files = Arc::clone(&self.audio_files);
        let practice_id = self.practice_id.clone();
        let discard_files = Arc::clone(&self.audio_files);
        RecordingPathStrategy::new(
            move || {
                let Some(ask) = lock(&slot).take() else {
                    return Err(Box::new(TakeSetupFailed) as DestinationError);
                };
                let (reply, answer) = mpsc::channel();
                ask(TakeSetupRequest { op, reply });
                // Unanswered (the screen is gone): nothing to record into.
                let setup = answer
                    .recv()
                    .map_err(|_| Box::new(TakeSetupFailed) as DestinationError)??;
                files
                    .new_recording_path(
                        &practice_id,
                        "wav",
                        Some(&setup.take_id),
                        Some(setup.start_ms),
                    )
                    .map_err(DestinationError::from)
            },
            // Failed/invalidated starts drop the created WAV; an armed take's
            // file is never discarded here (it is kept for the boot sweep on
            // hard kills).
            move |path| {
                discard_files
                    .delete_audio(path)
                    .map_err(DestinationError::from)
            },
        )
    }

    /// A start reached its take setup: resolve the armed target (creating an
    /// empty take when nothing is armed), freeze it, stamp the start
    /// position and clear the live peaks; the start then creates the WAV.
    #[must_use]
    pub fn take_setup(
        &mut self,
        request: TakeSetupRequest,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        if self.start_flight(request.op).is_none() {
            // Not a start in flight (it cannot be: the start waits for this
            // answer).
            let _ = request.reply.send(Err(Box::new(TakeSetupFailed)));
            return self.finish();
        }
        if self.settings.bluetooth_in_out_conflict_now() {
            host.toast(
                "Recording from the Bluetooth headset lowers playback quality. \
                 Pick a different mic in Settings.",
            );
        }
        // Record onto the armed take (the selected vocal lane) at the
        // playhead, so the live waveform draws on that lane. If nothing
        // recordable is armed, create an empty take to record into — there's
        // no separate REC lane.
        self.take_setup_started = true;
        match self.record_target_id(host) {
            Some(target) => self.finish_setup(target, request.reply, host),
            None => {
                if let Some(start) = self.start_flight_mut(request.op) {
                    start.setup = Some(request.reply);
                }
                self.effects
                    .push(RecordingEffect::CreateEmptyTake(request.op));
            }
        }
        self.finish()
    }

    /// The empty take a take setup asked for: its id, or `None` when its
    /// save did not land.
    #[must_use]
    pub fn empty_take_created(
        &mut self,
        op: RecordingOp,
        created: Option<String>,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        let Some(reply) = self.start_flight_mut(op).and_then(|s| s.setup.take()) else {
            return self.finish();
        };
        // The id the take was created with, never a re-read of the
        // selection: a failed save reverts the model and moves the selection
        // to the first lane — usually the backing, which the take would then
        // mute as its punch-in target for the whole recording. Nothing to
        // record into: abort (the save already said so).
        match created {
            None => {
                let _ = reply.send(Err(Box::new(TakeSetupFailed)));
            }
            Some(id) => {
                self.created_empty_take = Some(id.clone());
                self.finish_setup(id, reply, host);
            }
        }
        self.finish()
    }

    fn finish_setup(
        &mut self,
        target: String,
        reply: mpsc::Sender<SetupReply>,
        host: &dyn RecordingHost,
    ) {
        self.take_generation += 1;
        self.recording_target_id = Some(target.clone());
        self.recording_start_ms = i64::try_from(host.position().as_millis()).unwrap_or(i64::MAX);
        self.start_position_held = true;
        self.recording_pre_roll_ms = 0.0;
        self.backing_rolling = false;
        self.head_zero_ms = 0.0;
        self.rec_peaks.clear();
        self.clear_peaks_view();
        let _ = reply.send(Ok(TakeSetup {
            take_id: target,
            start_ms: self.recording_start_ms,
        }));
    }

    /// The mic's start came back: armed, failed or cancelled.
    #[must_use]
    pub fn start_done(
        &mut self,
        done: StartDone,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        let op = done.op;
        let Some(start) = self
            .start_flight_mut(op)
            .filter(|s| matches!(s.stage, StartStage::Arming) && s.awaiting_mic)
        else {
            return self.finish();
        };
        start.awaiting_mic = false;
        self.cancelling_start = false;
        self.start_position_held = false;
        // Armed, failed or cancelled: the arm window closed either way.
        self.notify();
        let r = match done.outcome {
            StartOutcome::Threw {
                setup_failed,
                message,
            } => {
                self.arm_failed(op, setup_failed, &message, host);
                return self.finish();
            }
            StartOutcome::Settled(r) => r,
        };
        if let Some(failure) = r.failure() {
            match failure {
                MicRecordingFailure::PermissionDenied => host.toast(mic_permission::DENIED_MESSAGE),
                // The writer could not create the WAV (disk full /
                // permissions) and nothing would be recorded — fail loudly
                // instead of "recording" into nowhere.
                MicRecordingFailure::FileCreateFailed => {
                    host.toast("Couldn't create the recording file. Check your free disk space.")
                }
                // No usable audio device (the take setup was skipped
                // entirely).
                MicRecordingFailure::EngineUnavailable => {
                    host.toast("No audio device is available to record with.")
                }
                // The mic armed but never delivered a frame within the ready
                // timeout (async input-stream open failed or stalled). The
                // take was already discarded by the controller.
                MicRecordingFailure::InputNotDelivering => host.toast(
                    "The microphone did not start. Try again, or pick a \
                     different input device in Settings.",
                ),
            }
            self.finally_arm(op);
            return self.finish();
        }
        // Invalidated mid-wait (Stop, route popped / disposed): a silent
        // no-op.
        let Some(path) = r.path().map(str::to_string) else {
            self.finally_arm(op);
            return self.finish();
        };
        // Armed: the take is recording into its lane, which is no longer
        // empty.
        self.created_empty_take = None;
        // Its level ticks start now (the recorder armed its timer just before
        // returning, and the first tick lands an interval later).
        self.take_clock = Some((self.now)());
        // The take is armed, which means the user can STOP it right now —
        // every wait below must re-check, or a continuation resuming after
        // the stop would restart the transport against a finalized take and
        // overwrite the pre-roll the placement already consumed. The
        // generation token (not just the recording bool) closes the ABA:
        // after this take's stop, a NEW take can arm while this continuation
        // is still parked, and `is_recording` alone would read true again for
        // the wrong take.
        let take_gen = self.take_generation;
        if let Some(start) = self.start_flight_mut(op) {
            start.take_gen = take_gen;
            start.path = Some(path);
            start.stage = StartStage::Resyncing;
        }
        // Mute the take being recorded into (you can't monitor what you're
        // laying down); every other lane still obeys mute/solo. Play freely
        // (no range wrap) from the cursor as a reference.
        self.effects.push(RecordingEffect::Resync(op));
        self.finish()
    }

    /// Dart's `catch (e)` around the arm: setup failed partway (the empty
    /// take's save, the WAV's creation, the resync).
    fn arm_failed(
        &mut self,
        op: RecordingOp,
        setup_failed: bool,
        message: &str,
        host: &mut dyn RecordingHost,
    ) {
        let take_gen = self.start_flight(op).map_or(-1, |s| s.take_gen);
        // If OUR take armed before the throw, stop it — the WAV stays on disk
        // for the boot sweep — and surface the error so REC isn't left
        // dead-buttoned. Generation-guarded: after this take's stop a NEW
        // take can already be armed, and a bare is_recording check would
        // stop it.
        if self.mic.is_recording() && self.take_generation == take_gen {
            let _ = self.mic.stop();
        }
        if !setup_failed {
            let text = host.friendly_error(message);
            host.toast(&text);
        }
        self.notify();
        self.finally_arm(op);
    }

    /// Whether the armed take `take_gen` is no longer the one recording.
    fn take_stopped(&self, take_gen: i64) -> bool {
        self.disposed || !self.mic.is_recording() || self.take_generation != take_gen
    }

    /// The armed take's resync landed: start the backing from the take's
    /// start, and note the pre-roll.
    fn armed_resynced(
        &mut self,
        op: RecordingOp,
        result: Result<(), SyncError>,
        host: &mut dyn RecordingHost,
    ) {
        let Some((take_gen, path)) = self
            .start_flight(op)
            .map(|s| (s.take_gen, s.path.clone().unwrap_or_default()))
        else {
            return;
        };
        if let Err(e) = result {
            self.arm_failed(op, false, &e.to_string(), host);
            return;
        }
        if self.take_stopped(take_gen) {
            self.finally_arm(op);
            return;
        }
        self.transport.set_loop(false, 0, 0);
        self.transport.seek(millis(self.recording_start_ms));
        if self.take_stopped(take_gen) {
            self.finally_arm(op);
            return;
        }
        self.effects
            .push(RecordingEffect::SetRecordingPosition(millis(
                self.recording_start_ms,
            )));
        self.transport.play();
        if self.take_stopped(take_gen) {
            // A stop landed between the check above and play(): its own pause
            // may have run FIRST, in which case this play just restarted a
            // transport the finalize believes is stopped — undo it. ONLY while
            // the transport is still ours: a different generation means a NEW
            // take owns it, and pausing here would kill that take's backing.
            if !self.disposed && self.take_generation == take_gen {
                self.transport.pause();
            }
            self.finally_arm(op);
            return;
        }
        // Pre-roll: the recording has been rolling since the arm (WAV offset
        // 0), but the backing only started just now — everything above
        // (resync, decode, seek) sat between the two. record_written_frames
        // counts exactly the frames already in this take's ring, so its value
        // at this moment IS the take's file offset (engine-rate, 48 kHz) — no
        // global-counter subtraction, no ±1-callback ambiguity. The acoustic
        // round-trip is compensated separately (record_latency_ms). Guarded: a
        // stop that raced the play() above has already read the pre-roll, so
        // a late write here would leak into the NEXT take. The backing is
        // rolling; read here, no tick lands between this and its pre-roll.
        self.backing_rolling = true;
        self.head_zero_ms = self.taken_ms();
        if let Some(engine) = self.graph_engine.clone() {
            if !self.take_stopped(take_gen) {
                let pre = engine.record_written_frames();
                // Kept fractional: the placement below rounds the whole sum
                // once. Truncated to whole ms here, every take sat up to 1 ms
                // late.
                if pre > 0 {
                    self.recording_pre_roll_ms = pre as f64 * 1000.0 / 48000.0;
                }
                // The forming take moves back to where it plays
                // (live_take_start_ms).
                self.notify();
                // Where a stop will place it, for a crash's recovery to place
                // it by. Not waited for: this start is done here, and held
                // open on a slow disk it outlived its take, clearing its flags
                // as it returned under the next take's arm.
                let start_ms = round_to_i64(
                    self.recording_start_ms as f64
                        - self.recording_pre_roll_ms
                        - engine.record_latency_ms(),
                );
                let files = Arc::clone(&self.audio_files);
                self.effects.push(RecordingEffect::Run(RecordingJob {
                    op: None,
                    work: Box::new(move || {
                        files.write_aligned_start(&path, start_ms);
                    }),
                }));
            }
        }
        self.finally_arm(op);
    }

    /// Dart's `finally` of the arm, then of the start: never armed, drop the
    /// empty take this start created for itself. Even once the screen is
    /// gone (Back in the arm window): the host's gate and save still run
    /// after dispose, and skipping it here left the empty "Take N" in the
    /// practice for good.
    fn finally_arm(&mut self, op: RecordingOp) {
        if self.start_flight(op).is_none() {
            return;
        }
        if let Some(created) = self.created_empty_take.take() {
            self.discarding_take = true;
            self.notify();
            if let Some(start) = self.start_flight_mut(op) {
                start.stage = StartStage::Discarding;
            }
            self.effects
                .push(RecordingEffect::DiscardEmptyTake(op, created));
            return;
        }
        self.start_finished(op);
    }

    /// The empty take a start that never armed created is dropped.
    #[must_use]
    pub fn empty_take_discarded(&mut self, op: RecordingOp) -> Vec<RecordingEffect> {
        if !self
            .start_flight(op)
            .is_some_and(|s| matches!(s.stage, StartStage::Discarding))
        {
            return self.finish();
        }
        self.discarding_take = false;
        if !self.disposed {
            self.notify();
        }
        self.start_finished(op);
        self.finish()
    }

    /// The start's own `finally`: the setup flag clears and the quit no
    /// longer waits on it.
    fn start_finished(&mut self, op: RecordingOp) {
        let Some(at) = self.starts.iter().position(|s| s.op == op) else {
            return;
        };
        let start = self.starts.remove(at);
        self.take_setup_started = false;
        self.finalizer.unregister(&start.cancel_for_quit);
        self.settle_waiters();
    }

    /// Stop: stops the hardware + pumps and lands the take; in REC's arm
    /// window, cancels the start.
    #[must_use]
    pub fn stop_recording(&mut self) -> Vec<RecordingEffect> {
        self.stop_take();
        self.finish()
    }

    /// The body of [`RecordingController::stop_recording`]; the stop that
    /// now lands the take, if one does.
    fn stop_take(&mut self) -> Option<RecordingOp> {
        // Stops the hardware + pumps and unregisters the armed window's quit
        // finalizer; `None` means no take was in flight. Everything up to the
        // `stopping` latch below runs in this call, so the latch is armed
        // before a racing REC tap can be handled.
        let Some(stopped) = self.mic.stop() else {
            // Still arming (permission, take setup, the mic's ready
            // handshake): cancel the start, which then arms nothing and drops
            // its file. Ignored before, the take armed and rolled moments
            // after the user's Stop.
            if self.mic.is_starting() {
                self.mic.invalidate_pending_start();
                self.cancelling_start = true;
                self.notify();
                // Stop pauses the transport too, where it is, as stopping an
                // armed take does: a REC pressed over a playing backing (a
                // punch on the fly) otherwise left the backing playing on
                // after the cancel.
                self.transport.pause();
            }
            return None;
        };
        self.stopping = true;
        // The mic's stop just unregistered the armed window's quit finalizer,
        // but the take's row commits only at the end of the finalize tail —
        // hold the quit gate over it, or a quit in this window exits mid-save
        // and boot recovery re-adopts the WAV at timeline zero, losing the
        // placement, punch-in target and latency alignment. Listed per
        // practice too: a screen reopened on it while the take is still
        // landing waits for the take (see the host's load).
        let hold = self.mic.begin_stop_finalize();
        let landing = self.finalizer.begin_landing(&self.practice_id);
        let op = self.op();
        self.finish_stop(op, stopped, landing, hold);
        Some(op)
    }

    /// The tail of the stop (meter reset → pause → placement → gated
    /// finalize), up to the placement's file work.
    fn finish_stop(
        &mut self,
        op: RecordingOp,
        stopped: MicRecordingStop,
        landing: Landing,
        hold: StopFinalizeHold,
    ) {
        self.set_record_level(0.0);
        // The recording loop was driving the shared MIC meter; unless
        // monitoring is also on (its own timer keeps updating it), nothing
        // else will, so clear it — otherwise the meter sticks at the last
        // captured peak.
        if !self.input_monitor_on {
            self.held_input_level = 0.0;
            self.set_input_level(0.0);
        }
        self.transport.pause();
        // The take disabled the engine's loop wrap. Re-arm it here rather
        // than relying on the finalize's save: a finalize that drops an
        // unreadable partial take returns before saving, which left the loop
        // button lit over a transport that no longer looped. (The mic stopped
        // above, so this push is no longer held off as a mid-take one.)
        self.effects.push(RecordingEffect::EnsureLoopRange);
        // Placement compensation, two independent terms:
        //  * pre-roll — the WAV has been recording since the arm, but the
        //    backing started `recording_pre_roll_ms` later, so the sung
        //    content begins that far INTO the file;
        //  * record latency — the first sample sung against the backing is
        //    the response to audio the singer heard ~round-trip ago (output
        //    IO + acoustic + input IO + AEC).
        // Both shift the take EARLIER on the timeline. When the shift would
        // land before timeline zero (recording from the top — the common
        // case), the compensation is applied by CUTTING that much off the
        // file's head instead: clamping the position alone would silently
        // discard the whole compensation and leave the take late.
        let latency_ms = self
            .graph_engine
            .as_ref()
            .map_or(0.0, |e| e.record_latency_ms());
        let raw_start_ms =
            round_to_i64(self.recording_start_ms as f64 - self.recording_pre_roll_ms - latency_ms);
        let path = stopped.path.clone();
        let files = Arc::clone(&self.audio_files);
        self.effects.push(RecordingEffect::Run(RecordingJob {
            op: Some(op),
            work: Box::new(move || {
                // Noted beside the take before any cut, as the start the cut
                // leaves (see `write_aligned_start_to`): a take whose save
                // then fails is recovered at boot, and still read the arm's
                // estimate — negative, so it was cut a second time.
                let noted = files.write_aligned_start(&path, raw_start_ms.max(0));
                if raw_start_ms < 0 && noted {
                    // Not without the note: the arm's negative estimate beside
                    // a cut file is a second cut at recovery. Best-effort: a
                    // take that can't be trimmed (unparseable partial after an
                    // IO error) keeps its head and sits late by the untrimmed
                    // amount — strictly better than losing the take.
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        trim_wav_head_in_place(&path, -raw_start_ms)
                    }));
                }
            }),
        }));
        self.stop = Some(StopFlight {
            op,
            stopped,
            stage: StopStage::Placing,
            target_id: None,
            start_ms: raw_start_ms.max(0),
            analysis: None,
            staged: None,
            save_ran: false,
            landing: Some(landing),
            hold: Some(hold),
        });
    }

    /// A worker's file work ran.
    #[must_use]
    pub fn job_done(
        &mut self,
        done: JobDone,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        let Some(op) = done.op else {
            return self.finish();
        };
        if !self
            .stop
            .as_ref()
            .is_some_and(|s| s.op == op && matches!(s.stage, StopStage::Placing))
        {
            return self.finish();
        }
        let target_id = self.recording_target_id.take();
        if let Some(stop) = self.stop.as_mut() {
            stop.target_id = target_id;
        }
        self.rec_peaks.clear();
        // The screen can be left during the work above (Stop, then Back: the
        // route no longer waits once is_recording is false). The finalize
        // below still has to run, so don't touch the disposed view on the
        // way.
        if !self.disposed {
            self.clear_peaks_view();
        }
        self.notify();
        self.try_finalize(op, host);
        self.finish()
    }

    /// The finalize under the busy gate. The gate is normally free here —
    /// every structural action pre-guards on the whole take window (arm →
    /// recording → this stop) — but the finalize must NEVER be dropped (the
    /// take would be stranded on disk until the next boot, which re-adopts
    /// it at 0:00 without its lane or placement), so on the off-chance an
    /// action still holds the gate, wait it out and retry. Even once the
    /// screen is disposed: the host's save still writes after dispose (it
    /// only skips the UI resync), and every gate holder finishes.
    fn try_finalize(&mut self, op: RecordingOp, host: &mut dyn RecordingHost) {
        let Some(stop) = self.stop.as_ref().filter(|s| s.op == op) else {
            return;
        };
        let path = stop.stopped.path.clone();
        let clean = stop.stopped.clean;
        let slot: ReadSlot = Arc::new(Mutex::new(None));
        let action = {
            let slot = Arc::clone(&slot);
            let analyze = Arc::clone(&self.analyze);
            let files = Arc::clone(&self.audio_files);
            move || {
                let read = read_take(&path, clean, &*analyze, &*files)?;
                *lock(&slot) = Some(read);
                Ok(())
            }
        };
        let stage = match host.busy_gate().run(action) {
            Some(run) => {
                self.effects.push(RecordingEffect::Finalize(op, run));
                StopStage::Reading(slot)
            }
            None => {
                self.effects.push(RecordingEffect::RetryFinalize {
                    op,
                    after: Self::GATE_RETRY,
                });
                StopStage::AwaitingGate
            }
        };
        if let Some(stop) = self.stop.as_mut() {
            stop.stage = stage;
        }
    }

    /// The finalize's wait for the busy gate is over: try again.
    #[must_use]
    pub fn finalize_retry_due(
        &mut self,
        op: RecordingOp,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        if self
            .stop
            .as_ref()
            .is_some_and(|s| s.op == op && matches!(s.stage, StopStage::AwaitingGate))
        {
            self.try_finalize(op, host);
        }
        self.finish()
    }

    /// The finalize's read of the take landed (the gate is still held).
    #[must_use]
    pub fn finalize_ran(
        &mut self,
        op: RecordingOp,
        outcome: BusyOutcome<String>,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        let Some(slot) = self
            .stop
            .as_ref()
            .filter(|s| s.op == op)
            .and_then(|s| match &s.stage {
                StopStage::Reading(slot) => Some(Arc::clone(slot)),
                _ => None,
            })
        else {
            return self.finish();
        };
        if let Err(e) = outcome {
            // The finalize threw (a clean take that would not analyze): the
            // gate surfaces it, and the `finally` still unmutes the take.
            self.end_finalize(Err(e), host);
            return self.finish();
        }
        let read = lock(&slot).take();
        let Some((stopped_clean, dropped_frames)) = self
            .stop
            .as_ref()
            .map(|s| (s.stopped.clean, s.stopped.dropped_frames))
        else {
            return self.finish();
        };
        let analysis = match read {
            Some(TakeRead::Clean(analysis)) => analysis,
            Some(TakeRead::Partial(analysis)) => {
                host.toast("Recording hit a disk error. The take may be incomplete.");
                analysis
            }
            Some(TakeRead::Unreadable) => {
                host.toast("Recording failed. The take could not be saved.");
                self.end_finalize(Ok(()), host);
                return self.finish();
            }
            Some(TakeRead::Held) => {
                host.toast(
                    "Recording hit a disk error, and the take could not be read yet. \
                     It was kept, and Vocara will try to recover it the next time it opens.",
                );
                self.end_finalize(Ok(()), host);
                return self.finish();
            }
            None => {
                self.end_finalize(
                    Err(ActionError::Failed("the take was not read".to_string())),
                    host,
                );
                return self.finish();
            }
        };
        // The writer finalized fine but the recording ring overflowed mid-take
        // (writer thread or disk stalled): the WAV is valid yet missing frames
        // as silent holes. Keep the take — most of it is fine — but never
        // present it as gapless.
        if stopped_clean && dropped_frames > 0 {
            let dropped_ms = dropped_frames * 1000 / 48000;
            host.toast(&format!(
                "Recording dropped about {dropped_ms} ms of audio while the disk \
                 was busy. The take has small gaps."
            ));
        }
        if let Some(stop) = self.stop.as_mut() {
            stop.analysis = Some(analysis);
        }
        let staged = self.stage(host);
        if let Some(stop) = self.stop.as_mut() {
            stop.staged = staged;
            stop.stage = StopStage::Saving(SaveAttempt::First);
        }
        self.effects.push(RecordingEffect::Save(op));
        self.finish()
    }

    /// Puts the recording into the model; returns the lane it landed on and,
    /// dropped onto an armed take, the clip it became.
    fn stage(&mut self, host: &mut dyn RecordingHost) -> Option<Staged> {
        let stop = self.stop.as_ref()?;
        let analysis = stop.analysis.as_ref()?;
        let path = stop.stopped.path.as_str();
        let start_ms = stop.start_ms;
        let target = stop
            .target_id
            .as_deref()
            .and_then(|id| host.track_by_id(id))
            .filter(|t| t.kind == AudioTrackKind::Vocal);
        let Some(target) = target else {
            // No armed take → the recording becomes a new take at the cursor.
            let landed_on = host.append_vocal(path, analysis, start_ms);
            host.select_recorded_clip(&landed_on, None);
            return Some(Staged {
                lane: landed_on,
                clip: None,
            });
        };
        // Drop the recording onto the armed take as its own clip at the
        // cursor — a separate clip, never merged into existing audio (Logic /
        // Cubase).
        let clip = AudioClip {
            id: new_id(),
            audio_file_path: path.to_string(),
            duration_ms: analysis.duration_ms,
            start_ms,
            waveform_peaks: analysis.peaks.clone(),
        };
        let clip_id = clip.id.clone();
        host.update_track(&target.id, &|t| {
            let mut next = t.clone();
            next.clips.push(clip.clone());
            next
        });
        // Select BOTH the lane and the clip (matching add_clip_to_take):
        // selection isn't gated during a take, so if the user tapped a
        // different lane while recording, leaving the lane selection stale
        // would pair this clip id with the wrong lane and the inspector would
        // analyze the wrong subject.
        host.select_recorded_clip(&target.id, Some(&clip_id));
        Some(Staged {
            lane: target.id.clone(),
            clip: Some(clip_id),
        })
    }

    /// Whether the model holds the take, judged by the lane's and clip's
    /// ids, which survive the commit: the take comes back under its pooled
    /// path, and judged by its staging path it was staged twice and played
    /// doubled.
    fn kept(&self, host: &dyn RecordingHost) -> bool {
        let Some(staged) = self.stop.as_ref().and_then(|s| s.staged.as_ref()) else {
            return false;
        };
        host.track_by_id(&staged.lane).is_some_and(|lane| {
            staged
                .clip
                .as_ref()
                .is_none_or(|clip| lane.clips.iter().any(|c| &c.id == clip))
        })
    }

    fn restage(&mut self, host: &mut dyn RecordingHost) {
        let staged = self.stage(host);
        if let Some(stop) = self.stop.as_mut() {
            stop.staged = staged;
        }
    }

    fn save_again(&mut self, op: RecordingOp, attempt: SaveAttempt) {
        if let Some(stop) = self.stop.as_mut() {
            stop.stage = StopStage::Saving(attempt);
        }
        self.effects.push(RecordingEffect::Save(op));
    }

    /// A save of the finalize landed (`ok`: THIS write persisted).
    #[must_use]
    pub fn saved(
        &mut self,
        op: RecordingOp,
        ok: bool,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        let Some(attempt) = self
            .stop
            .as_ref()
            .filter(|s| s.op == op)
            .and_then(|s| match s.stage {
                StopStage::Saving(attempt) => Some(attempt),
                _ => None,
            })
        else {
            return self.finish();
        };
        match attempt {
            // A save that lands has committed the take, unless an edit's save
            // queued ahead of it carried the take, failed and reverted it away
            // first: this one then saved the model without it. (A failing
            // save queued behind it reverts to a store that holds it.)
            SaveAttempt::First => {
                if !ok || !self.kept(host) {
                    // A failed save reverts the model to the store's, which
                    // never held the take: it vanished from the screen and no
                    // later save brought it back, only the next launch's
                    // recovery of its file, as a new take at 0:00 off its
                    // lane. Stage it again — unless the model kept it (a newer
                    // edit's queued save carrying it) — and retry once.
                    if !self.kept(host) {
                        self.restage(host);
                    }
                    self.save_again(op, SaveAttempt::Retry);
                    return self.finish();
                }
            }
            // Saves run one at a time, so every save queued ahead of the
            // retry has settled by now. One that carried the take (an edit
            // made while the take's save ran) and failed in turn reverted it
            // away, and the retry then saved a model without it: the take
            // vanished with no notice. That was the edit's failure, not the
            // take's: stage it once more.
            SaveAttempt::Retry => {
                if ok && !self.kept(host) {
                    self.restage(host);
                    self.save_again(op, SaveAttempt::Last);
                    return self.finish();
                }
                self.save_verdict(ok, host);
            }
            SaveAttempt::Last => self.save_verdict(ok, host),
        }
        if let Some(stop) = self.stop.as_mut() {
            stop.save_ran = true;
        }
        // Only AFTER the save: it re-reads the clip at its pooled path, which
        // is the key the analysis caches look it up by. Refreshed before it,
        // the dock analyzed the staging path nothing shows — and, computing
        // only on these pushes, then sat on "not available" for the new take
        // while the inspector (when open) analyzed the take a second time.
        if let Some(lane) = self
            .stop
            .as_ref()
            .and_then(|s| s.staged.as_ref())
            .map(|s| s.lane.clone())
        {
            self.effects.push(RecordingEffect::TrackAudioChanged(lane));
        }
        self.end_finalize(Ok(()), host);
        self.finish()
    }

    fn save_verdict(&mut self, ok: bool, host: &mut dyn RecordingHost) {
        if !ok || !self.kept(host) {
            host.toast(
                "Couldn't save the take. It will be recovered the next time \
                 Vocara opens.",
            );
        }
    }

    /// The finalize ended (`outcome`: whether it threw): the gate is handed
    /// back, then the stop's `finally`.
    fn end_finalize(&mut self, outcome: BusyOutcome<String>, host: &mut dyn RecordingHost) {
        if let Some(error) = host.busy_gate().finish(outcome) {
            self.effects.push(RecordingEffect::GateError(error));
        }
        // The take punched into was silent in the engine for the whole
        // recording, and only the save's resync lifted that. A finalize that
        // drops an unreadable partial returns before saving, and one that
        // throws (a file too short to decode) never gets there: without this
        // the lane played silent, its M button unlit, until some later edit
        // saved.
        let Some(stop) = self.stop.as_mut() else {
            return;
        };
        if !stop.save_ran && !self.disposed {
            stop.stage = StopStage::Resyncing;
            let op = stop.op;
            self.effects.push(RecordingEffect::Resync(op));
            return;
        }
        self.stop_finished();
    }

    /// The stop's tail settled: the take is listed as landed, the quit no
    /// longer held, and REC may arm again.
    fn stop_finished(&mut self) {
        let Some(mut stop) = self.stop.take() else {
            return;
        };
        drop(stop.landing.take());
        drop(stop.hold.take());
        self.stopping = false;
        self.settle_waiters();
    }

    /// A resync the controller asked for landed.
    #[must_use]
    pub fn resynced(
        &mut self,
        op: RecordingOp,
        result: Result<(), SyncError>,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        if self
            .start_flight(op)
            .is_some_and(|s| matches!(s.stage, StartStage::Resyncing))
        {
            self.armed_resynced(op, result, host);
        } else if self
            .stop
            .as_ref()
            .is_some_and(|s| s.op == op && matches!(s.stage, StopStage::Resyncing))
        {
            if let Err(e) = result {
                DiagnosticsLog::instance().log(
                    "recording",
                    &format!("the resync after a take that never landed failed: {e}"),
                );
            }
            self.stop_finished();
        }
        self.finish()
    }

    // --- Leaving and quitting ---------------------------------------------------

    /// Ends the take in flight before the screen goes away: an armed take
    /// stops and lands ([`RecordingController::stop_recording`]); one still
    /// in REC's arm window is cancelled, and this also waits for that start
    /// to unwind. The page leaves on [`RecordingEffect::Leave`]. Leaving only
    /// waited for a take already recording: in the arm window the page
    /// popped at once, and a handshake landing during its exit transition
    /// armed a take there that the dispose then dropped for boot recovery.
    ///
    /// Only the first of several Backs pressed while it waits (on the
    /// permission prompt, say, where nothing visibly happens) leaves: the
    /// others get no [`RecordingEffect::Leave`] — each queued a pop of its
    /// own, and the page left one page too far, or threw with nothing beneath
    /// to pop.
    #[must_use]
    pub fn stop_before_leaving(&mut self) -> Vec<RecordingEffect> {
        if self.leaving {
            return self.finish();
        }
        self.leaving = true;
        self.notify();
        let start = self.latest_start;
        let stop = self.stop_take();
        self.waiters.push(Waiter::Leave { stop, start });
        self.settle_waiters();
        self.finish()
    }

    /// Lets the screen know when a quit asks something of the controller:
    /// `wake` is called (on the quit's thread) with each request; the screen
    /// then calls [`RecordingController::serve_quit_requests`] on the UI
    /// thread. Without a waker the requests wait for a call of their own.
    pub fn set_quit_waker(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        self.quit.queue().waker = Some(Arc::new(wake));
    }

    /// Whether a quit is waiting to be served.
    pub fn has_quit_requests(&self) -> bool {
        !self.quit.queue().asked.is_empty()
    }

    /// What the app's recorder is handed as its `on_quit_finalize`: asks
    /// this controller to stop the armed take. A test's injected recorder
    /// takes it to register for its armed window, as the real one does.
    #[cfg(test)]
    pub(crate) fn quit_hook(&self) -> vocara_services::mic_recording_controller::QuitFinalize {
        let quit = Arc::clone(&self.quit);
        Arc::new(move || quit.ask(QuitKind::StopTake))
    }

    /// Serves the quit's requests: what the quit's finalizers awaited in
    /// Dart. A take armed stops and lands; one in REC's arm window is
    /// cancelled, and a start that reached its take setup is waited for
    /// (which drops the empty take it made).
    #[must_use]
    pub fn serve_quit_requests(&mut self) -> Vec<RecordingEffect> {
        let asked = std::mem::take(&mut self.quit.queue().asked);
        for (kind, latch) in asked {
            match kind {
                QuitKind::CancelStart => {
                    let start = self.latest_start;
                    let stop = self.stop_take();
                    self.waiters.push(Waiter::QuitCancel {
                        latch,
                        stop,
                        start,
                        then_start: None,
                    });
                }
                QuitKind::StopTake => {
                    let stop = if self.is_recording() {
                        self.stop_take()
                    } else {
                        None
                    };
                    match stop {
                        Some(stop) => self.waiters.push(Waiter::QuitStop { latch, stop }),
                        None => latch.open(),
                    }
                }
            }
        }
        self.settle_waiters();
        self.finish()
    }

    fn stop_running(&self, op: Option<RecordingOp>) -> bool {
        op.is_some_and(|op| self.stop.as_ref().is_some_and(|s| s.op == op))
    }

    fn start_running(&self, op: Option<RecordingOp>) -> bool {
        op.is_some_and(|op| self.start_flight(op).is_some())
    }

    /// Releases every waiter whose take has ended.
    fn settle_waiters(&mut self) {
        let waiters = std::mem::take(&mut self.waiters);
        for mut waiter in waiters {
            let settled = match &mut waiter {
                Waiter::QuitStop { latch, stop } => {
                    let done = !self.stop_running(Some(*stop));
                    if done {
                        latch.open();
                    }
                    done
                }
                Waiter::QuitCancel {
                    latch,
                    stop,
                    start,
                    then_start,
                } => {
                    if self.stop_running(*stop) {
                        false
                    } else {
                        // `if (start != null && _takeSetupStarted) await
                        // start`, judged once the stop is done.
                        let wait = *then_start.get_or_insert_with(|| {
                            if start.is_some() && self.take_setup_started {
                                *start
                            } else {
                                None
                            }
                        });
                        let done = !self.start_running(wait);
                        if done {
                            latch.open();
                        }
                        done
                    }
                }
                Waiter::Leave { stop, start } => {
                    let done = !self.stop_running(*stop) && !self.start_running(*start);
                    if done {
                        self.leaving = false;
                        self.notify();
                        self.effects.push(RecordingEffect::Leave);
                    }
                    done
                }
            };
            if !settled {
                self.waiters.push(waiter);
            }
        }
    }

    // --- Input monitoring ---------------------------------------------------------

    /// Toggle live input monitoring — open/close the mic stream and ramp the
    /// monitor in/out, and poll the input level for the meter. No-op on
    /// engines without monitoring. Turning it on asks for the microphone
    /// first ([`RecordingEffect::RequestMicPermission`]).
    #[must_use]
    pub fn toggle_input_monitor(&mut self, host: &mut dyn RecordingHost) -> Vec<RecordingEffect> {
        let Some(engine) = self.graph_engine.clone() else {
            return self.finish();
        };
        // Synchronous re-entrancy guard: the ON branch waits for a permission
        // round-trip, so a fast double-tap could otherwise arm monitoring
        // twice and orphan a level timer.
        if self.toggling_monitor {
            return self.finish();
        }
        if self.input_monitor_on {
            engine.stop_monitor();
            self.input_monitor_on = false;
            self.input_level_timer = false;
            self.held_input_level = 0.0;
            self.set_input_level(0.0);
            self.notify();
            return self.finish();
        }
        if !engine.is_available() {
            host.toast(PlaybackController::NO_DEVICE_MESSAGE);
            return self.finish();
        }
        self.toggling_monitor = true;
        self.effects.push(RecordingEffect::RequestMicPermission);
        self.finish()
    }

    /// The answer to the monitor's [`RecordingEffect::RequestMicPermission`].
    #[must_use]
    pub fn monitor_permission_answered(
        &mut self,
        granted: bool,
        host: &mut dyn RecordingHost,
    ) -> Vec<RecordingEffect> {
        if !self.toggling_monitor {
            return self.finish();
        }
        self.toggling_monitor = false;
        let Some(engine) = self.graph_engine.clone() else {
            return self.finish();
        };
        if !granted {
            host.toast(mic_permission::DENIED_MESSAGE);
            return self.finish();
        }
        // The view could have been disposed during the permission wait.
        if self.disposed {
            return self.finish();
        }
        engine.start_monitor();
        self.input_monitor_on = true;
        self.held_input_level = 0.0;
        // Cancel any prior timer before arming so a leftover never leaks (one
        // timer flag here).
        self.input_level_timer = true;
        self.notify();
        self.finish()
    }

    // --- Device watch ---------------------------------------------------------------

    /// One tick of the 1 s device watch: lost audio devices self-heal. The
    /// playhead is driven by the output stream's callback, so if that stream
    /// dies (a Bluetooth headset flips to HFP, a device is unplugged),
    /// transport would otherwise freeze. On an output-lost event the current
    /// output is re-opened.
    #[must_use]
    pub fn device_watch_tick(&mut self, host: &mut dyn RecordingHost) -> Vec<RecordingEffect> {
        if !self.device_watch_active() {
            return self.finish();
        }
        let Some(engine) = self.graph_engine.clone() else {
            return self.finish();
        };
        // Per-bit claims: the poll clears only the masked bits, so this watch
        // and the technique page's level tick each consume their own events
        // instead of the fragile whole-process single-poller convention.
        let events = engine.poll_device_events(
            GraphAudioPlaybackService::EVENT_OUTPUT_LOST
                | GraphAudioPlaybackService::EVENT_INPUT_LOST
                | GraphAudioPlaybackService::EVENT_AEC_BYPASSED
                | GraphAudioPlaybackService::EVENT_INPUT_FELL_BACK
                | GraphAudioPlaybackService::EVENT_RECORD_LIMIT
                | GraphAudioPlaybackService::EVENT_RECORD_FAILED
                | GraphAudioPlaybackService::EVENT_OUTPUT_REROUTED,
        );
        // A chosen output plugged back in is switched back to (the Settings
        // panel, which watches for that, can't be open over a song).
        let rebound = self.settings.refresh_output_devices();
        // Opening the mic hands the output to the duplex unit, which can move
        // playback off the selected device without any device SET changing —
        // so monitor safety has to be re-derived on the tick, not only when
        // refresh_devices sees a difference.
        self.settings.reconcile_monitor_safety();
        if events & GraphAudioPlaybackService::EVENT_OUTPUT_LOST != 0 && !rebound {
            // Output lost: re-open the current output so transport doesn't
            // freeze. Deliberately the REQUESTED device, not the one currently
            // rendering: re-asking for the user's pick re-attaches it if it
            // came back, where re-asking for a fallback would strand them
            // there. Skipped when the refresh above just re-opened it: a
            // second rebuild is a second gap.
            engine.set_output_device(&engine.current_output_device());
        }
        if events & GraphAudioPlaybackService::EVENT_OUTPUT_REROUTED != 0 {
            // Playback is not on the device the user chose. Most often this is
            // the mic opening: the duplex unit renders through the
            // microphone's endpoint or the system default, so the backing
            // track moves too — it is not only the monitor that is affected,
            // and saying so is the difference between "my headphones went
            // quiet" and a mystery.
            host.toast(
                "Playback moved off your selected output device while the \
                 microphone is open. Hear-yourself monitoring is muted until it \
                 returns.",
            );
        }
        if events & GraphAudioPlaybackService::EVENT_INPUT_FELL_BACK != 0 {
            // The selected microphone could not be opened; capture fell back
            // to the system default. The device label in Settings is now
            // wrong for this stream — silent misattribution is worse than the
            // notice.
            host.toast(INPUT_FELL_BACK_NOTICE);
        }
        // Streaming health: log underrun growth (never toast — brief silence
        // already told the user; the log is for diagnosing feeder timing on
        // real hardware, where offline bit-exactness can't reach).
        let xruns = engine.stream_xruns();
        if xruns > self.last_stream_xruns {
            DiagnosticsLog::instance().log(
                "engine",
                &format!(
                    "stream underruns grew to {xruns} (+{})",
                    xruns - self.last_stream_xruns
                ),
            );
            self.last_stream_xruns = xruns;
        }
        if events & GraphAudioPlaybackService::EVENT_AEC_BYPASSED != 0 {
            // The mic opened WITHOUT echo cancellation (VPIO negotiated
            // mismatched element rates): capture works, but speaker playback
            // can bleed into the take. Silent degradation is worse than the
            // notice.
            host.toast(AEC_BYPASSED_NOTICE);
        }
        if events & GraphAudioPlaybackService::EVENT_RECORD_FAILED != 0 && self.is_recording() {
            // A write to the take's WAV failed (a full disk, a revoked
            // permission): nothing more of the take reaches the disk while
            // the clock runs on, and the singer sang on into nothing until
            // Stop said so. Stop now, as for the limit below: the finalize
            // keeps what landed and says whether it could be read.
            host.toast(RECORD_WRITE_FAILED_NOTICE);
            self.stop_take();
        }
        if events & GraphAudioPlaybackService::EVENT_RECORD_LIMIT != 0 && self.is_recording() {
            // The take reached the longest recording a WAV header can
            // describe, so the engine stopped feeding the writer. Finalize
            // now: the file on disk is valid and complete up to the limit, and
            // the recording clock would otherwise keep running over audio
            // nothing is storing. Unreachable while the length limit below
            // stops every take within the hour; kept as the engine's backstop.
            host.toast(
                "Recording reached the six-hour maximum length. \
                 The take was saved up to that point.",
            );
            self.stop_take();
        }
        if self.is_recording() && self.taken_ms() >= Self::TAKE_LENGTH_LIMIT.as_millis() as f64 {
            // See TAKE_LENGTH_LIMIT: stopped here, the take is kept whole.
            host.toast(&take_length_limit_notice());
            self.stop_take();
        }
        if events & GraphAudioPlaybackService::EVENT_INPUT_LOST != 0 {
            self.on_input_device_lost(host);
        } else if self.is_recording() || self.input_monitor_on || host.live_monitor() {
            // No explicit input-loss event, but capture is supposed to be live
            // (recording, input monitoring, OR the live-spectrogram view — all
            // pull the same input frames). macOS VPIO raises no device-loss
            // event, so a mic unplugged mid-capture freezes silently; detect
            // it by the engine's input frame counter not advancing for ~2 s
            // and recover through the same graceful path.
            let frames = engine.input_frames_received();
            if frames != self.last_input_frames {
                self.last_input_frames = frames;
                self.input_stall_ticks = 0;
                self.input_delivered = true;
            } else if self.input_delivered && {
                self.input_stall_ticks += 1;
                self.input_stall_ticks >= 2
            } {
                self.input_stall_ticks = 0;
                self.on_input_device_lost(host);
            }
        } else {
            // Keep the baseline synced while idle so arming starts from the
            // right count.
            self.last_input_frames = engine.input_frames_received();
            self.input_stall_ticks = 0;
            self.input_delivered = false;
        }
        self.finish()
    }

    /// The input device vanished mid-session (mic unplugged, a Bluetooth
    /// headset flipped profile). Like a pro DAW, never let capture die
    /// silently: stop recording gracefully — finalizing KEEPS the take
    /// captured so far — and drop monitoring, each with a clear notice. The
    /// mic is NOT switched mid-take (that would splice two different inputs
    /// into one take).
    fn on_input_device_lost(&mut self, host: &mut dyn RecordingHost) {
        // Recording, input monitoring and the live view can be active
        // together (starting a take does NOT stop monitoring), so tear down
        // EVERY active input consumer — a single else-if would leave the
        // others' streams open and freeze the shared MIC meter at its last
        // peak (the dead device's input level never updates again) until a
        // later watchdog cycle. One notice.
        let was_recording = self.is_recording();
        let was_monitoring = self.input_monitor_on;
        let was_live = host.live_monitor();
        if !was_recording && !was_monitoring && !was_live {
            return;
        }
        host.toast(if was_recording {
            "Recording stopped: the input device was disconnected."
        } else if was_monitoring {
            "Input monitoring stopped: the input device was disconnected."
        } else {
            "Live view stopped: the input device was disconnected."
        });
        // The stop first: while monitoring is still on it intentionally
        // leaves the meter for monitoring to own; the monitor toggle's OFF
        // branch then closes the monitor stream AND resets the meter to 0, so
        // it can't pin on the stale peak of the now-dead device.
        if was_recording {
            self.stop_take();
        }
        if was_monitoring {
            let effects = self.toggle_input_monitor(host);
            self.effects.extend(effects);
        }
        if was_live {
            self.effects.push(RecordingEffect::StopLiveView);
        }
    }

    // --- Teardown -------------------------------------------------------------------

    /// MUST run before the view model disposes the engine: the device watch
    /// and meter timers poll it, and the mic's dispose closes a racing arm's
    /// native WAV handle (and unregisters the quit finalizer). A take's
    /// finalize in flight still runs to its end: keep answering its effects.
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.input_level_timer = false;
        self.mic.dispose();
    }
}

impl Drop for RecordingController {
    /// Nothing will answer the controller again: a quit waiting on it is let
    /// go, and the quit no longer waits on a start that cannot settle.
    fn drop(&mut self) {
        self.quit.close();
        for start in std::mem::take(&mut self.starts) {
            self.finalizer.unregister(&start.cancel_for_quit);
        }
        for waiter in std::mem::take(&mut self.waiters) {
            match waiter {
                Waiter::QuitCancel { latch, .. } | Waiter::QuitStop { latch, .. } => latch.open(),
                Waiter::Leave { .. } => {}
            }
        }
        self.mic.dispose();
    }
}

/// What the song page says when a write to the take's WAV failed mid-take
/// and the take was stopped for it.
const RECORD_WRITE_FAILED_NOTICE: &str = "Recording stopped: the take could not be written to \
     disk, which may be full. What was recorded up to then is being saved.";

/// What the song page says when a take reached the longest one it keeps.
fn take_length_limit_notice() -> String {
    format!(
        "Takes can be up to {} minutes. This one stopped at that length and was saved.",
        MAX_PLAYBACK_DURATION.as_secs() / 60
    )
}

/// The finalize's read of the stopped take (the busy gate's action, on a
/// worker). `Err` is the finalize throwing.
fn read_take(
    path: &str,
    clean: bool,
    analyze: &(dyn Fn(&str) -> Result<WaveformResult, String> + Send + Sync),
    files: &dyn AudioFiles,
) -> Result<TakeRead, String> {
    if clean {
        return analyze(path).map(TakeRead::Clean);
    }
    // The engine reported an IO error during the take (disk full, write or
    // finalize failure). Be conservative: keep the take only if the partial
    // WAV is still present and readable, so no take row points at unusable
    // audio.
    let partial = catch_unwind(AssertUnwindSafe(|| {
        if Path::new(path).exists() {
            analyze(path).ok()
        } else {
            None
        }
    }))
    .ok()
    .flatten();
    if let Some(analysis) = partial {
        return Ok(TakeRead::Partial(analysis));
    }
    // Not readable now. Deleted only when it is provably no recording: gone,
    // or not the shape the recorder writes. An analysis can fail for reasons
    // that say nothing about the file (over the length the decode accepts, a
    // read the disk failed for now), and boot recovery holds such a file back
    // to try again; deleting it here threw away the only copy of a take that
    // a later launch could have recovered.
    let not_a_recording = matches!(probe_wav(Path::new(path)), Ok(probe)
        if !probe.is_some_and(|p| p.is_recorder_shaped()));
    if not_a_recording {
        // Best-effort cleanup of the unusable partial file.
        let _ = catch_unwind(AssertUnwindSafe(|| files.delete_audio(path)));
        Ok(TakeRead::Unreadable)
    } else {
        Ok(TakeRead::Held)
    }
}
