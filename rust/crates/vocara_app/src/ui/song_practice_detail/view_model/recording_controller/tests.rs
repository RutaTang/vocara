//! Port of `test/recording_controller_test.dart`, plus the recording
//! controller's assertions from the tests that drive it through the whole
//! detail view model (`song_practice_take_flow_test.dart`,
//! `song_practice_structural_edit_test.dart`) and the song-side counterpart
//! of `technique_recording_dropped_frames_test.dart`.
//!
//! The [`Screen`] harness plays the view model: it implements the host,
//! applies the controller's effects as the view model will (sibling calls in
//! place, async work on threads whose answers come back through a channel),
//! and pumps those answers and the mic's ticks — Dart's event loop. Dart's
//! completers are [`Gate`]s; its parked futures (the mixer's resyncs, a
//! save, the empty take's creation and drop) stay parked in the harness until
//! a test answers them. Each test passes a private [`RecordingFinalizer`]
//! where Dart used the app-wide instance: the Rust tests run in parallel. The
//! 1 s device watch is ticked by the tests themselves (Dart waited real
//! seconds for it); the record-clock tests run on real time, as Dart's did.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx};
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::audio_file_service::{AudioFileError, AudioFiles};
use vocara_services::engine::{EngineBindings, EngineError};
use vocara_services::graph_audio_playback_service::{GraphAudioPlaybackService, SyncError};
use vocara_services::mic_recording_controller::{
    MicEvent, MicRecorder, MicRecordingFailure, MicRecordingStart, MicRecordingStop, PendingStart,
    RecordingPathStrategy, StopFinalizeGate, StopFinalizeHold,
};
use vocara_services::playback_engine::{
    Broadcast, BusPlayback, PlaybackEngine, Subscription, TrackPlayback,
};
use vocara_services::recording_finalizer::{Finalizer, RecordingFinalizer};
use vocara_services::waveform_service::WaveformResult;

use super::*;
use crate::util::busy_gate::BusyGate;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A Dart completer the test completes: [`Gate::wait`] returns once opened.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn new() -> Arc<Gate> {
        Arc::new(Gate::default())
    }

    fn open(&self) {
        *guard(&self.open) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut open = guard(&self.open);
        while !*open {
            open = self.changed.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }
}

// --- Fakes ----------------------------------------------------------------------

/// Transport log double: records every transport call so the tests can
/// assert which TAKE'S continuation drove it.
#[derive(Default)]
struct LogEngine {
    position: Broadcast<Duration>,
    playing: Broadcast<bool>,
    calls: Mutex<Vec<String>>,
}

impl LogEngine {
    fn calls(&self) -> Vec<String> {
        guard(&self.calls).clone()
    }

    fn plays(&self) -> usize {
        self.calls().iter().filter(|c| *c == "play").count()
    }
}

impl PlaybackEngine for LogEngine {
    fn position(&self) -> Subscription<Duration> {
        self.position.subscribe()
    }

    fn playing(&self) -> Subscription<bool> {
        self.playing.subscribe()
    }

    fn current_position(&self) -> Duration {
        Duration::ZERO
    }

    fn is_playing(&self) -> bool {
        false
    }

    fn set_loop(&self, _enabled: bool, _start_ms: i64, _end_ms: i64) {
        guard(&self.calls).push("setLoop".into());
    }

    fn sync(&self, _tracks: Vec<TrackPlayback>, _buses: Vec<BusPlayback>) {}

    fn play(&self) {
        guard(&self.calls).push("play".into());
    }

    fn pause(&self) {
        guard(&self.calls).push("pause".into());
    }

    fn seek(&self, position: Duration) {
        guard(&self.calls).push(format!("seek:{}", position.as_millis()));
    }

    fn set_volume(&self, _id: &str, _volume: f64, _muted: bool) {}

    fn set_track_pan(&self, _id: &str, _pan: f64) {}

    fn set_master_volume(&self, _gain: f64) {}

    fn supports_effects(&self) -> bool {
        false
    }

    fn set_track_fx(&self, _track_id: &str, _fx: &TrackFx) {}

    fn set_bus_fx(&self, _bus_id: &str, _fx: &TrackFx) {}

    fn set_bus_volume(&self, _bus_id: &str, _volume: f64, _muted: bool) {}

    fn set_bus_pan(&self, _bus_id: &str, _pan: f64) {}

    fn bus_gain_reduction_db(&self, _bus_id: &str) -> f64 {
        0.0
    }

    fn set_track_send_level(&self, _track_id: &str, _slot: usize, _level_db: f64) {}

    fn set_bus_send_level(&self, _bus_id: &str, _slot: usize, _level_db: f64) {}

    fn set_reverb(&self, _reverb: &ReverbSettings) {}

    fn track_gain_reduction_db(&self, _track_id: &str) -> f64 {
        0.0
    }

    fn dispose(&self) {}
}

/// The take ring of Dart's `_RollingGraph`: it fills in real time from its
/// arm (plus [`Rolling::lead`]), and a stalled input delivers nothing (what
/// it missed never arrives).
#[derive(Default)]
struct Rolling {
    written: i64,
    lead: i64,
    dropped: i64,
    stalled: bool,
    lost: i64,
    since_arm: Option<Instant>,
}

impl Rolling {
    fn due(&self) -> i64 {
        self.since_arm
            .map_or(0, |at| at.elapsed().as_micros() as i64 * 48 / 1000)
    }

    fn written(&mut self) -> u64 {
        if !self.stalled {
            self.written =
                (self.written + 1).max(self.due() + self.lead - self.dropped - self.lost);
        }
        self.written as u64
    }
}

/// The native engine, as far as recording, monitoring and the 1 s device
/// watch touch it (Dart's `_FakeGraph`, `_HandFedGraph`, `_LosingGraph`,
/// `_DeadGraph` and `_RollingGraph` in one); anything else is a loud failure
/// (the trait's default).
#[derive(Default)]
struct Graph {
    s: Mutex<GraphState>,
}

#[derive(Default)]
struct GraphState {
    /// Frames already in the take's ring as its backing starts (the
    /// pre-roll).
    written_frames: u64,
    /// The input round-trip the take is compensated for, in frames.
    latency_frames: u32,
    /// The recording offset Settings gives the service, in ms.
    record_offset_ms: f64,
    /// The engine's input counter, fed by hand.
    frames: u64,
    /// The device watch's polls (each tick polls once).
    polls: usize,
    /// Events the next poll reports (masked, cleared as claimed).
    pending_events: u32,
    /// Reports its output lost once per `lost_reported` reset.
    losing: bool,
    lost_reported: bool,
    output_calls: Vec<String>,
    monitor_starts: usize,
    /// The live input peak (a rolling input reads 0.5).
    level: f32,
    rolling: Option<Rolling>,
}

impl Graph {
    fn new() -> Arc<Graph> {
        Arc::new(Graph::default())
    }

    fn rolling() -> Arc<Graph> {
        let g = Graph::new();
        g.s().rolling = Some(Rolling::default());
        g
    }

    fn s(&self) -> MutexGuard<'_, GraphState> {
        guard(&self.s)
    }

    fn service(self: &Arc<Self>) -> Arc<GraphAudioPlaybackService> {
        let svc = Arc::new(GraphAudioPlaybackService::with_bindings(
            Arc::clone(self) as Arc<dyn EngineBindings>,
            None,
        ));
        svc.set_record_offset_ms(self.s().record_offset_ms);
        svc
    }

    fn polls(&self) -> usize {
        self.s().polls
    }

    fn set_lead(&self, lead: i64) {
        if let Some(r) = self.s().rolling.as_mut() {
            r.lead = lead;
        }
    }

    fn set_dropped(&self, dropped: i64) {
        if let Some(r) = self.s().rolling.as_mut() {
            r.dropped = dropped;
        }
    }

    fn set_stalled(&self, on: bool) {
        if let Some(r) = self.s().rolling.as_mut() {
            if r.stalled && !on {
                r.lost = r.due() + r.lead - r.written;
            }
            r.stalled = on;
        }
    }

    fn dropped(&self) -> i64 {
        self.s().rolling.as_ref().map_or(0, |r| r.dropped)
    }

    fn written_now(&self) -> i64 {
        let mut s = self.s();
        match s.rolling.as_mut() {
            Some(r) => r.written() as i64,
            None => s.written_frames as i64,
        }
    }
}

impl EngineBindings for Graph {
    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn destroy(&self) {}

    fn poll_device_event(&self, mask: u32) -> u32 {
        let mut s = self.s();
        s.polls += 1;
        if s.losing {
            if s.lost_reported {
                return 0;
            }
            s.lost_reported = true;
            return GraphAudioPlaybackService::EVENT_OUTPUT_LOST & mask;
        }
        let events = s.pending_events & mask;
        s.pending_events &= !mask;
        events
    }

    fn stream_xruns(&self) -> u64 {
        0
    }

    fn input_frames_received(&self) -> u64 {
        let s = self.s();
        match &s.rolling {
            Some(r) => r.written as u64,
            None => s.frames,
        }
    }

    fn record_written_frames(&self) -> u64 {
        let mut s = self.s();
        let fixed = s.written_frames;
        match s.rolling.as_mut() {
            Some(r) => r.written(),
            None => fixed,
        }
    }

    fn record_dropped_frames(&self) -> u64 {
        self.s()
            .rolling
            .as_ref()
            .map_or(0, |r| r.dropped.max(0) as u64)
    }

    fn record_latency_frames(&self) -> u32 {
        self.s().latency_frames
    }

    fn start_monitor(&self) -> Result<(), EngineError> {
        self.s().monitor_starts += 1;
        Ok(())
    }

