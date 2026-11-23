//! The shared mic-recording core (port of
//! `lib/data/services/mic_recording_controller.dart`): one guarded start →
//! pump → stop lifecycle used by every screen that records the microphone.
//!
//! A start runs the full guarded arm sequence: mic permission, WAV
//! destination creation via a [`RecordingPathStrategy`],
//! [`GraphAudioPlaybackService::start_input_recording`] on the engine, the
//! ready handshake, then the 30 ms capture pump feeding a [`LiveMonitor`]
//! (live spectrogram/metrics) and the 50 ms level/device-watch timer, and it
//! registers with the [`RecordingFinalizer`] so an in-flight take is
//! finalized if the app quits mid-record. [`MicRecordingController::stop`]
//! tears the hardware and timers down and reports the path plus the writer's
//! clean/dirty flag; [`MicRecordingController::abort`] additionally discards
//! the file. Every wait in a start is bracketed by disposal/generation guards
//! ([`MicRecordingController::invalidate_pending_start`],
//! [`MicRecordingController::dispose`]) so a start pending behind the
//! permission dialog or file IO can never arm a hot mic after its owning
//! session ended.
//!
//! The controller does NOT own the engine: the `acquire_engine` closure
//! borrows it per take (the technique detail view model creates its private
//! engine lazily there and applies the Settings input device; the song
//! detail view model hands over its playback engine). Engine disposal stays
//! with the owner.
//!
//! The owners' extra needs are representable as they were in Dart:
//!  * count-in / take setup: owner-side work before the start (nothing here
//!    arms hardware until the engine start succeeds), or inside a custom
//!    [`RecordingPathStrategy`]'s create, which runs after the permission
//!    gate;
//!  * per-take destination: a custom [`RecordingPathStrategy`] wrapping the
//!    audio file service's new-recording path and delete;
//!  * meter / live waveform peaks / elapsed clock: with
//!    [`MicRecordingOptions::level_ticks`], every 50 ms level tick hands the
//!    raw engine peak over as [`MicEvent::LevelTick`] (Dart's `onLevelTick`),
//!    while [`MicRecordingController::level`] keeps the plain clamped level
//!    for simple meters;
//!  * device-loss handling: with [`MicRecordingOptions::watch_input_device`]
//!    (Dart's non-null `onInputDeviceLost`) the level tick polls the engine's
//!    input events and reports [`MicEvent::InputDeviceLost`],
//!    [`MicEvent::RecordLimitReached`] and the non-fatal
//!    [`MicEvent::DeviceNotice`]s; an owner with its own device watch leaves
//!    it off — the poll claims only the masked bits, so two watchers can
//!    coexist;
//!  * quit finalization: [`MicRecordingOptions::on_quit_finalize`] is
//!    registered for exactly the armed window, and
//!    [`MicRecordingController::begin_stop_finalize`] extends the same gate
//!    over the owner's post-stop finalize tail, which runs after the stop
//!    unregistered the armed window.
//!
//! # Threading
//!
//! Dart ran the controller on the UI isolate: `start` was an `async` method
//! whose synchronous stretches ran without anything interleaving, and its
//! awaits (the permission dialog, the destination's file IO, the ready
//! handshake's 25 ms polls) were where a stop, an invalidation, a dispose or
//! a timer got in. Its two `Timer.periodic`s fired on the same isolate, and
//! their callbacks called the owner back synchronously. The port keeps those
//! interleavings:
//!
//! - **The state lock is the isolate.** One mutex holds what the Dart object
//!   owned. Every method holds it for its whole body, and a start holds it
//!   for each of its synchronous stretches, releasing it only where Dart
//!   awaited. The controller is `Send + Sync`; its methods may be called
//!   from any thread.
//! - **A start is two halves.** [`MicRecordingController::begin_start`] is
//!   Dart's synchronous prologue: called on the UI thread at the tap, it
//!   flips [`MicRecordingController::is_starting`] at once (so a double tap
//!   in the same frame is guarded) and snapshots the generation. The
//!   [`PendingStart`] it returns is the rest: [`PendingStart::run`] blocks
//!   for the permission prompt, the destination, the engine's arm and up to
//!   [`MicRecordingOptions::ready_timeout`] of ready polls, so the owner runs
//!   it on a blocking worker (`tokio::task::spawn_blocking`) and gets the
//!   [`MicRecordingStart`] back as a message. A pending start dropped unrun
//!   still clears its starting flag (Dart's `finally`).
//! - **The timers are ticks.** The owner drives the 30 ms capture pump and
//!   the 50 ms level/device watch by calling [`MicRecordingController::tick`]
//!   from the UI thread: an iced subscription to
//!   `iced::time::every(MicRecordingController::TICK_PERIOD)` while
//!   [`MicRecordingController::next_tick_due`] is `Some` (or a timer of its
//!   own aimed at that instant). Each call fires every timer that is due,
//!   once, in the order the Dart VM's timer heap fired them; a timer late by
//!   more than its interval skips the ticks it missed, as Dart's periodic
//!   `Timer` did (no catch-up burst). A timer's work runs under the lock;
//!   the events it raised (Dart's callbacks) are handed to the owner's
//!   `on_event` after the lock is released, so a handler may stop the take
//!   on the spot, and a timer still due behind it then no longer fires, as a
//!   timer the callback cancelled did not. The events of one callback are
//!   handed over together once it has run: a [`MicEvent::DeviceNotice`]
//!   reaches the owner after its tick's own engine reads, where Dart called
//!   `onDeviceNotice` before them (only an owner that stopped the take on a
//!   notice could tell; none does).
//! - **Quit finalizers run on the finalizer's threads**:
//!   [`MicRecordingOptions::on_quit_finalize`] is called there, and must be
//!   safe to call from another thread than the owner's.
//!
//! Lock order: the controller's state lock, then the engine's own lock, the
//! monitor's state or the finalizer's state, all leaves; the stop-finalize
//! gate's tail lock, then the finalizer's state. Nothing takes them the other
//! way, and no owner closure (permission, `acquire_engine`, the destination's
//! create and discard, the quit finalize, `on_event`) is ever called with the
//! state lock held.

#[cfg(test)]
mod tests;

use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::io;
use std::ops::{Deref, DerefMut};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::util::dart_num::clamp_f64;

use crate::graph_audio_playback_service::GraphAudioPlaybackService;
use crate::mic_permission;
use crate::monitor_service::{downmix_interleaved_stereo, LiveMonitor};
use crate::recording_finalizer::{Finalizer, RecordingFinalizer};

/// Why a start attempt failed. The owning view model maps these to its own
/// user-facing messages — toasts/notices stay in the view model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MicRecordingFailure {
    /// Microphone permission was denied (the user must enable it in System
    /// Settings › Privacy & Security › Microphone).
    PermissionDenied,

    /// The audio engine has no usable audio device, so nothing can record.
    EngineUnavailable,

    /// The engine could not create the WAV file (disk full / permissions);
    /// nothing would be recorded.
    FileCreateFailed,

    /// The recording armed but the microphone never started delivering frames
    /// within the ready timeout (the native input-stream open is asynchronous
    /// and can fail after `start_input_recording` returns success). The take
    /// was discarded — recording ahead against a silent mic would bake a hole
    /// into the front of the take.
    InputNotDelivering,
}

/// Result of a start attempt. Exactly one shape:
///
///  * started — the WAV is being written at the path;
///  * failed — nothing was armed and any created file was discarded;
///  * invalidated — the start was abandoned mid-wait (the owner invalidated
///    it, or the controller was disposed), or refused at the door because a
///    take was already starting or recording. Nothing was armed, any created
///    file was discarded; callers treat it as a silent no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicRecordingStart {
    Started(String),
    Failed(MicRecordingFailure),
    Invalidated,
}

