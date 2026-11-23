//! The view model's tests: the view-model cases of the Dart tests that
//! drive `SongPracticeDetailViewModel` (see each module's doc for its source
//! file), plus the pure helpers'.
//!
//! The [`Screen`] harness plays the screen: it runs each effect as the
//! screen will (a job on a thread of its own, its answer back through a
//! channel) and pumps the answers — Dart's event loop. Dart's completers
//! are [`Gate`]s the fakes park on; a test opens them. Each test passes a
//! private [`RecordingFinalizer`] where Dart used the app-wide instance: the
//! Rust tests run in parallel. A quit's requests to the recording wake the
//! screen, which serves them as its answers come in.

mod export;
mod interpretations;
mod lyrics;
mod placement;
mod pure;
mod reopen;
mod save_base;
mod selection;
mod structural;
mod take_flow;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx};
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_summary::PracticeSummary;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique::Technique;
use vocara_model::models::voice_metrics::VoiceMetrics;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::app_settings_service::{AppSettingsService, DeviceQueries};
use vocara_services::audio_dsp::AnalysisParams;
use vocara_services::audio_edit_service::{AudioEditError, AudioEdits, SplitPaths};
use vocara_services::audio_file_service::{AudioFileError, AudioFiles};
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::playback_engine::{
    Broadcast, BusPlayback, PlaybackEngine, Subscription, TrackPlayback,
};
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::source_separation_service::{
    FakeSourceSeparationService, SeparationError, SeparationResult, SeparationTicket,
    SourceSeparationService,
};
use vocara_services::spectrogram_service::{SpectrogramAnalyses, TakeAnalysisBundle};
use vocara_services::waveform_service::WaveformResult;
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};
use vocara_store::sync::sync_hashes::Loaded;

use super::super::analysis_cache;
use super::super::interpretations_controller::VersionsOp;
use super::super::mixer_controller::EngineHandle;
use super::super::recording_controller::{self, RecordingOp, StartDone, TakeSetupRequest};
use super::*;
use crate::util::busy_gate::BusyOutcome;

pub(super) fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn at() -> DateTime {
    DateTime::from_millis_since_epoch(1_767_225_600_000, false).expect("a date")
}

/// A Dart completer the test completes: [`Gate::wait`] returns once opened.
#[derive(Default)]
pub(super) struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    pub(super) fn new() -> Arc<Gate> {
        Arc::new(Gate::default())
    }

    pub(super) fn open(&self) {
        *guard(&self.open) = true;
        self.changed.notify_all();
    }

    pub(super) fn wait(&self) {
        let mut open = guard(&self.open);
        while !*open {
            open = self.changed.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }
}

// --- Fakes ------------------------------------------------------------------------

/// Just enough engine surface for the playback/mixer controllers: counts
/// graph syncs, remembers the last sync's paths and the last mute pushed
/// for each source (through a sync or a live level push, whichever came
/// last).
#[derive(Default)]
pub(super) struct FakeEngine {
    position: Broadcast<Duration>,
    playing_events: Broadcast<bool>,
    pub syncs: AtomicUsize,
    pub last_paths: Mutex<Vec<String>>,
    pub muted: Mutex<HashMap<String, bool>>,
    pub playing: Mutex<bool>,
    pub calls: Mutex<Vec<String>>,
}

impl FakeEngine {
    pub(super) fn syncs(&self) -> usize {
        self.syncs.load(Ordering::SeqCst)
    }
}

impl PlaybackEngine for FakeEngine {
    fn position(&self) -> Subscription<Duration> {
        self.position.subscribe()
    }

    fn playing(&self) -> Subscription<bool> {
        self.playing_events.subscribe()
    }

    fn current_position(&self) -> Duration {
        Duration::ZERO
    }

    fn is_playing(&self) -> bool {
        *guard(&self.playing)
    }

    fn set_loop(&self, _enabled: bool, _start_ms: i64, _end_ms: i64) {}

    fn sync(&self, tracks: Vec<TrackPlayback>, _buses: Vec<BusPlayback>) {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        *guard(&self.last_paths) = tracks.iter().map(|t| t.path.clone()).collect();
        let mut muted = guard(&self.muted);
        for t in &tracks {
            muted.insert(t.id.clone(), t.muted);
        }
    }