    fn stop_monitor(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn input_level(&self) -> f32 {
        let s = self.s();
        if s.rolling.is_some() {
            0.5
        } else {
            s.level
        }
    }

    fn current_output_device(&self) -> String {
        "headphones".to_string()
    }

    fn set_output_device(&self, uid: &str) -> Result<(), EngineError> {
        self.s().output_calls.push(uid.to_string());
        Ok(())
    }

    fn input_intent_seq(&self) -> u64 {
        1
    }

    fn input_applied_state(&self) -> u64 {
        (1 << 24) | 7
    }

    fn input_stream_state(&self) -> u64 {
        (7 << 1) | 1
    }

    fn start_input_recording(&self, _path: &str) -> Result<(), EngineError> {
        if let Some(r) = self.s().rolling.as_mut() {
            r.written = 0;
            r.lost = 0;
            r.since_arm = Some(Instant::now());
        }
        Ok(())
    }

    fn stop_input_recording(&self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// The app settings, as far as the controller uses them.
#[derive(Default)]
struct Settings {
    output_refreshes: AtomicUsize,
    /// What `refresh_output_devices` answers: whether it switched the output.
    rebinds: AtomicBool,
    syncs: AtomicUsize,
}

impl RecordingSettings for Settings {
    fn sync_input_channel(&self) {
        self.syncs.fetch_add(1, Ordering::SeqCst);
    }

    fn bluetooth_in_out_conflict_now(&self) -> bool {
        false
    }

    fn refresh_output_devices(&self) -> bool {
        self.output_refreshes.fetch_add(1, Ordering::SeqCst);
        self.rebinds.load(Ordering::SeqCst)
    }

    fn reconcile_monitor_safety(&self) {}
}

/// The audio files: where each recording was asked for, and the aligned
/// starts written beside each take.
#[derive(Default)]
struct Files {
    s: Mutex<FilesState>,
}

#[derive(Default)]
struct FilesState {
    n: usize,
    /// Where the recordings go (`/fake` when unset).
    dir: Option<PathBuf>,
    placements: Vec<(Option<String>, Option<i64>)>,
    aligned: IndexMap<String, i64>,
    deleted: Vec<String>,
}

impl Files {
    fn placements(&self) -> Vec<(Option<String>, Option<i64>)> {
        guard(&self.s).placements.clone()
    }

    fn aligned(&self) -> Vec<i64> {
        guard(&self.s).aligned.values().copied().collect()
    }
}

impl AudioFiles for Files {
    fn import_audio(
        &self,
        _practice_id: &str,
        _source_path: &str,
    ) -> Result<String, AudioFileError> {
        unimplemented!("import_audio")
    }

    fn new_recording_path(
        &self,
        _practice_id: &str,
        _extension: &str,
        take_id: Option<&str>,
        start_ms: Option<i64>,
    ) -> Result<String, AudioFileError> {
        let mut s = guard(&self.s);
        s.placements.push((take_id.map(str::to_string), start_ms));
        let name = format!("take_{}.wav", s.n);
        s.n += 1;
        Ok(match &s.dir {
            Some(dir) => dir.join(name).to_string_lossy().into_owned(),
            None => format!("/fake/{name}"),
        })
    }

    fn write_aligned_start(&self, path: &str, start_ms: i64) -> bool {
        guard(&self.s).aligned.insert(path.to_string(), start_ms);
        true
    }

    fn is_app_storage(&self, _path: &str) -> bool {
        false
    }

    fn delete_audio(&self, path: &str) -> Result<(), AudioFileError> {
        guard(&self.s).deleted.push(path.to_string());
        Ok(())
    }
}

#[derive(Default)]
struct MicState {
    recording: bool,
    starting: bool,
    generation: u64,
    start_calls: usize,
    stop_calls: usize,
    last_path: Option<String>,
    /// What the next stop reports for the writer: false = it hit an IO
    /// error.
    clean: bool,
    dropped_frames: u64,
    /// The start fails this way before its setup (the real recorder's
    /// permission and engine checks).
    failure: Option<MicRecordingFailure>,
}

/// Recorder double faithful to the real one where the controller depends on
/// it: its start runs the destination's create (where the take setup — the
/// generation bump included — lives), and its stop flips the armed flag at
/// once. Dart's `_FakeMic` reads `is_starting` false throughout; with
/// `reports_starting` it is the take-flow test's recorder (and, with a
/// `ready` gate, `_ArmingMic`): starting until its start returns, parked on
/// the permission prompt and the ready handshake while those gates are
/// shut, and arming nothing once `invalidate_pending_start` landed
/// meanwhile. The stop-finalize hold registers with the finalizer, so a quit
/// can be driven against the stop's finalize window.
struct FakeMic {
    s: Arc<Mutex<MicState>>,
    reports_starting: bool,
    permission: Option<Arc<Gate>>,
    ready: Option<Arc<Gate>>,
    stop_gate: StopFinalizeGate,
}

impl FakeMic {
    fn new(finalizer: &Arc<RecordingFinalizer>) -> Arc<FakeMic> {
        Arc::new(FakeMic::build(finalizer, false, None, None))
    }

    /// `_ArmingMic`: parks in its arm window (after the take setup) until
    /// `ready` opens.
    fn arming(finalizer: &Arc<RecordingFinalizer>, ready: &Arc<Gate>) -> Arc<FakeMic> {
        Arc::new(FakeMic::build(finalizer, true, None, Some(ready)))
    }

    /// The take-flow test's recorder, parked on its permission prompt and
    /// its ready handshake while the given gates are shut.
    fn flow(
        finalizer: &Arc<RecordingFinalizer>,
        permission: Option<&Arc<Gate>>,
        ready: Option<&Arc<Gate>>,
    ) -> Arc<FakeMic> {
        Arc::new(FakeMic::build(finalizer, true, permission, ready))
    }

    fn build(
        finalizer: &Arc<RecordingFinalizer>,
        reports_starting: bool,
        permission: Option<&Arc<Gate>>,
        ready: Option<&Arc<Gate>>,
    ) -> FakeMic {
        FakeMic {
            s: Arc::new(Mutex::new(MicState {
                clean: true,
                ..MicState::default()
            })),
            reports_starting,
            permission: permission.cloned(),
            ready: ready.cloned(),
            stop_gate: StopFinalizeGate::new(Arc::clone(finalizer)),
        }
    }

    fn s(&self) -> MutexGuard<'_, MicState> {
        guard(&self.s)
    }
}

impl MicRecorder for FakeMic {
    fn level(&self) -> f64 {
        0.0
    }

    fn revision(&self) -> u64 {
        0
    }

    fn is_recording(&self) -> bool {
        self.s().recording
    }

    fn is_starting(&self) -> bool {
        self.s().starting
    }

    fn feeding_monitor(&self) -> bool {
        false
    }

    fn invalidate_pending_start(&self) {
        let mut s = self.s();
        s.generation += 1;
        s.starting = false;
    }

    fn begin_start(&self, destination: RecordingPathStrategy) -> PendingStart {
        let generation = {
            let mut s = self.s();
            s.start_calls += 1;
            if self.reports_starting {
                s.starting = true;
            }
            s.generation
        };
        let s = Arc::clone(&self.s);
        let permission = self.permission.clone();
        let ready = self.ready.clone();
        let reports = self.reports_starting;
        PendingStart::from_fn(move || {
            let outcome = (|| {
                if let Some(p) = &permission {
                    p.wait();
                    if guard(&s).generation != generation {
                        return Ok(MicRecordingStart::Invalidated);
                    }
                }
                if let Some(failure) = guard(&s).failure {
                    return Ok(MicRecordingStart::Failed(failure));
                }
                let path = destination.create()?;
                if let Some(r) = &ready {
                    r.wait();
                }
                let mut st = guard(&s);
                if st.generation != generation {
                    return Ok(MicRecordingStart::Invalidated);
                }
                if reports {
                    st.starting = false;
                }
                st.last_path = Some(path.clone());
                st.recording = true;
                Ok(MicRecordingStart::Started(path))
            })();
            if reports {
                guard(&s).starting = false;
            }
            outcome
        })
    }

    fn tick(&self, _on_event: &mut dyn FnMut(MicEvent)) {}

    fn next_tick_due(&self) -> Option<Instant> {
        None
    }

    fn stop(&self) -> Option<MicRecordingStop> {
        let mut s = self.s();
        if !s.recording {
            return None;
        }
        s.stop_calls += 1;
        s.recording = false;
        Some(MicRecordingStop {
            path: s.last_path.clone().unwrap_or_default(),
            clean: s.clean,
            dropped_frames: s.dropped_frames,
        })
    }

    fn abort(&self) -> Result<(), DestinationError> {
        Ok(())
    }

    fn begin_stop_finalize(&self) -> StopFinalizeHold {
        self.stop_gate.begin()
    }

    fn dispose(&self) {}
}

// --- The view model's side ----------------------------------------------------

fn at() -> DateTime {
    DateTime::local_ymd(2026, 6, 20)
}

fn take(id: &str, name: &str) -> AudioTrack {
    AudioTrack {
        version_id: Some("v1".to_string()),
        ..AudioTrack::new(id, AudioTrackKind::Vocal, name, at())
    }
}

/// Dart's setUp practice: one interpretation and one empty take, `t1`.
fn practice() -> SongPractice {
    SongPractice {
        active_version_id: Some("v1".to_string()),
        versions: vec![Arc::new(TakeVersion::new("v1", "A", at()))],
        vocal_tracks: vec![Arc::new(take("t1", "Take 1"))],
        ..SongPractice::new("p1", "P", at(), at())
    }
}

/// The take-flow test's seed: two interpretations, `take1` holding a clip,
/// and (when asked) a backing lane.
fn flow_practice(backing: bool) -> SongPractice {
    SongPractice {
        active_version_id: Some("v1".to_string()),
        versions: vec![
            Arc::new(TakeVersion::new("v1", "Interpretation 1", at())),
            Arc::new(TakeVersion::new("v2", "Interpretation 2", at())),
        ],
        accompaniment: backing.then(|| {
            Arc::new(
                AudioTrack::new("acc", AudioTrackKind::Accompaniment, "Backing", at()).with_audio(
                    "/pool/acc.wav",
                    0,
                    5000,
                    Vec::new(),
                ),
            )
        }),
        vocal_tracks: vec![Arc::new(take("take1", "Take 1").with_audio(
            "/pool/take1.wav",
            0,
            1000,
            Vec::new(),
        ))],
        ..SongPractice::new("p1", "Song", at(), at())
    }
}

/// The view model's model, selection and gate.
struct Host {
    practice: SongPractice,
    /// What the store holds (the take-flow tests' repository).
    stored: SongPractice,
    selected: Option<String>,
    position: Duration,
    gate: BusyGate,
    toasts: Vec<String>,
    live: bool,
    /// What `append_vocal` answers without touching the model (Dart's
    /// `'t1'`); `None` appends a new take, as the view model does.
    append_lane: Option<String>,
    appended: usize,
    selections: Vec<(String, Option<String>)>,
}

impl Host {
    fn new(practice: SongPractice) -> Host {
        Host {
            stored: practice.clone(),
            practice,
            selected: Some("t1".to_string()),
            position: Duration::ZERO,
            gate: BusyGate::new(),
            toasts: Vec::new(),
            live: false,
            append_lane: Some("t1".to_string()),
            appended: 0,
            selections: Vec::new(),
        }
    }

    fn clips_of(p: &SongPractice, id: &str) -> usize {
        p.vocal_tracks
            .iter()
            .find(|t| t.id == id)
            .map_or(0, |t| t.clips.len())
    }

    /// The repository's save: a write that lands is what the store holds; a
    /// failed one reverts the model to the store's.
    fn repo_save(&mut self, fail: bool) -> bool {
        if fail {
            self.practice = self.stored.clone();
            false
        } else {
            self.stored = self.practice.clone();
            true
        }
    }
}

impl RecordingHost for Host {
    fn practice(&self) -> Option<&SongPractice> {
        Some(&self.practice)
    }

    fn selected_track_id(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>> {
        if let Some(a) = self.practice.accompaniment.as_ref().filter(|a| a.id == id) {
            return Some(Arc::clone(a));
        }
        self.practice
            .vocal_tracks
            .iter()
            .find(|t| t.id == id)
            .cloned()
    }

    // Applied, as the host does: the finalize checks the model holds the take.
    fn update_track(&mut self, id: &str, update: &dyn Fn(&AudioTrack) -> AudioTrack) {
        self.practice.vocal_tracks = self
            .practice
            .vocal_tracks
            .iter()
            .map(|t| {
                if t.id == id {
                    Arc::new(update(t))
                } else {
                    Arc::clone(t)
                }
            })
            .collect();
    }

    fn append_vocal(&mut self, path: &str, analysis: &WaveformResult, start_ms: i64) -> String {
        self.appended += 1;
        if let Some(lane) = &self.append_lane {
            return lane.clone();
        }
        let id = format!("rec{}", self.appended);
        let track = take(&id, "Take N").with_audio(
            path,
            start_ms,
            analysis.duration_ms,
            analysis.peaks.clone(),
        );
        self.practice.vocal_tracks.push(Arc::new(track));
        id
    }

    fn select_recorded_clip(&mut self, track_id: &str, clip_id: Option<&str>) {
        self.selected = Some(track_id.to_string());
        self.selections
            .push((track_id.to_string(), clip_id.map(str::to_string)));
    }

    fn position(&self) -> Duration {
        self.position
    }

    fn busy_gate(&mut self) -> &mut BusyGate {
        &mut self.gate
    }

    fn live_monitor(&self) -> bool {
        self.live
    }

    fn toast(&mut self, message: &str) {
        self.toasts.push(message.to_string());
    }

    fn friendly_error(&self, error: &str) -> String {
        error.to_string()
    }
}

/// Answers coming back to the UI thread.
enum Msg {
    Setup(TakeSetupRequest),
    StartDone(StartDone),
    JobDone(JobDone),
    Finalized(RecordingOp, BusyOutcome<String>),
    RetryDue(RecordingOp),
    Saved(RecordingOp, bool),
    Created(RecordingOp, Option<String>),
    Discarded(RecordingOp),
    Resynced(RecordingOp, Result<(), SyncError>),
    Permission(bool),
    QuitAsked,
}

/// A save: `Some(ok)` answers now, `None` parks it.
type SavePolicy = Box<dyn FnMut(&mut Host) -> Option<bool>>;

/// The empty take's creation: `Some(id)` answers now, `None` parks it.
type CreatePolicy = Box<dyn FnMut(&mut Host) -> Option<Option<String>>>;

/// The view model and its event loop, around the controller.
struct Screen {
    c: RecordingController,
    host: Host,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    transport: Arc<LogEngine>,
    files: Arc<Files>,
    settings: Arc<Settings>,
    finalizer: Arc<RecordingFinalizer>,
    save: SavePolicy,
    saves: usize,
    parked_saves: Vec<RecordingOp>,
    create_empty: CreatePolicy,
    parked_creates: Vec<RecordingOp>,
    park_discards: bool,
    discarded: Vec<String>,
    parked_discards: Vec<RecordingOp>,
    /// Resyncs answer at once; otherwise each parks (Dart's `mixer.gates`).
    auto_resync: bool,
    resyncs: Vec<Option<RecordingOp>>,
    loop_rearms: usize,
    live_view_stops: usize,
    audio_changed: Vec<String>,
    gate_errors: Vec<String>,
    leaves: usize,
    permission: bool,
}

struct Opts {
    /// `None` records through the real [`MicRecordingController`].
    mic: Option<Arc<dyn MicRecorder>>,
    graph: Option<Arc<Graph>>,
    finalizer: Arc<RecordingFinalizer>,
    analyze: Option<TakeAnalyzer>,
    practice: SongPractice,
}

impl Opts {
    fn new(finalizer: &Arc<RecordingFinalizer>) -> Opts {
        Opts {
            mic: None,
            graph: Some(Graph::new()),
            finalizer: Arc::clone(finalizer),
            analyze: None,
            practice: practice(),
        }
    }

    fn mic(mut self, mic: &Arc<FakeMic>) -> Opts {
        self.mic = Some(Arc::clone(mic) as Arc<dyn MicRecorder>);
        self
    }

    fn graph(mut self, graph: &Arc<Graph>) -> Opts {
        self.graph = Some(Arc::clone(graph));
        self
    }
}

fn finalizer() -> Arc<RecordingFinalizer> {
    Arc::new(RecordingFinalizer::for_testing())
}

impl Screen {
    fn new(opts: Opts) -> Screen {
        let transport = Arc::new(LogEngine::default());
        let files = Arc::new(Files::default());
        let settings = Arc::new(Settings::default());
        let graph = opts.graph.as_ref().map(Graph::service);
        let analyze = opts.analyze.unwrap_or_else(|| {
            // Dart's `_FakeWaveforms`.
            Arc::new(|_: &str| {
                Ok(WaveformResult {
                    peaks: Vec::new(),
                    duration_ms: 100,
                })
            })
        });
        let mut c = RecordingController::with_options(
            Arc::clone(&transport) as Arc<dyn PlaybackEngine>,
            graph,
            Arc::clone(&files) as Arc<dyn AudioFiles>,
            Arc::clone(&settings) as Arc<dyn RecordingSettings>,
            "p1",
            Arc::clone(&opts.finalizer),
            RecordingControllerOptions {
                mic: opts.mic,
                analyze: Some(analyze),
                request_permission: Some(Arc::new(|| true)),
                now: None,
            },
        );
        let (tx, rx) = mpsc::channel();
        let wake = tx.clone();
        c.set_quit_waker(move || {
            let _ = wake.send(Msg::QuitAsked);
        });
        Screen {
            c,
            host: Host::new(opts.practice),
            tx,
            rx,
            transport,
            files,
            settings,
            finalizer: opts.finalizer,
            save: Box::new(|host: &mut Host| Some(host.repo_save(false))),
            saves: 0,
            parked_saves: Vec::new(),
            create_empty: Box::new(|_: &mut Host| Some(None)),
            parked_creates: Vec::new(),
            park_discards: false,
            discarded: Vec::new(),
            parked_discards: Vec::new(),
            auto_resync: false,
            resyncs: Vec::new(),
            loop_rearms: 0,
            live_view_stops: 0,
            audio_changed: Vec::new(),
            gate_errors: Vec::new(),
            leaves: 0,
            permission: true,
        }
    }

    /// What the take-flow tests open: the view model's own take creation
    /// (an empty take saved into the model) and drop, real appends, and the
    /// engine's resyncs answered at once.
    fn flow(mut opts: Opts, backing: bool) -> Screen {
        opts.practice = flow_practice(backing);
        let mut s = Screen::new(opts);
        s.host.selected = Some(if backing { "acc" } else { "take1" }.to_string());
        s.host.append_lane = None;
        s.auto_resync = true;
        s.create_empty = Box::new(|host: &mut Host| {
            let mut t = take("fresh", "Take 2");
            t.version_id = host.practice.active_version().map(|v| v.id.clone());
            host.practice.vocal_tracks.push(Arc::new(t));
            host.selected = Some("fresh".to_string());
            Some(host.repo_save(false).then(|| "fresh".to_string()))
        });
        s
    }

    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }

    fn run(&mut self, effects: Vec<RecordingEffect>) {
        for effect in effects {
            self.effect(effect);
        }
    }

    fn effect(&mut self, effect: RecordingEffect) {
        match effect {
            RecordingEffect::StopLiveView => {
                self.live_view_stops += 1;
                self.host.live = false;
            }
            RecordingEffect::Start(job) => {
                let tx = self.tx.clone();
                let setup = self.tx.clone();
                thread::spawn(move || {
                    let done = job.run(move |request| {
                        let _ = setup.send(Msg::Setup(request));
                    });
                    let _ = tx.send(Msg::StartDone(done));
                });
            }
            RecordingEffect::CreateEmptyTake(op) => match (self.create_empty)(&mut self.host) {
                Some(id) => self.send(Msg::Created(op, id)),
                None => self.parked_creates.push(op),
            },
            RecordingEffect::DiscardEmptyTake(op, id) => {
                self.discarded.push(id.clone());
                // The view model's drop: an empty take leaves the model and
                // the store; one with clips is left alone.
                let empty = |t: &Arc<AudioTrack>| t.id == id && t.clips.is_empty();
                self.host.practice.vocal_tracks.retain(|t| !empty(t));
                self.host.stored.vocal_tracks.retain(|t| !empty(t));
                if self.park_discards {
                    self.parked_discards.push(op);
                } else {
                    self.send(Msg::Discarded(op));
                }
            }
            RecordingEffect::Resync(op) => {
                if self.auto_resync {
                    self.resyncs.push(None);
                    self.send(Msg::Resynced(op, Ok(())));
                } else {
                    self.resyncs.push(Some(op));
                }
            }
            RecordingEffect::SetRecordingPosition(d) => self.host.position = d,
            RecordingEffect::EnsureLoopRange => self.loop_rearms += 1,
            RecordingEffect::Run(job) => {
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let _ = tx.send(Msg::JobDone(job.run()));
                });
            }
            RecordingEffect::Finalize(op, run) => {
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let _ = tx.send(Msg::Finalized(op, run.run()));
                });
            }
            RecordingEffect::RetryFinalize { op, after } => {
                let tx = self.tx.clone();
                thread::spawn(move || {
                    thread::sleep(after);
                    let _ = tx.send(Msg::RetryDue(op));
                });
            }
            RecordingEffect::Save(op) => {
                self.saves += 1;
                match (self.save)(&mut self.host) {
                    Some(ok) => self.send(Msg::Saved(op, ok)),
                    None => self.parked_saves.push(op),
                }
            }
            RecordingEffect::TrackAudioChanged(lane) => self.audio_changed.push(lane),
            RecordingEffect::GateError(e) => self.gate_errors.push(e.to_string()),
            RecordingEffect::RequestMicPermission => self.send(Msg::Permission(self.permission)),
            RecordingEffect::Leave => self.leaves += 1,
        }
    }

    fn dispatch(&mut self, msg: Msg) {
        let h = &mut self.host;
        let effects = match msg {
            Msg::Setup(request) => self.c.take_setup(request, h),
            Msg::StartDone(done) => self.c.start_done(done, h),
            Msg::JobDone(done) => self.c.job_done(done, h),
            Msg::Finalized(op, outcome) => self.c.finalize_ran(op, outcome, h),
            Msg::RetryDue(op) => self.c.finalize_retry_due(op, h),
            Msg::Saved(op, ok) => self.c.saved(op, ok, h),
            Msg::Created(op, id) => self.c.empty_take_created(op, id, h),
            Msg::Discarded(op) => self.c.empty_take_discarded(op),
            Msg::Resynced(op, result) => self.c.resynced(op, result, h),
            Msg::Permission(granted) => self.c.monitor_permission_answered(granted, h),
            Msg::QuitAsked => self.c.serve_quit_requests(),
        };
        self.run(effects);
    }

    /// One turn of the event loop: the mic's due ticks, then the next answer
    /// (waiting up to `wait` for one).
    fn turn(&mut self, wait: Duration) {
        if self
            .c
            .next_mic_tick_due()
            .is_some_and(|due| due <= Instant::now())
        {
            let effects = self.c.mic_tick();
            self.run(effects);
        }
        if let Ok(msg) = self.rx.recv_timeout(wait) {
            self.dispatch(msg);
        }
    }

    /// Spins the event loop until `ready` (bounded).
    fn pump_until(&mut self, ready: impl Fn(&Screen) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready(self) {
            if Instant::now() > deadline {
                return false;
            }
            self.turn(ms(2));
        }
        true
    }

    /// Spins the event loop for `d` (Dart's `Future.delayed`).
    fn pump_for(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while Instant::now() < deadline {
            self.turn(ms(1).min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// REC; the start it made, if one got past the guard.
    fn start(&mut self) -> Option<RecordingOp> {
        let before = self.c.latest_start;
        let effects = self.c.start_recording(&mut self.host);
        self.run(effects);
        let after = self.c.latest_start;
        if after != before {
            after
        } else {
            None
        }
    }

    /// Dart's `await start`.
    fn await_start(&mut self, op: Option<RecordingOp>) {
        let op = op.expect("a start past its guard");
        assert!(
            self.pump_until(|s| s.c.start_flight(op).is_none()),
            "the start never settled"
        );
    }

    /// Stop, without waiting for it.
    fn stop(&mut self) {
        let effects = self.c.stop_recording();
        self.run(effects);
    }

    /// Dart's `await stopRecording()`.
    fn stop_and_wait(&mut self) {
        self.stop();
        assert!(
            self.pump_until(|s| !s.c.is_stopping()),
            "the stop never landed"
        );
    }

    fn parked_resyncs(&self) -> usize {
        self.resyncs.len()
    }

    /// Completes Dart's `mixer.gates[i]`.
    fn complete_resync(&mut self, i: usize, result: Result<(), SyncError>) {
        let op = self.resyncs[i].take().expect("a parked resync");
        let effects = self.c.resynced(op, result, &mut self.host);
        self.run(effects);
    }

    /// Answers the parked save `i`.
    fn answer_save(&mut self, i: usize, ok: bool) {
        let op = self.parked_saves[i];
        let effects = self.c.saved(op, ok, &mut self.host);
        self.run(effects);
    }

    /// REC, its arm and the resync that mutes the punched take, answered
    /// (Dart's `start` → `mixer.gates.single.complete()` → `await start`).
    fn record(&mut self) {
        let op = self.start();
        assert!(self.pump_until(|s| s.parked_resyncs() == 1));
        self.complete_resync(0, Ok(()));
        self.await_start(op);
    }

    fn toggle_monitor(&mut self) {
        let effects = self.c.toggle_input_monitor(&mut self.host);
        self.run(effects);
        self.pump_for(ms(5));
    }

    fn watch_tick(&mut self) {
        let effects = self.c.device_watch_tick(&mut self.host);
        self.run(effects);
    }

    /// A quit on its own thread (Dart's `finalizeAll()` future): whether it
    /// returned.
    fn quit(&self) -> Arc<AtomicBool> {
        let done = Arc::new(AtomicBool::new(false));
        let finalizer = Arc::clone(&self.finalizer);
        let flag = Arc::clone(&done);
        thread::spawn(move || {
            finalizer.finalize_all();
            flag.store(true, Ordering::SeqCst);
        });
        done
    }
}

fn sync_failed() -> SyncError {
    SyncError::Panicked("resync failed".to_string())
}

// --- recording_controller_test.dart -----------------------------------------

#[test]
fn a_take_records_under_a_name_holding_its_lane_and_start_and_notes_where_its_stop_would_place_it()
{
    // What boot recovery places a crashed take by.
    let f = finalizer();
    let graph = Graph::new();
    graph.s().written_frames = 9600; // 200 ms of pre-roll at 48 kHz
    graph.s().latency_frames = 1440; // 30 ms
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.host.position = ms(2500);
    s.record();
    assert_eq!(s.files.placements(), [(Some("t1".to_string()), Some(2500))]);
    assert!(s.pump_until(|s| !s.files.aligned().is_empty()));
    assert_eq!(s.files.aligned(), [2500 - 200 - 30]);
    s.stop_and_wait();
}

#[test]
fn the_recording_offset_moves_a_take_past_the_measured_round_trip() {
    // What no device reports, the singer sets in Settings: the take lands
    // that much earlier than the round trip alone puts it, at its arm's
    // estimate and at its stop.
    let f = finalizer();
    let graph = Graph::new();
    graph.s().written_frames = 9600; // 200 ms of pre-roll at 48 kHz
    graph.s().latency_frames = 1440; // 30 ms
    graph.s().record_offset_ms = 20.0;
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.host.position = ms(2500);
    s.record();
    assert!(s.pump_until(|s| !s.files.aligned().is_empty()));
    assert_eq!(s.files.aligned(), [2500 - 200 - 30 - 20]);
    s.stop_and_wait();
    assert_eq!(s.files.aligned(), [2500 - 200 - 30 - 20]);
}

#[test]
fn a_stop_notes_the_start_its_cut_leaves_before_it_cuts() {
    // A take whose save then failed is recovered at boot: still reading the
    // arm's negative estimate, it was cut a second time.
    let f = finalizer();
    let graph = Graph::new();
    graph.s().written_frames = 9600; // 200 ms of pre-roll at 48 kHz
    graph.s().latency_frames = 1440; // 30 ms
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.host.position = ms(100);
    s.record();
    assert!(s.pump_until(|s| !s.files.aligned().is_empty()));
    assert_eq!(s.files.aligned(), [100 - 200 - 30]);
    s.stop_and_wait();
    assert_eq!(s.files.aligned(), [0]);
}

#[test]
fn happy_path_aba_take_a_continuation_parked_across_a_stop_and_b_arm_never_drives_b_transport() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    // A arms and parks inside its resync.
    let a = s.start();
    assert!(s.pump_until(|s| s.parked_resyncs() == 1));
    assert!(s.c.is_recording());

    // A stops (disarms at once + runs the gated finalize)…
    s.stop_and_wait();
    assert!(!s.c.is_recording());

    // …and B arms, parking on ITS resync.
    let b = s.start();
    assert!(s.pump_until(|s| s.parked_resyncs() == 2));
    assert!(s.c.is_recording(), "B is armed");

    // A's parked continuation resumes NOW — is_recording is true again
    // (B's!), and only the generation token tells the takes apart.
    s.complete_resync(0, Ok(()));
    s.await_start(a);
    assert_eq!(
        s.transport.plays(),
        0,
        "A's stale continuation must not start B's transport"
    );

    // B's own continuation completes normally.
    s.complete_resync(1, Ok(()));
    s.await_start(b);
    assert_eq!(s.transport.plays(), 1);
    s.stop_and_wait();
}

#[test]
fn exception_path_aba_take_a_throwing_after_a_stop_and_b_arm_must_not_stop_take_b() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    let a = s.start();
    assert!(s.pump_until(|s| s.parked_resyncs() == 1));
    s.stop_and_wait();
    assert_eq!(mic.s().stop_calls, 1);

    let b = s.start();
    assert!(s.pump_until(|s| s.parked_resyncs() == 2));
    assert!(s.c.is_recording(), "B is armed");

    // A's parked resync FAILS: the catch sees is_recording == true — B's —
    // and before the generation guard it stopped "the" take, killing B.
    s.complete_resync(0, Err(sync_failed()));
    s.await_start(a); // the controller catches internally; the start settles
    assert_eq!(
        mic.s().stop_calls,
        1,
        "A's failure cleanup must not stop take B"
    );
    assert!(s.c.is_recording(), "B survives");

    s.complete_resync(1, Ok(()));
    s.await_start(b);
    s.stop_and_wait();
}

