//! Port of `test/mic_recording_controller_test.dart`, plus the Rust-side
//! seams (the two-phase start, the ticks, the pending start dropped unrun,
//! the controller dropped while armed) and the branches the Dart suite left
//! untested (an unavailable engine, the record limit, the level ticks, the
//! capture pump's downmix, a late tick).
//!
//! The engine is the real [`GraphAudioPlaybackService`] over fake
//! [`EngineBindings`] that implement only what the controller touches;
//! anything else panics (the trait's default), as Dart's fake threw on
//! `noSuchMethod`.
//!
//! Dart's widget tester advanced a fake clock through the timers (`pump`);
//! here a [`FakeClock`] anchors the controller's timers and [`pump`] ticks
//! the controller at every instant a timer falls due, as `FakeAsync`
//! fired them. The ready handshake runs on real time, as Dart's `Stopwatch`
//! did. Dart's completers are [`Gate`]s and its pending futures threads.
//! Each test passes a private [`RecordingFinalizer`] where Dart used the
//! app-wide instance: the Rust tests run in parallel.

use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::engine::{EngineBindings, EngineError};
use crate::monitor_service::MonitorService;
use crate::playback_engine::PlaybackEngine;

const TAKE: &str = "/fake/take.wav";

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Fake of the native engine: implements only the calls the recording
/// controller makes; anything else is a hard failure (the controller must
/// not reach into playback/graph APIs).
struct Fake {
    s: Mutex<FakeState>,
}

struct FakeState {
    start_result: bool,
    stop_clean: bool,
    level_value: f32,
    device_events: u32,
    /// Interleaved stereo, handed out by every pull.
    frames: Vec<f32>,

    start_calls: usize,
    stop_calls: usize,
    capture_starts: usize,
    capture_stops: usize,
    input_level_calls: usize,
    pull_calls: usize,
    polls: usize,
    last_recording_path: Option<String>,

    /// When set, the arm panics (a native call failing outright).
    throw_on_start: bool,
    /// When set, starting the live capture panics (after the take armed).
    throw_on_capture: bool,

    /// Per-call advance of the input frame counter. The default (a warm
    /// stream) keeps the level tick's stall watch fed; 0 simulates a mic
    /// that stopped calling back.
    frames_received_step: u64,
    frames_received: u64,

    /// Scripted per-take recorded-progress values returned by successive
    /// `record_written_frames` reads; once exhausted, every further read
    /// ADVANCES by `written_tail_step` (a live stream keeps delivering). The
    /// default is a warm stream; a script of [0] with tail 0 models a take
    /// whose ring never receives a frame, and a non-empty script with tail 0
    /// models frames that LANDED once (an outgoing stream's real audio) with
    /// the current stream silent — the delta condition's exact target.
    record_written_script: Vec<u64>,
    written_tail_step: u64,
    written_reads: usize,
    written_value: u64,

    /// The published input-intent sequence (the arm's watermark).
    intent_seq: u64,

    /// Scripted host-converged pairs returned by successive
    /// `input_applied_state` reads (last repeats), packed
    /// `(seq << 24) | generation` like the native word. Default: converged to
    /// the arm's intent on generation 1.
    applied_state_script: Vec<u64>,
    applied_reads: usize,

    /// Scripted state words (`(generation << 1) | running`) for successive
    /// `input_stream_state` reads (last repeats). Default: generation 1,
    /// running.
    stream_state_script: Vec<u64>,
    state_reads: usize,

    /// Frames the fake reports as dropped by the recording ring at stop.
    dropped_frames: u64,

    /// The generations `reopen_input` was asked to reopen, in order.
    reopens: Vec<u64>,
    /// The uids `actual_input_device` and `current_input_device` answer.
    actual_input: String,
    selected_input: String,
    /// When set, the stream follows this timeline instead of the scripts.
    plan: Option<Plan>,
}

/// A stream's life on the ready handshake's own clock, which the poll sleep
/// moves (see [`controller_on_plan`]): when the arm's stream (generation 1)
/// comes up and delivers, and what a reopen brings (generation 2, up
/// [`REOPEN_UP`] after it is asked).
#[derive(Clone)]
struct Plan {
    now: Arc<Mutex<Duration>>,
    /// `None`: its open failed (nothing ever runs until a reopen).
    up_at: Option<Duration>,
    delivers_at: Option<Duration>,
    /// How long after the reopen was asked the reopened stream delivers;
    /// `None`: never.
    reopen_delivers_after: Option<Duration>,
    reopened_at: Option<Duration>,
}

/// How long the fake host takes to bring the reopened stream up.
const REOPEN_UP: Duration = Duration::from_millis(100);

impl Plan {
    fn new(
        up_at: Option<u64>,
        delivers_at: Option<u64>,
        reopen_delivers_after: Option<u64>,
    ) -> Plan {
        Plan {
            now: Arc::default(),
            up_at: up_at.map(ms),
            delivers_at: delivers_at.map(ms),
            reopen_delivers_after: reopen_delivers_after.map(ms),
            reopened_at: None,
        }
    }

    /// `(applied, state word, delivering)` now.
    fn view(&self) -> (u64, u64, bool) {
        let t = *self.now.lock().unwrap();
        if let Some(r) = self.reopened_at {
            let since = t.saturating_sub(r);
            if since >= REOPEN_UP {
                let delivering = self.reopen_delivers_after.is_some_and(|d| since >= d);
                return ((1 << 24) | 2, (2 << 1) | 1, delivering);
            }
            // Generation 1 torn down, generation 2 not up yet.
            return ((1 << 24) | 1, 1 << 1, false);
        }
        match self.up_at {
            Some(up) if t >= up => (
                (1 << 24) | 1,
                (1 << 1) | 1,
                self.delivers_at.is_some_and(|d| t >= d),
            ),
            _ => (0, 0, false),
        }
    }
}

impl Default for FakeState {
    fn default() -> Self {
        FakeState {
            start_result: true,
            stop_clean: true,
            level_value: 0.0,
            device_events: 0,
            frames: Vec::new(),
            start_calls: 0,
            stop_calls: 0,
            capture_starts: 0,
            capture_stops: 0,
            input_level_calls: 0,
            pull_calls: 0,
            polls: 0,
            last_recording_path: None,
            throw_on_start: false,
            throw_on_capture: false,
            frames_received_step: 512,
            frames_received: 0,
            record_written_script: vec![512],
            written_tail_step: 480,
            written_reads: 0,
            written_value: 0,
            intent_seq: 1,
            applied_state_script: vec![(1 << 24) | 1],
            applied_reads: 0,
            stream_state_script: vec![3],
            state_reads: 0,
            dropped_frames: 0,
            reopens: Vec::new(),
            actual_input: String::new(),
            selected_input: String::new(),
            plan: None,
        }
    }
}

impl Fake {
    fn new() -> Arc<Fake> {
        Arc::new(Fake {
            s: Mutex::new(FakeState::default()),
        })
    }

    fn s(&self) -> MutexGuard<'_, FakeState> {
        self.s.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn scripted(script: &[u64], reads: &mut usize) -> u64 {
    let i = (*reads).min(script.len() - 1);
    *reads += 1;
    script[i]
}

impl EngineBindings for Fake {
    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn destroy(&self) {}

    fn start_input_recording(&self, path: &str) -> Result<(), EngineError> {
        let (throw, ok) = {
            let mut s = self.s();
            s.start_calls += 1;
            s.last_recording_path = Some(path.to_string());
            (s.throw_on_start, s.start_result)
        };
        if throw {
            panic!("native arm failed");
        }
        if ok {
            Ok(())
        } else {
            Err(EngineError::Refused {
                call: "ve_start_input_recording",
                code: -1,
            })
        }
    }

    fn stop_input_recording(&self) -> Result<(), EngineError> {
        let mut s = self.s();
        s.stop_calls += 1;
        if s.stop_clean {
            Ok(())
        } else {
            Err(EngineError::RecordingIo)
        }
    }

    fn start_capture(&self) -> Result<(), EngineError> {
        let throw = {
            let mut s = self.s();
            s.capture_starts += 1;
            s.throw_on_capture
        };
        if throw {
            panic!("capture failed");
        }
        Ok(())
    }