    fn play(&self) {
        *guard(&self.playing) = true;
        guard(&self.calls).push("play".into());
    }

    fn pause(&self) {
        *guard(&self.playing) = false;
        guard(&self.calls).push("pause".into());
    }

    fn seek(&self, position: Duration) {
        guard(&self.calls).push(format!("seek:{}", position.as_millis()));
    }

    fn set_volume(&self, id: &str, _volume: f64, muted: bool) {
        guard(&self.muted).insert(id.to_string(), muted);
    }

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

/// The in-memory store (Dart's `_FakeRepo`): `stored` plays the on-disk
/// aggregate, every write appends to the shared `events` log (interleaved
/// with the file fakes' deletes, so save-vs-delete ORDER is assertable),
/// and the in-flight counter proves saves are serialized.
pub(super) struct FakeRepo {
    pub s: Mutex<RepoState>,
    pub events: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
pub(super) struct RepoState {
    pub stored: Option<SongPractice>,
    pub fail_saves: bool,
    /// Fails only the NEXT save, then heals.
    pub fail_next_save: bool,
    pub save_no: usize,
    pub fail_at: Vec<usize>,
    pub in_flight: usize,
    pub max_in_flight: usize,
    pub saves_started: usize,
    /// When set, loads park on it.
    pub load_gate: Option<Arc<Gate>>,
    /// When set, the next save comes back merged with these words from a
    /// peer.
    pub peer_words_on_next_save: Option<Vec<LyricLine>>,
    /// When set, saves park on it.
    pub save_gate: Option<Arc<Gate>>,
    /// The next load fails.
    pub fail_next_load: bool,
    /// When set, the next load reads the store, then parks on it before it
    /// answers.
    pub stale_load_gate: Option<Arc<Gate>>,
    pub loads: usize,
    /// When set, the next save comes back with this take folded in from
    /// another writer.
    pub fold_in_on_next_save: Option<AudioTrack>,
}

impl FakeRepo {
    pub(super) fn new(stored: SongPractice, events: Arc<Mutex<Vec<String>>>) -> Arc<FakeRepo> {
        Arc::new(FakeRepo {
            s: Mutex::new(RepoState {
                stored: Some(stored),
                ..RepoState::default()
            }),
            events,
        })
    }

    pub(super) fn s(&self) -> MutexGuard<'_, RepoState> {
        guard(&self.s)
    }

    pub(super) fn stored(&self) -> SongPractice {
        self.s().stored.clone().expect("stored")
    }
}

fn store_error(m: &str) -> vocara_store::Error {
    vocara_store::Error::State(m.to_string())
}

impl SongPracticeRepository for FakeRepo {
    fn list_practices(&self) -> vocara_store::Result<Vec<SongPractice>> {
        Ok(self.s().stored.clone().into_iter().collect())
    }

    fn list_practice_summaries(&self) -> vocara_store::Result<Vec<PracticeSummary>> {
        Ok(Vec::new())
    }

    fn get_practice(&self, _id: &str) -> vocara_store::Result<SongPractice> {
        self.s().stored.clone().ok_or_else(|| store_error("gone"))
    }

    fn load_practice(&self, _id: &str) -> vocara_store::Result<Loaded<SongPractice>> {
        let stale = self.s().stale_load_gate.take();
        if let Some(stale) = stale {
            let read = self.stored();
            stale.wait();
            return Ok(Loaded::new(read, IndexMap::new()));
        }
        let gate = self.s().load_gate.clone();
        if let Some(gate) = gate {
            gate.wait();
        }
        let mut s = self.s();
        s.loads += 1;
        if s.fail_next_load {
            s.fail_next_load = false;
            return Err(store_error("load failed"));
        }
        Ok(Loaded::new(
            s.stored.clone().ok_or_else(|| store_error("gone"))?,
            IndexMap::new(),
        ))
    }

    fn create_practice(&self, _title: &str) -> vocara_store::Result<SongPractice> {
        Err(store_error("not faked"))
    }

    fn save_practice(&self, practice: &SongPractice) -> vocara_store::Result<SongPractice> {
        self.s().stored = Some(practice.clone());
        Ok(practice.clone())
    }