#[test]
fn stopping_a_take_re_arms_the_loop_wrap_without_waiting_on_a_save() {
    // The take disables the engine's wrap; only the finalize's save used to
    // re-arm it, so a finalize that drops an unreadable partial take (no
    // save) left the loop button lit over a transport that no longer looped.
    // Here the save re-arms nothing, so the stop itself must.
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.record();
    assert_eq!(s.loop_rearms, 0);
    s.stop_and_wait();
    assert_eq!(s.loop_rearms, 1);
}

#[test]
fn a_take_that_never_lands_still_unmutes_the_take_it_punched_into() {
    // The armed take is silent in the engine for the whole recording, and
    // only the finalize's save resynced it. A disk error that leaves an
    // unreadable partial drops the take before that save, so the lane stayed
    // silent on playback with its M button unlit.
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    mic.s().clean = false; // the partial is not even on disk here
    s.record();

    s.stop();
    assert!(s.pump_until(|s| s.parked_resyncs() == 2));
    s.complete_resync(1, Ok(()));
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(s.saves, 0, "nothing landed to save");
    assert_eq!(
        s.host.toasts,
        ["Recording failed. The take could not be saved."]
    );
}

#[test]
fn quit_during_the_stop_finalize_window_waits_for_the_take_save() {
    // The armed window's finalizer is unregistered inside the mic's stop,
    // but the take's row only commits at the end of the finalize tail
    // (analyze + save). A quit landing in that window used to find nothing
    // registered, exit mid-save, and strand the take for boot recovery at
    // timeline zero.
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.save = Box::new(|_: &mut Host| None);
    s.record();
    assert!(s.c.is_recording());

    // Stop: the finalize tail parks on the in-flight save.
    s.stop();
    assert!(s.pump_until(|s| s.saves == 1));

    // Quit now: the quit must hold the exit until the save committed.
    let quit = s.quit();
    s.pump_for(ms(50));
    assert!(
        !quit.load(Ordering::SeqCst),
        "the quit gate must cover the stop-finalize window"
    );

    s.answer_save(0, true);
    assert!(s.pump_until(|_| quit.load(Ordering::SeqCst)));
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(s.saves, 1, "the quit awaited the SAME save, no re-run");
    assert!(!s.finalizer.has_active());
}