    fn stop_capture(&self) -> Result<(), EngineError> {
        self.s().capture_stops += 1;
        Ok(())
    }

    fn input_frames(&self, out: &mut [f32]) -> usize {
        let mut s = self.s();
        s.pull_calls += 1;
        let n = (s.frames.len() / 2).min(out.len() / 2);
        out[..n * 2].copy_from_slice(&s.frames[..n * 2]);
        n
    }

    fn input_level(&self) -> f32 {
        let mut s = self.s();
        s.input_level_calls += 1;
        s.level_value
    }

    fn poll_device_event(&self, mask: u32) -> u32 {
        let mut s = self.s();
        s.polls += 1;
        let events = s.device_events & mask;
        s.device_events &= !mask; // per-bit claim, like the native poll
        events
    }

    fn input_frames_received(&self) -> u64 {
        let mut s = self.s();
        s.frames_received += s.frames_received_step;
        s.frames_received
    }

    fn record_written_frames(&self) -> u64 {
        let mut s = self.s();
        if let Some(plan) = &s.plan {
            if plan.view().2 {
                s.written_value += 480;
            }
            s.written_reads += 1;
            return s.written_value;
        }
        if s.written_reads < s.record_written_script.len() {
            s.written_value = s.record_written_script[s.written_reads];
        } else {
            s.written_value += s.written_tail_step;
        }
        s.written_reads += 1;
        s.written_value
    }

    fn input_intent_seq(&self) -> u64 {
        self.s().intent_seq
    }

    fn input_applied_state(&self) -> u64 {
        let s = &mut *self.s();
        if let Some(plan) = &s.plan {
            s.applied_reads += 1;
            return plan.view().0;
        }
        scripted(&s.applied_state_script, &mut s.applied_reads)
    }

    fn input_stream_state(&self) -> u64 {
        let s = &mut *self.s();
        if let Some(plan) = &s.plan {
            s.state_reads += 1;
            return plan.view().1;
        }
        scripted(&s.stream_state_script, &mut s.state_reads)
    }

    fn reopen_input(&self, generation: u64) -> Result<(), EngineError> {
        let mut s = self.s();
        s.reopens.push(generation);
        if let Some(plan) = s.plan.as_mut() {
            plan.reopened_at = Some(*plan.now.lock().unwrap());
        }
        Ok(())
    }

    fn actual_input_device(&self) -> String {
        self.s().actual_input.clone()
    }

    fn current_input_device(&self) -> String {
        self.s().selected_input.clone()
    }

    fn record_dropped_frames(&self) -> u64 {
        self.s().dropped_frames
    }
}

/// A one-shot signal (Dart's `Completer<void>`).
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn open(&self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }

    fn wait(&self) {
        let mut open = self.0 .0.lock().unwrap();
        while !*open {
            open = self.0 .1.wait(open).unwrap();
        }
    }
}

/// Waits (bounded) until `cond` holds.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(ms(1));
    }
}

/// A path strategy with no real IO, recording create/discard calls, whose
/// create can be gated to park a start mid-wait.
struct Dest {
    path: String,
    created: Mutex<Vec<String>>,
    discarded: Mutex<Vec<String>>,
    gate: Option<Gate>,
    /// Opened when a create begins.
    entered: Gate,
}

impl Dest {
    fn new(path: &str) -> Arc<Dest> {
        Arc::new(Dest {
            path: path.to_string(),
            created: Mutex::new(Vec::new()),
            discarded: Mutex::new(Vec::new()),
            gate: None,
            entered: Gate::default(),
        })
    }

    fn gated(path: &str) -> Arc<Dest> {
        Arc::new(Dest {
            path: path.to_string(),
            created: Mutex::new(Vec::new()),
            discarded: Mutex::new(Vec::new()),
            gate: Some(Gate::default()),
            entered: Gate::default(),
        })
    }

    fn strategy(self: &Arc<Self>) -> RecordingPathStrategy {
        let (c, d) = (Arc::clone(self), Arc::clone(self));
        RecordingPathStrategy::new(
            move || {
                c.entered.open();
                if let Some(gate) = &c.gate {
                    gate.wait();
                }
                c.created.lock().unwrap().push(c.path.clone());
                Ok(c.path.clone())
            },
            move |p| {
                d.discarded.lock().unwrap().push(p.to_string());
                Ok(())
            },
        )
    }

    fn created(&self) -> Vec<String> {
        self.created.lock().unwrap().clone()
    }

    fn discarded(&self) -> Vec<String> {
        self.discarded.lock().unwrap().clone()
    }
}

/// The controller's timer clock, moved by [`pump`].
struct FakeClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl FakeClock {
    fn new() -> Arc<FakeClock> {
        Arc::new(FakeClock {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        })
    }

    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap()
    }

    fn set(&self, at: Instant) {
        *self.offset.lock().unwrap() = at.saturating_duration_since(self.base);
    }
}

/// Dart's `tester.pump(duration)`: moves the clock `n` ms on, ticking the
/// controller at every instant a timer falls due on the way.
fn pump(c: &MicRecordingController, clock: &FakeClock, n: u64, on_event: &mut dyn FnMut(MicEvent)) {
    let end = clock.now() + ms(n);
    for _ in 0..100_000 {
        match c.next_tick_due() {
            Some(due) if due <= end => {
                clock.set(due.max(clock.now()));
                c.tick(on_event);
            }
            _ => break,
        }
    }
    clock.set(end);
}

fn pump_quiet(c: &MicRecordingController, clock: &FakeClock, n: u64) {
    pump(c, clock, n, &mut |_| {});
}

/// The fake engine behind a real service, the timer clock, a private quit
/// registry and the controller's diagnostics lines.
struct Rig {
    fake: Arc<Fake>,
    engine: Arc<GraphAudioPlaybackService>,
    clock: Arc<FakeClock>,
    finalizer: Arc<RecordingFinalizer>,
    logs: Arc<Mutex<Vec<(String, String)>>>,
}

impl Rig {
    /// The `mic` lines the controller wrote, in order.
    fn mic_lines(&self) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(area, _)| area == "mic")
            .map(|(_, line)| line.clone())
            .collect()
    }
}

fn rig() -> Rig {
    rig_with(RecordingFinalizer::for_testing())
}

fn rig_with(finalizer: RecordingFinalizer) -> Rig {
    let fake = Fake::new();
    let engine = Arc::new(GraphAudioPlaybackService::with_bindings(
        Arc::clone(&fake) as Arc<dyn EngineBindings>,
        None,
    ));
    Rig {
        fake,
        engine,
        clock: FakeClock::new(),
        finalizer: Arc::new(finalizer),
        logs: Arc::default(),
    }
}

/// The rig's diagnostics collector, as the controller's log seam.
fn log_sink(rig: &Rig) -> LogSink {
    let logs = Arc::clone(&rig.logs);
    Arc::new(move |area: &str, line: &str| {
        logs.lock()
            .unwrap()
            .push((area.to_string(), line.to_string()))
    })
}

/// Permission granted, everything else at its default.
fn granted() -> MicRecordingOptions {
    MicRecordingOptions {
        request_permission: Arc::new(|| true),
        ..MicRecordingOptions::default()
    }
}

fn controller(rig: &Rig, options: MicRecordingOptions) -> MicRecordingController {
    controller_with_sleep(rig, options, Arc::new(thread::sleep))
}

fn controller_with_sleep(
    rig: &Rig,
    mut options: MicRecordingOptions,
    poll_sleep: PollSleep,
) -> MicRecordingController {
    if options.finalizer.is_none() {
        options.finalizer = Some(Arc::clone(&rig.finalizer));
    }
    let engine = Arc::clone(&rig.engine);
    let clock = Arc::clone(&rig.clock);
    MicRecordingController::build(
        Arc::new(move || Arc::clone(&engine)),
        options,
        Seams {
            clock: Arc::new(move || clock.now()),
            ready_clock: Arc::new(Instant::now),
            poll_sleep,
            log: log_sink(rig),
        },
    )
}