impl MicRecordingStart {
    /// The WAV path being recorded into, when the recording armed.
    pub fn path(&self) -> Option<&str> {
        match self {
            MicRecordingStart::Started(path) => Some(path),
            _ => None,
        }
    }

    /// Why setup failed, when it did.
    pub fn failure(&self) -> Option<MicRecordingFailure> {
        match self {
            MicRecordingStart::Failed(failure) => Some(*failure),
            _ => None,
        }
    }

    /// True when the recording armed and is now in flight.
    pub fn started(&self) -> bool {
        matches!(self, MicRecordingStart::Started(_))
    }
}

/// What [`MicRecordingController::stop`] hands back: where the take was
/// written and whether the writer finalized it cleanly. `clean == false`
/// means the writer hit an IO error (create/write/finalize) during the take,
/// so the WAV on disk is missing, damaged, or incomplete — the owner decides
/// whether the partial file is worth keeping. `dropped_frames > 0` means the
/// recording ring overflowed (writer thread stalled) and the WAV is missing
/// that many 48 kHz frames as silent holes — the file finalized fine, but it
/// is not gapless, so the owner should tell the user instead of presenting it
/// as clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicRecordingStop {
    pub path: String,
    pub clean: bool,
    pub dropped_frames: u64,
}

/// What a [`RecordingPathStrategy`] fails with: anything the owner's own
/// destination code throws (the temp-dir strategy's [`io::Error`], the song
/// page's take-setup failure). The technique view model tells a file-system
/// failure from the rest by downcasting, as Dart tested
/// `e is FileSystemException`.
pub type DestinationError = Box<dyn StdError + Send + Sync + 'static>;

/// Best-effort delete of the temp dir holding a temp-strategy recording at
/// `path` (see [`RecordingPathStrategy::temp_dir`]: one dir per take, so
/// removing the parent directory removes the take). `None` is a no-op, and
/// so is a bare file name: Dart's `File(path).parent` made that the working
/// directory, which this never deletes (no caller passes one).
pub fn delete_temp_recording_dir(path: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    let Some(dir) = Path::new(path).parent() else {
        return;
    };
    if dir.as_os_str().is_empty() {
        return;
    }
    // Every failure is swallowed (Dart's `catch (_) {}`).
    if dir.is_dir() {
        let _ = fs::remove_dir_all(dir);
    }
}

type CreateFn = dyn Fn() -> Result<String, DestinationError> + Send + Sync;
type DiscardFn = dyn Fn(&str) -> Result<(), DestinationError> + Send + Sync;

/// Where a take's WAV lives and how the file of a take that must not survive
/// (start failed/invalidated, or the owner aborts) is cleaned up.
///
/// Both closures block (Dart's futures) and run on the thread that runs the
/// start or the abort, never under the controller's lock.
#[derive(Clone)]
pub struct RecordingPathStrategy {
    create: Arc<CreateFn>,
    discard: Arc<DiscardFn>,
}

impl fmt::Debug for RecordingPathStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordingPathStrategy")
            .finish_non_exhaustive()
    }
}

impl RecordingPathStrategy {
    /// A strategy over `create` (creates the destination and returns the WAV
    /// path to record into) and `discard` (best-effort cleanup of an
    /// aborted/failed take at the path).
    pub fn new(
        create: impl Fn() -> Result<String, DestinationError> + Send + Sync + 'static,
        discard: impl Fn(&str) -> Result<(), DestinationError> + Send + Sync + 'static,
    ) -> Self {
        RecordingPathStrategy {
            create: Arc::new(create),
            discard: Arc::new(discard),
        }
    }

    /// The ephemeral-take strategy: a fresh `<systemTemp>/<prefix>XXXXXX/` dir
    /// per take, recording into `<prefix>.wav` inside it; discard deletes the
    /// whole dir (never just the file, so nothing strands the dir).
    pub fn temp_dir(prefix: &str) -> Self {
        let prefix = prefix.to_string();
        RecordingPathStrategy::new(
            move || {
                let dir = create_temp_dir(&std::env::temp_dir(), &prefix)?;
                let wav = dir.join(format!("{prefix}.wav"));
                wav.into_os_string().into_string().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the temporary folder's path is not valid UTF-8",
                    )
                    .into()
                })
            },
            |path| {
                delete_temp_recording_dir(Some(path));
                Ok(())
            },
        )
    }

    /// Creates the destination and returns the WAV path to record into.
    pub fn create(&self) -> Result<String, DestinationError> {
        (self.create)()
    }

    /// Best-effort cleanup of an aborted/failed take at `path`.
    pub fn discard(&self, path: &str) -> Result<(), DestinationError> {
        (self.discard)(path)
    }
}

/// `Directory.systemTemp.createTemp(prefix)`: a new folder under `root` with
/// a random six-character suffix (the `mkdtemp` template Dart uses).
fn create_temp_dir(root: &Path, prefix: &str) -> io::Result<PathBuf> {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    for _ in 0..100 {
        let suffix: String = (0..6)
            .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
            .collect();
        let dir = root.join(format!("{prefix}{suffix}"));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// What a level tick reports to the owner (Dart's callbacks), handed to the
/// `on_event` of [`MicRecordingController::tick`] in the order Dart called
/// them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MicEvent {
    /// The engine reported the input device lost, or an input that was
    /// delivering stalled for [`MicRecordingController::STALL_LIMIT`]: the
    /// owner should stop the take gracefully (finalizing KEEPS what was
    /// captured) and surface its own notice. The tick observing the loss
    /// skipped its level update. Only with
    /// [`MicRecordingOptions::watch_input_device`] (Dart's `onInputDeviceLost`).
    InputDeviceLost,

    /// The take reached the longest recording a WAV header can describe: the
    /// owner should stop the take, which KEEPS everything captured up to the
    /// limit. Nothing more is being recorded from that moment, so a notice
    /// alone would leave the clock running over audio no one is storing.
    /// Only with [`MicRecordingOptions::watch_input_device`] (Dart's
    /// `onRecordLimitReached`).
    RecordLimitReached,

    /// A user-facing message: the input degraded without being lost (echo
    /// cancellation unavailable, so playback can bleed into the take; or the
    /// selected microphone fell back to the system default). Only with
    /// [`MicRecordingOptions::watch_input_device`] (Dart's `onDeviceNotice`);
    /// an owner not interested drops it.
    DeviceNotice(&'static str),

    /// Once per level tick: the raw engine input peak (linear 0..1,
    /// unclamped), for owners that derive their own meter scale, live
    /// waveform peaks, or elapsed time. [`MicRecordingController::level`]
    /// was updated first. Only with [`MicRecordingOptions::level_ticks`]
    /// (Dart's `onLevelTick`).
    LevelTick(f64),
}

/// The notice for a microphone opened without echo cancellation.
pub const AEC_BYPASSED_NOTICE: &str = "Echo cancellation is unavailable for this microphone. \
     Use headphones to keep playback out of the recording.";

/// The notice for a selected microphone that fell back to the system default.
pub const INPUT_FELL_BACK_NOTICE: &str = "Your selected microphone is unavailable. \
     Recording from the system default microphone instead.";

/// Borrows the engine to record with (see [`MicRecordingController::new`]).
pub type AcquireEngine = Arc<dyn Fn() -> Arc<GraphAudioPlaybackService> + Send + Sync>;

/// The microphone permission request: blocks until it is answered, true when
/// granted.
pub type RequestPermission = Arc<dyn Fn() -> bool + Send + Sync>;

/// The owner's stop-and-save for a quit mid-record: blocks until settled.
pub type QuitFinalize = Arc<dyn Fn() + Send + Sync>;

/// The optional parts of a [`MicRecordingController`] (Dart's named
/// constructor parameters).
#[derive(Clone)]
pub struct MicRecordingOptions {
    /// When provided, recording also feeds this monitor with the live input
    /// (engine capture + 30 ms pump), for real-time analysis views.
    pub monitor: Option<Arc<dyn LiveMonitor>>,

    /// Registered with the [`RecordingFinalizer`] for exactly the armed
    /// window: runs the owner's stop-and-save path when the app quits
    /// mid-record. Called on a quit-finalizer thread.
    pub on_quit_finalize: Option<QuitFinalize>,

    /// Dart's non-null `onInputDeviceLost`: the level tick polls the
    /// engine's input-lost, echo-cancellation, fallback and record-limit
    /// events and reports them ([`MicEvent::InputDeviceLost`],
    /// [`MicEvent::RecordLimitReached`], [`MicEvent::DeviceNotice`]), and
    /// counts a delivering input that stalls as lost. When false the
    /// controller does not poll device events at all (an owner with its own
    /// device watch keeps polling the input bits itself; the poll claims
    /// only the masked bits, so two watchers no longer steal each other's
    /// events).
    pub watch_input_device: bool,

    /// Dart's non-null `onLevelTick`: every level tick reports
    /// [`MicEvent::LevelTick`].
    pub level_ticks: bool,

    /// Injectable for tests; defaults to the real macOS permission round-trip
    /// ([`mic_permission::request`]).
    pub request_permission: RequestPermission,

    /// The quit-finalize registry an armed take registers with; an owner (and
    /// its tests) can pass a private registry. `None` is the app-wide
    /// [`RecordingFinalizer::instance`].
    pub finalizer: Option<Arc<RecordingFinalizer>>,

    /// How long a start waits for the armed mic to actually deliver frames
    /// before giving up (see [`MicRecordingFailure::InputNotDelivering`]). A
    /// warm stream answers within one callback (~10 ms); a cold
    /// VoiceProcessingIO open takes a few hundred. Injectable so tests can
    /// force the timeout deterministically.
    pub ready_timeout: Duration,
}

impl Default for MicRecordingOptions {
    fn default() -> Self {
        MicRecordingOptions {
            monitor: None,
            on_quit_finalize: None,
            watch_input_device: false,
            level_ticks: false,
            request_permission: Arc::new(mic_permission::request),
            finalizer: None,
            ready_timeout: MicRecordingController::DEFAULT_READY_TIMEOUT,
        }
    }
}

impl fmt::Debug for MicRecordingOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MicRecordingOptions")
            .field("monitor", &self.monitor.is_some())
            .field("on_quit_finalize", &self.on_quit_finalize.is_some())
            .field("watch_input_device", &self.watch_input_device)
            .field("level_ticks", &self.level_ticks)
            .field("ready_timeout", &self.ready_timeout)
            .finish_non_exhaustive()
    }
}