#[test]
fn a_finalize_waiting_on_the_busy_gate_still_commits_after_the_screen_is_disposed() {
    // Stop, then Back: the route stops waiting once is_recording reads
    // false. The finalize used to give up on its next retry if the gate was
    // held, stranding the take until boot recovery re-adopted it at 0:00.
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.record();
    assert!(s.c.is_recording());

    let held = s.host.gate.run(|| Ok(())).expect("the gate was free");
    s.stop();
    s.pump_for(ms(20));
    s.c.dispose();
    s.pump_for(ms(120));
    assert_eq!(s.saves, 0, "the finalize waits out the gate");
    let outcome = held.run();
    let _ = s.host.gate.finish(outcome);
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(s.saves, 1, "the take never committed");
}

#[test]
fn stop_while_rec_is_still_arming_cancels_the_take() {
    // The arm window (permission, take setup, the mic's ready handshake) can
    // last a couple of seconds. is_recording is still false there, so Stop
    // only stopped the transport, and the take then armed and rolled anyway.
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::arming(&f, &ready);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    let notified = s.c.revision();

    let start = s.start();
    assert!(s.pump_until(|s| s.c.is_starting_take()));
    assert!(s.c.revision() > notified, "the transport shows the arm");

    s.stop();
    // A REC tap while the cancelled start unwinds must not interleave with
    // its take setup.
    assert_eq!(s.start(), None);
    ready.open();
    assert!(s.pump_until(|s| !s.c.is_starting_take()));

    assert!(!s.c.is_recording(), "nothing records after Stop");
    assert_eq!(mic.s().start_calls, 1);
    s.await_start(start);
    assert!(!s.transport.calls().contains(&"play".to_string()));
}

#[test]
fn stop_while_rec_is_arming_pauses_the_transport_where_it_is() {
    // A REC pressed over a playing backing (a punch on the fly) left the
    // backing playing on after the cancel. Like stopping an armed take, the
    // cancel pauses without rewinding the playhead.
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::arming(&f, &ready);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    let start = s.start();
    assert!(s.pump_until(|s| s.c.is_starting_take()));
    guard(&s.transport.calls).clear();
    s.stop();
    assert_eq!(s.transport.calls(), ["pause"]);
    ready.open();
    s.await_start(start);
}

/// An arming start with nothing armed: REC creates (and saves) the empty
/// take `fresh` to record into.
fn empty_take_screen(f: &Arc<RecordingFinalizer>, mic: &Arc<FakeMic>) -> Screen {
    let mut s = Screen::new(Opts::new(f).mic(mic));
    s.host.selected = None;
    s.create_empty = Box::new(|_: &mut Host| Some(Some("fresh".to_string())));
    s
}

#[test]
fn a_start_dropping_its_empty_take_still_reads_as_starting() {
    // The drop is a structural edit holding the busy gate: a REC in that
    // moment is refused, so the transport must show the take as in flight.
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::arming(&f, &ready);
    let mut s = empty_take_screen(&f, &mic);
    s.park_discards = true;
    let start = s.start();
    assert!(s.pump_until(|s| s.c.is_starting_take()));
    s.stop();
    ready.open();
    assert!(s.pump_until(|s| !s.parked_discards.is_empty()));
    assert!(s.c.is_starting_take(), "the empty take is being dropped");
    assert!(s.c.is_cancelling_start());
    let op = s.parked_discards[0];
    let effects = s.c.empty_take_discarded(op);
    s.run(effects);
    s.await_start(start);
    assert!(!s.c.is_starting_take());
}

#[test]
fn a_rec_that_never_arms_drops_the_empty_take_it_created() {
    // With nothing armed, REC creates (and saves) an empty "Take N" to record
    // into; a start cancelled by Stop, or one that fails, left it behind.
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::arming(&f, &ready);
    let mut s = empty_take_screen(&f, &mic);
    let start = s.start();
    assert!(s.pump_until(|s| s.c.is_starting_take()));
    s.stop();
    ready.open();
    s.await_start(start);
    assert_eq!(s.discarded, ["fresh"]);
}