/// A controller whose ready handshake runs on `plan`'s clock: each poll sleep
/// moves it on by the sleep, so a wait of seconds takes no real time and the
/// times in its lines are exact. The fake's stream follows the plan.
fn controller_on_plan(rig: &Rig, plan: Plan, timeout: Duration) -> MicRecordingController {
    let base = Instant::now();
    let (read, step) = (Arc::clone(&plan.now), Arc::clone(&plan.now));
    rig.fake.s().plan = Some(plan);
    let engine = Arc::clone(&rig.engine);
    let clock = Arc::clone(&rig.clock);
    MicRecordingController::build(
        Arc::new(move || Arc::clone(&engine)),
        MicRecordingOptions {
            ready_timeout: timeout,
            finalizer: Some(Arc::clone(&rig.finalizer)),
            ..granted()
        },
        Seams {
            clock: Arc::new(move || clock.now()),
            ready_clock: Arc::new(move || base + *read.lock().unwrap()),
            poll_sleep: Arc::new(move |d| *step.lock().unwrap() += d),
            log: log_sink(rig),
        },
    )
}

/// The start's outcome, which these tests expect to be an answer, not an
/// error.
fn start(c: &MicRecordingController, dest: &Arc<Dest>) -> MicRecordingStart {
    c.start(dest.strategy())
        .expect("the fake destination never fails")
}

/// A start run on a thread of its own (Dart's unawaited start future): the
/// synchronous half runs here, the rest there.
fn spawn_start(
    c: &MicRecordingController,
    dest: &Arc<Dest>,
) -> thread::JoinHandle<Result<MicRecordingStart, DestinationError>> {
    let pending = c.begin_start(dest.strategy());
    thread::spawn(move || pending.run())
}

/// A monitor that records what it is fed.
#[derive(Default)]
struct FakeMonitor {
    active: AtomicBool,
    fed: Mutex<Vec<Vec<f32>>>,
}

impl LiveMonitor for FakeMonitor {
    fn start(&self) {
        self.active.store(true, Ordering::SeqCst);
    }

    fn feed_samples(&self, samples: &[f32]) {
        self.fed.lock().unwrap().push(samples.to_vec());
    }

    fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

// A start that threw past its destination left the file behind, and one that
// threw after arming left the mic recording with nothing to stop it: the
// owner heard only the throw.
#[test]
fn a_start_that_panics_before_arming_discards_its_file() {
    let rig = rig();
    rig.fake.s().throw_on_start = true;
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let outcome = catch_unwind(AssertUnwindSafe(|| c.start(dest.strategy())));

    assert!(outcome.is_err(), "the panic reaches the owner");
    assert!(!c.is_recording());
    assert!(!c.is_starting());
    assert_eq!(dest.discarded(), [TAKE]);
}

#[test]
fn a_start_that_panics_after_arming_stops_the_mic_and_discards_its_file() {
    let rig = rig();
    rig.fake.s().throw_on_capture = true;
    let monitor = Arc::new(MonitorService::default());
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            monitor: Some(monitor.clone()),
            ..granted()
        },
    );

    let outcome = catch_unwind(AssertUnwindSafe(|| c.start(dest.strategy())));

    assert!(outcome.is_err());
    assert!(!c.is_recording());
    assert!(!c.feeding_monitor());
    assert_eq!(
        rig.fake.s().stop_calls,
        1,
        "the armed recording is disarmed"
    );
    assert_eq!(rig.fake.s().capture_stops, 1);
    assert_eq!(dest.discarded(), [TAKE]);
    assert!(!rig.finalizer.has_active());
}

// Rust's counterpart of a destination that throws (Dart's start rethrew it):
// the failure is the owner's to read, nothing armed.
#[test]
fn a_destination_that_fails_fails_the_start_and_arms_nothing() {
    let rig = rig();
    let c = controller(&rig, granted());
    let failing = RecordingPathStrategy::new(
        || Err(io::Error::new(io::ErrorKind::StorageFull, "no space left").into()),
        |_| Ok(()),
    );

    let err = c.start(failing).expect_err("the destination's failure");

    assert!(
        err.downcast_ref::<io::Error>().is_some(),
        "a file-system failure, as the technique view model tells it"
    );
    assert!(!c.is_recording());
    assert!(!c.is_starting());
    assert_eq!(rig.fake.s().start_calls, 0);
    assert!(!rig.finalizer.has_active());
}

#[test]
fn start_failure_wav_create_fails_arms_nothing_and_discards_the_file() {
    let rig = rig();
    rig.fake.s().start_result = false;
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let result = start(&c, &dest);

    assert!(!result.started());
    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::FileCreateFailed)
    );
    assert!(!c.is_recording());
    assert!(!c.is_starting());
    assert_eq!(dest.discarded(), [TAKE]); // the created destination is gone
    assert!(!rig.finalizer.has_active());
    // No timers were armed: time passing never polls the engine level.
    assert_eq!(c.next_tick_due(), None);
    pump_quiet(&c, &rig.clock, 500);
    assert_eq!(rig.fake.s().input_level_calls, 0);
    assert_eq!(c.level(), 0.0);
}

#[test]
fn permission_denied_fails_before_anything_is_created() {
    let rig = rig();
    let acquired = Arc::new(AtomicUsize::new(0));
    let dest = Dest::new(TAKE);
    let engine = Arc::clone(&rig.engine);
    let c = MicRecordingController::build(
        Arc::new({
            let acquired = Arc::clone(&acquired);
            move || {
                acquired.fetch_add(1, Ordering::SeqCst);
                Arc::clone(&engine)
            }
        }),
        MicRecordingOptions {
            request_permission: Arc::new(|| false),
            finalizer: Some(Arc::clone(&rig.finalizer)),
            ..MicRecordingOptions::default()
        },
        Seams::default(),
    );

    let result = start(&c, &dest);

    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::PermissionDenied)
    );
    assert_eq!(acquired.load(Ordering::SeqCst), 0); // denied → the engine is never touched
    assert!(dest.created().is_empty());
}

// Dart never tested this branch: an engine with no device fails the start
// before its destination is made.
#[test]
fn an_unavailable_engine_fails_the_start_before_the_destination_is_made() {
    let rig = rig();
    PlaybackEngine::dispose(&*rig.engine);
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let result = start(&c, &dest);

    assert_eq!(
        result,
        MicRecordingStart::Failed(MicRecordingFailure::EngineUnavailable)
    );
    assert!(dest.created().is_empty());
    assert_eq!(rig.fake.s().start_calls, 0);
    assert!(!c.is_starting());
}

#[test]
fn start_then_a_dirty_stop_reports_the_path_and_tears_the_pumps_and_monitor_down() {
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.level_value = 0.7;
        s.stop_clean = false;
    }
    let monitor = Arc::new(MonitorService::default());
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            monitor: Some(monitor.clone()),
            ..granted()
        },
    );

    let pending = c.begin_start(dest.strategy());
    // The pending flag is set synchronously, before the first wait resolves,
    // so a double-tap in the same frame can be guarded.
    assert!(c.is_starting());
    let result = pending.run().unwrap();

    assert!(result.started());
    assert_eq!(result.path(), Some(TAKE));
    assert_eq!(rig.fake.s().last_recording_path.as_deref(), Some(TAKE));
    assert!(c.is_recording());
    assert!(!c.is_starting());
    assert!(c.feeding_monitor());
    assert_eq!(rig.fake.s().capture_starts, 1);
    assert!(monitor.active());
    assert!(rig.finalizer.has_active());

    // Level pump: 50 ms cadence updates the clamped level; capture pump
    // (30 ms) pulls input frames for the monitor.
    pump_quiet(&c, &rig.clock, 120);
    // The engine's level is an f32 (the C ABI's), widened.
    assert_eq!(c.level(), f64::from(0.7_f32));
    assert_eq!(rig.fake.s().input_level_calls, 2);
    assert!(rig.fake.s().pull_calls >= 3);

    let stopped = c.stop().expect("a take was in flight");
    assert_eq!(stopped.path, TAKE);
    assert!(!stopped.clean); // honors the engine's stop result
    assert!(!c.is_recording());
    assert_eq!(c.level(), 0.0);
    assert!(!c.feeding_monitor());
    assert_eq!(rig.fake.s().stop_calls, 1);
    assert_eq!(rig.fake.s().capture_stops, 1);
    assert!(!monitor.active());
    assert!(!rig.finalizer.has_active());
    // A stopped take's file belongs to the owner — never discarded here.
    assert!(dest.discarded().is_empty());
    // Pumps are gone: no further engine polls.
    let level_calls = rig.fake.s().input_level_calls;
    pump_quiet(&c, &rig.clock, 300);
    assert_eq!(rig.fake.s().input_level_calls, level_calls);

    // Stopping again is a no-op.
    assert_eq!(c.stop(), None);
}