/// The time source the timers are anchored to (`Instant::now`; a test
/// clock in the tests).
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The ready handshake's wait between polls (`thread::sleep`; gated in the
/// tests that must order a parked start's wake against another start).
type PollSleep = Arc<dyn Fn(Duration) + Send + Sync>;

/// The test seams (see [`Clock`] and [`PollSleep`]).
struct Seams {
    clock: Clock,
    poll_sleep: PollSleep,
}

impl Default for Seams {
    fn default() -> Self {
        Seams {
            clock: Arc::new(Instant::now),
            poll_sleep: Arc::new(thread::sleep),
        }
    }
}

/// Live-preview cadence: pull captured frames into the monitor every 30 ms
/// so the live spectrogram/metrics stay smooth.
const CAPTURE_INTERVAL: Duration = Duration::from_millis(30);

/// The ready handshake's poll interval.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The device events the level tick claims when it watches the input.
const WATCHED_EVENTS: u32 = GraphAudioPlaybackService::EVENT_INPUT_LOST
    | GraphAudioPlaybackService::EVENT_AEC_BYPASSED
    | GraphAudioPlaybackService::EVENT_INPUT_FELL_BACK
    | GraphAudioPlaybackService::EVENT_RECORD_LIMIT;

/// The shared mic-recording core (see the module doc).
///
/// Dropping the controller disposes it (see
/// [`MicRecordingController::dispose`]).
pub struct MicRecordingController {
    inner: Arc<Inner>,
}

impl fmt::Debug for MicRecordingController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MicRecordingController")
            .field("is_recording", &self.is_recording())
            .field("is_starting", &self.is_starting())
            .finish_non_exhaustive()
    }
}

/// What the controller and its pending starts share.
struct Inner {
    /// Borrows the engine to record with; called once per start, after the
    /// permission gate (so a denied/abandoned start never touches — or lazily
    /// creates — an engine). The owner keeps engine ownership and disposal.
    acquire_engine: AcquireEngine,
    monitor: Option<Arc<dyn LiveMonitor>>,
    watch_input_device: bool,
    level_ticks: bool,
    request_permission: RequestPermission,
    finalizer: Arc<RecordingFinalizer>,
    /// Registered with the finalizer while a take is armed: calls the
    /// owner's quit finalize. One allocation for the controller's life (Dart's
    /// method tear-off), so register and unregister match.
    finalize_on_quit: Finalizer,
    /// The post-stop finalize tails held against quit (see
    /// [`MicRecordingController::begin_stop_finalize`]).
    stop_finalizes: StopFinalizeGate,
    ready_timeout: Duration,
    clock: Clock,
    poll_sleep: PollSleep,
    state: Mutex<State>,
    /// See [`MicRecordingController::revision`].
    revision: AtomicU64,
}

/// What the Dart object owned (see the module doc).
struct State {
    /// Live input level (0..1, clamped) while recording, for a meter; reset
    /// to 0 on stop (Dart's `level` notifier).
    level: f64,

    /// The engine of the current (or latest) take, kept only so teardown can
    /// close a racing arm's native WAV handle. Never created or disposed here.
    engine: Option<Arc<GraphAudioPlaybackService>>,

    /// The armed take (Dart's `_isRecording`, `_path`, `_destination` and its
    /// two timers): `Some` while a take is armed (WAV being written, pumps
    /// running).
    armed: Option<Armed>,

    starting: bool,
    feeding_monitor: bool,
    disposed: bool,

    /// Bumped every time a NATIVE recording arms (start_input_recording
    /// returned true). A parked start's failure/invalidation cleanup may only
    /// call engine.stop_input_recording() while this still equals the value
    /// captured at ITS OWN arm — otherwise A's cleanup, resuming after A was
    /// invalidated and B armed, would stop B's native recording (the
    /// pending-start ABA).
    native_arm_token: u64,

    /// Bumped whenever a pending start must be abandoned; each start
    /// snapshots it before its waits and arms nothing on a mismatch.
    generation: u64,

    /// Orders timers due at the same instant (see [`PeriodicTimer::seq`]).
    timer_seq: u64,
}

impl State {
    fn observed(&self) -> Observed {
        Observed {
            recording: self.armed.is_some(),
            starting: self.starting,
            feeding_monitor: self.feeding_monitor,
            level: self.level.to_bits(),
        }
    }

    /// Dart's `level.value = v`: a `ValueNotifier` notified only on a change
    /// (the revision bump follows from [`Locked`]'s comparison).
    fn set_level(&mut self, level: f64) {
        self.level = level;
    }

    fn next_timer_seq(&mut self) -> u64 {
        self.timer_seq += 1;
        self.timer_seq
    }
}

/// The armed take.
struct Armed {
    path: String,
    destination: RecordingPathStrategy,
    /// The take's timers and the locals their Dart closures captured; `None`
    /// once cancelled, and while the arm is still setting them up.
    timers: Option<TakeTimers>,
}

