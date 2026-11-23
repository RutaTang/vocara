//! The view-model cases of `song_practice_take_flow_test.dart` that record
//! through the view model: a take driven by the real view model and the real
//! recording controller, from REC through the arm window and the take to
//! Stop, the stop window and the take's save — with the user acting in the
//! windows between. (The recording controller's own halves of these cases
//! are its tests'; the cases that need no recording are `interpretations`,
//! `reopen` and `placement`.)
//!
//! The screen is the module above's, opened over the take-flow test's own
//! fakes: [`FlowRepo`] (Dart's `_FakeRepo`: a save gate, failing saves, a
//! commit that reports failure, the asset pool's re-read), [`FlowFiles`]
//! (`_FakeFiles`: recordings and imports numbered) and [`Waveforms`]
//! (`_FakeWaveforms`: an analysis that parks while its gate is shut). The
//! engine is the real [`GraphAudioPlaybackService`] over [`Graph`] (Dart's
//! `_FakeGraphEngine`), so the recording controller has its native engine:
//! bindings that commit every sync, take the transport and the settings'
//! pushes, and report a take's record counters. The recorder is [`FakeMic`]
//! (Dart's `_FakeMic`): its start runs the take setup, parked on its
//! permission prompt and its ready handshake while their gates are shut. It
//! has no level ticks, so no tick is driven; where Dart waited a few
//! milliseconds for its event loop to reach a point, the tests pump the
//! screen until that point is reached (or, where the point is that nothing
//! happens, for the same few milliseconds).

use std::sync::atomic::AtomicBool;

use vocara_model::models::audio_device::AudioDevice;
use vocara_services::engine::{EngineBindings, EngineError};
use vocara_services::graph_audio_playback_service::{GraphAudioPlaybackService, StagingDecode};
use vocara_services::mic_recording_controller::{
    DestinationError, MicEvent, MicRecorder, MicRecordingStart, MicRecordingStop, PendingStart,
    RecordingPathStrategy, StopFinalizeGate, StopFinalizeHold,
};
use vocara_services::recording_finalizer::Finalizer;

use super::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// --- The engine ---------------------------------------------------------------------

/// The native engine as far as the screen touches it around a take (Dart's
/// `_FakeGraphEngine`; with frames in the take's ring as its backing starts,
/// `_PreRollEngine`): the settings' attach and pushes, the transport, the
/// mixer's syncs and live levels, and a take's record counters. Every sync
/// commits, each track staged from the engine's cache (nothing decodes). It
/// keeps each source's last pushed mute (by its file: the bindings never see
/// a lane's id), every seek and every input-channel mode. Anything else (the
/// device watch, the live capture, the real recorder's calls, a take's own
/// effects) is a loud failure, the trait's default: no case here makes one.
#[derive(Default)]
struct Graph {
    s: Mutex<GraphState>,
}

#[derive(Default)]
struct GraphState {
    /// Frames already in the take's ring as its backing starts (the
    /// pre-roll).
    written_frames: u64,
    buses: i32,
    /// The files of the graph being staged and of the live one, by index.
    staging: Vec<String>,
    live: Vec<String>,
    /// A commit is staged and not yet published: the setters address it.
    staged: bool,
    muted: HashMap<String, bool>,
    /// Every seek, in milliseconds.
    seeks: Vec<i64>,
    /// Every input-channel mode pushed, in order.
    input_channels: Vec<u32>,
}

impl Graph {
    fn new() -> Arc<Graph> {
        Arc::new(Graph::default())
    }

    /// `_PreRollEngine`: `frames` in the take's ring as its backing starts.
    fn pre_roll(frames: u64) -> Arc<Graph> {
        let g = Graph::new();
        g.s().written_frames = frames;
        g
    }

    fn s(&self) -> MutexGuard<'_, GraphState> {
        guard(&self.s)
    }

    /// A screen's engine over these bindings.
    fn service(self: &Arc<Self>) -> Arc<GraphAudioPlaybackService> {
        let decode: StagingDecode =
            Arc::new(|path: &str, _: u32| panic!("{path} is staged from the cache, never decoded"));
        Arc::new(GraphAudioPlaybackService::with_bindings(
            Arc::clone(self) as Arc<dyn EngineBindings>,
            Some(decode),
        ))
    }

    /// The last mute pushed for the source playing `path`.
    fn muted(&self, path: &str) -> Option<bool> {
        self.s().muted.get(path).copied()
    }

    fn seeks(&self) -> Vec<i64> {
        self.s().seeks.clone()
    }

    fn input_channels(&self) -> Vec<u32> {
        self.s().input_channels.clone()
    }
}

impl EngineBindings for Graph {
    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn destroy(&self) {}

    // --- The settings' attach and pushes ---------------------------------------------

    fn actual_output_device(&self) -> String {
        String::new()
    }