#[test]
fn a_rec_cancelled_as_the_screen_closes_still_drops_its_empty_take() {
    // Back in REC's arm window cancels the start, and the page is gone
    // before that start unwinds: the drop skipped a disposed controller, so
    // the empty "Take N" REC had saved stayed in the practice for good.
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::arming(&f, &ready);
    let mut s = empty_take_screen(&f, &mic);
    let start = s.start();
    assert!(s.pump_until(|s| s.c.is_starting_take()));
    s.stop();
    s.c.dispose();
    ready.open();
    s.await_start(start);
    assert_eq!(s.discarded, ["fresh"]);
}

#[test]
fn a_rec_that_arms_keeps_the_take_it_created() {
    let f = finalizer();
    let ready = Gate::new();
    ready.open();
    let mic = FakeMic::arming(&f, &ready);
    let mut s = empty_take_screen(&f, &mic);
    s.record();
    assert!(s.c.is_recording());
    assert!(s.discarded.is_empty());
    s.stop_and_wait();
}

#[test]
fn a_rec_after_a_cancelled_arm_records_normally() {
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::arming(&f, &ready);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    let first = s.start();
    assert!(s.pump_until(|s| s.c.is_starting_take()));
    s.stop();
    ready.open();
    assert!(s.pump_until(|s| !s.c.is_starting_take()));
    assert!(!s.c.is_recording());
    s.await_start(first);

    s.record();
    assert!(s.c.is_recording());
    s.stop_and_wait();
}

/// The real recorder over a rolling input, recording from 10 s.
fn rolling_screen(f: &Arc<RecordingFinalizer>, graph: &Arc<Graph>) -> Screen {
    let mut s = Screen::new(Opts::new(f).graph(graph));
    s.host.position = Duration::from_secs(10);
    s
}

/// The pre-roll the arm noted beside the take (the start it would place it
/// at, from 10 s).
fn pre_roll_of(s: &mut Screen) -> i64 {
    assert!(s.pump_until(|s| !s.files.aligned().is_empty()));
    10_000 - s.files.aligned()[0]
}

fn head_ms(s: &Screen) -> i64 {
    s.host.position.as_millis() as i64
}

#[test]
fn the_record_clock_keeps_real_time_through_a_stalled_tick() {
    // The clock counted level ticks, and a periodic timer skips the ticks a
    // stalled isolate overran (a long frame, a GC pause): the playhead, the
    // clock and the forming waveform fell behind the backing for the rest of
    // the take.
    let f = finalizer();
    let graph = Graph::rolling();
    let mut s = rolling_screen(&f, &graph);
    s.record();
    assert!(s.c.is_recording());
    let wall = Instant::now();
    let clock0 = s.c.recording_duration_ms();

    s.pump_for(ms(300));
    let stall = Instant::now();
    while stall.elapsed() < ms(400) {} // the UI thread stalls
    s.pump_for(ms(300));

    let elapsed = wall.elapsed().as_millis() as i64;
    let clock = s.c.recording_duration_ms() - clock0;
    let head = head_ms(&s);
    s.stop_and_wait();
    // Behind by up to ~100 ms with nothing wrong (the clock counts 50 ms
    // level ticks and the head can be a tick stale), so 200 ms leaves room
    // for a loaded runner; the skipped ticks put the bug ~400 ms behind.
    assert!(
        (elapsed - 200..=elapsed + 150).contains(&clock),
        "clock {clock} vs {elapsed}"
    );
    assert!(
        (elapsed - 200..=elapsed + 150).contains(&(head - 10_000)),
        "head {head} vs {elapsed}"
    );
    assert!(
        s.c.recording_peaks().is_empty(),
        "the stop clears the forming waveform"
    );
}

#[test]
fn the_playhead_waits_at_rec_until_the_backing_starts() {
    // The take rolls from the arm, the backing a pre-roll later: a head run
    // from the arm led what was heard, and a lyric stamped at it mid-take
    // saved that late a time.
    let f = finalizer();
    let graph = Graph::rolling();
    let mut s = rolling_screen(&f, &graph);
    let start = s.start();
    assert!(s.pump_until(|s| s.parked_resyncs() == 1));
    // The backing is still being set up while the take records.
    s.pump_for(ms(300));
    assert!(s.c.recording_duration_ms() > 0, "the take ticks");
    assert_eq!(head_ms(&s), 10_000);
    assert_eq!(s.c.live_take_start_ms(), 10_000);

    s.complete_resync(0, Ok(()));
    s.await_start(start);
    // Drawn from where its first frame plays, as the stop will place it.
    let pre_roll = pre_roll_of(&mut s);
    assert!(pre_roll >= 300, "pre-roll {pre_roll}");
    // Within a millisecond: the start is rounded from the fractional
    // pre-roll as a whole, the drawing start from the pre-roll alone, and
    // the two differ on a half-millisecond tie.
    assert!((s.c.live_take_start_ms() - (10_000 - pre_roll)).abs() <= 1);
    s.stop_and_wait();
}

#[test]
fn the_playhead_follows_the_frames_the_take_holds() {
    // The clock starts once the mic reports ready and steps a tick at a
    // time, while the pre-roll is counted in frames from the arm: run by the
    // clock, the head trailed the backing by the difference.
    let f = finalizer();
    let graph = Graph::rolling();
    let mut s = rolling_screen(&f, &graph);
    s.record();
    let pre_roll = pre_roll_of(&mut s);

    graph.set_lead(48_000); // a second the clock never saw
    s.pump_for(ms(200));
    let head = head_ms(&s) - 10_000;
    let held = graph.written_now() * 1000 / 48_000 - pre_roll;
    s.stop_and_wait();
    // + 1: the head is rounded to the nearest ms, while `held` truncates the
    // frames and the pre-roll comes from a start rounded on its own, so the
    // two can differ by 1 ms with no timing error.
    assert!(
        (held - 120..=held + 1).contains(&head),
        "head {head} vs held {held}"
    );
}

#[test]
fn the_playhead_keeps_with_the_backing_through_dropped_frames_and_a_stalled_input() {
    // The ring counts only the frames it kept: frames dropped from a full
    // ring, or an input delivering nothing for a moment, left the head behind
    // the backing for the rest of the take.
    let f = finalizer();
    let graph = Graph::rolling();
    let mut s = rolling_screen(&f, &graph);
    s.record();
    let pre_roll = pre_roll_of(&mut s);
    s.pump_for(ms(300));

    graph.set_dropped(9600); // 200 ms the ring had no room for
    s.pump_for(ms(400));
    let head = head_ms(&s) - 10_000;
    let taken = (graph.written_now() + graph.dropped()) * 1000 / 48_000 - pre_roll;
    // + 1: rounding, as in the test above.
    assert!(
        (taken - 120..=taken + 1).contains(&head),
        "dropped: head {head} vs taken {taken}"
    );

    graph.set_stalled(true);
    s.pump_for(ms(100));
    let stalled_at = head_ms(&s);
    s.pump_for(ms(1500));
    let moved = head_ms(&s) - stalled_at;
    s.stop_and_wait();
    assert!(moved >= 1300, "the head stood still: moved {moved}");
}

#[test]
fn an_input_stall_before_the_backing_starts_leaves_the_head_with_it() {
    // The head ran from the pre-roll in frames, which a stall before the
    // backing started left short of the clock: ahead by the stall all take.
    let f = finalizer();
    let graph = Graph::rolling();
    let mut s = rolling_screen(&f, &graph);
    let start = s.start();
    assert!(s.pump_until(|s| s.parked_resyncs() == 1));
    graph.set_stalled(true);
    s.pump_for(ms(600));
    graph.set_stalled(false);
    s.complete_resync(0, Ok(()));
    s.await_start(start);
    let since_play = Instant::now();

    s.pump_for(ms(500));
    let head = head_ms(&s) - 10_000;
    let played = since_play.elapsed().as_millis() as i64;
    s.stop_and_wait();
    // Ahead only by the hops between play() and since_play's start; the bug
    // put it ahead by the whole 600 ms stall.
    assert!(
        (played - 150..=played + 200).contains(&head),
        "head {head} vs played {played}"
    );
}

#[test]
fn monitoring_outlasts_a_mic_still_warming_up() {
    // A Bluetooth headset switching to its headset profile delivers nothing
    // for a second or two, which the engine allows for before it reports the
    // input lost. The 1 s watchdog counted those silent ticks too and, two
    // ticks after the headphone button, stopped monitoring as
    // "disconnected".
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.toggle_monitor();
    assert!(s.c.input_monitor_enabled());
    s.watch_tick();
    s.watch_tick();
    assert!(s.c.input_monitor_enabled(), "the mic is warming up");
    assert!(s.host.toasts.is_empty());

    graph.s().frames = 4800; // the mic starts delivering
    s.watch_tick();
    assert!(s.c.input_monitor_enabled());
}

#[test]
fn monitor_on_an_engine_that_failed_to_open_stays_off_and_says_so() {
    // An engine that failed to open made Monitor a no-op that still showed
    // ON.
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    if let Some(engine) = &s.c.graph_engine {
        PlaybackEngine::dispose(&**engine); // the engine is not available
    }

    s.toggle_monitor();

    assert!(!s.c.input_monitor_enabled());
    assert_eq!(graph.s().monitor_starts, 0);
    assert_eq!(s.host.toasts, [PlaybackController::NO_DEVICE_MESSAGE]);
}

#[test]
fn the_device_watch_looks_for_a_returning_output_on_every_tick() {
    // The Settings panel can't be open over a song, so a chosen output
    // plugged back in there is only noticed if the song's own device watch
    // looks.
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    assert!(s.c.device_watch_active());
    s.watch_tick();
    s.watch_tick();
    assert!(s.settings.output_refreshes.load(Ordering::SeqCst) >= 2);
}

#[test]
fn an_output_lost_in_the_tick_that_switched_back_to_the_returning_output_is_not_reopened_twice() {
    let f = finalizer();
    let graph = Graph::new();
    graph.s().losing = true;
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.settings.rebinds.store(true, Ordering::SeqCst);
    s.watch_tick();
    s.watch_tick();
    assert!(graph.s().lost_reported);
    assert!(graph.s().output_calls.is_empty(), "the refresh already did");

    s.settings.rebinds.store(false, Ordering::SeqCst);
    graph.s().lost_reported = false;
    s.watch_tick();
    s.watch_tick();
    assert!(graph.s().lost_reported);
    assert_eq!(
        graph.s().output_calls,
        ["headphones"],
        "no refresh did then"
    );
}

#[test]
fn monitoring_still_stops_when_a_delivering_mic_goes_silent() {
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.toggle_monitor();
    let on = graph.polls();
    graph.s().frames = 4800; // delivering by the first tick, then nothing
    s.watch_tick();
    s.watch_tick();
    s.watch_tick();
    assert_eq!(graph.polls(), on + 3);
    assert!(!s.c.input_monitor_enabled());
    assert_eq!(
        s.host.toasts,
        ["Input monitoring stopped: the input device was disconnected."]
    );
}

// --- The rest of the controller ---------------------------------------------

#[test]
fn a_clean_stop_with_ring_overflow_surfaces_the_dropped_frames_notice() {
    // The song side of technique_recording_dropped_frames_test: the WAV is
    // valid yet missing frames as silent holes; the take is kept, but never
    // presented as gapless.
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    mic.s().dropped_frames = 4800;
    s.record();
    s.stop_and_wait();
    assert_eq!(
        s.host.toasts,
        [
            "Recording dropped about 100 ms of audio while the disk was busy. \
          The take has small gaps."
        ]
    );
    assert_eq!(s.saves, 1, "the take is kept");
}