/// The armed take's two periodic timers and their closures' state.
struct TakeTimers {
    /// The engine the take armed on (the closures' captured `engine`).
    engine: Arc<GraphAudioPlaybackService>,
    /// The 30 ms capture pump; only when feeding a monitor.
    capture: Option<PeriodicTimer>,
    /// The 50 ms level/device-watch timer.
    level: PeriodicTimer,
    last_frames: u64,
    delivered: bool,
    stalled_ticks: u64,
}

/// A Dart `Timer.periodic`, fired by [`MicRecordingController::tick`].
#[derive(Clone, Copy)]
struct PeriodicTimer {
    /// When it fires next.
    next: Instant,
    interval: Duration,
    /// The Dart VM's timer id, renewed every time the timer is re-armed:
    /// timers due at the same instant fire in id order, so the one re-armed
    /// longest ago goes first.
    seq: u64,
}

impl PeriodicTimer {
    fn new(now: Instant, interval: Duration, seq: u64) -> Self {
        PeriodicTimer {
            next: now + interval,
            interval,
            seq,
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.next <= now
    }

    /// The Dart VM, before running an overdue periodic callback: overdue by
    /// more than one interval, the ticks it missed are skipped (no catch-up
    /// burst).
    fn skip_missed(&mut self, now: Instant) {
        let overdue = now.saturating_duration_since(self.next);
        if overdue > self.interval {
            let missed = overdue.as_nanos() / self.interval.as_nanos().max(1);
            let skip = self.interval.as_nanos().saturating_mul(missed);
            let skip = Duration::from_nanos(u64::try_from(skip).unwrap_or(u64::MAX));
            self.next = self.next.checked_add(skip).unwrap_or(now);
        }
    }

    /// The Dart VM, after the callback: the next wakeup an interval on, with
    /// a fresh id.
    fn rearm(&mut self, seq: u64) {
        self.next = self.next.checked_add(self.interval).unwrap_or(self.next);
        self.seq = seq;
    }
}

/// What the owner observes; a change bumps the revision.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Observed {
    recording: bool,
    starting: bool,
    feeding_monitor: bool,
    level: u64,
}

/// The state, locked; bumps the revision on release when what the owner
/// observes changed in between.
struct Locked<'a> {
    revision: &'a AtomicU64,
    before: Observed,
    st: MutexGuard<'a, State>,
}

impl Deref for Locked<'_> {
    type Target = State;

    fn deref(&self) -> &State {
        &self.st
    }
}

impl DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.st
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        if self.st.observed() != self.before {
            self.revision.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Inner {
    fn locked(&self) -> Locked<'_> {
        // A panic under the lock (a failing native call or monitor) is
        // unwound by the start that hit it; the state is still consistent.
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        Locked {
            revision: &self.revision,
            before: st.observed(),
            st,
        }
    }

    /// Dart's `stop()` body, under the lock.
    fn stop_locked(&self, st: &mut State) -> Option<MicRecordingStop> {
        let armed = st.armed.as_mut()?;
        armed.timers = None;
        st.set_level(0.0);
        let engine = st.engine.clone();
        let clean = engine.as_ref().is_none_or(|e| e.stop_input_recording());
        // Read AFTER stop: the counter is stable once the recording is
        // disarmed, and any overflow during the final drain is included.
        let dropped_frames = engine.as_ref().map_or(0, |e| e.record_dropped_frames());
        if st.feeding_monitor {
            if let Some(e) = &engine {
                e.stop_capture();
            }
            if let Some(m) = &self.monitor {
                m.stop();
            }
            st.feeding_monitor = false;
        }
        let armed = st.armed.take()?;
        self.finalizer.unregister(&self.finalize_on_quit);
        Some(MicRecordingStop {
            path: armed.path,
            clean,
            dropped_frames,
        })
    }

    /// The capture pump's callback (Dart's 30 ms timer).
    fn capture_tick(&self, timers: &TakeTimers) {
        let frames = timers.engine.pull_input_frames(); // interleaved stereo
        if frames.is_empty() {
            return;
        }
        if let Some(monitor) = &self.monitor {
            monitor.feed_samples(&downmix_interleaved_stereo(&frames));
        }
    }

    /// The level/device-watch callback (Dart's 50 ms timer).
    fn level_tick(&self, st: &mut State, timers: &mut TakeTimers, events: &mut Vec<MicEvent>) {
        let engine = Arc::clone(&timers.engine);
        // Input loss must stop the take gracefully — finalizing KEEPS what
        // was captured — instead of letting capture die silently. Poll only
        // when the owner handles it here (see watch_input_device). The poll
        // claims ONLY its own bits, and it also surfaces the two degradations
        // this recording cares about: no echo cancellation (playback bleeds
        // into the take) and a selected-mic fallback (the take records from
        // the system default, not the shown mic) — both were silently
        // discarded by the old clear-everything poll.
        if self.watch_input_device {
            let polled = engine.poll_device_events(WATCHED_EVENTS);
            if polled & GraphAudioPlaybackService::EVENT_AEC_BYPASSED != 0 {
                events.push(MicEvent::DeviceNotice(AEC_BYPASSED_NOTICE));
            }
            if polled & GraphAudioPlaybackService::EVENT_INPUT_FELL_BACK != 0 {
                events.push(MicEvent::DeviceNotice(INPUT_FELL_BACK_NOTICE));
            }
            if polled & GraphAudioPlaybackService::EVENT_RECORD_LIMIT != 0 {
                events.push(MicEvent::RecordLimitReached);
                return;
            }
            if polled & GraphAudioPlaybackService::EVENT_INPUT_LOST != 0 {
                events.push(MicEvent::InputDeviceLost);
                return;
            }
            // VPIO raises no loss event when its unit just stops calling back
            // (a headset carrying both legs dropping out): the take sat
            // "recording", its meter frozen, with nothing said. An input that
            // delivered and then stalls for STALL_LIMIT is lost too, as the
            // song page's own device watch judges it (not one still warming
            // up: a Bluetooth profile switch delivers nothing for a while).
            let frames = engine.input_frames_received();
            if frames != timers.last_frames {
                timers.last_frames = frames;
                timers.delivered = true;
                timers.stalled_ticks = 0;
            } else if timers.delivered && {
                timers.stalled_ticks += 1;
                timers.stalled_ticks * MicRecordingController::LEVEL_INTERVAL_MS
                    >= MicRecordingController::STALL_LIMIT.as_millis() as u64
            } {
                timers.stalled_ticks = 0;
                events.push(MicEvent::InputDeviceLost);
                return;
            }
        }
        let peak = engine.input_level();
        st.set_level(clamp_f64(peak, 0.0, 1.0));
        if self.level_ticks {
            events.push(MicEvent::LevelTick(peak));
        }
    }

    /// Fires the earliest timer due at `now` that has not fired in this tick
    /// yet; its events go to `events`. False when none is due.
    fn fire_one(&self, now: Instant, fired: &mut Fired, events: &mut Vec<MicEvent>) -> bool {
        let mut st = self.locked();
        // Belt and braces: the timers are cancelled on dispose.
        if st.disposed {
            return false;
        }
        let Some(mut timers) = st.armed.as_mut().and_then(|a| a.timers.take()) else {
            return false;
        };
        let capture_due = !fired.capture && timers.capture.is_some_and(|t| t.due(now));
        let level_due = !fired.level && timers.level.due(now);
        let capture_first = match (capture_due, level_due, timers.capture) {
            (true, true, Some(c)) => (c.next, c.seq) <= (timers.level.next, timers.level.seq),
            (true, _, _) => true,
            _ => false,
        };
        let did_fire = capture_due || level_due;
        if capture_first {
            fired.capture = true;
            if let Some(t) = timers.capture.as_mut() {
                t.skip_missed(now);
            }
            self.capture_tick(&timers);
            let seq = st.next_timer_seq();
            if let Some(t) = timers.capture.as_mut() {
                t.rearm(seq);
            }
        } else if level_due {
            fired.level = true;
            timers.level.skip_missed(now);
            self.level_tick(&mut st, &mut timers, events);
            let seq = st.next_timer_seq();
            timers.level.rearm(seq);
        }
        if let Some(armed) = st.armed.as_mut() {
            armed.timers = Some(timers);
        }
        did_fire
    }
}