#[test]
fn invalidated_mid_permission_arms_nothing_and_never_acquires_the_engine() {
    let rig = rig();
    let acquired = Arc::new(AtomicUsize::new(0));
    let (asked, permission) = (Gate::default(), Gate::default());
    let dest = Dest::new(TAKE);
    let engine = Arc::clone(&rig.engine);
    let c = MicRecordingController::build(
        Arc::new({
            let acquired = Arc::clone(&acquired);
            move || {
                acquired.fetch_add(1, Ordering::SeqCst);
                Arc::clone(&engine)
            }
        }),
        MicRecordingOptions {
            request_permission: Arc::new({
                let (asked, permission) = (asked.clone(), permission.clone());
                move || {
                    asked.open();
                    permission.wait();
                    true
                }
            }),
            finalizer: Some(Arc::clone(&rig.finalizer)),
            ..MicRecordingOptions::default()
        },
        Seams::default(),
    );

    let started = spawn_start(&c, &dest);
    assert!(c.is_starting());
    asked.wait();

    // The owner cancels the session while the permission dialog pends; the
    // pending flag clears immediately so UI guards unblock.
    c.invalidate_pending_start();
    assert!(!c.is_starting());

    permission.open();
    let result = started.join().unwrap().unwrap();

    assert!(!result.started());
    assert_eq!(result.failure(), None); // silent no-op, not an error
    assert_eq!(acquired.load(Ordering::SeqCst), 0);
    assert!(dest.created().is_empty());
    assert!(!c.is_recording());
    assert!(!rig.finalizer.has_active());
}

#[test]
fn invalidated_mid_path_create_discards_the_just_created_file() {
    let rig = rig();
    let dest = Dest::gated(TAKE);
    let c = controller(&rig, granted());

    let started = spawn_start(&c, &dest);
    dest.entered.wait(); // past permission, parked on create

    c.invalidate_pending_start();
    dest.gate.as_ref().unwrap().open();
    let result = started.join().unwrap().unwrap();

    assert!(!result.started());
    assert_eq!(result.failure(), None);
    assert_eq!(dest.created(), [TAKE]);
    assert_eq!(dest.discarded(), [TAKE]); // temp cleaned
    assert_eq!(rig.fake.s().start_calls, 0); // hardware never armed
    assert!(!c.is_recording());
}

#[test]
fn dispose_during_start_arms_nothing_discards_the_file_and_registers_no_finalizer() {
    let rig = rig();
    let dest = Dest::gated(TAKE);
    let c = controller(&rig, granted());

    let started = spawn_start(&c, &dest);
    dest.entered.wait(); // past permission, parked on create

    c.dispose();
    dest.gate.as_ref().unwrap().open();
    let result = started.join().unwrap().unwrap();

    assert!(!result.started());
    assert_eq!(result.failure(), None);
    assert_eq!(dest.discarded(), [TAKE]);
    assert_eq!(rig.fake.s().start_calls, 0);
    assert!(!c.is_recording());
    assert!(!rig.finalizer.has_active());
    pump_quiet(&c, &rig.clock, 500);
    assert_eq!(rig.fake.s().input_level_calls, 0); // no timers ever armed
}

#[test]
fn input_device_lost_is_reported_once_and_skips_the_level_update() {
    let rig = rig();
    rig.fake.s().level_value = 0.9;
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            watch_input_device: true,
            ..granted()
        },
    );

    assert!(start(&c, &dest).started());
    assert!(c.is_recording());

    rig.fake.s().device_events = GraphAudioPlaybackService::EVENT_INPUT_LOST;
    let mut lost = 0;
    // Mirrors the view models: stop the take, keeping what was captured.
    pump(&c, &rig.clock, 50, &mut |event| {
        if event == MicEvent::InputDeviceLost {
            lost += 1;
            c.stop();
        }
    });

    assert_eq!(lost, 1);
    assert!(!c.is_recording());
    // The loss tick returned before reading the level, and stop reset it.
    assert_eq!(rig.fake.s().input_level_calls, 0);
    assert_eq!(c.level(), 0.0);
}

#[test]
fn aec_bypass_and_input_fallback_surface_as_notices_while_the_take_keeps_recording() {
    // They were silently discarded before.
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            watch_input_device: true,
            ..granted()
        },
    );
    let mut notices = Vec::new();
    let mut lost = 0;
    let mut on_event = |event| match event {
        MicEvent::DeviceNotice(message) => notices.push(message),
        MicEvent::InputDeviceLost => lost += 1,
        _ => {}
    };

    assert!(start(&c, &dest).started());
    rig.fake.s().device_events = GraphAudioPlaybackService::EVENT_AEC_BYPASSED;
    pump(&c, &rig.clock, 50, &mut on_event);
    rig.fake.s().device_events = GraphAudioPlaybackService::EVENT_INPUT_FELL_BACK;
    pump(&c, &rig.clock, 50, &mut on_event);

    assert_eq!(notices.len(), 2);
    assert!(notices[0].contains("Echo cancellation"));
    assert!(notices[1].contains("system default"));
    assert_eq!(lost, 0, "non-fatal degradations must not stop the take");
    assert!(c.is_recording());
    c.stop();
}

// Dart never tested this branch: the record limit stops the take like a
// loss, and its tick skips the level update.
#[test]
fn the_record_limit_is_reported_and_skips_the_level_update() {
    let rig = rig();
    rig.fake.s().level_value = 0.5;
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            watch_input_device: true,
            ..granted()
        },
    );
    assert!(start(&c, &dest).started());

    rig.fake.s().device_events =
        GraphAudioPlaybackService::EVENT_RECORD_LIMIT | GraphAudioPlaybackService::EVENT_INPUT_LOST;
    let mut events = Vec::new();
    pump(&c, &rig.clock, 50, &mut |event| {
        events.push(event);
        c.stop();
    });

    // The limit wins over a loss in the same poll, and both bits are claimed.
    assert_eq!(events, [MicEvent::RecordLimitReached]);
    assert_eq!(rig.fake.s().device_events, 0);
    assert_eq!(rig.fake.s().input_level_calls, 0);
    assert!(!c.is_recording());
}

/// A write failing mid-take (the disk full) is reported on the next level
/// tick, not at Stop: the owner stops the take there, and the stop says what
/// the writer said, that the take on disk is damaged. It wins over the limit
/// in the same poll (the take was not saved up to it), and every bit is
/// claimed.
#[test]
fn a_failed_write_is_reported_mid_take_and_skips_the_level_update() {
    let rig = rig();
    rig.fake.s().level_value = 0.5;
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            watch_input_device: true,
            ..granted()
        },
    );
    assert!(start(&c, &dest).started());

    rig.fake.s().stop_clean = false;
    rig.fake.s().device_events = GraphAudioPlaybackService::EVENT_RECORD_FAILED
        | GraphAudioPlaybackService::EVENT_RECORD_LIMIT;
    let mut events = Vec::new();
    let mut stopped = None;
    pump(&c, &rig.clock, 50, &mut |event| {
        events.push(event);
        stopped = c.stop();
    });

    assert_eq!(events, [MicEvent::RecordWriteFailed]);
    assert_eq!(rig.fake.s().device_events, 0);
    assert_eq!(rig.fake.s().input_level_calls, 0);
    assert!(!c.is_recording());
    let stopped = stopped.expect("the owner's stop lands");
    assert!(!stopped.clean, "the take on disk is reported damaged");
}