#[test]
fn a_clean_stop_without_overflow_surfaces_no_notice() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.record();
    s.stop_and_wait();
    assert!(s.host.toasts.is_empty());
}

#[test]
fn a_readable_partial_take_after_a_disk_error_is_kept_and_said_to_be_incomplete() {
    let f = finalizer();
    let dir = tempfile::tempdir().expect("a temp dir");
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    guard(&s.files.s).dir = Some(dir.path().to_path_buf());
    mic.s().clean = false;
    s.record();
    let path = mic.s().last_path.clone().expect("armed");
    std::fs::write(&path, b"partial").expect("the partial on disk");
    s.stop_and_wait();
    assert_eq!(
        s.host.toasts,
        ["Recording hit a disk error. The take may be incomplete."]
    );
    assert_eq!(s.saves, 1);
    assert_eq!(Host::clips_of(&s.host.practice, "t1"), 1);
    assert!(guard(&s.files.s).deleted.is_empty());
}

#[test]
fn an_unreadable_partial_take_is_deleted() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.auto_resync = true;
    mic.s().clean = false;
    s.record_auto();
    s.stop_and_wait();
    assert_eq!(guard(&s.files.s).deleted, ["/fake/take_0.wav"]);
    assert_eq!(s.saves, 0);
}

/// A canonical mono float WAV holding `frames` frames: what the recorder
/// writes.
fn recorder_wav(frames: u32) -> Vec<u8> {
    let data = frames * 4;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    b.resize(44 + data as usize, 0);
    b
}

/// After a disk error, a partial take that is a recording but will not
/// analyze now (over the decode's length, a read the disk failed) is NOT
/// deleted: boot recovery holds such a file back and tries again, and the
/// page says so instead of calling the take lost.
#[test]
fn a_recorded_partial_that_will_not_analyze_now_is_left_for_recovery() {
    let f = finalizer();
    let dir = tempfile::tempdir().expect("a temp dir");
    let mic = FakeMic::new(&f);
    let mut opts = Opts::new(&f).mic(&mic);
    opts.analyze = Some(Arc::new(|_: &str| {
        Err("This audio is longer than 60 minutes.".to_string())
    }));
    let mut s = Screen::new(opts);
    guard(&s.files.s).dir = Some(dir.path().to_path_buf());
    mic.s().clean = false;
    s.record_auto();
    let path = mic.s().last_path.clone().expect("armed");
    std::fs::write(&path, recorder_wav(4800)).expect("the partial on disk");
    s.stop_and_wait();
    assert!(Path::new(&path).exists(), "the only copy of the take stays");
    assert!(guard(&s.files.s).deleted.is_empty());
    assert_eq!(s.saves, 0);
    assert_eq!(
        s.host.toasts,
        [
            "Recording hit a disk error, and the take could not be read yet. It was kept, \
             and Vocara will try to recover it the next time it opens."
        ]
    );
}

/// What will not analyze and is not even the recorder's shape is no take:
/// deleted, and said to be lost.
#[test]
fn a_partial_that_is_no_recording_is_deleted() {
    let f = finalizer();
    let dir = tempfile::tempdir().expect("a temp dir");
    let mic = FakeMic::new(&f);
    let mut opts = Opts::new(&f).mic(&mic);
    opts.analyze = Some(Arc::new(|_: &str| Err("unreadable".to_string())));
    let mut s = Screen::new(opts);
    guard(&s.files.s).dir = Some(dir.path().to_path_buf());
    mic.s().clean = false;
    s.record_auto();
    let path = mic.s().last_path.clone().expect("armed");
    std::fs::write(&path, b"partial").expect("the partial on disk");
    s.stop_and_wait();
    assert_eq!(guard(&s.files.s).deleted, [path]);
    assert_eq!(
        s.host.toasts,
        ["Recording failed. The take could not be saved."]
    );
}

#[test]
fn a_clean_take_that_will_not_analyze_fails_the_finalize_through_the_gate() {
    // Dart's finalize threw out of the busy gate, whose `onError` surfaced
    // it; its `finally` still unmuted the take.
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut opts = Opts::new(&f).mic(&mic);
    opts.analyze = Some(Arc::new(|_: &str| Err("too short".to_string())));
    let mut s = Screen::new(opts);
    s.auto_resync = true;
    s.record_auto();
    s.stop_and_wait();
    assert_eq!(s.gate_errors, ["too short"]);
    assert_eq!(s.resyncs.len(), 2, "the take is unmuted");
    assert_eq!(s.saves, 0);
    assert!(!s.host.gate.busy());
}

#[test]
fn a_take_punched_into_an_armed_lane_lands_as_its_own_clip_selected_with_its_lane() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.record();
    s.host.selected = None; // the user tapped elsewhere mid-take
    s.stop_and_wait();
    let t1 = s.host.track_by_id("t1").expect("t1");
    assert_eq!(t1.clips.len(), 1);
    let clip = &t1.clips[0];
    assert_eq!(clip.audio_file_path, "/fake/take_0.wav");
    assert_eq!(clip.duration_ms, 100);
    assert_eq!(
        s.host.selections,
        [("t1".to_string(), Some(clip.id.clone()))]
    );
    // Only after the save: the analysis keys follow the saved path.
    assert_eq!(s.audio_changed, ["t1"]);
    assert_eq!(s.host.appended, 0);
}

#[test]
fn a_take_with_nothing_armed_becomes_a_new_take() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.host.append_lane = None;
    s.record();
    // The target left the model mid-take.
    s.host.practice.vocal_tracks.clear();
    s.stop_and_wait();
    assert_eq!(s.host.appended, 1);
    assert_eq!(s.host.selections, [("rec1".to_string(), None)]);
    assert_eq!(s.audio_changed, ["rec1"]);
    let rec1 = s.host.track_by_id("rec1").expect("the new take");
    assert_eq!(rec1.clips[0].start_ms, 0);
    assert_eq!(rec1.clips[0].audio_file_path, "/fake/take_0.wav");
}

#[test]
fn a_new_take_appended_to_a_lane_the_model_does_not_keep_is_staged_again_then_said_lost() {
    // The view model answering with a lane its model then does not hold:
    // each save judges the take lost by its ids and stages it again, and the
    // last says it will be recovered.
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.record();
    s.host.practice.vocal_tracks.clear();
    s.stop_and_wait();
    assert_eq!(s.host.appended, 3);
    assert_eq!(s.saves, 3);
    assert_eq!(
        s.host.toasts,
        ["Couldn't save the take. It will be recovered the next time Vocara opens."]
    );
}

#[test]
fn the_level_tick_drives_the_meters_on_a_decibel_scale() {
    assert_eq!(RecordingController::meter_from_peak(0.0), 0.0);
    assert_eq!(RecordingController::meter_from_peak(1e-6), 0.0);
    assert_eq!(RecordingController::meter_from_peak(1.0), 1.0);
    assert_eq!(RecordingController::meter_from_peak(2.0), 1.0);
    // -20 dBFS sits 60 % up the 50 dB scale.
    assert!((RecordingController::meter_from_peak(0.1) - 0.6).abs() < 1e-12);
    // Below -50 dBFS it is empty.
    assert_eq!(RecordingController::meter_from_peak(0.001), 0.0);
}

#[test]
fn mic_permission_denied_says_so_and_arms_nothing() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    mic.s().failure = Some(MicRecordingFailure::PermissionDenied);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    let start = s.start();
    s.await_start(start);
    assert_eq!(s.host.toasts, [mic_permission::DENIED_MESSAGE]);
    assert!(!s.c.is_recording());
    assert!(s.files.placements().is_empty(), "no take was set up");
}

#[test]
fn each_failure_of_the_start_says_what_went_wrong() {
    for (failure, message) in [
        (
            MicRecordingFailure::FileCreateFailed,
            "Couldn't create the recording file. Check your free disk space.",
        ),
        (
            MicRecordingFailure::EngineUnavailable,
            "No audio device is available to record with.",
        ),
        (
            MicRecordingFailure::InputNotDelivering,
            "The microphone did not start. Try again, or pick a different input \
             device in Settings.",
        ),
    ] {
        let f = finalizer();
        let mic = FakeMic::new(&f);
        mic.s().failure = Some(failure);
        let mut s = Screen::new(Opts::new(&f).mic(&mic));
        let start = s.start();
        s.await_start(start);
        assert_eq!(s.host.toasts, [message]);
        assert!(!s.finalizer.has_active(), "the quit no longer waits on it");
    }
}

#[test]
fn a_start_without_the_native_engine_turns_the_live_view_off_and_records_nothing() {
    let f = finalizer();
    let mut opts = Opts::new(&f);
    opts.graph = None;
    let mut s = Screen::new(opts);
    assert!(!s.c.device_watch_active());
    let start = s.start();
    assert_eq!(s.live_view_stops, 1);
    assert!(!s.c.is_start_in_flight());
    assert!(start.is_some());
    assert!(!s.finalizer.has_active(), "the quit no longer waits on it");
}

#[test]
fn the_record_limit_stops_the_take_and_keeps_it() {
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.record();
    graph.s().pending_events = GraphAudioPlaybackService::EVENT_RECORD_LIMIT;
    s.watch_tick();
    assert!(!s.c.is_recording());
    assert!(s.c.is_stopping());
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(
        s.host.toasts,
        ["Recording reached the six-hour maximum length. The take was saved up to that point."]
    );
    assert_eq!(s.saves, 1);
}

/// A write to the take's WAV failed mid-take (the disk full): the device
/// watch stops the take on the tick that hears of it, as at the limit, and
/// the finalize keeps what landed and says it may be incomplete.
#[test]
fn a_failed_write_stops_the_take_at_once_and_keeps_what_landed() {
    let f = finalizer();
    let dir = tempfile::tempdir().expect("a temp dir");
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    guard(&s.files.s).dir = Some(dir.path().to_path_buf());
    s.record();
    let path = mic.s().last_path.clone().expect("armed");
    std::fs::write(&path, b"partial").expect("the partial on disk");
    mic.s().clean = false;
    graph.s().pending_events = GraphAudioPlaybackService::EVENT_RECORD_FAILED
        | GraphAudioPlaybackService::EVENT_RECORD_LIMIT;
    s.watch_tick();
    assert!(!s.c.is_recording(), "stopped on the tick");
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(
        s.host.toasts,
        [
            "Recording stopped: the take could not be written to disk, which may be full. \
             What was recorded up to then is being saved.",
            "Recording hit a disk error. The take may be incomplete.",
        ]
    );
    assert_eq!(s.saves, 1, "what landed is kept");
    assert_eq!(mic.s().stop_calls, 1);
}

/// A take is stopped a few seconds short of the hour the take's analysis
/// accepts, and kept: recorded on past it, it failed its analysis at Stop,
/// was never staged, and boot recovery failed it on every launch.
#[test]
fn a_take_reaching_the_length_limit_is_stopped_and_kept() {
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.record();
    let limit_frames = RecordingController::TAKE_LENGTH_LIMIT.as_millis() as u64 * 48;
    graph.s().written_frames = limit_frames - 48_000;
    s.watch_tick();
    assert!(s.c.is_recording(), "a second short of the limit it goes on");
    graph.s().written_frames = limit_frames;
    s.watch_tick();
    assert!(!s.c.is_recording(), "one tick at the limit stops it");
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(
        s.host.toasts,
        ["Takes can be up to 60 minutes. This one stopped at that length and was saved."]
    );
    assert_eq!(s.saves, 1, "the take lands");
    assert!(RecordingController::TAKE_LENGTH_LIMIT < MAX_PLAYBACK_DURATION);
}