    fn save_practice_with_base(
        &self,
        practice: &SongPractice,
        _base_hashes: &IndexMap<String, String>,
    ) -> vocara_store::Result<Loaded<SongPractice>> {
        let gate = {
            let mut s = self.s();
            s.in_flight += 1;
            s.saves_started += 1;
            s.max_in_flight = s.max_in_flight.max(s.in_flight);
            s.save_gate.clone()
        };
        if let Some(gate) = gate {
            gate.wait();
        }
        // Yield, so an overlapping (non-serialized) second save would be
        // caught by the in-flight counter.
        thread::sleep(Duration::from_millis(1));
        let mut s = self.s();
        s.in_flight -= 1;
        s.save_no += 1;
        let no = s.save_no;
        if s.fail_saves || s.fail_next_save || s.fail_at.contains(&no) {
            s.fail_next_save = false;
            guard(&self.events).push("save-failed".into());
            return Err(store_error("save failed"));
        }
        if let Some(peer) = s.peer_words_on_next_save.take() {
            // A concurrent writer's lyrics, kept by the three-way merge.
            let stored = SongPractice {
                lyrics: peer,
                ..practice.clone()
            };
            s.stored = Some(stored.clone());
            guard(&self.events).push("save-merged".into());
            let mut base = IndexMap::new();
            base.insert("lyrics".to_string(), "peer".to_string());
            let mut sent = IndexMap::new();
            sent.insert("lyrics".to_string(), "mine".to_string());
            return Ok(Loaded {
                aggregate: stored,
                base_hashes: base,
                sent_hashes: Some(sent),
                reread: true,
            });
        }
        if let Some(other) = s.fold_in_on_next_save.take() {
            let mut stored = practice.clone();
            stored.vocal_tracks.push(Arc::new(other));
            s.stored = Some(stored.clone());
            guard(&self.events).push("save".into());
            return Ok(Loaded::new(stored, IndexMap::new()));
        }
        s.stored = Some(practice.clone());
        guard(&self.events).push("save".into());
        Ok(Loaded::new(practice.clone(), IndexMap::new()))
    }

    fn delete_practice(&self, _id: &str) -> vocara_store::Result<()> {
        Ok(())
    }
}

/// The technique library (Dart's `_FakeTechRepo`).
#[derive(Default)]
pub(super) struct FakeTechRepo {
    pub list: Mutex<Option<Vec<Technique>>>,
}

impl TechniqueRepository for FakeTechRepo {
    fn list_techniques(&self) -> vocara_store::Result<Vec<Technique>> {
        guard(&self.list)
            .clone()
            .ok_or_else(|| store_error("unreadable"))
    }

    fn get_technique(&self, _id: &str) -> vocara_store::Result<Technique> {
        Err(store_error("not faked"))
    }

    fn load_technique(&self, _id: &str) -> vocara_store::Result<Loaded<Technique>> {
        Err(store_error("not faked"))
    }

    fn save_technique(&self, technique: &Technique) -> vocara_store::Result<Technique> {
        Ok(technique.clone())
    }

    fn save_technique_with_base(
        &self,
        _technique: &Technique,
        _base_hashes: &IndexMap<String, String>,
    ) -> vocara_store::Result<Loaded<Technique>> {
        Err(store_error("not faked"))
    }