#[test]
fn an_input_that_stops_delivering_mid_take_counts_as_lost_after_the_stall_limit() {
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            watch_input_device: true,
            ..granted()
        },
    );
    let lost = Cell::new(0);
    let mut on_event = |event| {
        if event == MicEvent::InputDeviceLost {
            lost.set(lost.get() + 1);
        }
    };

    assert!(start(&c, &dest).started());
    pump(&c, &rig.clock, 200, &mut on_event); // delivering
    rig.fake.s().frames_received_step = 0; // the unit stops calling back, no event
    pump(&c, &rig.clock, 1500, &mut on_event);
    assert_eq!(lost.get(), 0, "not stalled for the limit yet");
    pump(&c, &rig.clock, 700, &mut on_event);
    assert_eq!(lost.get(), 1);
    c.stop();
}

#[test]
fn an_input_that_has_not_delivered_yet_is_not_counted_lost() {
    let rig = rig();
    rig.fake.s().frames_received_step = 0;
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            watch_input_device: true,
            ..granted()
        },
    );
    let mut lost = 0;

    assert!(start(&c, &dest).started());
    pump(&c, &rig.clock, 5000, &mut |event| {
        if event == MicEvent::InputDeviceLost {
            lost += 1;
        }
    });

    assert_eq!(lost, 0, "a mic still warming up");
    c.stop();
}

// The owners without a device watch of their own leave the poll alone (the
// song page's watch is the input bits' only consumer there), and the level
// ticks are reported only to an owner that asked for them.
#[test]
fn without_the_watch_nothing_is_polled_and_level_ticks_carry_the_raw_peak() {
    let rig = rig();
    rig.fake.s().level_value = 1.5;
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            level_ticks: true,
            ..granted()
        },
    );
    assert!(start(&c, &dest).started());

    let mut events = Vec::new();
    pump(&c, &rig.clock, 100, &mut |event| events.push(event));

    assert_eq!(events, [MicEvent::LevelTick(1.5), MicEvent::LevelTick(1.5)]);
    assert_eq!(c.level(), 1.0, "the meter's level is clamped");
    assert_eq!(rig.fake.s().polls, 0);
    c.stop();

    // Without level ticks, a tick reports nothing.
    let c = controller(&rig, granted());
    assert!(start(&c, &dest).started());
    let mut events = Vec::new();
    pump(&c, &rig.clock, 100, &mut |event| events.push(event));
    assert!(events.is_empty());
    c.stop();
}

#[test]
fn the_capture_pump_feeds_the_monitor_the_downmixed_input() {
    let rig = rig();
    rig.fake.s().frames = vec![0.2, 0.4, -1.0, 1.0];
    let monitor = Arc::new(FakeMonitor::default());
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            monitor: Some(monitor.clone()),
            ..granted()
        },
    );
    assert!(start(&c, &dest).started());
    assert!(monitor.active());

    pump_quiet(&c, &rig.clock, 30);
    assert_eq!(*monitor.fed.lock().unwrap(), [vec![0.3_f32, 0.0]]);

    // Nothing captured: nothing fed.
    rig.fake.s().frames.clear();
    pump_quiet(&c, &rig.clock, 30);
    assert_eq!(monitor.fed.lock().unwrap().len(), 1);
    c.stop();
    assert!(!monitor.active());
}

// The order the Dart VM fired two timers due at once (the one re-armed longest
// ago first), and a callback that stops the take cancelling the timer still
// due behind it.
#[test]
fn a_handler_stopping_the_take_keeps_a_timer_due_behind_it_from_firing() {
    let rig = rig();
    let monitor = Arc::new(FakeMonitor::default());
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            monitor: Some(monitor),
            watch_input_device: true,
            ..granted()
        },
    );
    assert!(start(&c, &dest).started());

    // Captures at 30, 60, 90 and 120; level ticks at 50 and 100.
    pump_quiet(&c, &rig.clock, 149);
    assert_eq!(rig.fake.s().pull_calls, 4);
    // At 150 both are due: the level timer (re-armed at 100) before the
    // capture (re-armed at 120).
    rig.fake.s().device_events = GraphAudioPlaybackService::EVENT_INPUT_LOST;
    pump(&c, &rig.clock, 1, &mut |_| {
        c.stop();
    });

    assert!(!c.is_recording());
    assert_eq!(
        rig.fake.s().pull_calls,
        4,
        "the cancelled capture never fired"
    );
}

// Dart's periodic timer skipped the ticks a stalled isolate overran instead
// of firing them in a burst.
#[test]
fn a_late_tick_fires_each_timer_once_and_skips_the_missed_ticks() {
    let rig = rig();
    let monitor = Arc::new(FakeMonitor::default());
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            monitor: Some(monitor),
            ..granted()
        },
    );
    assert!(start(&c, &dest).started());
    let armed_at = rig.clock.now();

    rig.clock.set(armed_at + ms(175));
    c.tick(&mut |_| {});

    assert_eq!(rig.fake.s().input_level_calls, 1);
    assert_eq!(rig.fake.s().pull_calls, 1);
    // The level timer (due at 50) skips 100 and 150 to fire next at 200;
    // the capture (due at 30) skips 60..150 to fire next at 180.
    assert_eq!(c.next_tick_due(), Some(armed_at + ms(180)));
    rig.clock.set(armed_at + ms(199));
    c.tick(&mut |_| {});
    assert_eq!(rig.fake.s().input_level_calls, 1);
    assert_eq!(rig.fake.s().pull_calls, 2);
    c.stop();
}

#[test]
fn abort_stops_the_hardware_and_discards_the_take_file() {
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    assert!(start(&c, &dest).started());
    assert!(c.is_recording());

    c.abort().unwrap();

    assert!(!c.is_recording());
    assert_eq!(rig.fake.s().stop_calls, 1);
    assert_eq!(dest.discarded(), [TAKE]);
    assert!(!rig.finalizer.has_active());

    // Not recording: a no-op.
    c.abort().unwrap();
    assert_eq!(dest.discarded(), [TAKE]);
}

/// A controller the quit finalize can reach (Dart's `late final controller`
/// captured by its own callback).
fn quit_aware(
    rig: &Rig,
    on_quit: impl Fn(&MicRecordingController) + Send + Sync + 'static,
) -> Arc<MicRecordingController> {
    let slot: Arc<OnceLock<Weak<MicRecordingController>>> = Arc::new(OnceLock::new());
    let c = Arc::new(controller(
        rig,
        MicRecordingOptions {
            on_quit_finalize: Some(Arc::new({
                let slot = Arc::clone(&slot);
                move || {
                    if let Some(c) = slot.get().and_then(Weak::upgrade) {
                        on_quit(&c);
                    }
                }
            })),
            ..granted()
        },
    ));
    let _ = slot.set(Arc::downgrade(&c));
    c
}

#[test]
fn the_quit_finalizer_is_registered_only_while_armed_and_runs_the_owner_callback() {
    let rig = rig();
    let dest = Dest::new(TAKE);
    let finalized = Arc::new(AtomicUsize::new(0));
    let c = quit_aware(&rig, {
        let finalized = Arc::clone(&finalized);
        move |c| {
            finalized.fetch_add(1, Ordering::SeqCst);
            c.stop();
        }
    });

    assert!(!rig.finalizer.has_active());
    assert!(start(&c, &dest).started());
    assert!(rig.finalizer.has_active());

    rig.finalizer.finalize_all();

    assert_eq!(finalized.load(Ordering::SeqCst), 1);
    assert!(!c.is_recording());
    assert!(!rig.finalizer.has_active());
}