/// The timers that fired in one [`MicRecordingController::tick`].
#[derive(Default)]
struct Fired {
    capture: bool,
    level: bool,
}

/// Disarms the native recording `token` armed, only while no newer start has
/// armed since (see [`State::native_arm_token`]).
fn disarm_if_still_ours(st: &State, engine: &GraphAudioPlaybackService, token: u64) {
    if st.native_arm_token == token {
        engine.stop_input_recording();
    }
}

/// Discard of an aborted start's file that never fails the start. A discard
/// strategy can fail (the audio file service's delete: permission/IO errors)
/// — log the failure instead of letting it escape (Dart's unawaited
/// discard's `catchError`).
fn discard_quietly(destination: &RecordingPathStrategy, path: &str) {
    // No path in the message — the diagnostics log never records user media
    // locations (the log's own scrubber would strip it anyway).
    match catch_unwind(AssertUnwindSafe(|| destination.discard(path))) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => DiagnosticsLog::instance().log("mic", &format!("discard failed: {e}")),
        Err(_) => DiagnosticsLog::instance().log("mic", "discard failed: a panic"),
    }
}

impl MicRecordingController {
    /// Meter/device-watch cadence (also the elapsed clock the song detail
    /// view model derives its live waveform peaks from — one
    /// [`MicEvent::LevelTick`] per tick). Public so an owner deriving
    /// durations from tick counts can alias it instead of hard-coding a
    /// second 50 that could drift.
    pub const LEVEL_INTERVAL_MS: u64 = 50;

    /// [`MicRecordingController::LEVEL_INTERVAL_MS`] as a duration.
    pub const LEVEL_INTERVAL: Duration = Duration::from_millis(Self::LEVEL_INTERVAL_MS);

    /// How long an input that was delivering may stall before the take counts
    /// it lost (see [`MicEvent::InputDeviceLost`]).
    pub const STALL_LIMIT: Duration = Duration::from_secs(2);

    /// The default [`MicRecordingOptions::ready_timeout`].
    pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(2);

    /// How often the owner should call [`MicRecordingController::tick`]
    /// while [`MicRecordingController::next_tick_due`] is `Some`: the
    /// greatest common divisor of the two timers' intervals, so every tick
    /// lands within 10 ms of when Dart's timer fired.
    pub const TICK_PERIOD: Duration = Duration::from_millis(10);

    /// A controller recording through the engine `acquire_engine` lends it
    /// per take (called on the thread that runs the start, after the
    /// permission gate; it must not call back into the controller).
    pub fn new(
        acquire_engine: impl Fn() -> Arc<GraphAudioPlaybackService> + Send + Sync + 'static,
        options: MicRecordingOptions,
    ) -> Self {
        Self::build(Arc::new(acquire_engine), options, Seams::default())
    }

    fn build(acquire_engine: AcquireEngine, options: MicRecordingOptions, seams: Seams) -> Self {
        let finalizer = options
            .finalizer
            .unwrap_or_else(RecordingFinalizer::instance);
        let on_quit = options.on_quit_finalize;
        let finalize_on_quit = Finalizer::new(move || {
            if let Some(finalize) = &on_quit {
                finalize();
            }
        });
        MicRecordingController {
            inner: Arc::new(Inner {
                acquire_engine,
                monitor: options.monitor,
                watch_input_device: options.watch_input_device,
                level_ticks: options.level_ticks,
                request_permission: options.request_permission,
                stop_finalizes: StopFinalizeGate::new(Arc::clone(&finalizer)),
                finalizer,
                finalize_on_quit,
                ready_timeout: options.ready_timeout,
                clock: seams.clock,
                poll_sleep: seams.poll_sleep,
                state: Mutex::new(State {
                    level: 0.0,
                    engine: None,
                    armed: None,
                    starting: false,
                    feeding_monitor: false,
                    disposed: false,
                    native_arm_token: 0,
                    generation: 0,
                    timer_seq: 0,
                }),
                revision: AtomicU64::new(0),
            }),
        }
    }

    /// Live input level (0..1, clamped) while recording, for a meter; 0 when
    /// stopped.
    pub fn level(&self) -> f64 {
        self.inner.locked().level
    }