#[test]
fn a_lost_input_stops_every_consumer_with_one_notice() {
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.toggle_monitor();
    s.record();
    s.host.live = true;
    let stops = s.live_view_stops;
    graph.s().pending_events = GraphAudioPlaybackService::EVENT_INPUT_LOST;
    s.watch_tick();
    assert!(!s.c.is_recording());
    assert!(!s.c.input_monitor_enabled());
    assert_eq!(s.live_view_stops, stops + 1);
    assert_eq!(
        s.host.toasts,
        ["Recording stopped: the input device was disconnected."]
    );
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(s.saves, 1, "finalizing KEEPS the take captured so far");
}

#[test]
fn the_device_notices_are_said() {
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    graph.s().pending_events = GraphAudioPlaybackService::EVENT_OUTPUT_REROUTED
        | GraphAudioPlaybackService::EVENT_INPUT_FELL_BACK
        | GraphAudioPlaybackService::EVENT_AEC_BYPASSED;
    s.watch_tick();
    assert_eq!(
        s.host.toasts,
        [
            "Playback moved off your selected output device while the microphone \
             is open. Hear-yourself monitoring is muted until it returns.",
            INPUT_FELL_BACK_NOTICE,
            AEC_BYPASSED_NOTICE,
        ]
    );
    s.c.dispose();
    assert!(!s.c.device_watch_active());
}

#[test]
fn the_monitor_meter_holds_its_peak_and_yields_to_a_take() {
    let f = finalizer();
    let graph = Graph::new();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic).graph(&graph));
    s.toggle_monitor();
    assert!(s.c.monitor_meter_active());
    graph.s().level = 0.5;
    let revision = s.c.meter_revision();
    s.c.monitor_meter_tick();
    let held = RecordingController::meter_from_peak(f64::from(0.5f32));
    assert_eq!(s.c.input_level(), held);
    assert!(s.c.meter_revision() > revision);

    // Between syllables it eases back instead of snapping to black.
    graph.s().level = 0.0;
    s.c.monitor_meter_tick();
    assert_eq!(s.c.input_level(), held - 0.05);

    // While a take records, the record-level tick owns the meter.
    s.record();
    s.c.monitor_meter_tick();
    assert_eq!(s.c.input_level(), held - 0.05);
    // Stopping leaves it to monitoring (still on), which reclaims it.
    s.stop_and_wait();
    assert_eq!(s.c.input_level(), held - 0.05);
    s.c.monitor_meter_tick();
    assert_eq!(s.c.input_level(), held - 0.05 - 0.05);

    // A second tap turns it off and drops the meter.
    s.toggle_monitor();
    assert!(!s.c.monitor_meter_active());
    assert_eq!(s.c.input_level(), 0.0);
}

#[test]
fn a_stop_with_monitoring_off_clears_the_meter_the_take_drove() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.record();
    s.c.on_level_tick(0.5);
    assert!(s.c.record_level() > 0.0);
    assert!(s.c.input_level() > 0.0);
    s.stop_and_wait();
    assert_eq!(s.c.record_level(), 0.0);
    assert_eq!(s.c.input_level(), 0.0);
}

#[test]
fn monitor_permission_denied_says_so() {
    let f = finalizer();
    let mic = FakeMic::new(&f);
    let mut s = Screen::new(Opts::new(&f).mic(&mic));
    s.permission = false;
    s.toggle_monitor();
    assert!(!s.c.input_monitor_enabled());
    assert_eq!(s.host.toasts, [mic_permission::DENIED_MESSAGE]);
}

#[test]
fn a_quit_while_a_take_records_through_the_real_recorder_stops_and_lands_it() {
    // The recorder's own quit finalizer, registered for the armed window:
    // `if (isRecording) await stopRecording()`.
    let f = finalizer();
    let graph = Graph::rolling();
    let mut s = rolling_screen(&f, &graph);
    s.record();
    assert!(s.c.is_recording());
    let quit = s.quit();
    assert!(s.pump_until(|_| quit.load(Ordering::SeqCst)));
    assert!(!s.c.is_recording());
    assert!(!s.c.is_stopping(), "the quit waited for the take to land");
    assert_eq!(s.saves, 1);
    assert!(!s.finalizer.has_active());
}

// --- song_practice_take_flow_test.dart (the controller's part) ----------------

#[test]
fn flow_rec_inside_the_arm_window_reads_as_starting_and_lands_on_the_armed_take() {
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::flow(&f, None, Some(&ready));
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    let start = s.start();
    s.pump_for(ms(10));
    assert!(s.c.is_starting_take());
    ready.open();
    s.await_start(start);
    assert_eq!(s.c.record_target_id(&s.host).as_deref(), Some("take1"));
    s.stop_and_wait();
    assert_eq!(Host::clips_of(&s.host.stored, "take1"), 2);
}

#[test]
fn flow_stop_opens_the_stop_window_and_the_take_lands_on_its_lane() {
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.record_auto();
    assert!(s.c.is_recording());
    s.host.position = ms(500);

    s.stop();
    // is_recording already reads false and the gate is free, but the take
    // is not in the model yet.
    assert!(!s.c.is_recording());
    assert!(s.c.is_stopping());
    assert!(!s.host.gate.busy(), "an edit took the gate mid-stop");
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert_eq!(Host::clips_of(&s.host.stored, "take1"), 2);
    assert_eq!(s.host.stored.vocal_tracks.len(), 1);
}

#[test]
fn flow_a_ruler_click_at_the_permission_prompt_still_moves_where_the_take_starts() {
    // The take's start position is taken after the prompt, and the backing
    // seeks back to it once the mic is ready: a click before that point sets
    // it, a click after would be undone.
    let f = finalizer();
    let permission = Gate::new();
    let ready = Gate::new();
    let mic = FakeMic::flow(&f, Some(&permission), Some(&ready));
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    let start = s.start();
    s.pump_for(ms(5));
    assert!(!s.c.holds_start_position(), "at the permission prompt");
    s.host.position = ms(3000); // the seek the playback controller allows

    permission.open();
    assert!(s.pump_until(|s| s.c.holds_start_position()));
    assert_eq!(s.c.recording_start_ms(), 3000);

    ready.open();
    s.await_start(start);
    assert!(!s.c.holds_start_position());
    s.stop_and_wait();
}

#[test]
fn flow_a_cancelled_rec_unwinding_reads_as_cancelling() {
    // Stop was already pressed (REC still at the permission prompt): the
    // view model says to wait, not to stop recording.
    let f = finalizer();
    let permission = Gate::new();
    let mic = FakeMic::flow(&f, Some(&permission), None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    let start = s.start();
    s.pump_for(ms(10));
    s.stop();
    assert!(s.c.is_cancelling_start());
    assert!(s.c.is_starting_take());
    permission.open();
    s.await_start(start);
    assert!(!s.c.is_cancelling_start());
    assert!(!s.c.is_starting_take());
}

#[test]
fn flow_a_take_lands_where_its_pre_roll_puts_it_to_the_millisecond() {
    // 47 frames of pre-roll are 0.98 ms: truncated to whole ms, the take sat
    // a millisecond late.
    let f = finalizer();
    let graph = Graph::new();
    graph.s().written_frames = 47;
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic).graph(&graph), false);
    s.host.position = ms(2000);
    s.record_auto();
    s.host.position = ms(2500);
    s.stop_and_wait();
    let take1 = s.host.stored.vocal_tracks[0].clone();
    assert_eq!(take1.clips.last().map(|c| c.start_ms), Some(1999));
}

#[test]
fn flow_a_take_whose_save_fails_once_is_saved_again_and_lands_on_its_lane() {
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.record_auto();
    s.host.position = ms(500);
    let mut fails = 1;
    s.save = Box::new(move |host: &mut Host| {
        let fail = fails > 0;
        fails -= 1;
        Some(host.repo_save(fail))
    });
    s.stop_and_wait();
    assert_eq!(Host::clips_of(&s.host.stored, "take1"), 2);
    assert_eq!(Host::clips_of(&s.host.practice, "take1"), 2);
}

#[test]
fn flow_a_take_an_edits_failing_save_reverted_away_is_staged_again() {
    // The take's own save landed, but an edit's save queued ahead of it had
    // carried the take, failed and reverted it away: the take's save wrote
    // the model without it, and only a failed take save was checked.
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.record_auto();
    s.host.position = ms(500);
    let mut edits_failing = 1;
    s.save = Box::new(move |host: &mut Host| {
        if edits_failing > 0 {
            // The edits' failing saves ran first and reverted the model.
            edits_failing -= 1;
            host.repo_save(true);
        }
        Some(host.repo_save(false))
    });
    s.stop_and_wait();
    assert_eq!(Host::clips_of(&s.host.practice, "take1"), 2);
    assert_eq!(Host::clips_of(&s.host.stored, "take1"), 2);
    assert!(s.host.toasts.is_empty());
}

#[test]
fn flow_a_take_whose_save_fails_twice_says_it_will_be_recovered_at_the_next_launch() {
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.record_auto();
    let mut fails = 2;
    s.save = Box::new(move |host: &mut Host| {
        let fail = fails > 0;
        fails -= 1;
        Some(host.repo_save(fail))
    });
    s.stop_and_wait();
    assert_eq!(Host::clips_of(&s.host.stored, "take1"), 1);
    assert_eq!(
        s.host.toasts.last().map(String::as_str),
        Some("Couldn't save the take. It will be recovered the next time Vocara opens.")
    );
}

#[test]
fn flow_a_take_whose_save_fails_beside_an_edit_whose_save_fails_too_still_lands_on_its_lane() {
    // The take's save failed while an edit made during it queued a save of
    // its own, holding the take: judged right then, the take was kept, so it
    // was not staged again. The edit's save then failed and reverted the
    // model to the store's, the retry saved that, and the take vanished with
    // no word of it.
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.record_auto();
    s.host.position = ms(500);
    let mut n = 0;
    s.save = Box::new(move |host: &mut Host| {
        n += 1;
        match n {
            // The take's own save fails.
            1 => Some(host.repo_save(true)),
            // The edit's save ahead of the retry fails, then the retry lands.
            2 => {
                host.repo_save(true);
                Some(host.repo_save(false))
            }
            _ => Some(host.repo_save(false)),
        }
    });
    s.stop_and_wait();
    assert_eq!(
        Host::clips_of(&s.host.practice, "take1"),
        2,
        "the take is on its lane"
    );
    assert_eq!(
        Host::clips_of(&s.host.stored, "take1"),
        2,
        "and saved there"
    );
}

#[test]
fn flow_a_take_whose_save_committed_but_reported_failure_is_not_staged_twice() {
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.record_auto();
    let mut first = true;
    s.save = Box::new(move |host: &mut Host| {
        host.stored = host.practice.clone();
        // The store pools the take: it comes back under another path.
        host.practice.vocal_tracks = host
            .practice
            .vocal_tracks
            .iter()
            .map(|t| {
                let mut t = (**t).clone();
                for c in &mut t.clips {
                    c.audio_file_path = c.audio_file_path.replace("/fake/", "/pool/");
                }
                Arc::new(t)
            })
            .collect();
        let ok = !first;
        first = false;
        Some(ok)
    });
    s.stop_and_wait();
    assert_eq!(
        Host::clips_of(&s.host.stored, "take1"),
        2,
        "the take was staged twice"
    );
}