// Real time: the quit's clock runs on its own.
#[test]
fn a_take_re_armed_while_a_cut_off_quit_waited_is_stopped_by_the_exit_request() {
    // The controller registers the same finalizer for every take, marked as
    // work no quit has budgeted: unmarked, the exit request's run found the
    // budget spent and left take 2 recording.
    let rig = rig_with(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(50))
            .with_total_budget(ms(60)),
    );
    let dest = Dest::new(TAKE);
    let stops = Arc::new(AtomicUsize::new(0));
    let c = quit_aware(&rig, {
        let stops = Arc::clone(&stops);
        move |c| {
            if !c.is_recording() {
                return;
            }
            stops.fetch_add(1, Ordering::SeqCst);
            c.stop();
        }
    });
    let _wedged = rig.finalizer.begin_hold(); // a wedged save

    assert!(start(&c, &dest).started()); // take 1
    let began = Instant::now();
    let close = {
        let finalizer = Arc::clone(&rig.finalizer);
        thread::spawn(move || finalizer.finalize_all()) // stops take 1, then waits
    };
    wait_until("the close's run to stop take 1", || !c.is_recording());
    assert!(start(&c, &dest).started()); // REC again while it waits
    assert!(c.is_recording());
    close.join().unwrap(); // cut off
    assert!(began.elapsed() < Duration::from_secs(2));
    thread::sleep(ms(40)); // spent
    let exit = Instant::now();
    rig.finalizer.finalize_all();
    assert!(exit.elapsed() < Duration::from_secs(2));

    assert_eq!(
        stops.load(Ordering::SeqCst),
        2,
        "the exit request's run left take 2 armed"
    );
    assert!(!c.is_recording());
}

#[test]
fn guard_stop_finalize_holds_the_quit_over_the_tail_and_releases_it_once_done() {
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = Arc::new(controller(&rig, granted()));

    assert!(start(&c, &dest).started());
    assert!(c.stop().is_some());
    // stop() unregistered the armed-window finalizer; the owner's finalize
    // tail (analyze + save) is still ahead, and must re-hold the gate.
    assert!(!rig.finalizer.has_active());

    let gate = Gate::default();
    let guarded = {
        let (c, gate) = (Arc::clone(&c), gate.clone());
        thread::spawn(move || c.guard_stop_finalize(|| gate.wait()))
    };
    wait_until("the tail to hold the quit", || rig.finalizer.has_active());

    let quit_done = Arc::new(AtomicBool::new(false));
    let quit = {
        let (finalizer, done) = (Arc::clone(&rig.finalizer), Arc::clone(&quit_done));
        thread::spawn(move || {
            finalizer.finalize_all();
            done.store(true, Ordering::SeqCst);
        })
    };
    thread::sleep(ms(100));
    assert!(
        !quit_done.load(Ordering::SeqCst),
        "quit must wait for the in-flight tail"
    );

    gate.open();
    quit.join().unwrap();
    assert!(quit_done.load(Ordering::SeqCst));
    guarded.join().unwrap();
    assert!(!rig.finalizer.has_active());
}

#[test]
fn begin_stop_finalize_holds_the_quit_from_the_call() {
    // The owner's form: taken right after the stop, on the same thread, and
    // moved along with the tail's work.
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());
    assert!(start(&c, &dest).started());
    assert!(c.stop().is_some());

    let held = c.begin_stop_finalize();
    assert!(rig.finalizer.has_active());
    drop(held);
    assert!(!rig.finalizer.has_active());
}

#[test]
fn overlapping_tails_the_earlier_completing_keeps_the_quit_held_for_the_later() {
    let rig = rig();
    let c = controller(&rig, granted());

    let t1 = c.begin_stop_finalize();
    let t2 = c.begin_stop_finalize();
    drop(t1);
    assert!(
        rig.finalizer.has_active(),
        "the second tail is still draining"
    );

    let quit_done = Arc::new(AtomicBool::new(false));
    let quit = {
        let (finalizer, done) = (Arc::clone(&rig.finalizer), Arc::clone(&quit_done));
        thread::spawn(move || {
            finalizer.finalize_all();
            done.store(true, Ordering::SeqCst);
        })
    };
    thread::sleep(ms(50));
    assert!(!quit_done.load(Ordering::SeqCst));

    drop(t2);
    quit.join().unwrap();
    assert!(quit_done.load(Ordering::SeqCst));
    assert!(!rig.finalizer.has_active());
}

#[test]
fn dispose_while_recording_closes_the_wav_handle_without_discarding_the_file() {
    let rig = rig();
    let monitor = Arc::new(MonitorService::default());
    let dest = Dest::new(TAKE);
    let c = controller(
        &rig,
        MicRecordingOptions {
            monitor: Some(monitor.clone()),
            ..granted()
        },
    );

    assert!(start(&c, &dest).started());
    assert!(c.is_recording());

    c.dispose();

    assert_eq!(rig.fake.s().stop_calls, 1); // native handle closed
    assert_eq!(rig.fake.s().capture_stops, 1);
    // File cleanup is the OWNER's policy on dispose (the technique view model
    // deletes its temp dirs itself) — the controller must not discard.
    assert!(dest.discarded().is_empty());
    assert!(!rig.finalizer.has_active());
    // The monitor is the owner's to stop as it disposes it.
    assert!(monitor.active());
    pump_quiet(&c, &rig.clock, 500);
    assert_eq!(rig.fake.s().input_level_calls, 0); // pumps cancelled
}

#[test]
fn dropping_the_controller_disposes_it() {
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());
    assert!(start(&c, &dest).started());

    drop(c);

    assert_eq!(rig.fake.s().stop_calls, 1);
    assert!(!rig.finalizer.has_active());
}

#[test]
fn a_re_entrant_start_while_pending_or_recording_is_a_silent_no_op() {
    let rig = rig();
    let c = controller(&rig, granted());

    let first = Dest::new("/fake/a.wav");
    let second = Dest::new("/fake/b.wav");
    let first_start = c.begin_start(first.strategy());
    let r2 = c.begin_start(second.strategy()).run().unwrap();
    let r1 = first_start.run().unwrap();

    // Only the first start armed; the second was rejected outright.
    assert!(r1.started());
    assert!(c.is_recording());
    assert_eq!(rig.fake.s().start_calls, 1);
    assert_eq!(
        rig.fake.s().last_recording_path.as_deref(),
        Some("/fake/a.wav")
    );
    assert!(!r2.started());
    assert_eq!(r2.failure(), None);
    assert!(second.created().is_empty());

    // And while recording.
    let third = Dest::new("/fake/c.wav");
    assert_eq!(start(&c, &third), MicRecordingStart::Invalidated);
    assert!(third.created().is_empty());
    c.stop();
}

#[test]
fn a_pending_start_dropped_unrun_clears_its_starting_flag() {
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let pending = c.begin_start(dest.strategy());
    assert!(c.is_starting());
    drop(pending);

    assert!(!c.is_starting());
    assert!(dest.created().is_empty());
    // The door is open again.
    assert!(start(&c, &dest).started());
    c.stop();
}

/// The ready handshake's tests: a short timeout and the owner's outcome.
fn start_with_timeout(
    rig: &Rig,
    dest: &Arc<Dest>,
    timeout: Duration,
) -> (MicRecordingController, MicRecordingStart) {
    let c = controller(
        rig,
        MicRecordingOptions {
            ready_timeout: timeout,
            ..granted()
        },
    );
    let result = start(&c, dest);
    (c, result)
}

#[test]
fn a_mic_that_never_delivers_fails_with_input_not_delivering_disarmed_and_discarded() {
    // The take ring never receives a frame (async input-stream open failed
    // after start_input_recording claimed success). One real millisecond of
    // timeout guarantees the poll body runs at least once — a zero timeout
    // would short-circuit the loop and prove nothing about it.
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.record_written_script = vec![0];
        s.written_tail_step = 0;
    }
    let dest = Dest::new(TAKE);

    let (c, result) = start_with_timeout(&rig, &dest, ms(1));

    assert!(
        rig.fake.s().written_reads >= 1,
        "the poll body must actually have run"
    );
    assert!(!result.started());
    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert!(!c.is_recording());
    // The armed recording was rolled back and its file discarded.
    assert_eq!(rig.fake.s().start_calls, 1);
    assert_eq!(rig.fake.s().stop_calls, 1);
    assert_eq!(dest.discarded(), [TAKE]);
    assert!(!rig.finalizer.has_active());
    // Said, once, in the log; and the engine is marked for its owner.
    let lines = rig.mic_lines();
    let failures: Vec<_> = lines
        .iter()
        .filter(|l| l.starts_with("start: the microphone delivered nothing"))
        .collect();
    assert_eq!(failures.len(), 1, "{lines:?}");
    assert!(rig.engine.input_failed());
}