    /// Bumped whenever what the owner observes changes: the level (each time
    /// Dart's `level` notifier notified), [`MicRecordingController::is_recording`],
    /// [`MicRecordingController::is_starting`] or
    /// [`MicRecordingController::feeding_monitor`]. A view caching its meter
    /// redraws when it moves.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::SeqCst)
    }

    /// True while a take is armed (WAV being written, pumps running).
    pub fn is_recording(&self) -> bool {
        self.inner.locked().armed.is_some()
    }

    /// True during a start's pending window — set when
    /// [`MicRecordingController::begin_start`] is called (so a fast
    /// double-tap can be guarded before the permission/IO waits resolve) and
    /// cleared when it arms, fails, or is invalidated.
    pub fn is_starting(&self) -> bool {
        self.inner.locked().starting
    }

    /// True while the live input is being fed into the monitor (i.e.
    /// recording, with a monitor attached) — drives "live view" UI states.
    pub fn feeding_monitor(&self) -> bool {
        self.inner.locked().feeding_monitor
    }

    /// Abandons any start still pending behind its permission / destination
    /// waits: bumps the generation the pending start snapshotted (it will arm
    /// nothing and discard its file when it resumes) and clears
    /// [`MicRecordingController::is_starting`] immediately, so the owner's UI
    /// guards unblock without waiting for the abandoned waits (which can pend
    /// on the system permission dialog).
    pub fn invalidate_pending_start(&self) {
        let mut st = self.inner.locked();
        st.generation = st.generation.wrapping_add(1);
        st.starting = false;
    }

    /// The synchronous half of a start (Dart's `start` up to its first
    /// await): call it on the UI thread at the tap, then run the returned
    /// [`PendingStart`] on a blocking worker. A start while one is starting
    /// or recording (the owner's guards normally prevent it), or after
    /// dispose, is refused at the door: its pending start resolves to
    /// [`MicRecordingStart::Invalidated`] and touches nothing.
    pub fn begin_start(&self, destination: RecordingPathStrategy) -> PendingStart {
        let mut st = self.inner.locked();
        // Re-entrancy: one take at a time. The owner's can-record guards
        // normally prevent this; a start slipping through anyway is a silent
        // no-op.
        if st.disposed || st.armed.is_some() || st.starting {
            return PendingStart(PendingKind::Settled(MicRecordingStart::Invalidated));
        }
        st.starting = true;
        // Snapshot the generation: an invalidate/dispose during the permission
        // or destination waits abandons this start — arming the mic then would
        // leave it hot with no stop affordance in the owner's UI.
        let generation = st.generation;
        PendingStart(PendingKind::Controller(ControllerStart {
            inner: Arc::clone(&self.inner),
            destination,
            generation,
            settled: false,
        }))
    }

    /// Requests mic permission, creates the WAV via `destination`, starts the
    /// engine's input recording, then arms the capture/level pumps and the
    /// quit finalizer: [`MicRecordingController::begin_start`] and
    /// [`PendingStart::run`] in one blocking call. Returns started / failed /
    /// invalidated (see [`MicRecordingStart`]); on anything but started, no
    /// hardware or timers are left armed and any created file is discarded.
    ///
    /// An `Err` is the destination's own failure (Dart's start threw it); a
    /// panic in a native call, the monitor or the destination is unwound the
    /// same way (whatever the start had set up is undone, its file
    /// discarded) and then resumed.
    pub fn start(
        &self,
        destination: RecordingPathStrategy,
    ) -> Result<MicRecordingStart, DestinationError> {
        self.begin_start(destination).run()
    }

    /// Runs every timer that is due (see the module doc): the 30 ms capture
    /// pump and the 50 ms level/device watch, each at most once a call, in
    /// Dart's order, handing each timer's events to `on_event` once the lock
    /// is released. A no-op when no take is armed.
    pub fn tick(&self, on_event: &mut dyn FnMut(MicEvent)) {
        let now = (self.inner.clock)();
        let mut fired = Fired::default();
        loop {
            let mut events = Vec::new();
            if !self.inner.fire_one(now, &mut fired, &mut events) {
                return;
            }
            for event in events {
                on_event(event);
            }
        }
    }

    /// When the next timer is due, while a take is armed; `None` when there
    /// is nothing to tick.
    pub fn next_tick_due(&self) -> Option<Instant> {
        let st = self.inner.locked();
        let timers = st.armed.as_ref()?.timers.as_ref()?;
        Some(
            timers
                .capture
                .map_or(timers.level.next, |c| c.next.min(timers.level.next)),
        )
    }

    /// Stops the mic hardware, live capture/monitor feed and meter (without
    /// touching the file). Returns the take's path and whether the writer
    /// finalized it cleanly; `None` when no take is in flight.
    pub fn stop(&self) -> Option<MicRecordingStop> {
        let mut st = self.inner.locked();
        self.inner.stop_locked(&mut st)
    }

    /// Stops AND discards an in-flight take (its file is deleted via the path
    /// strategy) — for owners tearing down the session the take belonged to
    /// (e.g. cancelling the edit an example was being recorded for). No-op
    /// when not recording. Blocks for the discard; its failure is returned.
    pub fn abort(&self) -> Result<(), DestinationError> {
        let (destination, stopped) = {
            let mut st = self.inner.locked();
            let Some(armed) = st.armed.as_ref() else {
                return Ok(());
            };
            let destination = armed.destination.clone();
            (destination, self.inner.stop_locked(&mut st))
        };
        if let Some(stopped) = stopped {
            destination.discard(&stopped.path)?;
        }
        Ok(())
    }

    /// Holds the quit gate over the owner's post-stop finalize tail
    /// (waveform analysis, trimming, the save that commits the take row)
    /// until the returned guard is dropped. [`MicRecordingController::stop`]
    /// unregisters the armed-window finalizer synchronously, before any of
    /// that work has run; without this bracket a quit in the stop-to-save
    /// window exits mid-save and the take is left for boot recovery as a bare
    /// WAV — its timeline placement, punch-in target and latency alignment
    /// live only in the uncommitted save. Call it right after the stop, on
    /// the same thread, before handing the tail to a worker, so nothing
    /// separates the armed window's registration from this one; move the
    /// guard along with the work. A quit while the tail is in flight waits
    /// for it; nothing is re-executed.
    pub fn begin_stop_finalize(&self) -> StopFinalizeHold {
        self.inner.stop_finalizes.begin()
    }

    /// Runs `work` — the owner's post-stop finalize tail — while holding the
    /// quit gate (see [`MicRecordingController::begin_stop_finalize`]), and
    /// returns its outcome (a panic too).
    pub fn guard_stop_finalize<T>(&self, work: impl FnOnce() -> T) -> T {
        self.inner.stop_finalizes.guard(work)
    }

    /// Tears everything down: invalidates a pending start, cancels the
    /// pumps, and closes a racing arm's native WAV handle. The take FILE is
    /// not discarded — the owner keeps its path (from the start) and applies
    /// its own cleanup policy (the technique view model deletes its temp
    /// dirs; the song view model keeps the take for the boot sweep).
    /// Idempotent; dropping the controller does the same.
    pub fn dispose(&self) {
        let inner = &*self.inner;
        let mut st = inner.locked();
        if st.disposed {
            return;
        }
        st.disposed = true;
        // Stop a recording that is running OR still starting (its start waits
        // haven't resolved). The generation/disposed guards in the start keep
        // it from arming after this, so stopping here is belt-and-suspenders
        // against a hot mic.
        let active = st.armed.is_some() || st.starting;
        st.generation = st.generation.wrapping_add(1);
        st.starting = false;
        inner.finalizer.unregister(&inner.finalize_on_quit);
        if let Some(armed) = st.armed.as_mut() {
            armed.timers = None;
        }
        if active {
            if let Some(engine) = st.engine.clone() {
                engine.stop_input_recording();
                if st.feeding_monitor {
                    engine.stop_capture();
                }
            }
        }
        // Deliberately no monitor stop: the owner is mid-dispose and about to
        // dispose the monitor itself.
        st.armed = None;
        st.feeding_monitor = false;
    }
}

impl Drop for MicRecordingController {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// The asynchronous half of a start (see
/// [`MicRecordingController::begin_start`]); a fake recorder makes its own
/// with [`PendingStart::from_fn`].
pub struct PendingStart(PendingKind);

enum PendingKind {
    /// Refused at the door: nothing to run.
    Settled(MicRecordingStart),
    Controller(ControllerStart),
    Fn(Box<dyn FnOnce() -> Result<MicRecordingStart, DestinationError> + Send>),
}

impl fmt::Debug for PendingStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingStart").finish_non_exhaustive()
    }
}

impl PendingStart {
    /// A pending start that runs `start` (a fake recorder's).
    pub fn from_fn(
        start: impl FnOnce() -> Result<MicRecordingStart, DestinationError> + Send + 'static,
    ) -> Self {
        PendingStart(PendingKind::Fn(Box::new(start)))
    }

    /// Runs the start to its outcome (see [`MicRecordingController::start`]).
    /// Blocks on the permission prompt, the destination and the ready
    /// handshake: run it on a blocking worker.
    pub fn run(self) -> Result<MicRecordingStart, DestinationError> {
        match self.0 {
            PendingKind::Settled(outcome) => Ok(outcome),
            PendingKind::Controller(start) => start.run(),
            PendingKind::Fn(start) => start(),
        }
    }
}

/// A controller start admitted at the door. Dropped unrun, it settles as
/// Dart's `finally` would have.
struct ControllerStart {
    inner: Arc<Inner>,
    destination: RecordingPathStrategy,
    generation: u64,
    settled: bool,
}

/// What a start has set up, for [`ControllerStart::unwind`], and the file it
/// leaves to discard.
#[derive(Default)]
struct StartCx {
    created_path: Option<String>,
    /// The engine and token of this start's native arm (Dart's `disarm`).
    disarm: Option<(Arc<GraphAudioPlaybackService>, u64)>,
    discard: Option<String>,
}

impl ControllerStart {
    fn run(mut self) -> Result<MicRecordingStart, DestinationError> {
        let mut cx = StartCx::default();
        let outcome = catch_unwind(AssertUnwindSafe(|| self.body(&mut cx)));
        let outcome = match outcome {
            Ok(Ok(start)) => Ok(Ok(start)),
            Ok(Err(e)) => {
                self.unwind(&mut cx);
                Ok(Err(e))
            }
            Err(panic) => {
                self.unwind(&mut cx);
                Err(panic)
            }
        };
        self.settle();
        // The discard runs once the start has settled, as Dart's unawaited
        // discard ran on after the start returned.
        if let Some(path) = cx.discard.take() {
            discard_quietly(&self.destination, &path);
        }
        match outcome {
            Ok(result) => result,
            Err(panic) => resume_unwind(panic),
        }
    }

    /// Dart's `finally`: only this start's own outcome clears the flag —
    /// after an invalidate a NEWER start may already be pending and own it.
    fn settle(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        let mut st = self.inner.locked();
        if st.generation == self.generation {
            st.starting = false;
        }
    }