#[test]
fn flow_recs_empty_take_a_failed_save_aborts_the_start_instead_of_arming_the_backing() {
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    assert_eq!(s.c.record_target_id(&s.host), None, "nothing armed");
    s.create_empty = Box::new(|_: &mut Host| Some(None)); // its save fails
    let start = s.start();
    s.await_start(start);
    assert!(!s.c.is_recording());
    assert_eq!(s.resyncs.len(), 0, "the backing was frozen as the target");
    assert!(s.host.toasts.is_empty(), "the failed save said so already");
    assert!(s.discarded.is_empty());
    assert!(s.files.placements().is_empty());
}

#[test]
fn flow_a_quit_while_rec_is_arming_leaves_no_empty_take_behind() {
    // The mic lists a take's quit work only once it arms: a quit in the arm
    // window found nothing to wait for and exited, and the empty "Take N"
    // REC had saved to record into reopened with the practice ever after.
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::flow(&f, None, Some(&ready));
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    let start = s.start();
    assert!(s.pump_until(|s| s.host.stored.vocal_tracks.len() == 2));
    assert!(s.c.is_starting_take());
    let quit = s.quit();
    s.pump_for(ms(10));
    assert!(!quit.load(Ordering::SeqCst), "the quit waits for the start");
    ready.open(); // the open answers (or times out)
    assert!(s.pump_until(|_| quit.load(Ordering::SeqCst)));
    s.await_start(start);
    assert!(!s.c.is_recording());
    let ids: Vec<_> = s
        .host
        .stored
        .vocal_tracks
        .iter()
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(ids, ["take1"]);
}

#[test]
fn flow_a_quit_at_the_permission_prompt_leaves_at_once() {
    // Nothing is set up before the prompt answers, and a cancelled start
    // sets nothing up after it: waiting on the prompt held the quit for its
    // whole budget (seconds) for nothing.
    let f = finalizer();
    let permission = Gate::new();
    let mic = FakeMic::flow(&f, Some(&permission), None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    let start = s.start();
    s.pump_for(ms(10));
    assert!(s.c.is_starting_take());
    let quit = s.quit();
    s.pump_for(ms(50));
    assert!(quit.load(Ordering::SeqCst), "the quit waited on the prompt");
    permission.open(); // answered after the quit
    s.await_start(start);
    assert!(!s.c.is_recording());
    assert_eq!(s.host.stored.vocal_tracks.len(), 1);
}

#[test]
fn flow_a_rec_pressed_during_a_quit_that_spent_its_budget_is_still_cancelled() {
    // The quit runs again on the exit request after the close click; a start
    // registered between the two had no budget of its own there (the first
    // run had spent the quit's), so the exit left at once and the empty take
    // REC had saved stayed for good.
    // Budgets wide enough that a loaded machine's slow arming still lands
    // inside the exit's run (100 ms flaked under CI-like load); the wedged
    // finalizer below spends the first run's whole budget all the same.
    let f = Arc::new(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(400))
            .with_total_budget(ms(400)),
    );
    let ready = Gate::new();
    let mic = FakeMic::flow(&f, None, Some(&ready));
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    // A wedged finalize spends the first run's whole budget.
    let wedged = Gate::new();
    let wedge = {
        let wedged = Arc::clone(&wedged);
        Finalizer::new(move || wedged.wait())
    };
    f.register(&wedge, false);
    f.finalize_all();

    let start = s.start();
    assert!(s.pump_until(|s| s.host.stored.vocal_tracks.len() == 2));
    let exit = s.quit(); // the exit request's run
    s.pump_for(ms(10));
    ready.open();
    assert!(s.pump_until(|_| exit.load(Ordering::SeqCst)));
    s.await_start(start);
    assert!(!s.c.is_recording());
    assert_eq!(s.discarded, ["fresh"]);
    assert_eq!(s.host.stored.vocal_tracks.len(), 1);
    f.unregister(&wedge);
    wedged.open();
}

#[test]
fn flow_a_rec_cancelled_while_arming_leaves_no_empty_take_behind() {
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::flow(&f, None, Some(&ready));
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    assert_eq!(s.c.record_target_id(&s.host), None, "nothing armed");
    let start = s.start();
    assert!(s.pump_until(|s| s.host.practice.vocal_tracks.len() == 2));
    s.stop();
    ready.open();
    s.await_start(start);
    assert!(!s.c.is_recording());
    assert_eq!(s.host.practice.vocal_tracks.len(), 1);
    assert_eq!(s.host.stored.vocal_tracks.len(), 1);
}

#[test]
fn flow_back_while_rec_is_still_arming_cancels_the_take_before_the_page_leaves() {
    // Back only waited for a take already recording. In REC's arm window the
    // page popped at once: the mic's handshake could still land during the
    // exit transition and arm a take (and play the backing) on a page that
    // was leaving, whose dispose then dropped it for boot recovery — beside
    // the empty "Take N" REC had saved to record into.
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::flow(&f, None, Some(&ready));
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    let start = s.start();
    assert!(s.pump_until(|s| s.host.stored.vocal_tracks.len() == 2));
    assert!(s.c.is_starting_take());

    let effects = s.c.stop_before_leaving(); // the system Back
    s.run(effects);
    s.pump_for(ms(20));
    assert_eq!(s.leaves, 0, "the page waits for the start to unwind");
    assert!(s.c.is_leaving());
    ready.open(); // the handshake lands
    s.await_start(start);
    assert!(s.pump_until(|s| s.leaves == 1));
    assert!(!s.c.is_leaving());
    assert!(!s.c.is_recording(), "a take armed on a leaving page");
    assert_eq!(s.host.stored.vocal_tracks.len(), 1);
    assert!(!s.transport.calls().contains(&"play".to_string()));
}

#[test]
fn flow_two_backs_in_recs_arm_window_leave_the_page_once() {
    let f = finalizer();
    let ready = Gate::new();
    let mic = FakeMic::flow(&f, None, Some(&ready));
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    let start = s.start();
    assert!(s.pump_until(|s| s.host.stored.vocal_tracks.len() == 2));
    let effects = s.c.stop_before_leaving();
    s.run(effects);
    s.pump_for(ms(20));
    // Nothing visibly happened: Back again.
    let effects = s.c.stop_before_leaving();
    assert!(effects.is_empty(), "only the first Back leaves");
    ready.open();
    s.await_start(start);
    s.pump_for(ms(20));
    assert_eq!(s.leaves, 1, "double pop");
}

#[test]
fn flow_a_second_back_while_the_take_lands_leaves_the_page_once() {
    let f = finalizer();
    let analyzing = Gate::new();
    let mic = FakeMic::flow(&f, None, None);
    let mut opts = Opts::new(&f).mic(&mic);
    // The finalize's waveform analysis takes a while (a long take).
    let gate = Arc::clone(&analyzing);
    opts.analyze = Some(Arc::new(move |_: &str| {
        gate.wait();
        Ok(WaveformResult {
            peaks: Vec::new(),
            duration_ms: 100,
        })
    }));
    let mut s = Screen::flow(opts, true);
    s.record_auto();
    assert!(s.c.is_recording());

    let effects = s.c.stop_before_leaving();
    s.run(effects);
    s.pump_for(ms(30));
    assert_eq!(s.leaves, 0, "the take is still landing");
    let effects = s.c.stop_before_leaving();
    assert!(effects.is_empty(), "the second Back leaves on its own");
    analyzing.open();
    assert!(s.pump_until(|s| s.leaves == 1));
    s.pump_for(ms(20));
    assert_eq!(s.leaves, 1);
    assert!(!s.c.is_stopping());
}

#[test]
fn flow_a_stopped_take_is_listed_as_landing_until_its_save_lands() {
    // Stop, then Back: the take's finalize (its analysis, then the save)
    // outlives the screen, and a screen reopened on the practice inside
    // those seconds waits for it (the view model's load reads the listing).
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.record_auto();
    s.save = Box::new(|_: &mut Host| None);
    assert!(!f.is_landing("p1"));
    s.stop();
    s.c.dispose();
    assert!(f.is_landing("p1"), "listed from the stop");
    assert!(s.pump_until(|s| s.saves == 1));
    assert!(f.is_landing("p1"), "still landing while it saves");
    let ok = s.host.repo_save(false);
    s.answer_save(0, ok);
    assert!(s.pump_until(|s| !s.c.is_stopping()));
    assert!(!f.is_landing("p1"));
    assert_eq!(Host::clips_of(&s.host.stored, "take1"), 2);
}

#[test]
fn flow_a_take_records_on_the_channel_choice_of_the_device_capturing_now() {
    // The devices are polled only while Settings is open: the take reads
    // the input channel of the device capturing now as it arms.
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    assert_eq!(s.settings.syncs.load(Ordering::SeqCst), 0);
    s.record_auto();
    assert_eq!(s.settings.syncs.load(Ordering::SeqCst), 1);
    s.stop_and_wait();
}

#[test]
fn flow_a_selected_visible_take_can_be_armed_when_the_active_interpretation_does_not_resolve() {
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    s.host.practice.active_version_id = None;
    s.host.selected = Some("take1".to_string());
    assert_eq!(s.c.record_target_id(&s.host).as_deref(), Some("take1"));
}

#[test]
fn the_record_target_is_a_vocal_take_listed_under_the_active_interpretation() {
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    s.host.selected = Some("acc".to_string());
    assert_eq!(s.c.record_target_id(&s.host), None, "a backing lane");
    s.host.selected = Some("gone".to_string());
    assert_eq!(s.c.record_target_id(&s.host), None, "no such lane");
    // A take of another interpretation that resolves is not on screen.
    let mut other = take("other", "Other");
    other.version_id = Some("v2".to_string());
    s.host.practice.vocal_tracks.push(Arc::new(other));
    s.host.selected = Some("other".to_string());
    assert_eq!(s.c.record_target_id(&s.host), None, "not listed");
    s.host.selected = Some("take1".to_string());
    assert_eq!(s.c.record_target_id(&s.host).as_deref(), Some("take1"));
}

#[test]
fn flow_rec_with_nothing_armed_lands_a_new_take_that_takes_the_selection() {
    // REC with a bus selected: the new take, not the bus, owns the
    // inspector (the host's selection clears the bus).
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), true);
    s.create_empty = Box::new(|_: &mut Host| Some(None));
    s.host.selected = None;
    s.create_empty = Box::new(|host: &mut Host| {
        host.practice
            .vocal_tracks
            .push(Arc::new(take("fresh", "Take 2")));
        Some(host.repo_save(false).then(|| "fresh".to_string()))
    });
    s.record_auto();
    s.stop_and_wait();
    let fresh = s.host.track_by_id("fresh").expect("the new take");
    assert_eq!(fresh.clips.len(), 1);
    assert_eq!(s.host.selected.as_deref(), Some("fresh"));
    assert_eq!(s.audio_changed, ["fresh"]);
}

// --- song_practice_structural_edit_test.dart (the controller's part) ----------

#[test]
fn rec_pressed_while_an_edit_runs_says_why_it_does_nothing() {
    // Every structural edit refused in that window said so; REC alone was
    // silent, and read as a broken button.
    let f = finalizer();
    let mic = FakeMic::flow(&f, None, None);
    let mut s = Screen::flow(Opts::new(&f).mic(&mic), false);
    let merge = s
        .host
        .gate
        .run(|| Ok(()))
        .expect("the merge holds the gate");
    assert_eq!(s.start(), None);
    assert_eq!(s.host.toasts, ["Wait for the current edit to finish."]);
    assert!(!s.c.is_starting_take());
    let outcome = merge.run();
    let _ = s.host.gate.finish(outcome);
}

impl Screen {
    /// [`Screen::record`] with the resyncs answered at once.
    fn record_auto(&mut self) {
        self.auto_resync = true;
        let op = self.start();
        self.await_start(op);
    }
}