// The start's one retry (see `ControllerStart::body`): a stream that does not
// deliver by half the ready timeout is reopened, once, unless it came up less
// than half the timeout ago; the reopened stream gets half the timeout of its
// own; and the failure says what the check saw. These run the handshake on the
// plan's clock, so the times are exact.

/// The ready timeout these tests run with: the default, whose half is the
/// engine's 2.5 s warm-up allowance.
const READY: Duration = MicRecordingController::DEFAULT_READY_TIMEOUT;

#[test]
fn the_default_wait_outlasts_the_engines_warm_up_allowance_twice() {
    // A Bluetooth headset switching to its headset profile delivers nothing
    // for up to 2.5 s: the toast must not come before a stream has had that
    // long, nor before the reopen has had it too.
    assert!(READY >= 2 * vocara_engine::input_health::INPUT_WARMUP);
}

#[test]
fn a_stream_that_never_delivers_is_reopened_at_half_the_timeout_and_the_reopen_arms_the_take() {
    // The arm found a stream up at once (kept, or opened at once) that never
    // delivers: an earlier session's unit left running, a stalled one.
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller_on_plan(&rig, Plan::new(Some(0), None, Some(300)), READY);

    let result = start(&c, &dest);

    assert!(result.started(), "{result:?}");
    assert_eq!(
        rig.fake.s().reopens,
        [1],
        "one reopen, of the stream it armed on"
    );
    assert_eq!(
        rig.mic_lines(),
        [
            "start: no frames 2500 ms after the arm; asking the engine to reopen the input \
             (stream gen 1 running, written 0)",
            "start: the reopened input delivered 2800 ms after the arm (reopen asked at 2500 ms)",
        ]
    );
    assert!(!rig.engine.input_failed());
    assert!(dest.discarded().is_empty());
    c.stop();
}

#[test]
fn a_stream_still_warming_up_gets_its_whole_allowance_before_any_reopen() {
    // A cold open that came up at 1.5 s and delivers 2 s later (a headset
    // switching profiles): at half the timeout it has been up only 1 s, and a
    // reopen would restart its switch. It arms at 3.5 s, past the old 2 s
    // timeout that refused it outright.
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller_on_plan(&rig, Plan::new(Some(1500), Some(3500), None), READY);

    let result = start(&c, &dest);

    assert!(result.started(), "{result:?}");
    assert!(rig.fake.s().reopens.is_empty());
    assert!(rig.mic_lines().is_empty(), "{:?}", rig.mic_lines());
    c.stop();
}

#[test]
fn a_failed_open_is_retried_by_the_reopen() {
    // The arm's open failed (nothing ever runs): the reopen at half the
    // timeout is the host's second try, and it comes up and delivers.
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller_on_plan(&rig, Plan::new(None, None, Some(200)), READY);

    let result = start(&c, &dest);

    assert!(result.started(), "{result:?}");
    assert_eq!(rig.fake.s().reopens, [0]);
    assert_eq!(
        rig.mic_lines()[0],
        "start: no frames 2500 ms after the arm; asking the engine to reopen the input \
         (stream gen 0 not running, written 0)"
    );
    c.stop();
}

#[test]
fn a_mic_still_silent_after_its_reopen_fails_with_one_line_saying_what_the_check_saw() {
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.frames_received_step = 0;
        s.actual_input = "AppleUSBAudioEngine:Vendor:Mic:1234:1".into();
        s.selected_input = "AppleUSBAudioEngine:Vendor:Mic:1234:1".into();
    }
    let dest = Dest::new(TAKE);
    let c = controller_on_plan(&rig, Plan::new(Some(0), None, None), READY);

    let result = start(&c, &dest);

    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert_eq!(rig.fake.s().reopens, [1], "one reopen only");
    let lines = rig.mic_lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        lines[1],
        "start: the microphone delivered nothing in 5000 ms (reopen asked at 2500 ms): \
         arm seq 1, applied seq 1 gen 2, stream gen 2 running, written 0 -> 0, \
         frames received +0, input open: the selected input, selected: a chosen input"
    );
    assert!(
        !lines.iter().any(|l| l.contains("AppleUSBAudioEngine")),
        "no device uid reaches the log"
    );
    assert!(rig.engine.input_failed());
    // Disarmed and discarded, as any refused start.
    assert_eq!(rig.fake.s().stop_calls, 1);
    assert_eq!(dest.discarded(), [TAKE]);
    assert!(!c.is_recording() && !c.is_starting());
}

#[test]
fn a_stream_that_came_up_late_is_reopened_once_it_has_had_half_the_timeout_and_the_wait_extends() {
    // Up at 2 s and silent: reopened at 4.5 s, once it has had its 2.5 s, and
    // the reopened stream still gets 2.5 s of its own: the toast at 7 s.
    let rig = rig();
    let dest = Dest::new(TAKE);
    let c = controller_on_plan(&rig, Plan::new(Some(2000), None, None), READY);

    let result = start(&c, &dest);

    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert_eq!(rig.fake.s().reopens, [1]);
    let lines = rig.mic_lines();
    assert!(
        lines[0].starts_with("start: no frames 4500 ms after the arm;"),
        "{lines:?}"
    );
    assert!(
        lines[1].starts_with(
            "start: the microphone delivered nothing in 7000 ms (reopen asked at 4500 ms)"
        ),
        "{lines:?}"
    );
}

#[test]
fn after_input_not_delivering_the_next_press_starts_from_nothing_the_failure_left() {
    let rig = rig();
    let first = Dest::new("/fake/a.wav");
    let c = controller_on_plan(&rig, Plan::new(Some(0), None, None), READY);
    assert_eq!(
        start(&c, &first).failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert!(rig.engine.input_failed());

    // The mic works now (the hardware allows): the next press arms at once.
    rig.fake.s().plan = None;
    let second = Dest::new("/fake/b.wav");
    let result = start(&c, &second);

    assert_eq!(result, MicRecordingStart::Started("/fake/b.wav".into()));
    assert!(
        !rig.engine.input_failed(),
        "cleared by the start that armed"
    );
    assert_eq!(
        rig.fake.s().reopens,
        [1],
        "the working mic needed no reopen"
    );
    assert_eq!(rig.fake.s().start_calls, 2);
    assert_eq!(
        rig.fake.s().stop_calls,
        1,
        "only the failed arm was disarmed"
    );
    assert!(second.discarded().is_empty());
    c.stop();
}

#[test]
fn a_dying_or_foreign_stream_cannot_fake_ready_when_nothing_landed_in_the_ring() {
    // The global counter advances (pre-arm callbacks, an old stream's final
    // push) but record_written_frames stays 0 — nothing entered THIS take's
    // ring, so the recorded-progress ack must refuse across REAL poll
    // iterations, not a short-circuited loop.
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.frames_received_step = 512;
        s.record_written_script = vec![0];
        s.written_tail_step = 0;
    }
    let dest = Dest::new(TAKE);

    let (_c, result) = start_with_timeout(&rig, &dest, ms(1));

    assert!(
        rig.fake.s().written_reads >= 1,
        "the poll body must actually have run against the fake"
    );
    assert!(!result.started());
    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert_eq!(rig.fake.s().stop_calls, 1);
    assert_eq!(dest.discarded(), [TAKE]);
}

#[test]
fn ready_lands_as_soon_as_frames_provably_land_in_the_take_ring() {
    // A cold stream delivering on a later poll: the first polls see an empty
    // take ring (the VoiceProcessingIO open is still in flight); frames then
    // land and the very next poll arms the take.
    let rig = rig();
    rig.fake.s().record_written_script = vec![0, 0, 480];
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let result = start(&c, &dest);

    assert!(result.started());
    assert!(
        rig.fake.s().written_reads >= 3,
        "ready must have waited out the empty polls"
    );
    c.stop();
}