    fn abandoned(st: &State, generation: u64) -> bool {
        st.disposed || st.generation != generation
    }

    fn body(&self, cx: &mut StartCx) -> Result<MicRecordingStart, DestinationError> {
        let inner = &*self.inner;
        let generation = self.generation;
        if !(inner.request_permission)() {
            return Ok(MicRecordingStart::Failed(
                MicRecordingFailure::PermissionDenied,
            ));
        }
        // The owner may have been disposed during the permission wait (it
        // could not tear down a not-yet-started recording), or invalidated
        // this start (its edit session was cancelled/saved). Bail before
        // touching — or lazily creating — the engine.
        if Self::abandoned(&inner.locked(), generation) {
            return Ok(MicRecordingStart::Invalidated);
        }
        // Called without the lock (it may create the engine, which takes a
        // while), so the guard is taken again after it: an invalidate that
        // landed meanwhile abandons the start before anything is created.
        let engine = (inner.acquire_engine)();
        {
            let mut st = inner.locked();
            if Self::abandoned(&st, generation) {
                return Ok(MicRecordingStart::Invalidated);
            }
            st.engine = Some(Arc::clone(&engine));
        }
        if !engine.is_available() {
            return Ok(MicRecordingStart::Failed(
                MicRecordingFailure::EngineUnavailable,
            ));
        }
        let path = self.destination.create()?;
        cx.created_path = Some(path.clone());
        let mut st = inner.locked();
        // Same guard after the destination wait: a dispose/invalidate in this
        // window can't stop a recording that hasn't started. Don't arm
        // hardware/timers; drop the just-created destination.
        if Self::abandoned(&st, generation) {
            cx.discard = Some(path);
            return Ok(MicRecordingStart::Invalidated);
        }
        // A false return means the writer could not create the WAV (disk full
        // / permissions) and nothing would be recorded: fail loudly instead of
        // "recording" into nowhere, and arm no timers or state.
        if !engine.start_input_recording(&path) {
            cx.discard = Some(path);
            return Ok(MicRecordingStart::Failed(
                MicRecordingFailure::FileCreateFailed,
            ));
        }
        st.native_arm_token = st.native_arm_token.wrapping_add(1);
        let arm_token = st.native_arm_token;
        // Only THIS start's cleanup paths may disarm the native recording,
        // and only while no newer start has armed since (see
        // native_arm_token).
        cx.disarm = Some((Arc::clone(&engine), arm_token));

        // Ready handshake. start_input_recording == true proves only that the
        // WAV exists — the native input-stream open is deliberately
        // asynchronous (a blocking open froze the UI), so the mic may still be
        // opening, or may fail to open, right now. Wait (bounded) for BOTH
        // halves of the proof:
        //  * recorded progress — record_written_frames counts only frames that
        //    actually LANDED in this take's recording ring (reset at the arm);
        //    a nonzero value is unforgeable (pre-arm and dying-stream
        //    callbacks read the disarmed flag and never write this ring);
        //  * ordering + same-instance anchoring — the applied pair
        //    (seq << 24 | generation) is published only on a SUCCESSFUL
        //    convergence, its seq must be >= this arm's intent sequence, and
        //    its generation must equal the CURRENT state word's generation
        //    with the running bit set. That anchors all three facts to ONE
        //    stream instance: a warm stream's frames plus a later failed
        //    reopen's watermark (or any other splice of evidence from two
        //    generations) can no longer assemble a false ready — the reviewed
        //    splice was exactly "old stream wrote frames, teardown ran, reopen
        //    failed, applied advanced anyway".
        // When the stream is already open (monitoring), everything resolves
        // within one callback (~10 ms); a cold VoiceProcessingIO open takes a
        // few hundred.
        let arm_seq = engine.input_intent_seq();
        let mut ready = false;
        // DELTA progress under a STABLE pairing: recorded frames are the
        // file's truth but carry no generation — an outgoing stream's frames
        // (real audio, genuinely in the take) could otherwise combine with a
        // freshly converged stream that has not delivered yet. So ready
        // demands growth: once the applied/state pairing holds, remember
        // (pairing, written) and require written to INCREASE while the SAME
        // pairing still holds — only the currently-running stream can produce
        // that increase. A pairing change resets the baseline; a stream that
        // converged but never delivers stays at its baseline into the
        // timeout.
        let mut baseline_pairing: Option<u64> = None;
        let mut baseline_written = 0u64;
        let ready_clock = Instant::now();
        while ready_clock.elapsed() < inner.ready_timeout {
            if Self::abandoned(&st, generation) {
                disarm_if_still_ours(&st, &engine, arm_token);
                cx.discard = Some(path);
                return Ok(MicRecordingStart::Invalidated);
            }
            let applied = engine.input_applied_state();
            let stream = engine.input_stream_state();
            let pairing_ok = GraphAudioPlaybackService::applied_seq_of(applied) >= arm_seq
                && GraphAudioPlaybackService::input_state_running(stream)
                && GraphAudioPlaybackService::applied_gen_of(applied)
                    == GraphAudioPlaybackService::state_gen_of(stream);
            if pairing_ok {
                if baseline_pairing != Some(applied) {
                    baseline_pairing = Some(applied);
                    baseline_written = engine.record_written_frames();
                }
                if engine.record_written_frames() > baseline_written
                    && engine.input_applied_state() == applied
                    && engine.input_stream_state() == stream
                {
                    // The growth is BRACKETED: the pairing was re-read after
                    // the progress observation and matched the opening
                    // snapshot bit for bit, so no teardown/restart
                    // interleaved — a SetInput could otherwise clear running,
                    // let the outgoing stream's in-flight callback grow the
                    // counter, and fail its reopen while this poll was still
                    // holding the stale snapshot (the state → progress →
                    // state discipline the native side documents).
                    ready = true;
                    break;
                }
            } else {
                baseline_pairing = None;
            }
            drop(st);
            (inner.poll_sleep)(READY_POLL_INTERVAL);
            st = inner.locked();
        }
        if !ready {
            disarm_if_still_ours(&st, &engine, arm_token);
            cx.discard = Some(path);
            return Ok(MicRecordingStart::Failed(
                MicRecordingFailure::InputNotDelivering,
            ));
        }
        st.armed = Some(Armed {
            path: path.clone(),
            destination: self.destination.clone(),
            timers: None,
        });
        let now = (inner.clock)();
        let capture = match &inner.monitor {
            Some(monitor) => {
                // Live preview: the engine captures the same input it's
                // recording, so the live spectrogram / tuner / metrics run off
                // it. Set first, so a start that throws part way is stopped
                // whole.
                st.feeding_monitor = true;
                engine.start_capture();
                monitor.start();
                let seq = st.next_timer_seq();
                Some(PeriodicTimer::new(now, CAPTURE_INTERVAL, seq))
            }
            None => None,
        };
        let last_frames = engine.input_frames_received();
        let seq = st.next_timer_seq();
        let level = PeriodicTimer::new(now, MicRecordingController::LEVEL_INTERVAL, seq);
        if let Some(armed) = st.armed.as_mut() {
            armed.timers = Some(TakeTimers {
                engine: Arc::clone(&engine),
                capture,
                level,
                last_frames,
                delivered: false,
                stalled_ticks: 0,
            });
        }
        // Finalize this take if the app quits mid-record; unregistered on
        // stop.
        inner.finalizer.register(&inner.finalize_on_quit, true);
        Ok(MicRecordingStart::Started(path))
    }