    fn set_monitor_gain(&self, _gain: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_input_gain(&self, _gain: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_echo_free(&self, _uid: &str) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_input_channel(&self, mode: u32) -> Result<(), EngineError> {
        self.s().input_channels.push(mode);
        Ok(())
    }

    fn set_monitor_reverb(&self, _room: f32, _damping: f32, _wet: f32) -> Result<(), EngineError> {
        Ok(())
    }

    /// Closed until a take opens it.
    fn input_stream_state(&self) -> u64 {
        0
    }

    // --- A take's counters -------------------------------------------------------------

    fn record_latency_frames(&self) -> u32 {
        0
    }

    fn record_written_frames(&self) -> u64 {
        self.s().written_frames
    }

    fn record_dropped_frames(&self) -> u64 {
        0
    }

    // --- The transport -----------------------------------------------------------------

    fn play(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn pause(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn seek(&self, frame: i64) -> Result<(), EngineError> {
        self.s().seeks.push(frame / 48);
        Ok(())
    }

    fn set_loop(&self, _enabled: bool, _start: i64, _end: i64) -> Result<(), EngineError> {
        Ok(())
    }

    fn frame_pos(&self) -> i64 {
        0
    }

    // --- The syncs ---------------------------------------------------------------------

    fn buses_begin(&self) -> Result<(), EngineError> {
        self.s().buses = 0;
        Ok(())
    }

    fn buses_add(&self, _gain: f32, _muted: bool, _output: i32) -> Result<i32, EngineError> {
        let mut s = self.s();
        s.buses += 1;
        Ok(s.buses - 1)
    }

    fn bus_chain(&self, _bus: i32, _tags: &[u8]) -> Result<(), EngineError> {
        Ok(())
    }

    fn tracks_begin(&self) -> Result<(), EngineError> {
        self.s().staging.clear();
        Ok(())
    }

    /// Always a cache hit.
    fn tracks_add(
        &self,
        path: &str,
        _start_ms: i64,
        _gain: f32,
        muted: bool,
        _is_vocal: bool,
        _bus: i32,
    ) -> Result<i32, EngineError> {
        let mut s = self.s();
        s.staging.push(path.to_string());
        s.muted.insert(path.to_string(), muted);
        Ok(s.staging.len() as i32 - 1)
    }

    fn tracks_commit_staged(&self) -> Result<(), EngineError> {
        self.s().staged = true;
        Ok(())
    }

    fn tracks_publish(&self) -> Result<(), EngineError> {
        let mut s = self.s();
        if s.staged {
            s.live = s.staging.clone();
            s.staged = false;
        }
        Ok(())
    }

    fn commit_degraded_mask(&self) -> u64 {
        0
    }

    // --- Live levels (a sync's re-push, the mixer's moves) --------------------------------

    fn set_track_volume(&self, index: i32, _gain: f32, muted: bool) -> Result<(), EngineError> {
        let mut s = self.s();
        let graph = if s.staged { &s.staging } else { &s.live };
        if let Some(path) = usize::try_from(index)
            .ok()
            .and_then(|i| graph.get(i))
            .cloned()
        {
            s.muted.insert(path, muted);
        }
        Ok(())
    }

    fn set_pan(&self, _index: i32, _pan: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_master(&self, _gain: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_reverb(
        &self,
        _room: f32,
        _damping: f32,
        _wet: f32,
        _dry: f32,
        _width: f32,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_volume(&self, _bus: i32, _gain: f32, _muted: bool) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_pan(&self, _bus: i32, _pan: f32) -> Result<(), EngineError> {
        Ok(())
    }

    // A bus's effects (the takes here carry none of their own).

    fn set_bus_eq_band(
        &self,
        _bus: i32,
        _band: i32,
        _freq: f32,
        _q: f32,
        _gain_db: f32,
        _enabled: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_comp(
        &self,
        _bus: i32,
        _threshold_db: f32,
        _ratio: f32,
        _knee_db: f32,
        _attack_ms: f32,
        _release_ms: f32,
        _makeup_db: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_send(&self, _bus: i32, _send_db: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_deesser(
        &self,
        _bus: i32,
        _freq: f32,
        _threshold_db: f32,
        _range_db: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_gate(
        &self,
        _bus: i32,
        _threshold_db: f32,
        _range_db: f32,
        _attack_ms: f32,
        _release_ms: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_delay(
        &self,
        _bus: i32,
        _time_ms: f32,
        _feedback: f32,
        _mix: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_saturation(
        &self,
        _bus: i32,
        _drive_db: f32,
        _mix: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_doubler(
        &self,
        _bus: i32,
        _delay_ms: f32,
        _depth_ms: f32,
        _rate_hz: f32,
        _mix: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_limiter(
        &self,
        _bus: i32,
        _ceiling_db: f32,
        _release_ms: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_reverb_insert(
        &self,
        _bus: i32,
        _room: f32,
        _damping: f32,
        _wet: f32,
        _width: f32,
    ) -> Result<(), EngineError> {
        Ok(())
    }
}

// --- The recorder -------------------------------------------------------------------

/// Recorder double (Dart's `_FakeMic`): its start runs the destination's
/// create (the view model's take setup) and, while `ready` is shut, parks
/// there (the mic's ready handshake, up to two seconds on a cold open)
/// before arming; while `permission` is shut it parks before any setup (the
/// permission prompt, which a cancelled start leaves without setting up). It
/// reads as starting until its start returns, and arms nothing once
/// `invalidate_pending_start` (a Stop in the arm window) landed meanwhile.
/// Its stop hands back a clean take. The stop's finalize holds the quit
/// through the gate a real recorder keeps (Dart's fake ran it unguarded;
/// no case here quits during one).
struct FakeMic {
    s: Arc<Mutex<MicState>>,
    permission: Option<Arc<Gate>>,
    ready: Option<Arc<Gate>>,
    stop_gate: StopFinalizeGate,
}

#[derive(Default)]
struct MicState {
    recording: bool,
    starting: bool,
    /// Moved by `invalidate_pending_start` (Dart's `_invalidated`, which each
    /// start reset): a start that sees it moved arms nothing.
    generation: u64,
    path: Option<String>,
}

impl FakeMic {
    fn new(finalizer: &Arc<RecordingFinalizer>) -> Arc<FakeMic> {
        FakeMic::gated(finalizer, None, None)
    }

    fn gated(
        finalizer: &Arc<RecordingFinalizer>,
        permission: Option<&Arc<Gate>>,
        ready: Option<&Arc<Gate>>,
    ) -> Arc<FakeMic> {
        Arc::new(FakeMic {
            s: Arc::new(Mutex::new(MicState::default())),
            permission: permission.cloned(),
            ready: ready.cloned(),
            stop_gate: StopFinalizeGate::new(Arc::clone(finalizer)),
        })
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
        self.s().generation += 1;
    }

    fn begin_start(&self, destination: RecordingPathStrategy) -> PendingStart {
        let generation = {
            let mut s = self.s();
            s.starting = true;
            s.generation
        };
        let s = Arc::clone(&self.s);
        let permission = self.permission.clone();
        let ready = self.ready.clone();
        PendingStart::from_fn(move || {
            let outcome: Result<MicRecordingStart, DestinationError> = (|| {
                if let Some(p) = &permission {
                    p.wait();
                }
                if guard(&s).generation != generation {
                    return Ok(MicRecordingStart::Invalidated);
                }
                let path = destination.create()?;
                if let Some(r) = &ready {
                    r.wait();
                }
                let mut st = guard(&s);
                if st.generation != generation {
                    return Ok(MicRecordingStart::Invalidated);
                }
                st.starting = false;
                st.recording = true;
                st.path = Some(path.clone());
                Ok(MicRecordingStart::Started(path))
            })();
            guard(&s).starting = false;
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
        s.recording = false;
        Some(MicRecordingStop {
            path: s.path.clone().unwrap_or_default(),
            clean: true,
            dropped_frames: 0,
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

// --- The store, the files, the analysis ----------------------------------------------

/// In-memory repository (Dart's `_FakeRepo` of the take-flow test): `stored`
/// plays the on-disk aggregate.
struct FlowRepo {
    s: Mutex<FlowRepoState>,
}

struct FlowRepoState {
    stored: SongPractice,
    /// Fails only the NEXT save, then heals.
    fail_next_save: bool,
    /// Fails the next this-many saves, then heals.
    fail_saves: usize,
    /// The next save commits, then reports failure (its re-read failed).
    commit_then_fail: bool,
    /// Models the asset pool: a save re-reads every vocal clip at its POOL
    /// path, as the drift repository's hydrate does.
    pooling: bool,
    /// When set, a save parks until it opens (a slow disk).
    save_gate: Option<Arc<Gate>>,
    /// Saves parked on `save_gate` right now.
    parked: usize,
}

impl FlowRepo {
    fn new(stored: SongPractice) -> Arc<FlowRepo> {
        Arc::new(FlowRepo {
            s: Mutex::new(FlowRepoState {
                stored,
                fail_next_save: false,
                fail_saves: 0,
                commit_then_fail: false,
                pooling: false,
                save_gate: None,
                parked: 0,
            }),
        })
    }

    fn s(&self) -> MutexGuard<'_, FlowRepoState> {
        guard(&self.s)
    }

    fn stored(&self) -> SongPractice {
        self.s().stored.clone()
    }

    fn parked(&self) -> usize {
        self.s().parked
    }

    /// Parks the saves from now on (Dart's `repo.saveGate = Completer()`).
    fn hold_saves(&self) -> Arc<Gate> {
        let gate = Gate::new();
        self.s().save_gate = Some(Arc::clone(&gate));
        gate
    }

    /// Dart's `repo.saveGate = null; gate.complete()`.
    fn release(&self, gate: &Gate) {
        self.s().save_gate = None;
        gate.open();
    }

    /// A take as the pool re-reads it: each clip at its pool path.
    fn pooled(t: &AudioTrack) -> AudioTrack {
        let mut t = t.clone();
        for c in &mut t.clips {
            if !c.audio_file_path.starts_with("/pool/") {
                let name = c.audio_file_path.rsplit('/').next().unwrap_or_default();
                c.audio_file_path = format!("/pool/{name}");
            }
        }
        t
    }
}

impl SongPracticeRepository for FlowRepo {
    fn list_practices(&self) -> vocara_store::Result<Vec<SongPractice>> {
        Err(store_error("not faked"))
    }

    fn list_practice_summaries(&self) -> vocara_store::Result<Vec<PracticeSummary>> {
        Err(store_error("not faked"))
    }

    fn get_practice(&self, _id: &str) -> vocara_store::Result<SongPractice> {
        Err(store_error("not faked"))
    }

    fn load_practice(&self, _id: &str) -> vocara_store::Result<Loaded<SongPractice>> {
        Ok(Loaded::new(self.stored(), IndexMap::new()))
    }

    fn create_practice(&self, _title: &str) -> vocara_store::Result<SongPractice> {
        Err(store_error("not faked"))
    }

    fn save_practice(&self, _practice: &SongPractice) -> vocara_store::Result<SongPractice> {
        Err(store_error("not faked"))
    }

    fn save_practice_with_base(
        &self,
        practice: &SongPractice,
        _base_hashes: &IndexMap<String, String>,
    ) -> vocara_store::Result<Loaded<SongPractice>> {
        let gate = {
            let mut s = self.s();
            let gate = s.save_gate.clone();
            if gate.is_some() {
                s.parked += 1;
            }
            gate
        };
        if let Some(gate) = gate {
            gate.wait();
            self.s().parked -= 1;
        }
        let mut s = self.s();
        if s.fail_next_save || s.fail_saves > 0 {
            s.fail_next_save = false;
            s.fail_saves = s.fail_saves.saturating_sub(1);
            return Err(store_error("save failed"));
        }
        s.stored = if s.pooling {
            SongPractice {
                vocal_tracks: practice
                    .vocal_tracks
                    .iter()
                    .map(|t| Arc::new(FlowRepo::pooled(t)))
                    .collect(),
                ..practice.clone()
            }
        } else {
            practice.clone()
        };
        if s.commit_then_fail {
            s.commit_then_fail = false;
            return Err(store_error("re-read failed"));
        }
        Ok(Loaded::new(s.stored.clone(), IndexMap::new()))
    }

    fn delete_practice(&self, _id: &str) -> vocara_store::Result<()> {
        Err(store_error("not faked"))
    }
}

/// Dart's `_FakeFiles`: recordings and imports land numbered.
#[derive(Default)]
struct FlowFiles {
    imports: AtomicUsize,
    recordings: AtomicUsize,
}

impl AudioFiles for FlowFiles {
    fn import_audio(
        &self,
        _practice_id: &str,
        _source_path: &str,
    ) -> Result<String, AudioFileError> {
        let n = self.imports.fetch_add(1, Ordering::SeqCst);
        Ok(format!("/audio/p1/import{n}.wav"))
    }

    fn new_recording_path(
        &self,
        _practice_id: &str,
        _extension: &str,
        _take_id: Option<&str>,
        _start_ms: Option<i64>,
    ) -> Result<String, AudioFileError> {
        let n = self.recordings.fetch_add(1, Ordering::SeqCst);
        Ok(format!("/audio/p1/rec{n}.wav"))
    }

    fn write_aligned_start(&self, _path: &str, _start_ms: i64) -> bool {
        true
    }

    fn is_app_storage(&self, _path: &str) -> bool {
        false
    }

    fn delete_audio(&self, _path: &str) -> Result<(), AudioFileError> {
        Ok(())
    }
}

/// Waveform analysis that parks while its gate is shut (Dart's
/// `_FakeWaveforms`): the seconds a real analyze takes, during which the UI
/// stays interactive. One second of silence.
#[derive(Default)]
struct Waveforms {
    gate: Mutex<Option<Arc<Gate>>>,
}

impl Waveforms {
    fn analyzer(self: &Arc<Self>) -> TakeAnalyzer {
        let waveforms = Arc::clone(self);
        Arc::new(move |_: &str| {
            let gate = guard(&waveforms.gate).clone();
            if let Some(gate) = gate {
                gate.wait();
            }
            Ok(WaveformResult {
                peaks: Vec::new(),
                duration_ms: 1000,
            })
        })
    }

    /// Parks the analyses from now on until the returned gate opens (Dart's
    /// `waveforms.gate = Completer()`).
    fn hold(&self) -> Arc<Gate> {
        let gate = Gate::new();
        *guard(&self.gate) = Some(Arc::clone(&gate));
        gate
    }
}

// --- The world and its screens -----------------------------------------------------------

/// What each test's screens share (Dart's `repo`, `waveforms`,
/// `spectrograms` and the quit's registry).
struct World {
    repo: Arc<FlowRepo>,
    files: Arc<FlowFiles>,
    waveforms: Arc<Waveforms>,
    spectro: Arc<CountingSpectro>,
    finalizer: Arc<RecordingFinalizer>,
}

/// What a screen opens on (Dart's `openVm` arguments): without a graph, the
/// plain transport fake and no recorder.
#[derive(Default)]
struct Open {
    graph: Option<Arc<Graph>>,
    mic: Option<Arc<FakeMic>>,
    app_settings: Option<Arc<AppSettingsService>>,
}

impl Open {
    fn graph(graph: &Arc<Graph>, mic: &Arc<FakeMic>) -> Open {
        Open {
            graph: Some(Arc::clone(graph)),
            mic: Some(Arc::clone(mic)),
            app_settings: None,
        }
    }
}

impl World {
    fn new(seed: SongPractice) -> World {
        World::with_finalizer(seed, &Arc::new(RecordingFinalizer::for_testing()))
    }

    fn with_finalizer(seed: SongPractice, finalizer: &Arc<RecordingFinalizer>) -> World {
        World {
            repo: FlowRepo::new(seed),
            files: Arc::new(FlowFiles::default()),
            waveforms: Arc::new(Waveforms::default()),
            spectro: Arc::new(CountingSpectro::default()),
            finalizer: Arc::clone(finalizer),
        }
    }

    /// A detail screen over the repository, loading in the background
    /// (Dart's `openVm`).
    fn open_vm(&self, open: Open) -> Screen {
        let deps = Deps {
            repository: Arc::clone(&self.repo) as Arc<dyn SongPracticeRepository>,
            technique_repository: Arc::new(FakeTechRepo::default()),
            audio_files: Arc::clone(&self.files) as Arc<dyn AudioFiles>,
            spectrograms: Arc::clone(&self.spectro) as Arc<dyn SpectrogramAnalyses>,
            separation: Arc::new(FakeSourceSeparationService { available: false }),
            app_settings: open.app_settings.unwrap_or_else(settings),
        };
        let engine = match &open.graph {
            Some(graph) => EngineHandle::Graph(graph.service()),
            None => EngineHandle::Other(Arc::new(FakeEngine::default()) as Arc<dyn PlaybackEngine>),
        };
        let options = Options {
            engine: Some(engine),
            edits: Some(Arc::new(FakeEdits::default()) as Arc<dyn AudioEdits>),
            finalizer: Some(Arc::clone(&self.finalizer)),
            probe_duration: Some(Arc::new(|_: &str| Ok(None))),
            analyze: Some(self.waveforms.analyzer()),
            mic: open.mic.map(|mic| mic as Arc<dyn MicRecorder>),
            ..Options::default()
        };
        Screen::open(deps, "p1", options)
    }

    /// [`World::open_vm`], once its load has landed (Dart's `pumpVm`).
    fn pump_vm(&self, open: Open) -> Screen {
        let mut s = self.open_vm(open);
        s.settle();
        assert!(s.vm.practice().is_some(), "initial load should complete");
        s
    }
}

impl Screen {
    /// REC (Dart's `vm.recording.startRecording()`, not awaited).
    fn rec(&mut self) {
        self.act(|vm| vm.recording_do(|r, h| r.start_recording(h)));
    }

    /// Dart's `await start`: the start in flight armed, failed or unwound.
    fn await_start(&mut self) {
        self.pump_until(|s| !s.vm.recording().is_start_in_flight());
    }

    /// Dart's `await vm.recording.startRecording()`.
    fn record(&mut self) {
        self.rec();
        self.await_start();
    }

    /// Dart's `vm.stopRecording()`, not awaited.
    fn stop(&mut self) {
        self.act(|vm| vm.stop_recording());
    }

    fn select(&mut self, track_id: &str) {
        self.act(|vm| vm.select_track(track_id));
    }

    /// Dart's `await vm.mixer.createBus()`: the new bus's id.
    fn create_bus(&mut self) -> String {
        let mut id = None;
        self.act(|vm| {
            vm.mixer_do(|m, h| {
                let made = m.create_bus(h, "");
                id = made.as_ref().map(|(id, _)| id.clone());
                made
            })
        });
        self.settle();
        id.expect("a bus")
    }

    /// Back (the system Back or the back arrow: both end the take first).
    fn back(&mut self) {
        self.act(|vm| vm.stop_before_leaving());
    }

    /// Pumps until `cond` holds, for at most `d`: whether it held.
    fn pump_within(&mut self, d: Duration, cond: impl Fn(&Screen) -> bool) -> bool {
        let deadline = Instant::now() + d;
        while !cond(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            if let Ok(msg) = self.rx.recv_timeout(left.min(ms(2))) {
                self.handle(msg);
            }
        }
        true
    }

    /// Pumps for `d` (Dart's `Future.delayed`), where the point is what does
    /// NOT happen meanwhile.
    fn pump_for(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            if let Ok(msg) = self.rx.recv_timeout(left) {
                self.handle(msg);
            }
        }
    }
}

/// A quit on its own thread (Dart's `finalizer.finalizeAll()` future):
/// whether it returned.
fn quit(finalizer: &Arc<RecordingFinalizer>) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let finalizer = Arc::clone(finalizer);
    let flag = Arc::clone(&done);
    thread::spawn(move || {
        finalizer.finalize_all();
        flag.store(true, Ordering::SeqCst);
    });
    done
}

// --- Fixtures -------------------------------------------------------------------------

fn backing() -> AudioTrack {
    AudioTrack::new("acc", AudioTrackKind::Accompaniment, "Backing", at()).with_audio(
        "/pool/acc.wav",
        0,
        5000,
        Vec::new(),
    )
}

/// Dart's `_seed`: two interpretations, the first on screen holding take 1
/// (and, when asked, a backing lane).
fn seed(accompaniment: bool) -> SongPractice {
    let mut take1 = take("take1", "Take 1", "/pool/take1.wav");
    take1.version_id = Some("v1".into());
    SongPractice {
        versions: vec![
            version("v1", "Interpretation 1"),
            version("v2", "Interpretation 2"),
        ],
        active_version_id: Some("v1".into()),
        accompaniment: accompaniment.then(|| Arc::new(backing())),
        vocal_tracks: vec![Arc::new(take1)],
        ..SongPractice::new("p1", "Song", at(), at())
    }
}

fn ids(p: &SongPractice) -> Vec<String> {
    p.vocal_tracks.iter().map(|t| t.id.clone()).collect()
}

/// How many clips lane `id` holds (0 when it is gone).
fn clips_of(p: &SongPractice, id: &str) -> usize {
    p.vocal_tracks
        .iter()
        .find(|t| t.id == id)
        .map_or(0, |t| t.clips.len())
}

// --- The take window guards every structural edit ------------------------------------------

#[test]
fn stop_an_edit_inside_the_stop_window_is_refused_and_the_take_lands_on_its_lane() {
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1"); // arm the existing take
    s.record();
    assert!(s.vm.is_recording());
    s.at_ms(500);

    s.stop();
    // is_recording already reads false and the gate is free, but the take is
    // not in the model yet.
    assert!(!s.vm.is_recording());
    assert!(s.vm.recording().is_stopping());
    s.act(|vm| vm.delete_track("take1"));
    s.act(|vm| vm.duplicate_track("take1"));
    s.act(|vm| vm.split_track_at_playhead("take1"));
    s.act(|vm| vm.reanalyze_track("take1"));
    let take = s.call(|vm| vm.add_vocal_take("/src/drop.wav"));
    let clip = s.call(|vm| vm.add_clip_to_take("take1", "/src/drop.wav", None));
    assert!(!s.vm.busy(), "an edit took the gate mid-stop");
    s.await_reply(take);
    s.await_reply(clip);
    s.settle();

    let stored = w.repo.stored();
    assert_eq!(clips_of(&stored, "take1"), 2, "the take punched into take1");
    assert_eq!(stored.vocal_tracks.len(), 1);
}

#[test]
fn rec_an_edit_inside_the_arm_window_is_refused() {
    let w = World::new(seed(false));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    s.rec();
    s.pump_for(ms(10));
    assert!(s.vm.recording().is_starting_take());
    s.act(|vm| vm.delete_track("take1"));
    assert_eq!(ids(s.practice()), ["take1"]);
    ready.open();
    s.await_start();
    assert_eq!(s.vm.record_target_id().as_deref(), Some("take1"));
    s.stop();
    s.settle();
    assert_eq!(clips_of(&w.repo.stored(), "take1"), 2);
}

#[test]
fn an_edit_refused_while_a_stopped_take_saves_says_so_not_to_stop_recording() {
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.record();
    let gate = w.waveforms.hold();
    s.stop();
    s.pump_for(ms(10));
    assert!(s.vm.recording().is_stopping());

    let (refused, effects) = s.vm.refuse_structural_edit();
    s.run(effects);
    assert!(refused);
    // The interpretation menu too: it said to stop recording.
    s.act(|vm| vm.versions_do(|v, h| v.create("Fresh", h)));
    assert_eq!(
        s.toasts,
        [
            "Wait for the take to finish saving.",
            "Wait for the take to finish saving."
        ]
    );
    gate.open();
    s.settle();
}

#[test]
fn a_failed_new_take_leaves_a_rec_still_arming_to_make_its_own_take() {
    // Its failure gave the earlier selection back, which the arming REC then
    // read as the take to record into: the take the user meant to be new
    // went into the one selected before.
    let w = World::new(seed(true));
    let permission = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, Some(&permission), None);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    let gate = w.repo.hold_saves();
    w.repo.s().fail_next_save = true;
    s.act(|vm| vm.new_take());
    s.pump_for(ms(5));
    s.rec();
    s.pump_for(ms(5));
    w.repo.release(&gate);
    // Dart's `await adding`: the new take's save is answered and what it
    // started has landed; only the start, parked on its prompt, is left.
    s.pump_until(|s| s.in_flight == 1);
    assert_ne!(s.vm.selected_track_id(), Some("take1"));
    s.stop();
    permission.open();
    s.await_start();
    s.settle();
}

#[test]
fn a_ruler_click_at_the_permission_prompt_still_moves_where_the_take_starts() {
    // The take's start position is taken after the prompt, and the backing
    // seeks back to it once the mic is ready: a click before that point sets
    // it, a click after would be undone — so once its start is taken, one is
    // ignored.
    let w = World::new(seed(false));
    let permission = Gate::new();
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, Some(&permission), Some(&ready));
    let graph = Graph::new();
    let mut s = w.pump_vm(Open::graph(&graph, &mic));
    s.rec();
    s.pump_for(ms(5));

    s.vm.seek(Duration::from_secs(3)); // at the permission prompt
    assert_eq!(graph.seeks(), [3000]);

    permission.open();
    s.pump_until(|s| s.vm.recording().holds_start_position());
    assert!(s.vm.recording().holds_start_position());
    s.vm.seek(Duration::from_secs(5)); // waiting on the mic
    assert_eq!(graph.seeks(), [3000]);

    ready.open();
    s.await_start();
    assert!(!s.vm.recording().holds_start_position());
    s.stop();
    s.settle();
}

#[test]
fn an_edit_refused_while_a_cancelled_rec_unwinds_says_to_wait_not_to_stop_recording() {
    // Stop was already pressed (REC still at the permission prompt): told to
    // stop recording, the user had nothing left to stop.
    let w = World::new(seed(false));
    let permission = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, Some(&permission), None);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.rec();
    s.pump_for(ms(10));
    s.stop();
    assert!(s.vm.recording().is_cancelling_start());

    let (refused, effects) = s.vm.refuse_structural_edit();
    s.run(effects);
    assert!(refused);
    s.act(|vm| vm.versions_do(|v, h| v.create("Fresh", h)));
    assert_eq!(
        s.toasts,
        [
            "Wait for the recording to stop.",
            "Wait for the recording to stop."
        ]
    );
    permission.open();
    s.await_start();
    s.settle();
}

// --- Interpretations wait out the take window ----------------------------------------------

#[test]
fn a_switch_while_the_mic_is_arming_is_refused_and_the_take_lands_on_screen() {
    let w = World::new(seed(false));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    s.rec();
    s.pump_for(ms(10));
    assert!(s.vm.recording().is_starting_take());
    s.act(|vm| vm.versions_do(|v, h| v.set_active("v2", h)));
    assert_eq!(
        InterpretationsController::active(s.vm.practice()).map(|v| v.id.clone()),
        Some("v1".to_string())
    );
    ready.open();
    s.await_start();
    s.stop();
    s.settle();
    let takes = InterpretationsController::vocal_takes(s.vm.practice());
    let take1 = takes.iter().find(|t| t.id == "take1").expect("take 1");
    assert_eq!(take1.clips.len(), 2);
}

#[test]
fn create_and_delete_inside_the_stop_window_are_refused() {
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    s.record();
    s.stop();
    assert!(s.vm.recording().is_stopping());
    s.act(|vm| vm.versions_do(|v, h| v.create("Late", h)));
    s.act(|vm| vm.versions_do(|v, h| v.delete("v1", h)));
    assert!(!s.vm.busy());
    s.settle();
    let stored = w.repo.stored();
    let versions: Vec<&str> = stored.versions.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(versions, ["v1", "v2"]);
    assert_eq!(stored.active_version_id.as_deref(), Some("v1"));
    assert_eq!(clips_of(&stored, "take1"), 2);
}

// --- Where a take lands --------------------------------------------------------------------

#[test]
fn a_take_lands_where_its_pre_roll_puts_it_to_the_millisecond() {
    // 47 frames of pre-roll are 0.98 ms: truncated to whole ms, the take sat
    // a millisecond late.
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::pre_roll(47), &mic));
    s.select("take1");
    s.at_ms(2000);
    s.record();
    s.at_ms(2500);
    s.stop();
    s.settle();
    let stored = w.repo.stored();
    let take1 = stored
        .vocal_tracks
        .iter()
        .find(|t| t.id == "take1")
        .expect("take 1");
    assert_eq!(take1.clips.last().map(|c| c.start_ms), Some(1999));
}

// --- A take whose save fails ----------------------------------------------------------------
//
// The stop's finalize ignored its save's outcome. A failed save reverts the
// model to the store's, so the take vanished from the screen and no later
// save brought it back: only the next launch's recovery re-adopted its file,
// as a new take at 0:00 off its lane.

#[test]
fn a_take_whose_save_fails_once_is_saved_again_and_lands_on_its_lane() {
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    s.record();
    s.at_ms(500);
    w.repo.s().fail_next_save = true;
    s.stop();
    s.settle();
    assert_eq!(clips_of(&w.repo.stored(), "take1"), 2);
    assert_eq!(clips_of(s.practice(), "take1"), 2);
}

#[test]
fn a_take_an_edits_failing_save_reverted_away_is_staged_again() {
    // The take's own save landed, but an edit's save queued ahead of it had
    // carried the take, failed and reverted it away: the take's save wrote
    // the model without it, and only a failed take save was checked.
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    s.record();
    s.at_ms(500);
    let gate = w.repo.hold_saves();
    w.repo.s().fail_saves = 2; // both edits' saves fail; the take's lands
    s.act(|vm| vm.rename_track("take1", "A")); // in flight
    s.pump_until(|_| w.repo.parked() == 1);
    s.act(|vm| vm.rename_track("take1", "B")); // queued, not started
    s.stop();
    // The take's save joins the queue: only the parked write is in flight.
    s.pump_until(|s| s.in_flight == 1);
    w.repo.release(&gate);
    s.settle();

    assert_eq!(clips_of(s.practice(), "take1"), 2);
    assert_eq!(clips_of(&w.repo.stored(), "take1"), 2);
}

#[test]
fn a_take_whose_clip_moves_to_the_pool_on_a_later_save_is_analyzed_there() {
    // An edit made while the take's save ran kept its newer model, holding
    // the take at its staging path, and the take was analyzed there. The
    // edit's save then moved the clip to the pool, which re-keys its
    // analysis, and refreshed only the folded-out lanes: the dock, a lookup
    // by path, sat blank on the take.
    let w = World::new(seed(true));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    w.repo.s().pooling = true;
    s.act(|vm| vm.toggle_inspector_panel()); // closed: the dock alone shows the take
    assert!(!s.vm.inspector_open());
    s.act(|vm| vm.analysis_do(|a, h| a.set_dock_open(true, h))); // the Voiceprint dock, on the take
    s.record(); // nothing armed: a new take
    let take = s.vm.selected_track_id().expect("a take").to_string();
    assert_ne!(take, "take1");
    s.at_ms(500);
    let gate = w.repo.hold_saves();
    s.stop();
    s.pump_until(|_| w.repo.parked() == 1);
    assert_eq!(w.repo.parked(), 1, "the take's save is in flight");
    s.act(|vm| vm.rename_track(&take, "A"));
    w.repo.release(&gate);
    s.settle();

    let lane = s.track(&take);
    assert_eq!(lane.clips.len(), 1);
    let path = &lane.clips[0].audio_file_path;
    assert!(path.starts_with("/pool/"), "{path}");
    assert!(guard(&w.spectro.asked).contains(path));
}

#[test]
fn a_take_whose_save_fails_twice_says_it_will_be_recovered_at_the_next_launch() {
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    s.record();
    w.repo.s().fail_saves = 2;
    s.stop();
    s.settle();
    let stored = w.repo.stored();
    assert_eq!(stored.vocal_tracks.len(), 1);
    assert_eq!(stored.vocal_tracks[0].clips.len(), 1);
    assert_eq!(
        s.toasts.last().map(String::as_str),
        Some("Couldn't save the take. It will be recovered the next time Vocara opens.")
    );
}

#[test]
fn a_take_whose_save_fails_beside_an_edit_whose_save_fails_too_still_lands_on_its_lane() {
    // The take's save failed while an edit made during it queued a save of
    // its own, holding the take: judged right then, the take was kept, so it
    // was not staged again. The edit's save then failed and reverted the
    // model to the store's, the retry saved that, and the take vanished with
    // no word of it.
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.select("take1");
    s.record();
    s.at_ms(500);
    // Park the take's save (a long take's pool copy), then fail it and the
    // edit's save behind it.
    let gate = w.repo.hold_saves();
    w.repo.s().fail_saves = 2;
    s.stop();
    s.pump_until(|_| w.repo.parked() == 1);
    s.act(|vm| vm.rename_track("take1", "Lead")); // an edit while it saves
    w.repo.release(&gate);
    s.settle();
    assert_eq!(
        clips_of(s.practice(), "take1"),
        2,
        "the take is on its lane"
    );
    assert_eq!(clips_of(&w.repo.stored(), "take1"), 2, "and saved there");
}

#[test]
fn a_take_whose_save_committed_but_reported_failure_is_not_staged_twice() {
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    w.repo.s().pooling = true;
    s.select("take1");
    s.record();
    w.repo.s().commit_then_fail = true;
    s.stop();
    s.settle();
    assert_eq!(
        clips_of(&w.repo.stored(), "take1"),
        2,
        "the take was staged twice"
    );
}

// --- REC's empty take ---------------------------------------------------------------------

#[test]
fn recs_empty_take_a_failed_save_aborts_the_start_instead_of_arming_the_backing() {
    let w = World::new(seed(true));
    let graph = Graph::new();
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&graph, &mic));
    assert_eq!(s.vm.selected_track_id(), Some("acc")); // the load selects the backing lane
    assert_eq!(s.vm.record_target_id(), None); // nothing armed
    w.repo.s().fail_next_save = true; // the empty take's save fails
    s.record();
    assert!(!s.vm.is_recording());
    assert_ne!(
        graph.muted("/pool/acc.wav"),
        Some(true),
        "the backing was frozen as the punch-in target and muted"
    );
    assert_eq!(ids(s.practice()), ["take1"]);
}

#[test]
fn a_rec_cancelled_while_arming_gives_the_selection_back() {
    // REC selected the empty take it made (and cleared a bus selection);
    // dropping the take fell back to the first lane instead.
    let w = World::new(seed(false));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    let bus = s.create_bus();
    assert_eq!(s.vm.selected_bus_id(), Some(bus.as_str()));
    s.rec();
    s.pump_until(|s| s.practice().vocal_tracks.len() == 2);
    assert_eq!(s.vm.selected_bus_id(), None, "the empty take took it");
    s.stop();
    ready.open();
    s.await_start();
    assert_eq!(ids(s.practice()), ["take1"]);
    assert_eq!(s.vm.selected_bus_id(), Some(bus.as_str()));
}

#[test]
fn a_lane_picked_while_rec_arms_is_kept_when_the_rec_is_cancelled() {
    // The give-back restored the pre-REC selection over the user's pick.
    let w = World::new(seed(false));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    let bus = s.create_bus();
    assert_eq!(s.vm.selected_bus_id(), Some(bus.as_str()));
    s.rec();
    s.pump_until(|s| s.practice().vocal_tracks.len() == 2);
    s.select("take1"); // picked during the arm window
    s.stop();
    ready.open();
    s.await_start();
    assert_eq!(s.vm.selected_track_id(), Some("take1"));
    assert_eq!(s.vm.selected_bus_id(), None);
}

#[test]
fn a_quit_while_rec_is_arming_leaves_no_empty_take_behind() {
    // The mic lists a take's quit work only once it arms: a quit in the arm
    // window found nothing to wait for and exited, and the empty "Take N" REC
    // had saved to record into reopened with the practice ever after.
    let w = World::new(seed(true));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.rec();
    s.pump_until(|_| w.repo.stored().vocal_tracks.len() == 2);
    assert!(s.vm.recording().is_starting_take());
    assert_eq!(w.repo.stored().vocal_tracks.len(), 2, "REC saved one");
    let quit = quit(&w.finalizer);
    // Dart's 10 ms: the quit's request reached the screen, which cancelled
    // the start.
    s.pump_until(|s| s.vm.recording().is_cancelling_start());
    ready.open(); // the open answers (or times out)
    s.pump_until(|_| quit.load(Ordering::SeqCst));
    s.await_start();
    assert!(!s.vm.is_recording());
    assert_eq!(ids(&w.repo.stored()), ["take1"]);
}

#[test]
fn a_quit_at_the_permission_prompt_leaves_at_once() {
    // Nothing is set up before the prompt answers, and a cancelled start
    // sets nothing up after it: waiting on the prompt held the quit for its
    // whole budget (seconds) for nothing.
    let w = World::new(seed(true));
    let permission = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, Some(&permission), None);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.rec();
    s.pump_for(ms(10));
    assert!(s.vm.recording().is_starting_take());
    let quit = quit(&w.finalizer);
    // Dart gave it 50 ms. A wait on the prompt holds the quit for its whole
    // budget (5 s): a second tells the two apart on a loaded machine too.
    let left = s.pump_within(ms(1000), |_| quit.load(Ordering::SeqCst));
    assert!(left, "the quit waited on the prompt");
    permission.open(); // answered after the quit
    s.await_start();
    assert!(!s.vm.is_recording());
    assert_eq!(ids(&w.repo.stored()), ["take1"]);
}

#[test]
fn a_rec_pressed_during_a_quit_that_spent_its_budget_is_still_cancelled() {
    // The quit runs again on the exit request after the close click; a start
    // registered between the two had no budget of its own there (the first
    // run had spent the quit's), so the exit left at once and the empty take
    // REC had saved stayed for good.
    let f = Arc::new(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(100))
            .with_total_budget(ms(100)),
    );
    let w = World::with_finalizer(seed(true), &f);
    let ready = Gate::new();
    let mic = FakeMic::gated(&f, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    // A wedged finalize spends the first run's whole budget.
    let wedged = Gate::new();
    let wedge = {
        let wedged = Arc::clone(&wedged);
        Finalizer::new(move || wedged.wait())
    };
    f.register(&wedge, false);
    f.finalize_all();

    s.rec();
    s.pump_until(|_| w.repo.stored().vocal_tracks.len() == 2);
    assert_eq!(w.repo.stored().vocal_tracks.len(), 2, "REC saved one");
    let exit = quit(&f); // the exit request's run
                         // Dart's 10 ms: the exit's request cancelled the start.
    s.pump_until(|s| s.vm.recording().is_cancelling_start());
    ready.open();
    s.pump_until(|_| exit.load(Ordering::SeqCst));
    s.await_start();
    assert!(!s.vm.is_recording());
    assert_eq!(ids(&w.repo.stored()), ["take1"]);
    f.unregister(&wedge);
    wedged.open();
}

#[test]
fn a_rec_cancelled_while_arming_leaves_no_empty_take_behind() {
    let w = World::new(seed(true));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    assert_eq!(s.vm.record_target_id(), None); // nothing armed
    s.rec();
    s.pump_until(|s| s.practice().vocal_tracks.len() == 2);
    assert_eq!(s.practice().vocal_tracks.len(), 2, "REC made one");
    s.stop();
    ready.open();
    s.await_start();
    assert!(!s.vm.is_recording());
    assert_eq!(ids(s.practice()), ["take1"]);
    assert_eq!(ids(&w.repo.stored()), ["take1"]);
}

// --- Back in the take window -----------------------------------------------------------------
//
// Dart's widget tests pushed the screen on a router and pressed the system
// Back or the back arrow; both call `stop_before_leaving`, and the page
// leaves on the `Leave` it answers. Which route that pops is the screen's.

#[test]
fn back_while_rec_is_still_arming_cancels_the_take_before_the_page_leaves() {
    // Back only waited for a take already recording. In REC's arm window the
    // page popped at once: the mic's handshake could still land during the
    // exit transition and arm a take (and play the backing) on a page that
    // was leaving, whose dispose then dropped it for boot recovery — beside
    // the empty "Take N" REC had saved to record into.
    let w = World::new(seed(true));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.rec();
    s.pump_until(|_| w.repo.stored().vocal_tracks.len() == 2);
    assert!(s.vm.recording().is_starting_take());
    assert_eq!(w.repo.stored().vocal_tracks.len(), 2, "REC made a take");

    s.back();
    s.pump_for(ms(20));
    ready.open(); // the handshake lands
    s.pump_until(|s| s.leaves > 0 && !s.vm.recording().is_start_in_flight());
    s.settle();
    assert!(!s.vm.is_recording(), "a take armed on a leaving page");
    assert_eq!(s.leaves, 1);
    assert_eq!(ids(&w.repo.stored()), ["take1"]);
}

#[test]
fn two_backs_in_recs_arm_window_leave_the_page_once() {
    let w = World::new(seed(true));
    let ready = Gate::new();
    let mic = FakeMic::gated(&w.finalizer, None, Some(&ready));
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.rec();
    s.pump_until(|_| w.repo.stored().vocal_tracks.len() == 2);
    assert!(s.vm.recording().is_starting_take());
    s.back();
    s.pump_for(ms(20));
    // The page is still there (nothing visibly happened): press Back again.
    if s.leaves == 0 {
        s.back();
        s.pump_for(ms(5));
    }
    ready.open();
    s.await_start();
    s.settle();
    assert_eq!(s.leaves, 1, "double pop");
}

#[test]
fn a_second_back_while_the_take_lands_leaves_the_page_once() {
    let w = World::new(seed(true));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    s.record();
    assert!(s.vm.is_recording());
    // The finalize's waveform analysis takes a while (a long take).
    let gate = w.waveforms.hold();
    s.back();
    s.pump_for(ms(30));
    // The finalize's analysis is still running and nothing visibly happened:
    // the user presses Back again. It popped at once, and the first Back
    // popped a second page when the take landed.
    assert_eq!(s.leaves, 0);
    s.back();
    s.pump_for(ms(5));
    // The finalize lands while the page is still animating out.
    gate.open();
    s.pump_until(|s| !s.vm.recording().is_stopping());
    s.settle();
    assert_eq!(s.leaves, 1);
}

// --- Where the selection goes ------------------------------------------------------------------

#[test]
fn rec_with_a_bus_selected_the_new_take_not_the_bus_owns_the_inspector() {
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    let bus = s.create_bus();
    assert_eq!(s.vm.selected_bus_id(), Some(bus.as_str()));
    s.record();
    s.stop();
    s.settle();
    let take = s.practice().vocal_tracks.last().cloned().expect("a take");
    assert_ne!(take.id, "take1");
    assert_eq!(take.clips.len(), 1);
    assert_eq!(s.vm.selected_track_id(), Some(take.id.as_str()));
    assert_eq!(s.vm.selected_bus_id(), None);
}

#[test]
fn the_dock_follows_a_recording_punched_into_a_take() {
    // Inspector collapsed, dock open on an analysis tab: the dock computes
    // only when pushed, so the push must name the clip as the model shows it.
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open::graph(&Graph::new(), &mic));
    w.repo.s().pooling = true;
    s.act(|vm| vm.toggle_inspector_panel());
    assert!(!s.vm.inspector_open());
    s.act(|vm| vm.analysis_do(|a, h| a.show_dock_tab(DockTab::Spectrogram, h)));

    s.select("take1");
    s.record();
    s.stop();
    s.settle();
    let lane = s.vm.selected_track().expect("a lane");
    let clip_id = s.vm.selected_clip_id().expect("a clip");
    let clip = lane.clip_by_id(clip_id).expect("the clip");
    assert_eq!(clip.audio_file_path, "/pool/rec0.wav");
    assert!(guard(&w.spectro.asked).contains(&"/pool/rec0.wav".to_string()));
}

// --- Reopening while a take lands -----------------------------------------------------------

#[test]
fn a_practice_reopened_while_its_last_take_is_landing_shows_the_take() {
    // Stop, then Back: the route no longer waits once is_recording is false,
    // and the take's finalize (its analysis, then the save) outlives the
    // screen. Reopening the practice inside those seconds read it without
    // the take, which then appeared only after some later edit saved.
    let w = World::new(seed(false));
    let mic = FakeMic::new(&w.finalizer);
    let mut first = w.pump_vm(Open::graph(&Graph::new(), &mic));
    first.select("take1");
    first.record();
    assert!(first.vm.is_recording());

    let gate = w.waveforms.hold(); // the take is being analyzed
    first.stop();
    first.vm.dispose();

    let mut reopened = w.open_vm(Open::default());
    reopened.pump_for(ms(20));
    gate.open();
    first.pump_until(|s| !s.vm.recording().is_stopping());
    first.settle();
    reopened.pump_until(|s| s.vm.practice().is_some() && !s.vm.catching_up());
    reopened.settle();

    assert_eq!(
        clips_of(reopened.practice(), "take1"),
        2,
        "the take it was reopened on"
    );
}

// --- The device capturing now ----------------------------------------------------------------

#[test]
fn a_take_records_on_the_channel_choice_of_the_device_capturing_now() {
    // The devices are polled only while Settings is open: a default that
    // moved to a plugged-in interface since the screen opened left the take
    // on the built-in mic's choice.
    fn input(uid: &str, name: &str, is_default: bool) -> AudioDevice {
        AudioDevice {
            is_default,
            is_input: true,
            ..AudioDevice::new(uid, name)
        }
    }
    let mic_in = |is_default| input("built-in", "Built-in", is_default);
    let iface = |is_default| input("iface", "Interface", is_default);
    let ins = Arc::new(Mutex::new(vec![iface(true)]));
    let settings = Arc::new(AppSettingsService::with_queries(
        Arc::new(LocalStorageService::with_root(std::env::temp_dir().join(
            format!("vocara-vm-test-{}", vocara_model::util::id::new_id()),
        ))),
        DeviceQueries {
            enumerate_outputs: Box::new(Vec::new),
            enumerate_inputs: Box::new({
                let ins = Arc::clone(&ins);
                move || guard(&ins).clone()
            }),
            input_channel_count: Box::new(|_| 2),
        },
    ));
    settings.refresh_devices();
    settings.set_input_channel(GraphAudioPlaybackService::INPUT_CHANNEL_2);
    *guard(&ins) = vec![mic_in(true), iface(false)];
    settings.refresh_devices(); // the default back on the built-in mic

    let w = World::new(seed(false));
    let graph = Graph::new();
    let mic = FakeMic::new(&w.finalizer);
    let mut s = w.pump_vm(Open {
        app_settings: Some(settings),
        ..Open::graph(&graph, &mic)
    });
    *guard(&ins) = vec![mic_in(false), iface(true)];
    s.select("take1");
    s.record();
    assert_eq!(
        graph.input_channels().last(),
        Some(&GraphAudioPlaybackService::INPUT_CHANNEL_2)
    );
    s.stop();
    s.settle();
}