#[test]
fn frames_in_the_ring_alone_are_not_ready_the_host_must_have_converged_past_the_arm() {
    // A warm stream's frames land instantly, but the host has not yet
    // converged to an intent as new as this arm — a queued older teardown
    // could still be ahead of it. Ready must wait for applied >= arm_seq (all
    // on the same generation).
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.intent_seq = 5;
        s.applied_state_script = vec![(3 << 24) | 1, (4 << 24) | 1, (5 << 24) | 1];
    }
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let result = start(&c, &dest);

    assert!(result.started());
    assert!(
        rig.fake.s().applied_reads >= 3,
        "ready must have waited for the host to converge to the arm"
    );
    c.stop();
}

#[test]
fn evidence_from_two_generations_never_splices_into_ready() {
    // The reviewed splice: an old warm stream wrote real frames, its teardown
    // ran, and a FAILED reopen used to advance the watermark anyway. Modeled
    // exactly: applied stays paired with generation 1 (the last SUCCESSFUL
    // convergence), while the current state word shows generation 2 running.
    // The generation-equality condition must refuse; applied never catches up
    // (the reopen keeps failing), so the start times out.
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.intent_seq = 2;
        s.applied_state_script = vec![(2 << 24) | 1]; // seq new enough, gen STALE
        s.stream_state_script = vec![5]; // generation 2, running
    }
    let dest = Dest::new(TAKE);

    let (_c, result) = start_with_timeout(&rig, &dest, ms(1));

    assert!(!result.started());
    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert!(
        rig.fake.s().applied_reads >= 1,
        "the generation comparison must actually have run"
    );
}

#[test]
fn the_reviewed_cross_generation_combo_is_refused() {
    // An outgoing stream's frames plus a freshly-converged stream that has not
    // delivered yet. G1 wrote real frames into the take after the arm, was
    // torn down by a stale off-intent, and G2 then converged SUCCESSFULLY —
    // applied and the state word agree on G2, running. Every absolute
    // condition holds, but the take's counter never moves again (G2 has not
    // delivered): the DELTA condition must refuse until actual growth, which
    // never comes here.
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.record_written_script = vec![480]; // G1's frames, already in the WAV
        s.written_tail_step = 0; // G2 converged but silent
    }
    let dest = Dest::new(TAKE);

    let (_c, result) = start_with_timeout(&rig, &dest, ms(1));

    assert!(!result.started());
    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert!(
        rig.fake.s().written_reads >= 2,
        "the baseline and at least one delta comparison must have run"
    );
}

#[test]
fn the_same_combo_arms_once_the_converged_stream_actually_delivers() {
    // Same shape, but G2 comes alive: the counter grows past the baseline
    // under the unchanged pairing and ready lands.
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.record_written_script = vec![480, 480, 480];
        s.written_tail_step = 480;
    }
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let result = start(&c, &dest);

    assert!(result.started());
    assert!(rig.fake.s().written_reads >= 4);
    c.stop();
}

#[test]
fn the_growth_observation_is_bracketed_a_state_change_before_the_re_read_refuses_ready() {
    // Opening reads say running (gen 1) and the counter grows — but by the
    // confirming re-read the state word has changed (a SetInput cleared
    // running; the growth came from the outgoing stream's in-flight
    // callback). The state → progress → state bracket must refuse; the state
    // never recovers here, so the start times out.
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.stream_state_script = vec![3, 2]; // running at the open read, gone at recheck
        s.record_written_script = vec![480];
        s.written_tail_step = 480; // the counter keeps growing (stale callback)
    }
    let dest = Dest::new(TAKE);

    let (_c, result) = start_with_timeout(&rig, &dest, ms(1));

    assert!(!result.started());
    assert_eq!(
        result.failure(),
        Some(MicRecordingFailure::InputNotDelivering)
    );
    assert!(
        rig.fake.s().state_reads >= 2,
        "the confirming re-read must actually have happened"
    );
}

#[test]
fn pending_start_aba_an_invalidated_starts_cleanup_never_disarms_a_newer_arm() {
    // A arms natively and parks in its ready wait (nothing ever lands in its
    // ring); A is invalidated and B starts immediately. A's cleanup used to
    // call stop_input_recording() unconditionally — stopping B's native
    // recording. The arm token must make it a no-op. Dart's B armed in
    // microtasks before A's 25 ms poll timer fired; here A's first poll sleep
    // is held until B has armed.
    let rig = rig();
    {
        let mut s = rig.fake.s();
        s.record_written_script = vec![0];
        s.written_tail_step = 0; // A never becomes ready
    }
    let (parked, release) = (Gate::default(), Gate::default());
    let first_sleep = Arc::new(AtomicBool::new(true));
    let c = controller_with_sleep(
        &rig,
        granted(),
        Arc::new({
            let (parked, release) = (parked.clone(), release.clone());
            move |d| {
                if first_sleep.swap(false, Ordering::SeqCst) {
                    parked.open();
                    release.wait();
                } else {
                    thread::sleep(d);
                }
            }
        }),
    );
    let dest_a = Dest::new("/fake/a.wav");
    let dest_b = Dest::new("/fake/b.wav");

    let a = spawn_start(&c, &dest_a);
    parked.wait(); // A armed natively and sleeps in its ready wait
    assert_eq!(rig.fake.s().start_calls, 1, "A armed natively");

    c.invalidate_pending_start();
    // B arms while A's cleanup is still parked behind its poll sleep. Reset
    // the written script so B's ready handshake succeeds.
    {
        let mut s = rig.fake.s();
        s.record_written_script = vec![512];
        s.written_tail_step = 480;
        s.written_reads = 0;
    }
    let b_result = start(&c, &dest_b);
    release.open();
    let a_result = a.join().unwrap().unwrap();

    assert!(!a_result.started(), "A was invalidated");
    assert_eq!(a_result.failure(), None);
    assert!(b_result.started(), "B armed");
    assert_eq!(
        rig.fake.s().stop_calls,
        0,
        "A's invalidation cleanup must not stop B's native recording"
    );
    assert!(c.is_recording(), "B survives");
    assert_eq!(dest_a.discarded(), ["/fake/a.wav"]);
    assert!(!c.is_starting(), "A's settle never touches B's flag");
    c.stop();
}

#[test]
fn ring_overflow_is_surfaced_stop_reports_dropped_frames() {
    let rig = rig();
    rig.fake.s().dropped_frames = 4800;
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    assert!(start(&c, &dest).started());

    let stopped = c.stop().expect("a take was in flight");
    assert!(stopped.clean);
    assert_eq!(stopped.dropped_frames, 4800);
}

#[test]
fn the_revision_moves_with_what_the_owner_observes() {
    let rig = rig();
    rig.fake.s().level_value = 0.25;
    let dest = Dest::new(TAKE);
    let c = controller(&rig, granted());

    let r0 = c.revision();
    let pending = c.begin_start(dest.strategy());
    let r1 = c.revision();
    assert!(r1 > r0, "starting");
    assert!(pending.run().unwrap().started());
    let r2 = c.revision();
    assert!(r2 > r1, "recording");
    pump_quiet(&c, &rig.clock, 50);
    let r3 = c.revision();
    assert!(r3 > r2, "the level moved");
    pump_quiet(&c, &rig.clock, 50);
    assert_eq!(c.revision(), r3, "the same level notifies nothing");
    c.stop();
    assert!(c.revision() > r3, "stopped");
}

// Real IO: the temp-dir strategy.
#[test]
fn the_temp_dir_strategy_makes_a_fresh_prefixed_dir_and_its_discard_removes_it() {
    let strategy = RecordingPathStrategy::temp_dir("vocara_test_take");
    let path = strategy.create().unwrap();
    let dir = Path::new(&path).parent().unwrap().to_path_buf();
    assert!(path.ends_with("/vocara_test_take.wav"));
    assert!(dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("vocara_test_take"));
    assert!(dir.is_dir());

    strategy.discard(&path).unwrap();
    assert!(!dir.exists());

    // The shared helper tolerates None and already-gone paths.
    delete_temp_recording_dir(None);
    delete_temp_recording_dir(Some(&path));
}