    /// Undoes what a start that THREW (its destination, a native call, the
    /// monitor) had set up past its destination: the owner hears the
    /// failure, not that a take armed, so nothing else would ever stop the
    /// mic or delete the file.
    fn unwind(&self, cx: &mut StartCx) {
        let undone = catch_unwind(AssertUnwindSafe(|| {
            let inner = &*self.inner;
            let mut st = inner.locked();
            if st.armed.is_some() {
                // Armed and part set up: undone as a stop undoes a take.
                // (As in Dart, this reads the controller's armed take, not
                // this start's: a start invalidated while its destination was
                // being created, which then failed after a newer start armed,
                // stops that newer take here.)
                inner.stop_locked(&mut st);
            } else if let Some((engine, token)) = &cx.disarm {
                disarm_if_still_ours(&st, engine, *token);
            }
        }));
        if undone.is_err() {
            DiagnosticsLog::instance().log("mic", "unwinding a failed start threw: a panic");
        }
        if let Some(path) = cx.created_path.take() {
            cx.discard = Some(path);
        }
    }
}

impl Drop for ControllerStart {
    fn drop(&mut self) {
        self.settle();
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

/// The post-stop finalize tails currently held against quit (Dart's
/// `_stopFinalizes` and `_holdQuitForFinalize`). A set, because a parked tail
/// can still be draining when the next take's stop starts its own (the same
/// overlap the take generation guards exist for), and the earlier tail's
/// completion must not drop the quit hold protecting the later one.
///
/// The controller keeps one; a fake recorder can keep its own to hold the
/// quit exactly as the real one does.
pub struct StopFinalizeGate {
    finalizer: Arc<RecordingFinalizer>,
    /// Registered with the finalizer while a tail is in flight: waits for
    /// the tails in flight when the quit calls it. One allocation, so
    /// register and unregister match.
    hold: Finalizer,
    tails: Arc<Mutex<Tails>>,
}

#[derive(Default)]
struct Tails {
    open: Vec<(u64, Arc<Latch>)>,
    next: u64,
}

impl fmt::Debug for StopFinalizeGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopFinalizeGate").finish_non_exhaustive()
    }
}

impl StopFinalizeGate {
    /// A gate holding quits on `finalizer`.
    pub fn new(finalizer: Arc<RecordingFinalizer>) -> Self {
        let tails = Arc::new(Mutex::new(Tails::default()));
        let hold = Finalizer::new({
            let tails = Arc::clone(&tails);
            move || {
                // Any outcome is already observed by the guarded work's owner;
                // here the quit only waits.
                let open: Vec<Arc<Latch>> = tails
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .open
                    .iter()
                    .map(|(_, latch)| Arc::clone(latch))
                    .collect();
                for latch in open {
                    latch.wait();
                }
            }
        });
        StopFinalizeGate {
            finalizer,
            hold,
            tails,
        }
    }

    /// Holds the quit until the returned guard is dropped (see
    /// [`MicRecordingController::begin_stop_finalize`]).
    pub fn begin(&self) -> StopFinalizeHold {
        let mut tails = self.tails.lock().unwrap_or_else(PoisonError::into_inner);
        // Registered and listed in one step: a quit calling the hold in
        // between found no tail to wait for (Dart could not interleave there).
        self.finalizer.register(&self.hold, false);
        let token = tails.next;
        tails.next += 1;
        let settled = Arc::new(Latch::default());
        tails.open.push((token, Arc::clone(&settled)));
        StopFinalizeHold {
            finalizer: Arc::clone(&self.finalizer),
            hold: self.hold.clone(),
            tails: Arc::clone(&self.tails),
            token,
            settled,
        }
    }

    /// Runs `work` while holding the quit, and returns its outcome (a panic
    /// too).
    pub fn guard<T>(&self, work: impl FnOnce() -> T) -> T {
        let _held = self.begin();
        work()
    }
}

/// A post-stop finalize tail held against quit (see
/// [`MicRecordingController::begin_stop_finalize`]); released when dropped.
#[must_use = "the quit is held only while this guard lives"]
pub struct StopFinalizeHold {
    finalizer: Arc<RecordingFinalizer>,
    hold: Finalizer,
    tails: Arc<Mutex<Tails>>,
    token: u64,
    settled: Arc<Latch>,
}

impl fmt::Debug for StopFinalizeHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopFinalizeHold").finish_non_exhaustive()
    }
}

impl Drop for StopFinalizeHold {
    fn drop(&mut self) {
        {
            let mut tails = self.tails.lock().unwrap_or_else(PoisonError::into_inner);
            tails.open.retain(|(token, _)| *token != self.token);
            if tails.open.is_empty() {
                self.finalizer.unregister(&self.hold);
            }
        }
        // Off the list first, then settled: a quit waiting on this tail
        // returns with the hold already gone, as Dart's finally ran before
        // the quit's wait on the same future resumed.
        self.settled.open();
    }
}

/// What an owner drives (the controller, or a test's fake recorder; Dart's
/// view model tests faked `MicRecordingController`): each method is the
/// controller's own of the same name.
pub trait MicRecorder: Send + Sync {
    /// See [`MicRecordingController::level`].
    fn level(&self) -> f64;
    /// See [`MicRecordingController::revision`].
    fn revision(&self) -> u64;
    /// See [`MicRecordingController::is_recording`].
    fn is_recording(&self) -> bool;
    /// See [`MicRecordingController::is_starting`].
    fn is_starting(&self) -> bool;
    /// See [`MicRecordingController::feeding_monitor`].
    fn feeding_monitor(&self) -> bool;
    /// See [`MicRecordingController::invalidate_pending_start`].
    fn invalidate_pending_start(&self);
    /// See [`MicRecordingController::begin_start`].
    fn begin_start(&self, destination: RecordingPathStrategy) -> PendingStart;
    /// See [`MicRecordingController::tick`].
    fn tick(&self, on_event: &mut dyn FnMut(MicEvent));
    /// See [`MicRecordingController::next_tick_due`].
    fn next_tick_due(&self) -> Option<Instant>;
    /// See [`MicRecordingController::stop`].
    fn stop(&self) -> Option<MicRecordingStop>;
    /// See [`MicRecordingController::abort`].
    fn abort(&self) -> Result<(), DestinationError>;
    /// See [`MicRecordingController::begin_stop_finalize`].
    fn begin_stop_finalize(&self) -> StopFinalizeHold;
    /// See [`MicRecordingController::dispose`].
    fn dispose(&self);
}

impl dyn MicRecorder {
    /// See [`MicRecordingController::guard_stop_finalize`].
    pub fn guard_stop_finalize<T>(&self, work: impl FnOnce() -> T) -> T {
        let _held = self.begin_stop_finalize();
        work()
    }
}

impl MicRecorder for MicRecordingController {
    fn level(&self) -> f64 {
        MicRecordingController::level(self)
    }

    fn revision(&self) -> u64 {
        MicRecordingController::revision(self)
    }

    fn is_recording(&self) -> bool {
        MicRecordingController::is_recording(self)
    }

    fn is_starting(&self) -> bool {
        MicRecordingController::is_starting(self)
    }

    fn feeding_monitor(&self) -> bool {
        MicRecordingController::feeding_monitor(self)
    }

    fn invalidate_pending_start(&self) {
        MicRecordingController::invalidate_pending_start(self)
    }

    fn begin_start(&self, destination: RecordingPathStrategy) -> PendingStart {
        MicRecordingController::begin_start(self, destination)
    }

    fn tick(&self, on_event: &mut dyn FnMut(MicEvent)) {
        MicRecordingController::tick(self, on_event)
    }

    fn next_tick_due(&self) -> Option<Instant> {
        MicRecordingController::next_tick_due(self)
    }

    fn stop(&self) -> Option<MicRecordingStop> {
        MicRecordingController::stop(self)
    }

    fn abort(&self) -> Result<(), DestinationError> {
        MicRecordingController::abort(self)
    }

    fn begin_stop_finalize(&self) -> StopFinalizeHold {
        MicRecordingController::begin_stop_finalize(self)
    }

    fn dispose(&self) {
        MicRecordingController::dispose(self)
    }
}