    fn delete_technique(&self, _id: &str) -> vocara_store::Result<()> {
        Ok(())
    }
}

/// File fake: imports land as `/import/N.wav`; deletes land in the shared
/// `events` log.
pub(super) struct FakeFiles {
    pub events: Arc<Mutex<Vec<String>>>,
    pub imports: AtomicUsize,
    /// When set, the next import fails reading its source (moved, a
    /// disconnected drive).
    pub fail_next_read: Mutex<bool>,
    /// Paths `is_app_storage` claims.
    pub app_storage: Mutex<Vec<String>>,
}

impl FakeFiles {
    pub(super) fn new(events: Arc<Mutex<Vec<String>>>) -> Arc<FakeFiles> {
        Arc::new(FakeFiles {
            events,
            imports: AtomicUsize::new(0),
            fail_next_read: Mutex::new(false),
            app_storage: Mutex::new(Vec::new()),
        })
    }
}

impl AudioFiles for FakeFiles {
    fn import_audio(
        &self,
        _practice_id: &str,
        _source_path: &str,
    ) -> Result<String, AudioFileError> {
        if std::mem::take(&mut *guard(&self.fail_next_read)) {
            return Err(AudioFileError::Io(std::io::Error::other(
                "Cannot open file",
            )));
        }
        Ok(format!(
            "/import/{}.wav",
            self.imports.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn new_recording_path(
        &self,
        _practice_id: &str,
        _extension: &str,
        _take_id: Option<&str>,
        _start_ms: Option<i64>,
    ) -> Result<String, AudioFileError> {
        Ok("/rec/take.wav".into())
    }

    fn write_aligned_start(&self, _path: &str, _start_ms: i64) -> bool {
        true
    }

    fn is_app_storage(&self, path: &str) -> bool {
        guard(&self.app_storage)
            .iter()
            .any(|p| path.starts_with(p.as_str()))
    }

    fn delete_audio(&self, path: &str) -> Result<(), AudioFileError> {
        guard(&self.events).push(format!("delete:{path}"));
        Ok(())
    }
}

/// Edits fake (Dart's `_FakeEdits`).
#[derive(Default)]
pub(super) struct FakeEdits {
    /// Paths handed to `ensure_streamable_wav`.
    pub streamable_calls: Mutex<Vec<String>>,
    /// When set, `ensure_streamable_wav` reports this path as the
    /// transcoded WAV; `None` = passthrough.
    pub transcode_to: Mutex<Option<String>>,
    /// The transcode fails.
    pub fail_transcode: Mutex<bool>,
    dups: AtomicUsize,
    /// When set, the duplicate call at this 0-based index fails.
    pub fail_duplicate_at: Mutex<Option<usize>>,
    /// When set, each duplicate parks on it.
    pub duplicate_gate: Mutex<Option<Arc<Gate>>>,
}

impl AudioEdits for FakeEdits {
    fn split_wav(
        &self,
        _practice_id: &str,
        _source_path: &str,
        _split_file_ms: i64,
    ) -> Result<SplitPaths, AudioEditError> {
        Ok(SplitPaths {
            first_path: "/edit/first.wav".into(),
            second_path: "/edit/second.wav".into(),
        })
    }

    fn concat_wav(
        &self,
        _practice_id: &str,
        _first: &str,
        _second: &str,
    ) -> Result<String, AudioEditError> {
        Ok("/edit/merged.wav".into())
    }

    fn ensure_streamable_wav(
        &self,
        _practice_id: &str,
        path: &str,
    ) -> Result<String, AudioEditError> {
        guard(&self.streamable_calls).push(path.to_string());
        if *guard(&self.fail_transcode) {
            return Err(AudioEditError::Format("injected transcode failure".into()));
        }
        Ok(guard(&self.transcode_to)
            .clone()
            .unwrap_or_else(|| path.to_string()))
    }

    fn punch_in(
        &self,
        _practice_id: &str,
        _old_path: &str,
        _new_path: &str,
        _split_ms: i64,
    ) -> Result<Option<String>, AudioEditError> {
        Ok(None)
    }

    fn duplicate(&self, _practice_id: &str, _source_path: &str) -> Result<String, AudioEditError> {
        let gate = guard(&self.duplicate_gate).clone();
        if let Some(gate) = gate {
            gate.wait();
        }
        let n = self.dups.fetch_add(1, Ordering::SeqCst);
        if *guard(&self.fail_duplicate_at) == Some(n) {
            return Err(AudioEditError::Io(std::io::Error::other(
                "injected duplicate failure (disk full)",
            )));
        }
        Ok(format!("/edit/dup{n}.wav"))
    }
}

/// Analyses nothing (every compute comes back empty).
pub(super) struct NoSpectrograms;

impl SpectrogramAnalyses for NoSpectrograms {
    fn compute_from_file(
        &self,
        _path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<Arc<Spectrogram>>> {
        Ok(None)
    }

    fn compute_metrics_from_file(
        &self,
        _path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<Arc<VoiceMetrics>>> {
        Ok(None)
    }

    fn compute_both_from_file(
        &self,
        _path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<TakeAnalysisBundle>> {
        Ok(None)
    }
}

/// Records every path an analysis was asked for (Dart's `_CountingSpectro`).
#[derive(Default)]
pub(super) struct CountingSpectro {
    pub asked: Mutex<Vec<String>>,
}

impl SpectrogramAnalyses for CountingSpectro {
    fn compute_from_file(
        &self,
        path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<Arc<Spectrogram>>> {
        guard(&self.asked).push(path.to_string());
        Ok(None)
    }

    fn compute_metrics_from_file(
        &self,
        path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<Arc<VoiceMetrics>>> {
        guard(&self.asked).push(path.to_string());
        Ok(None)
    }

    fn compute_both_from_file(
        &self,
        path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<TakeAnalysisBundle>> {
        guard(&self.asked).push(path.to_string());
        Ok(None)
    }
}

/// Separation that parks until the test releases it (Dart's
/// `_GatedSeparation`).
pub(super) struct GatedSeparation {
    pub gate: Arc<Gate>,
    pub cancels: AtomicUsize,
}

impl SourceSeparationService for GatedSeparation {
    fn is_available(&self) -> bool {
        true
    }

    fn begin(&self) -> SeparationTicket {
        FakeSourceSeparationService::default().begin()
    }

    fn separate_with(
        &self,
        _ticket: SeparationTicket,
        _source: &std::path::Path,
        on_progress: vocara_services::source_separation_service::Progress<'_>,
    ) -> Result<SeparationResult, SeparationError> {
        if let Some(p) = on_progress {
            p(0.2);
        }
        self.gate.wait();
        Ok(SeparationResult {
            instrumental_path: "/stems/inst.wav".into(),
            vocal_path: "/stems/voc.wav".into(),
        })
    }

    fn cancel(&self) {
        self.cancels.fetch_add(1, Ordering::SeqCst);
    }
}

/// A waveform analysis answering one second of silence.
pub(super) fn ok_analyze() -> TakeAnalyzer {
    Arc::new(|_: &str| {
        Ok(WaveformResult {
            peaks: Vec::new(),
            duration_ms: 1000,
        })
    })
}

/// A waveform analysis that parks on `gate`, then answers one second.
pub(super) fn gated_analyze(gate: &Arc<Gate>) -> TakeAnalyzer {
    let gate = Arc::clone(gate);
    Arc::new(move |_: &str| {
        gate.wait();
        Ok(WaveformResult {
            peaks: Vec::new(),
            duration_ms: 1000,
        })
    })
}

/// A waveform analysis that always fails with something unexpected.
pub(super) fn throwing_analyze() -> TakeAnalyzer {
    Arc::new(|_: &str| panic!("injected analyze failure"))
}

pub(super) fn settings() -> Arc<AppSettingsService> {
    let root = std::env::temp_dir().join(format!(
        "vocara-vm-test-{}",
        vocara_model::util::id::new_id()
    ));
    Arc::new(AppSettingsService::with_queries(
        Arc::new(LocalStorageService::with_root(root)),
        DeviceQueries {
            enumerate_outputs: Box::new(Vec::new),
            enumerate_inputs: Box::new(Vec::new),
            input_channel_count: Box::new(|_| 0),
        },
    ))
}

// --- Fixtures --------------------------------------------------------------------

pub(super) fn take(id: &str, name: &str, path: &str) -> AudioTrack {
    AudioTrack::new(id, AudioTrackKind::Vocal, name, at()).with_audio(path, 0, 1000, Vec::new())
}

pub(super) fn clip(id: &str, path: &str, start_ms: i64, duration_ms: i64) -> AudioClip {
    AudioClip {
        id: id.into(),
        audio_file_path: path.into(),
        duration_ms,
        start_ms,
        waveform_peaks: Vec::new(),
    }
}

/// Dart's `_seedPractice`: a single-clip take and a two-clip take, one
/// line of lyrics.
pub(super) fn seed_practice(vocal_tracks: Option<Vec<AudioTrack>>) -> SongPractice {
    let mut p = SongPractice::new("p1", "Song", at(), at());
    p.lyrics = vec![LyricLine::new("la la la", None)];
    p.vocal_tracks = vocal_tracks
        .unwrap_or_else(|| {
            vec![take("single", "Take 1", "/pool/take1.wav"), {
                AudioTrack {
                    clips: vec![
                        clip("clip-a", "/pool/clipA.wav", 0, 400),
                        clip("clip-b", "/pool/clipB.wav", 500, 400),
                    ],
                    ..AudioTrack::new("multi", AudioTrackKind::Vocal, "Take 2", at())
                }
            }]
        })
        .into_iter()
        .map(Arc::new)
        .collect();
    p
}

/// Two adjacent single-clip takes, so merging 'top' down is legal.
pub(super) fn mergeable_takes() -> Vec<AudioTrack> {
    vec![
        take("top", "Top", "/pool/top.wav"),
        take("bottom", "Bottom", "/pool/bottom.wav"),
    ]
}

pub(super) fn version(id: &str, name: &str) -> Arc<TakeVersion> {
    Arc::new(TakeVersion::new(id, name, at()))
}

// --- The screen ------------------------------------------------------------------

/// An answer coming back to the UI thread.
enum Msg {
    Job(Box<JobDone>),
    Analysis(analysis_cache::JobDone),
    CleanedUp(VersionsOp, BusyOutcome<String>),
    Setup(TakeSetupRequest),
    StartDone(StartDone),
    RecordingJob(recording_controller::JobDone),
    Finalized(RecordingOp, BusyOutcome<String>),
    FinalizeRetry(RecordingOp),
    /// A quit asks something of the recording (the quit waker fired).
    QuitAsked,
}

/// The screen around the view model (see the module doc).
pub(super) struct Screen {
    pub vm: SongPracticeDetailViewModel,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    in_flight: usize,
    pub toasts: Vec<String>,
    pub replies: Vec<Reply>,
    pub leaves: usize,
}

impl Screen {
    pub(super) fn open(deps: Deps, practice_id: &str, options: Options) -> Screen {
        let (mut vm, effects) = SongPracticeDetailViewModel::new(deps, practice_id, options);
        let (tx, rx) = mpsc::channel();
        let wake = tx.clone();
        vm.set_quit_waker(move || {
            let _ = wake.send(Msg::QuitAsked);
        });
        let mut s = Screen {
            vm,
            tx,
            rx,
            in_flight: 0,
            toasts: Vec::new(),
            replies: Vec::new(),
            leaves: 0,
        };
        s.run(effects);
        s
    }

    fn spawn(&mut self, work: impl FnOnce() -> Msg + Send + 'static) {
        self.in_flight += 1;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(work());
        });
    }

    /// Runs the effects as the screen will.
    pub(super) fn run(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Run(job) => self.spawn(move || Msg::Job(Box::new(job.run()))),
                Effect::Analysis(job) => self.spawn(move || Msg::Analysis(job.run())),
                Effect::AnalysisMicPermission => {
                    let fx = self.vm.analysis_mic_permission_answered(true);
                    self.run(fx);
                }
                Effect::LaneHealRetry { .. } => {}
                Effect::StartRecording(job) => {
                    // The setup request is not counted: the start waits for
                    // its answer, so it comes while the start is in flight.
                    let tx = self.tx.clone();
                    self.spawn(move || {
                        Msg::StartDone(job.run(move |request| {
                            let _ = tx.send(Msg::Setup(request));
                        }))
                    });
                }
                Effect::RecordingWork(job) => self.spawn(move || Msg::RecordingJob(job.run())),
                Effect::FinalizeTake(op, run) => self.spawn(move || Msg::Finalized(op, run.run())),
                Effect::FinalizeRetry { op, after } => self.spawn(move || {
                    thread::sleep(after);
                    Msg::FinalizeRetry(op)
                }),
                Effect::MonitorMicPermission => {
                    let fx = self.vm.monitor_permission_answered(true);
                    self.run(fx);
                }
                Effect::InterpretationsCleanUp(op, run) => {
                    self.spawn(move || Msg::CleanedUp(op, run.run()));
                }
                Effect::Toast(m) => self.toasts.push(m),
                Effect::Leave => self.leaves += 1,
                Effect::Reply(r) => self.replies.push(r),
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        // Neither is counted: a setup request comes while its start is in
        // flight, and a quit's request from the quit's own thread.
        if !matches!(msg, Msg::Setup(_) | Msg::QuitAsked) {
            self.in_flight -= 1;
        }
        let fx = match msg {
            Msg::Job(done) => self.vm.job_done(*done),
            Msg::Analysis(done) => self.vm.analysis_job_done(done),
            Msg::CleanedUp(op, outcome) => self.vm.interpretations_cleaned_up(op, outcome),
            Msg::Setup(request) => self.vm.take_setup(request),
            Msg::StartDone(done) => self.vm.start_done(done),
            Msg::RecordingJob(done) => self.vm.recording_job_done(done),
            Msg::Finalized(op, outcome) => self.vm.finalize_ran(op, outcome),
            Msg::FinalizeRetry(op) => self.vm.finalize_retry_due(op),
            Msg::QuitAsked => self.vm.serve_quit_requests(),
        };
        self.run(fx);
    }

    /// Runs a view model call and its effects.
    pub(super) fn act(&mut self, f: impl FnOnce(&mut SongPracticeDetailViewModel) -> Vec<Effect>) {
        let fx = f(&mut self.vm);
        self.run(fx);
    }

    /// Starts an awaited call; its reply comes in later.
    pub(super) fn call(
        &mut self,
        f: impl FnOnce(&mut SongPracticeDetailViewModel) -> Started,
    ) -> Call {
        let started = f(&mut self.vm);
        self.run(started.effects);
        started.call
    }

    /// Puts the playhead at `ms` (Dart's `position.value = ...`).
    pub(super) fn at_ms(&mut self, ms: u64) {
        let fx = self
            .vm
            .playback_do(|p, _| p.set_position(Duration::from_millis(ms)));
        self.run(fx);
    }

    /// Waits for everything in flight to land (Dart's pump until idle).
    pub(super) fn settle(&mut self) {
        while self.in_flight > 0 {
            let msg = self
                .rx
                .recv_timeout(Duration::from_secs(20))
                .expect("work in flight never landed (a gate left shut?)");
            self.handle(msg);
        }
    }

    /// Pumps until `cond` holds (work parked on a gate stays parked).
    pub(super) fn pump_until(&mut self, cond: impl Fn(&Screen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond(self) {
            assert!(Instant::now() < deadline, "the condition never held");
            if let Ok(msg) = self.rx.recv_timeout(Duration::from_millis(2)) {
                self.handle(msg);
            }
        }
    }

    /// The reply to `call`, once it came.
    pub(super) fn reply(&self, call: Call) -> Option<&Reply> {
        self.replies.iter().find(|r| match r {
            Reply::Done(c) => *c == call,
            Reply::ClipAdded { call: c, .. }
            | Reply::LyricsSaved { call: c, .. }
            | Reply::Techniques { call: c, .. } => *c == call,
        })
    }

    /// Settles, then the reply to `call`.
    pub(super) fn await_reply(&mut self, call: Call) -> Reply {
        self.pump_until(|s| s.reply(call).is_some());
        self.reply(call).cloned().expect("a reply")
    }

    /// Whether a lyrics write landed (settling it first).
    pub(super) fn landed(&mut self, call: Call) -> bool {
        match self.await_reply(call) {
            Reply::LyricsSaved { landed, .. } => landed,
            other => panic!("not a lyrics write: {other:?}"),
        }
    }

    pub(super) fn practice(&self) -> &SongPractice {
        self.vm.practice().expect("loaded")
    }

    pub(super) fn track(&self, id: &str) -> Arc<AudioTrack> {
        self.vm.track_by_id(id).expect("the track")
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.vm.dispose();
    }
}

/// The in-memory screen of the structural-edit tests (Dart's `pumpVm`).
pub(super) struct Fixture {
    pub events: Arc<Mutex<Vec<String>>>,
    pub repo: Arc<FakeRepo>,
    pub files: Arc<FakeFiles>,
    pub edits: Arc<FakeEdits>,
    pub engine: Arc<FakeEngine>,
    pub techs: Arc<FakeTechRepo>,
    pub finalizer: Arc<RecordingFinalizer>,
}

/// The parts of a [`Fixture`] screen that vary.
pub(super) struct Parts {
    pub analyze: TakeAnalyzer,
    pub separation: Arc<dyn SourceSeparationService>,
    pub probe: Option<ProbeDuration>,
    pub picker: Option<SaveLocationPicker>,
    pub spectrograms: Option<Arc<dyn SpectrogramAnalyses>>,
}

impl Default for Parts {
    fn default() -> Self {
        Parts {
            analyze: ok_analyze(),
            separation: Arc::new(FakeSourceSeparationService { available: false }),
            probe: None,
            picker: None,
            spectrograms: None,
        }
    }
}

/// How a [`Fixture`] screen is put together.
pub(super) struct Setup {
    pub practice: SongPractice,
    pub analyze: TakeAnalyzer,
    pub separation: Arc<dyn SourceSeparationService>,
    pub probe: Option<ProbeDuration>,
    pub picker: Option<SaveLocationPicker>,
    /// The first load parks on it (the screen is returned before it lands).
    pub first_load: Option<Arc<Gate>>,
    pub finalizer: Option<Arc<RecordingFinalizer>>,
    pub spectrograms: Option<Arc<dyn SpectrogramAnalyses>>,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            practice: seed_practice(None),
            analyze: ok_analyze(),
            separation: Arc::new(FakeSourceSeparationService { available: false }),
            probe: None,
            picker: None,
            first_load: None,
            finalizer: None,
            spectrograms: None,
        }
    }
}

impl Fixture {
    /// Opens a screen on `setup.practice` and lets its load land (unless
    /// `first_load` holds it), with the event log cleared.
    pub(super) fn open(setup: Setup) -> (Fixture, Screen) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repo = FakeRepo::new(setup.practice, Arc::clone(&events));
        repo.s().stale_load_gate = setup.first_load.clone();
        let files = FakeFiles::new(Arc::clone(&events));
        let edits = Arc::new(FakeEdits::default());
        let engine = Arc::new(FakeEngine::default());
        let techs = Arc::new(FakeTechRepo::default());
        let finalizer = setup
            .finalizer
            .unwrap_or_else(|| Arc::new(RecordingFinalizer::for_testing()));
        let fixture = Fixture {
            events,
            repo,
            files,
            edits,
            engine,
            techs,
            finalizer,
        };
        let mut screen = fixture.screen(Parts {
            analyze: setup.analyze,
            separation: setup.separation,
            probe: setup.probe,
            picker: setup.picker,
            spectrograms: setup.spectrograms,
        });
        if setup.first_load.is_none() {
            screen.settle();
            assert!(
                screen.vm.practice().is_some(),
                "initial load should complete"
            );
            guard(&fixture.events).clear();
        }
        (fixture, screen)
    }

    pub(super) fn screen(&self, parts: Parts) -> Screen {
        let Parts {
            analyze,
            separation,
            probe,
            picker,
            spectrograms,
        } = parts;
        let deps = Deps {
            repository: Arc::clone(&self.repo) as Arc<dyn SongPracticeRepository>,
            technique_repository: Arc::clone(&self.techs) as Arc<dyn TechniqueRepository>,
            audio_files: Arc::clone(&self.files) as Arc<dyn AudioFiles>,
            spectrograms: spectrograms.unwrap_or_else(|| Arc::new(NoSpectrograms)),
            separation,
            app_settings: settings(),
        };
        let options = Options {
            engine: Some(EngineHandle::Other(
                Arc::clone(&self.engine) as Arc<dyn PlaybackEngine>
            )),
            edits: Some(Arc::clone(&self.edits) as Arc<dyn AudioEdits>),
            finalizer: Some(Arc::clone(&self.finalizer)),
            probe_duration: Some(probe.unwrap_or_else(|| Arc::new(|_: &str| Ok(None)))),
            save_location_picker: picker,
            analyze: Some(analyze),
            ..Options::default()
        };
        Screen::open(deps, "p1", options)
    }

    pub(super) fn events(&self) -> Vec<String> {
        guard(&self.events).clone()
    }

    pub(super) fn clear_events(&self) {
        guard(&self.events).clear();
    }
}
