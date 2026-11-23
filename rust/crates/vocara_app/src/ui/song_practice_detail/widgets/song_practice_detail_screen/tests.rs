//! The song practice page headless: the screen cases the earlier ports left
//! (each listed in its module's test doc) —
//!
//! - `takes_header_test.dart`: the VOCAL TAKES header at the smallest window;
//! - `timeline_scrollbar_test.dart` and `timeline_envelope_test.dart`: the
//!   scrollbar's thumb and the master / bus envelope, through their pure
//!   functions;
//! - `playhead_repaint_test.dart`'s screen cases that still apply (iced has
//!   no repaint boundaries to pin; what those tests checked beside them is
//!   ported): the dock's and a folded-out lane's heals, the clip and backing
//!   deletes asking first, a lane's menu acting on the take as it is now, a
//!   dock taller than the window, a resize counting every move, and a
//!   reopen behind a landing save;
//! - `song_practice_placement_test.dart`'s screen cases: a drop laid out end
//!   to end from the drop's playhead, a drop on a set backing said once its
//!   takes landed, a clip picked from the take menu at the tap's playhead;
//! - `song_practice_take_flow_test.dart`'s screen cases with the back arrow
//!   (Back in REC's arm window cancels the take before the page leaves; two
//!   Backs there, or while the take lands, leave once);
//! - `kept_subtree_fresh_build_test.dart`'s screen half: a timeline fader
//!   keeps its own drag when a lane above it goes;
//! - `detail_arrange_journey_test.dart`'s transport, dock, interpretation,
//!   New take and note cases on the page;
//! - the drop handlers (a drop with no audio, a drop on the catch-up guard)
//!   and the leave guards (a page left mid-save lingers, a drag left
//!   mid-gesture is committed, a quit's request is served).
//!
//! The page runs over fakes (an in-memory repository, file and edit fakes, a
//! fake engine and recorder, a gated analysis). Its work runs here, each job
//! on a thread of its own, and lands through the page's messages, as the
//! runtime runs its tasks; the dialogs it asks for are answered by the test.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Instant;

use iced::{window, Color, Event as IcedEvent, Point, Size};
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use indexmap::IndexMap;
use vocara_model::config::environment::AppEnvironment;
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx};
use vocara_model::models::audio_track::{AudioClip, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::practice_summary::PracticeSummary;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique::Technique;
use vocara_model::models::voice_metrics::VoiceMetrics;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::audio_dsp::AnalysisParams;
use vocara_services::audio_edit_service::{AudioEditError, AudioEdits, SplitPaths};
use vocara_services::audio_file_service::{AudioFileError, AudioFiles};
use vocara_services::mic_recording_controller::{
    DestinationError, MicEvent, MicRecorder, MicRecordingStart, MicRecordingStop, PendingStart,
    RecordingPathStrategy, StopFinalizeGate, StopFinalizeHold,
};
use vocara_services::playback_engine::{
    Broadcast, BusPlayback, Subscription as EngineSubscription, TrackPlayback,
};
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::source_separation_service::FakeSourceSeparationService;
use vocara_services::spectrogram_service::{SpectrogramAnalyses, TakeAnalysisBundle};
use vocara_services::waveform_service::WaveformResult;
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};
use vocara_store::sync::sync_hashes::Loaded;

use super::*;
use crate::app::library::Library;
use crate::router::{Navigation, Route};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::font;
use crate::ui::core::widgets::confirm_dialog::{Dialog, DialogResult};
use crate::ui::song_practice_detail::view_model::mixer_controller::EngineHandle;
use crate::ui::song_practice_detail::view_model::recording_controller::TakeAnalyzer;
use crate::ui::song_practice_detail::view_model::song_practice_detail_view_model::DockTab;
use crate::ui::song_practice_detail::widgets::live_ui::{self, LiveUi};
use crate::ui::song_practice_detail::widgets::track_row::TRACK_ROW_HEIGHT;
use crate::ui::DialogRequest;

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn at() -> DateTime {
    DateTime::local_ymd(2026, 6, 20)
}

// ----------------------------------------------------------------- fakes

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

/// The in-memory store: `stored` plays the on-disk aggregate.
#[derive(Default)]
struct MemRepo {
    s: Mutex<RepoState>,
}

#[derive(Default)]
struct RepoState {
    stored: Option<SongPractice>,
    saves: usize,
    /// When set, saves park on it.
    save_gate: Option<Arc<Gate>>,
    /// Every load fails.
    unreadable: bool,
}

impl MemRepo {
    fn with(practice: SongPractice) -> Arc<MemRepo> {
        let repo = MemRepo::default();
        guard(&repo.s).stored = Some(practice);
        Arc::new(repo)
    }

    fn s(&self) -> MutexGuard<'_, RepoState> {
        guard(&self.s)
    }

    fn stored(&self) -> SongPractice {
        self.s().stored.clone().expect("stored")
    }
}

fn store_error(m: &str) -> vocara_store::Error {
    vocara_store::Error::State(m.to_string())
}

impl SongPracticeRepository for MemRepo {
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
        let s = self.s();
        if s.unreadable {
            return Err(store_error("unreadable"));
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
        let gate = self.s().save_gate.clone();
        if let Some(gate) = gate {
            gate.wait();
        }
        let mut s = self.s();
        s.saves += 1;
        s.stored = Some(practice.clone());
        Ok(Loaded::new(practice.clone(), IndexMap::new()))
    }

    fn delete_practice(&self, _id: &str) -> vocara_store::Result<()> {
        Ok(())
    }
}

/// The technique library.
#[derive(Default)]
struct MemTechniques(Mutex<Vec<Technique>>);

impl TechniqueRepository for MemTechniques {
    fn list_techniques(&self) -> vocara_store::Result<Vec<Technique>> {
        Ok(guard(&self.0).clone())
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

/// File fake: imports land as `/import/N.wav`.
#[derive(Default)]
struct FakeFiles {
    imports: AtomicUsize,
    deleted: Mutex<Vec<String>>,
}

impl AudioFiles for FakeFiles {
    fn import_audio(&self, _practice_id: &str, _source: &str) -> Result<String, AudioFileError> {
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

    fn is_app_storage(&self, _path: &str) -> bool {
        false
    }

    fn delete_audio(&self, path: &str) -> Result<(), AudioFileError> {
        guard(&self.deleted).push(path.to_string());
        Ok(())
    }
}

/// Edits fake: every file is already streamable.
struct FakeEdits;

impl AudioEdits for FakeEdits {
    fn split_wav(&self, _: &str, _: &str, _: i64) -> Result<SplitPaths, AudioEditError> {
        Ok(SplitPaths {
            first_path: "/edit/first.wav".into(),
            second_path: "/edit/second.wav".into(),
        })
    }

    fn concat_wav(&self, _: &str, _: &str, _: &str) -> Result<String, AudioEditError> {
        Ok("/edit/merged.wav".into())
    }

    fn ensure_streamable_wav(&self, _: &str, path: &str) -> Result<String, AudioEditError> {
        Ok(path.to_string())
    }

    fn punch_in(&self, _: &str, _: &str, _: &str, _: i64) -> Result<Option<String>, AudioEditError> {
        Ok(None)
    }

    fn duplicate(&self, _: &str, _: &str) -> Result<String, AudioEditError> {
        Ok("/edit/dup.wav".into())
    }
}

/// Analyses nothing.
struct NoSpectrograms;

impl SpectrogramAnalyses for NoSpectrograms {
    fn compute_from_file(
        &self,
        _: &str,
        _: AnalysisParams,
        _: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<Arc<Spectrogram>>> {
        Ok(None)
    }

    fn compute_metrics_from_file(
        &self,
        _: &str,
        _: AnalysisParams,
        _: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<Arc<VoiceMetrics>>> {
        Ok(None)
    }

    fn compute_both_from_file(
        &self,
        _: &str,
        _: AnalysisParams,
        _: Option<&dyn Fn() -> bool>,
    ) -> std::io::Result<Option<TakeAnalysisBundle>> {
        Ok(None)
    }
}

/// An engine whose transport answers as the real one does: play and pause
/// report the playing flag, a seek reports the position.
#[derive(Default)]
struct FakeEngine {
    position: Broadcast<Duration>,
    playing_events: Broadcast<bool>,
    playing: Mutex<bool>,
}

impl PlaybackEngine for FakeEngine {
    fn position(&self) -> EngineSubscription<Duration> {
        self.position.subscribe()
    }

    fn playing(&self) -> EngineSubscription<bool> {
        self.playing_events.subscribe()
    }

    fn current_position(&self) -> Duration {
        Duration::ZERO
    }

    fn is_playing(&self) -> bool {
        *guard(&self.playing)
    }

    fn set_loop(&self, _enabled: bool, _start_ms: i64, _end_ms: i64) {}

    fn sync(&self, _tracks: Vec<TrackPlayback>, _buses: Vec<BusPlayback>) {}

    fn play(&self) {
        *guard(&self.playing) = true;
        self.playing_events.add(true);
    }

    fn pause(&self) {
        *guard(&self.playing) = false;
        self.playing_events.add(false);
    }

    fn seek(&self, position: Duration) {
        self.position.add(position);
    }

    fn set_volume(&self, _id: &str, _volume: f64, _muted: bool) {}

    fn set_track_pan(&self, _id: &str, _pan: f64) {}

    fn set_master_volume(&self, _gain: f64) {}

    fn supports_effects(&self) -> bool {
        true
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

/// The take-flow test's recorder (`_FakeMic`, and with a `ready` gate
/// `_ArmingMic`): its start runs the destination's create (the take setup),
/// then parks on `ready` while it is shut; starting until its start returns,
/// arming nothing once `invalidate_pending_start` landed meanwhile.
struct FakeMic {
    s: Arc<Mutex<MicState>>,
    ready: Option<Arc<Gate>>,
    stop_gate: StopFinalizeGate,
}

#[derive(Default)]
struct MicState {
    recording: bool,
    starting: bool,
    generation: u64,
    last_path: Option<String>,
}

impl FakeMic {
    fn new(finalizer: &Arc<RecordingFinalizer>, ready: Option<&Arc<Gate>>) -> Arc<FakeMic> {
        Arc::new(FakeMic {
            s: Arc::new(Mutex::new(MicState::default())),
            ready: ready.cloned(),
            stop_gate: StopFinalizeGate::new(Arc::clone(finalizer)),
        })
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
        guard(&self.s).recording
    }

    fn is_starting(&self) -> bool {
        guard(&self.s).starting
    }

    fn feeding_monitor(&self) -> bool {
        false
    }

    fn invalidate_pending_start(&self) {
        let mut s = guard(&self.s);
        s.generation += 1;
        s.starting = false;
    }

    fn begin_start(&self, destination: RecordingPathStrategy) -> PendingStart {
        let generation = {
            let mut s = guard(&self.s);
            s.starting = true;
            s.generation
        };
        let s = Arc::clone(&self.s);
        let ready = self.ready.clone();
        PendingStart::from_fn(move || {
            let outcome = (|| {
                let path = destination.create()?;
                if let Some(r) = &ready {
                    r.wait();
                }
                let mut st = guard(&s);
                if st.generation != generation {
                    return Ok(MicRecordingStart::Invalidated);
                }
                st.starting = false;
                st.last_path = Some(path.clone());
                st.recording = true;
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
        let mut s = guard(&self.s);
        if !s.recording {
            return None;
        }
        s.recording = false;
        Some(MicRecordingStop {
            path: s.last_path.clone().unwrap_or_default(),
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

/// A waveform analysis answering one second of silence.
fn ok_analyze() -> TakeAnalyzer {
    Arc::new(|_: &str| {
        Ok(WaveformResult {
            peaks: Vec::new(),
            duration_ms: 1000,
        })
    })
}

/// `_GatedWaveforms`: each analysis parks until the test releases it (the
/// seconds a real one takes, during which playback runs on).
#[derive(Default)]
struct GatedAnalyze {
    gates: Mutex<Vec<Arc<Gate>>>,
    gated: std::sync::atomic::AtomicBool,
}

impl GatedAnalyze {
    fn new() -> Arc<GatedAnalyze> {
        let g = GatedAnalyze::default();
        g.gated.store(true, Ordering::SeqCst);
        Arc::new(g)
    }

    fn analyzer(self: &Arc<Self>) -> TakeAnalyzer {
        let me = Arc::clone(self);
        Arc::new(move |_: &str| {
            if me.gated.load(Ordering::SeqCst) {
                let gate = Gate::new();
                guard(&me.gates).push(Arc::clone(&gate));
                gate.wait();
            }
            Ok(WaveformResult {
                peaks: Vec::new(),
                duration_ms: 1000,
            })
        })
    }

    fn parked(&self) -> usize {
        guard(&self.gates).len()
    }

    fn release_last(&self) {
        guard(&self.gates).last().expect("an analysis parked").open();
    }
}

// --------------------------------------------------------------- fixtures

fn take(id: &str, name: &str) -> AudioTrack {
    AudioTrack {
        version_id: Some("v1".into()),
        ..AudioTrack::new(id, AudioTrackKind::Vocal, name, at()).with_audio(
            format!("/pool/{id}.wav"),
            0,
            1000,
            Vec::new(),
        )
    }
}

fn backing() -> AudioTrack {
    AudioTrack::new("acc", AudioTrackKind::Accompaniment, "Backing", at()).with_audio(
        "/pool/acc.wav",
        0,
        60000,
        Vec::new(),
    )
}

/// A practice of `tracks` in one interpretation, "Interpretation 1".
fn practice(tracks: Vec<AudioTrack>) -> SongPractice {
    SongPractice {
        active_version_id: Some("v1".into()),
        versions: vec![Arc::new(TakeVersion::new("v1", "Interpretation 1", at()))],
        vocal_tracks: tracks.into_iter().map(Arc::new).collect(),
        timeline_duration_ms: 60000,
        ..SongPractice::new("p1", "Song", at(), at())
    }
}

/// How a page is put together.
struct Setup {
    practice: SongPractice,
    repo: Option<Arc<MemRepo>>,
    analyze: TakeAnalyzer,
    mic: Option<Arc<dyn MicRecorder>>,
    finalizer: Option<Arc<RecordingFinalizer>>,
    techniques: Vec<Technique>,
    /// Whether the first load runs before the page is handed over.
    settle: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            practice: practice(vec![take("t1", "Take 1")]),
            repo: None,
            analyze: ok_analyze(),
            mic: None,
            finalizer: None,
            techniques: Vec::new(),
            settle: true,
        }
    }
}

// ----------------------------------------------------------- the harness

/// A finished piece of work, coming back to the UI thread; `counted` when
/// it ends work in flight (a take setup arrives while its start still runs).
struct Landing {
    counted: bool,
    message: Message,
}

/// The page, its fakes, and what it asked for.
struct Harness {
    screen: SongPracticeDetailScreen,
    services: Services,
    repo: Arc<MemRepo>,
    files: Arc<FakeFiles>,
    engine: Arc<FakeEngine>,
    finalizer: Arc<RecordingFinalizer>,
    tx: mpsc::Sender<Landing>,
    rx: mpsc::Receiver<Landing>,
    in_flight: usize,
    renders: VecDeque<(Render, RenderRequest)>,
    picks: Vec<(DropZone, Option<i64>)>,
    navigations: Vec<Navigation>,
    dialog: Option<DialogRequest<Message>>,
    size: Size,
}

/// The app's fonts (Inter, and the Lucide icons), so text measures as it
/// does in the app.
fn settings() -> iced::Settings {
    let mut settings = crate::app::settings();
    settings.fonts = font::FACES
        .iter()
        .copied()
        .chain([icons::FONT_BYTES])
        .map(Cow::Borrowed)
        .collect();
    settings
}

fn glyph(icon: Icon) -> String {
    char::from(icon).to_string()
}

/// The `n`-th text reading `label` (0 first), in tree order.
fn nth(label: &str, n: usize) -> impl FnMut(Candidate<'_>) -> Option<Target> + Send {
    let label = label.to_string();
    let mut seen = 0;
    move |candidate: Candidate<'_>| {
        let Candidate::Text { content, .. } = &candidate else {
            return None;
        };
        if *content != label {
            return None;
        }
        seen += 1;
        (seen == n + 1).then(|| Target::from(candidate))
    }
}

impl Harness {
    fn open(setup: Setup) -> Self {
        let services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
        let repo = setup
            .repo
            .unwrap_or_else(|| MemRepo::with(setup.practice.clone()));
        let files = Arc::new(FakeFiles::default());
        let engine = Arc::new(FakeEngine::default());
        let finalizer = setup
            .finalizer
            .unwrap_or_else(|| Arc::new(RecordingFinalizer::for_testing()));
        let deps = Deps {
            repository: Arc::clone(&repo) as Arc<dyn SongPracticeRepository>,
            technique_repository: Arc::new(MemTechniques(Mutex::new(setup.techniques))),
            audio_files: Arc::clone(&files) as Arc<dyn AudioFiles>,
            spectrograms: Arc::new(NoSpectrograms),
            separation: Arc::new(FakeSourceSeparationService { available: false }),
            app_settings: Arc::clone(&services.app_settings),
        };
        let options = Options {
            engine: Some(EngineHandle::Other(
                Arc::clone(&engine) as Arc<dyn PlaybackEngine>
            )),
            edits: Some(Arc::new(FakeEdits)),
            finalizer: Some(Arc::clone(&finalizer)),
            probe_duration: Some(Arc::new(|_: &str| Ok(None))),
            save_location_picker: Some(Arc::new(|_: &str| Ok(None))),
            analyze: Some(setup.analyze),
            mic: setup.mic,
            ..Options::default()
        };
        let (screen, step) = SongPracticeDetailScreen::open("p1".into(), &services, deps, options);
        let (tx, rx) = mpsc::channel();
        let mut h = Harness {
            screen,
            services,
            repo,
            files,
            engine,
            finalizer,
            tx,
            rx,
            in_flight: 0,
            renders: VecDeque::new(),
            picks: Vec::new(),
            navigations: Vec::new(),
            dialog: None,
            size: Size::new(1440.0, 1000.0),
        };
        h.absorb(step);
        if setup.settle {
            h.settle();
            assert!(h.vm().practice().is_some(), "loaded");
        }
        h
    }

    fn vm(&self) -> &SongPracticeDetailViewModel {
        self.screen.view_model()
    }

    fn practice(&self) -> &SongPractice {
        self.vm().practice().expect("loaded")
    }

    fn track(&self, id: &str) -> Arc<AudioTrack> {
        self.vm().track_by_id(id).expect("the track")
    }

    fn send(&mut self, message: Message) {
        let step = self.screen.handle(message);
        self.absorb(step);
    }

    fn spawn(&mut self, work: impl FnOnce() -> Message + Send + 'static) {
        self.in_flight += 1;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Landing {
                counted: true,
                message: work(),
            });
        });
    }

    /// Runs the page's work as the runtime would, keeps its renders, open
    /// panels, navigation and dialog (its timers and widget operations are
    /// not run).
    fn absorb(&mut self, step: Step) {
        for effect in step.work {
            self.run(effect);
        }
        self.renders.extend(step.renders);
        self.picks.extend(step.picks);
        if let Some(navigation) = step.update.navigation {
            self.navigations.push(navigation);
        }
        if let Some(dialog) = step.update.dialog {
            self.dialog = Some(dialog);
        }
    }

    fn run(&mut self, effect: Effect) {
        match effect {
            Effect::Run(job) => self.spawn(move || Message::Job(Landed::new(job.run()))),
            Effect::Analysis(job) => self.spawn(move || Message::AnalysisJob(job.run())),
            Effect::AnalysisMicPermission => {
                self.spawn(|| Message::AnalysisMicPermission(true));
            }
            Effect::LaneHealRetry { .. } => {}
            Effect::StartRecording(job) => {
                let tx = self.tx.clone();
                self.spawn(move || {
                    Message::StartDone(job.run(move |request| {
                        let _ = tx.send(Landing {
                            counted: false,
                            message: Message::TakeSetup(request),
                        });
                    }))
                });
            }
            Effect::RecordingWork(job) => self.spawn(move || Message::RecordingJob(job.run())),
            Effect::FinalizeTake(op, run) => self.spawn(move || Message::Finalized(op, run.run())),
            Effect::FinalizeRetry { op, after } => self.spawn(move || {
                thread::sleep(after);
                Message::FinalizeRetry(op)
            }),
            Effect::MonitorMicPermission => self.spawn(|| Message::MonitorPermission(true)),
            Effect::InterpretationsCleanUp(op, run) => {
                self.spawn(move || Message::CleanedUp(op, run.run()));
            }
            other => panic!("the page handles {other:?} itself"),
        }
    }

    fn land(&mut self, landing: Landing) {
        if landing.counted {
            self.in_flight -= 1;
        }
        self.send(landing.message);
    }

    /// Lands everything in flight, and the renders it asked for.
    fn settle(&mut self) {
        loop {
            while self.in_flight > 0 {
                let landing = self
                    .rx
                    .recv_timeout(Duration::from_secs(20))
                    .expect("work in flight never landed (a gate left shut?)");
                self.land(landing);
            }
            let Some((target, render)) = self.renders.pop_front() else {
                return;
            };
            self.send(rendered(&target, render.run()));
        }
    }

    /// Lands work until `cond` holds (work parked on a gate stays parked).
    fn pump_until(&mut self, cond: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond(self) {
            assert!(Instant::now() < deadline, "the condition never held");
            if let Ok(landing) = self.rx.recv_timeout(Duration::from_millis(2)) {
                self.land(landing);
            }
        }
    }

    /// Puts the playhead at `ms` (Dart's `position.value = ...`).
    fn at_ms(&mut self, ms: u64) {
        let step = {
            let fx = self
                .screen
                .vm
                .playback_do(|p, _| p.set_position(Duration::from_millis(ms)));
            let mut step = Step::new();
            self.screen.absorb(fx, &mut step);
            step
        };
        self.absorb(step);
    }

    /// The dialog the page asked for.
    fn dialog(&self) -> &Dialog {
        &self.dialog.as_ref().expect("a dialog is open").dialog
    }

    /// Closes the open dialog with `result`.
    fn answer(&mut self, result: DialogResult) {
        let request = self.dialog.take().expect("a dialog is open");
        self.send((request.on_result)(result));
    }

    fn ui(&self) -> Simulator<'_, Message> {
        Simulator::with_size(settings(), self.size, self.screen.view())
    }

    fn feed(&mut self, messages: Vec<Message>) {
        for m in messages {
            self.send(m);
        }
    }

    fn click_nth(&mut self, label: &str, n: usize) {
        let messages: Vec<Message> = {
            let mut ui = self.ui();
            ui.click(nth(label, n))
                .unwrap_or_else(|e| panic!("{label} #{n}: {e:?}"));
            ui.into_messages().collect()
        };
        self.feed(messages);
    }

    fn click(&mut self, label: &str) {
        self.click_nth(label, 0);
    }

    fn find(&self, label: &str) -> Option<Target> {
        self.ui().find(nth(label, 0)).ok()
    }

    fn shows(&self, label: &str) -> bool {
        self.find(label).is_some()
    }

    fn count(&self, label: &str) -> usize {
        (0..)
            .take_while(|&n| self.ui().find(nth(label, n)).is_ok())
            .count()
    }

    /// Drops `paths` at `at`, as the OS does.
    fn drop_at(&mut self, at: Point, paths: &[&str]) {
        let messages: Vec<Message> = {
            let mut ui = self.ui();
            ui.point_at(at);
            let mut events = Vec::new();
            for p in paths {
                events.push(IcedEvent::Window(window::Event::FileHovered(
                    PathBuf::from(p),
                )));
            }
            for p in paths {
                events.push(IcedEvent::Window(window::Event::FileDropped(
                    PathBuf::from(p),
                )));
            }
            events.push(live_ui::redraw());
            let _ = ui.simulate(events);
            ui.into_messages().collect()
        };
        self.feed(messages);
    }

    /// How many times the page left (popped, or went Home).
    fn leaves(&self) -> usize {
        self.navigations
            .iter()
            .filter(|n| matches!(n, Navigation::PopOrGo(Route::Home)))
            .count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !thread::panicking() {
            self.screen.dispose(&self.services);
        }
    }
}

// ------------------------------------------------------- the pure parts

// timeline_scrollbar_test: a 10-minute song in a 600 px groove viewed
// through a 1-second window: the view's true share is 1 px, so the thumb
// sits on its 28 px floor.
const EXT: i64 = 600_000;
const SPAN: i64 = 1000;
const GROOVE: f64 = 600.0;

#[test]
fn at_the_last_page_the_thumb_ends_at_the_groove_end_not_past_it() {
    let t = timeline_scroll_thumb(EXT - SPAN, SPAN, EXT, GROOVE);
    assert_eq!(t.width, 28.0);
    assert!((t.left + t.width - GROOVE).abs() < 1e-9);
}

#[test]
fn at_the_first_page_the_thumb_starts_at_the_groove_start() {
    assert_eq!(timeline_scroll_thumb(0, SPAN, EXT, GROOVE).left, 0.0);
}

#[test]
fn dragging_the_thumb_across_its_whole_travel_pans_the_whole_song() {
    let t = timeline_scroll_thumb(0, SPAN, EXT, GROOVE);
    assert!((t.ms_per_px * (GROOVE - t.width) - (EXT - SPAN) as f64).abs() < 1e-6);
}

#[test]
fn at_fit_all_the_thumb_fills_the_groove_and_does_not_drag() {
    let t = timeline_scroll_thumb(0, EXT, EXT, GROOVE);
    assert_eq!(t, ScrollThumb {
        left: 0.0,
        width: GROOVE,
        ms_per_px: 0.0
    });
}

#[test]
fn a_song_shorter_than_the_minimum_view_span_does_not_divide_by_the_negative_range() {
    // The view never narrows below 1 s, so a 400 ms song has span > extent.
    let t = timeline_scroll_thumb(0, 1000, 400, GROOVE);
    assert_eq!(t, ScrollThumb {
        left: 0.0,
        width: GROOVE,
        ms_per_px: 0.0
    });
}

/// timeline_envelope_test's `_spikeX`: the x of the drawn spike (with a
/// single full-amplitude peak among zeros, the outline's highest point).
fn spike_x(p: &WaveformPainter, size: Size) -> f64 {
    let shape = p.shape(size).expect("something painted");
    let (x, _) = shape
        .outline
        .iter()
        .copied()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("an outline");
    x
}

fn spiked(at: usize) -> Arc<[f64]> {
    (0..1000).map(|i| if i == at { 1.0 } else { 0.0 }).collect()
}

// A 20 s timeline in 1000 buckets (20 ms each), loud only at 10 s.
#[test]
fn the_fit_all_view_spreads_the_envelope_over_the_lane() {
    let p = timeline_envelope_painter(spiked(500), 20000, 0, 20000, Color::WHITE);
    assert!((spike_x(&p, Size::new(600.0, 40.0)) - 300.0).abs() < 3.0);
}

// The master and bus lanes draw one envelope bucketed over the whole
// timeline, sliced to the view. While a take runs on past the timeline's end
// the view reaches past it too: an envelope still spread over the whole lane
// put every peak at the wrong time.
#[test]
fn a_view_reaching_past_the_timeline_end_keeps_peaks_on_time() {
    let p = timeline_envelope_painter(spiked(500), 20000, 0, 30000, Color::WHITE);
    let expected = 10000.0 / 30000.0 * 600.0;
    assert!((spike_x(&p, Size::new(600.0, 40.0)) - expected).abs() < 3.0);
}

#[test]
fn a_zoomed_view_paged_past_the_end_keeps_peaks_on_time() {
    let p = timeline_envelope_painter(spiked(850), 20000, 15000, 10000, Color::WHITE);
    // Bucket 850 starts at 17 000 ms: 2 s into the 10 s window.
    assert!((spike_x(&p, Size::new(600.0, 40.0)) - 2000.0 / 10000.0 * 600.0).abs() < 3.0);
}

#[test]
fn only_what_the_engine_decodes_counts_as_audio() {
    for ok in ["a.wav", "/x/B.MP3", "c.m4a", "d.aiff", "e.caf", "f.oga"] {
        assert!(is_audio_path(ok), "{ok}");
    }
    // Opus and WMA had no decoder: each drop failed later, unconverted.
    for no in ["a.opus", "b.wma", "lyrics.txt", "noext", "dir.wav/"] {
        assert!(!is_audio_path(no), "{no}");
    }
}

#[test]
fn a_horizontal_swipe_or_shift_wheel_pans_the_view() {
    // iced's positive x scrolls the content right: Flutter's delta is its
    // negation.
    assert_eq!(view::wheel_pan_ms(-60.0, 0.0, false, 600.0, 60000), Some(6000));
    assert_eq!(view::wheel_pan_ms(0.0, 30.0, true, 600.0, 60000), Some(-3000));
    // Plain vertical scroll falls through to the list.
    assert_eq!(view::wheel_pan_ms(0.0, 30.0, false, 600.0, 60000), None);
    assert_eq!(view::wheel_pan_ms(-60.0, 0.0, false, 0.0, 60000), None);
}

// ------------------------------------------------- loading and its errors

// home_reopen_test: 'a practice that cannot be loaded still has a way back'
// (the page side; the app's is in `through_the_app`). Loading and load
// failure showed no transport bar, so no back control.
#[test]
fn a_practice_that_cannot_be_loaded_says_so_and_has_a_way_back() {
    let repo = Arc::new(MemRepo::default());
    repo.s().unreadable = true;
    let mut h = Harness::open(Setup {
        repo: Some(repo),
        settle: false,
    ..Setup::default()
    });
    // Loading: a spinner under the bar with the back chevron.
    assert!(h.vm().load_running());
    assert!(h.shows(&glyph(Icon::ChevronLeft)));
    h.settle();
    assert!(h.shows(NOT_LOADED));
    h.click(&glyph(Icon::ChevronLeft));
    assert_eq!(h.leaves(), 1);
}

// ------------------------------------------------------- the takes header

/// The page at the 960 px minimum window: with both side panels open the
/// centre pane is 388 px wide.
fn narrow(name: &str) -> Harness {
    let mut p = practice(vec![take("t1", "Take 1")]);
    p.versions = vec![Arc::new(TakeVersion::new("v1", name, at()))];
    let mut h = Harness::open(Setup {
        practice: p,
        ..Setup::default()
    });
    h.size = Size::new(960.0, 700.0);
    h
}

/// The centre pane's right edge in a narrow page.
const CENTRE_RIGHT: f32 = 960.0 - 300.0;

// takes_header_test: at the smallest window a user-named interpretation of a
// little over a dozen characters overflowed the row and clipped the
// dropdown's chevron off its right edge.
#[test]
fn a_long_interpretation_name_gives_way_instead_of_overflowing() {
    let h = narrow("Powerful belt, second verse");
    assert!(h.shows("Powerful belt, second verse"));
    let chevron = h
        .find(&glyph(Icon::ChevronDown))
        .expect("the dropdown's chevron")
        .bounds();
    assert!(
        chevron.x + chevron.width <= CENTRE_RIGHT - 14.0 + 0.5,
        "the chevron stays inside the row: {chevron:?}"
    );
}

#[test]
fn the_dropdown_stays_flush_right() {
    let h = narrow("Tender");
    let chevron = h
        .find(&glyph(Icon::ChevronDown))
        .expect("the dropdown's chevron")
        .bounds();
    assert!(CENTRE_RIGHT - (chevron.x + chevron.width) < 40.0, "{chevron:?}");
}

// -------------------------------------------------------- the transport

// detail_arrange_journey: 'the transport toggles play ↔ pause' and 'the stop
// button halts playback and resets the icon to play'.
#[test]
fn the_transport_toggles_play_and_pause_and_stop_resets_it() {
    let mut h = Harness::open(Setup::default());
    assert!(h.shows(&glyph(Icon::Play)), "starts paused");
    h.click(&glyph(Icon::Play));
    assert!(h.vm().playback().is_playing(), "the engine's event drained");
    assert!(h.shows(&glyph(Icon::Pause)));
    h.click(&glyph(Icon::Pause));
    assert!(h.shows(&glyph(Icon::Play)));

    h.click(&glyph(Icon::Play));
    assert!(h.shows(&glyph(Icon::Pause)));
    h.click(&glyph(Icon::Square));
    assert!(!h.vm().playback().is_playing());
    assert!(h.shows(&glyph(Icon::Play)));
    assert!(!*guard(&h.engine.playing));
}

// --------------------------------------------------------------- the dock

// detail_arrange_journey: 'the bottom analysis dock opens'.
#[test]
fn the_bottom_dock_opens_and_closes() {
    let mut h = Harness::open(Setup::default());
    assert!(h.shows(&glyph(Icon::PanelBottomOpen)), "starts closed");
    h.click(&glyph(Icon::PanelBottomOpen));
    assert!(h.vm().analysis().dock_open());
    assert!(h.shows(&glyph(Icon::PanelBottomClose)));
    // A tab opens the dock on it.
    h.click(&glyph(Icon::PanelBottomClose));
    assert!(!h.vm().analysis().dock_open());
    h.click("MIXER");
    assert!(h.vm().analysis().dock_open());
    assert_eq!(h.vm().analysis().dock_tab(), DockTab::Mixer);
    assert!(h.shows("MASTER"), "the mixer desk's master strip");
}

// playhead_repaint_test: 'a dock taller than the window leaves the timeline
// whole'. The dragged height is clamped to a fixed range, not to the window:
// on a short window a tall dock collapsed the timeline.
#[test]
fn a_dock_taller_than_the_window_leaves_the_timeline_whole() {
    let mut h = Harness::open(Setup::default());
    h.send(Message::Dock(bottom_dock::Message::ShowDockTab(DockTab::Mixer)));
    // As tall as a drag lets it get.
    for _ in 0..20 {
        h.send(Message::Dock(bottom_dock::Message::Resize {
            dy: -100.0,
            cap: 10_000.0,
        }));
    }
    h.size = Size::new(1440.0, 560.0);
    let ruler = h.find("0:00").expect("the ruler's first label").bounds();
    let master = h.find("MASTER").expect("the master strip").bounds();
    // The ruler stays at the top, and the dock begins no higher than the
    // timeline's minimum below it.
    assert!(master.y >= ruler.y + MIN_TIMELINE_HEIGHT - 40.0, "{ruler:?} {master:?}");
    assert!(master.y + master.height <= h.size.height, "the dock's bottom shows");
}

// playhead_repaint_test: 'a dock resize counts every move, even several in
// one frame' — each move sets the height from the height on screen, read
// when it lands.
#[test]
fn a_dock_resize_counts_every_move_even_several_in_one_frame() {
    let mut h = Harness::open(Setup::default());
    h.send(Message::Dock(bottom_dock::Message::ShowDockTab(DockTab::Mixer)));
    let before = h.vm().analysis().dock_height();
    // Two moves of one batch, built from the same view.
    for _ in 0..2 {
        h.send(Message::Dock(bottom_dock::Message::Resize {
            dy: -10.0,
            cap: 10_000.0,
        }));
    }
    assert_eq!(h.vm().analysis().dock_height(), before + 20.0);
    // From a capped dock, a drag down moves it at once.
    h.send(Message::Dock(bottom_dock::Message::Resize { dy: 5.0, cap: 100.0 }));
    assert_eq!(h.vm().analysis().dock_height(), 100.0_f64.max(before.min(100.0)) - 5.0);
}

#[test]
fn the_dock_content_keeps_to_the_room_the_workspace_has() {
    assert_eq!(bottom_dock::content_height(300.0, 1000.0), 300.0);
    assert_eq!(
        bottom_dock::content_height(300.0, 200.0),
        200.0 - bottom_dock::CHROME_HEIGHT
    );
    assert_eq!(bottom_dock::content_height(300.0, 10.0), 0.0);
    assert_eq!(bottom_dock::resized_height(400.0, 200.0, 10.0), 190.0);
    assert_eq!(bottom_dock::resized_height(150.0, 200.0, -10.0), 160.0);
}

// playhead_repaint_test: 'the dock asks again for a spectrogram the cache
// let go' — the dock's post-frame heal.
#[test]
fn the_dock_asks_again_for_analysis_the_cache_let_go() {
    let mut h = Harness::open(Setup::default());
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::Select,
    });
    h.send(Message::Dock(bottom_dock::Message::ShowDockTab(
        DockTab::Spectrogram,
    )));
    h.settle();
    // NoSpectrograms settles every compute as unavailable; dropping the
    // settled half makes it lack again.
    let _ = h
        .screen
        .vm
        .analysis_do(|a, _| a.debug_drop_half("/pool/t1.wav", false));
    let lacking = h
        .vm()
        .with_analysis(|a, host| a.selected_analysis_incomplete(host));
    assert!(lacking, "the cache let the spectrogram go");
    // The next update (any message) asks again.
    h.send(Message::Done);
    assert!(h.in_flight > 0, "a compute was started");
    h.settle();
    assert!(!h
        .vm()
        .with_analysis(|a, host| a.selected_analysis_incomplete(host)));
}

// ------------------------------------------------------- folded-out lanes

// playhead_repaint_test: 'a folded-out lane asks again for metrics the cache
// let go' — after the frame that shows the lanes lacking data.
#[test]
fn a_folded_out_lane_asks_again_for_metrics_the_cache_let_go() {
    let mut h = Harness::open(Setup::default());
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ToggleExpand,
    });
    h.settle();
    let _ = h
        .screen
        .vm
        .analysis_do(|a, _| a.debug_drop_half("/pool/t1.wav", true));
    assert!(h.vm().with_analysis(|a, host| a.lane_lacks_data("t1", host)));
    let messages = {
        let mut ui = LiveUi::new(h.size);
        ui.run(h.screen.view(), [live_ui::redraw()])
    };
    assert!(
        messages.iter().any(|m| matches!(
            m,
            Message::Lanes {
                message: analysis_lanes::Message::HealLane(id),
                ..
            } if id == "t1"
        )),
        "{messages:?}"
    );
    h.feed(messages);
    h.settle();
    assert!(!h.vm().with_analysis(|a, host| a.lane_lacks_data("t1", host)));
}

// detail_arrange_journey: 'expanding a take flips its disclosure chevron and
// reveals lanes'.
#[test]
fn expanding_a_take_flips_its_chevron_and_reveals_its_lanes() {
    let mut h = Harness::open(Setup::default());
    assert!(h.shows(&glyph(Icon::ChevronRight)));
    h.click(&glyph(Icon::ChevronRight));
    h.settle();
    assert!(h.vm().analysis().is_track_expanded("t1"));
    assert!(h.shows("PITCH"), "the pitch lane folds out");
}

// ------------------------------------------------------------------ drops

/// Where the lane of `name` (its header label) is, to drop on.
fn lane_at(h: &Harness, name: &str) -> Point {
    let label = h.find(name).unwrap_or_else(|| panic!("{name}")).bounds();
    // Over the lane's timeline, right of its header.
    Point::new(label.x + 400.0, label.y + 20.0)
}

fn added_starts(h: &Harness, track: &str) -> Vec<i64> {
    h.track(track)
        .clips
        .iter()
        .filter(|c| c.audio_file_path.starts_with("/import/"))
        .map(|c| c.start_ms)
        .collect()
}

// song_practice_placement_test: 'a drop lays its files end to end from the
// drop playhead'. Each file's import read the playhead when it began — after
// the file before it had been copied, analyzed and saved — so a drop during
// playback spread its clips across the song.
#[test]
fn a_drop_lays_its_files_end_to_end_from_the_drop_playhead() {
    let analyze = GatedAnalyze::new();
    let mut h = Harness::open(Setup {
        analyze: analyze.analyzer(),
        ..Setup::default()
    });
    h.at_ms(5000);
    let lane = lane_at(&h, "TAKE 1");
    h.drop_at(lane, &["/src/a.wav", "/src/b.wav"]);
    h.pump_until(|_| analyze.parked() == 1);
    // Playback runs on while the first file imports.
    h.at_ms(11000);
    analyze.release_last();
    h.pump_until(|_| analyze.parked() == 2);
    analyze.release_last();
    h.settle();
    assert_eq!(added_starts(&h, "t1"), [5000, 6000]);
}

// song_practice_placement_test: 'a drop on a set backing says it became
// takes once they have landed'. Said as the drop began, the notice was
// replaced by the first import's own toast, and stood untrue when an import
// then failed.
#[test]
fn a_drop_on_a_set_backing_says_it_became_takes_once_they_have_landed() {
    let analyze = GatedAnalyze::new();
    let mut p = practice(vec![take("t1", "Take 1")]);
    p.accompaniment = Some(Arc::new(backing()));
    let mut h = Harness::open(Setup {
        practice: p,
        analyze: analyze.analyzer(),
        ..Setup::default()
    });
    let lane = lane_at(&h, "BACKING");
    h.drop_at(lane, &["/src/a.wav", "/src/b.wav"]);
    h.pump_until(|_| analyze.parked() == 1);
    let notice = backing_set_toast("Accompaniment");
    assert_ne!(h.screen.snack(), Some(notice.as_str()));
    analyze.release_last();
    h.pump_until(|_| analyze.parked() == 2);
    analyze.release_last();
    h.settle();
    assert_eq!(h.practice().vocal_tracks.len(), 3);
    assert_eq!(h.screen.snack(), Some(notice.as_str()));
    assert!(h.shows(&notice), "the snack bar shows it");
}

// song_practice_placement_test: 'a clip picked from the take menu lands where
// the playhead was at the tap' — "Add audio clip at playhead…" read the
// playhead only once the open panel closed, seconds of playback later.
#[test]
fn a_clip_picked_from_the_take_menu_lands_where_the_playhead_was_at_the_tap() {
    let mut h = Harness::open(Setup::default());
    h.at_ms(5000);
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ContextMenu,
    });
    assert!(h.shows("Add audio clip at playhead…"), "the lane menu is open");
    h.click("Add audio clip at playhead…");
    let (zone, at_ms) = h.picks.pop().expect("the open panel");
    assert_eq!(zone, DropZone::Take("t1".into()));
    // The user browses while the song plays on.
    h.at_ms(12000);
    h.send(Message::Picked {
        zone,
        paths: vec![PathBuf::from("/src/picked.wav")],
        at_ms,
    });
    h.settle();
    assert_eq!(added_starts(&h, "t1"), [5000]);
}

// detail_arrange_journey: 'dropping an audio file imports the backing
// track' — the hint gives way to the backing's lane.
#[test]
fn dropping_an_audio_file_imports_the_backing_track() {
    let mut h = Harness::open(Setup::default());
    let hint = "Drag a backing track here, or click to browse";
    let at = h.find(hint).expect("the backing hint").bounds().center();
    h.drop_at(at, &["/src/backing.wav"]);
    h.settle();
    assert!(h.practice().accompaniment.is_some());
    assert!(!h.shows(hint));
}

#[test]
fn a_drop_with_no_supported_audio_says_so() {
    let mut h = Harness::open(Setup::default());
    let lane = lane_at(&h, "TAKE 1");
    h.drop_at(lane, &["/src/lyrics.txt", "/src/voice.opus"]);
    assert_eq!(h.screen.snack(), Some(NO_AUDIO_IN_DROP));
    assert_eq!(h.files.imports.load(Ordering::SeqCst), 0);
}

#[test]
fn a_drop_hint_clicked_browses_for_its_zone() {
    let mut h = Harness::open(Setup::default());
    h.click("Drag a backing track here, or click to browse");
    assert_eq!(h.picks, [(DropZone::Backing, None)]);
}

// --------------------------------------------------------------- the editors

// detail_arrange_journey: 'adding a note via the take header persists it on
// the take'.
#[test]
fn adding_a_note_via_the_take_header_saves_it_on_the_take() {
    let mut h = Harness::open(Setup::default());
    h.click(&glyph(Icon::StickyNote));
    let editor = h.screen.note_editor().expect("the note editor opened");
    assert_eq!(editor.title(), "Note at 00:00");
    h.send(Message::NoteEditor(note_editor::Message::Edit(
        iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Paste(
            Arc::new("breath runs out here".into()),
        )),
    )));
    h.click("Add");
    assert!(h.screen.note_editor().is_none(), "confirming closes it");
    h.settle();
    let notes: Vec<String> = h
        .repo
        .stored()
        .vocal_tracks
        .iter()
        .flat_map(|t| t.notes.iter().map(|n| n.content.clone()))
        .collect();
    assert_eq!(notes, ["breath runs out here"]);
}

#[test]
fn the_technique_picker_lists_the_library_and_adds_the_pick() {
    let twang = Technique::new("T1", "Twang", at(), at());
    let mut h = Harness::open(Setup {
        techniques: vec![twang],
        ..Setup::default()
    });
    h.at_ms(2000);
    h.click(&glyph(Icon::Waypoints));
    let picker = h.screen.technique_picker().expect("the picker opened");
    assert_eq!(picker.title(), "Add technique at 00:02");
    assert!(picker.is_loading());
    h.settle();
    assert!(!h.screen.technique_picker().expect("open").is_loading());
    h.click("Twang");
    assert!(h.screen.technique_picker().is_none());
    h.settle();
    let markers = &h.track("t1").technique_markers;
    assert_eq!(markers.len(), 1);
    assert_eq!(
        (markers[0].technique_name.as_str(), markers[0].position_ms),
        ("Twang", 2000)
    );
    // Removing it asks first.
    let marker = markers[0].id.clone();
    h.send(Message::Lanes {
        track_id: "t1".into(),
        message: analysis_lanes::Message::EditTechnique(marker),
    });
    let Dialog::Confirm(confirm) = h.dialog() else {
        panic!("a confirmation");
    };
    assert_eq!(confirm.title, "Remove \"Twang\"?");
    h.answer(DialogResult::Confirmed(true));
    h.settle();
    assert!(h.track("t1").technique_markers.is_empty());
}

// --------------------------------------------------------------- the menus

// playhead_repaint_test: 'deleting a clip asks first, as deleting a take
// does'.
#[test]
fn deleting_a_clip_asks_first_as_deleting_a_take_does() {
    let mut t = take("t1", "Take 1");
    t.clips = vec![
        AudioClip {
            id: "c1".into(),
            audio_file_path: "/pool/c1.wav".into(),
            duration_ms: 400,
            start_ms: 0,
            waveform_peaks: Vec::new(),
        },
        AudioClip {
            id: "c2".into(),
            audio_file_path: "/pool/c2.wav".into(),
            duration_ms: 400,
            start_ms: 500,
            waveform_peaks: Vec::new(),
        },
    ];
    let mut h = Harness::open(Setup {
        practice: practice(vec![t]),
        ..Setup::default()
    });
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ClipContextMenu("c2".into()),
    });
    h.click("Delete clip");
    let Dialog::Confirm(confirm) = h.dialog() else {
        panic!("a confirmation");
    };
    assert_eq!(confirm.title, "Delete this clip?");
    assert_eq!(h.track("t1").clips.len(), 2, "nothing deleted before the answer");
    h.answer(DialogResult::Confirmed(false));
    assert_eq!(h.track("t1").clips.len(), 2, "cancelled");

    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ClipContextMenu("c2".into()),
    });
    h.click("Delete clip");
    h.answer(DialogResult::Confirmed(true));
    h.settle();
    let clips: Vec<String> = h.track("t1").clips.iter().map(|c| c.id.clone()).collect();
    assert_eq!(clips, ["c1"]);
}

// playhead_repaint_test: 'deleting the backing's clip asks to delete the
// backing' — a backing lane is its one clip.
#[test]
fn deleting_the_backings_clip_asks_to_delete_the_backing() {
    let mut p = practice(vec![take("t1", "Take 1")]);
    p.accompaniment = Some(Arc::new(backing()));
    let mut h = Harness::open(Setup {
        practice: p,
        ..Setup::default()
    });
    let clip = h.track("acc").clips[0].id.clone();
    h.send(Message::Row {
        track_id: "acc".into(),
        message: track_row::Message::ClipContextMenu(clip),
    });
    h.click("Delete clip");
    let Dialog::Confirm(confirm) = h.dialog() else {
        panic!("a confirmation");
    };
    assert_eq!(confirm.title, "Delete \"Backing\"?");
    h.answer(DialogResult::Confirmed(true));
    h.settle();
    assert!(h.practice().accompaniment.is_none());
}

// playhead_repaint_test: "a lane's menu offers the take as it is now, after
// a rename" — the Rename dialog offered the old name, and accepting it put
// the old name back.
#[test]
fn a_lanes_menu_offers_the_take_as_it_is_now_after_a_rename() {
    let mut h = Harness::open(Setup::default());
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ContextMenu,
    });
    h.click("Rename");
    h.answer(DialogResult::Text(Some("Lead".into())));
    h.settle();
    assert_eq!(h.track("t1").display_name, "Lead");
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ContextMenu,
    });
    h.click("Rename");
    let Dialog::Prompt(prompt) = h.dialog() else {
        panic!("a prompt");
    };
    assert_eq!(prompt.initial.as_deref(), Some("Lead"));
}

// detail_arrange_journey: 'the interpretation switcher opens its menu'.
#[test]
fn the_interpretation_switcher_opens_its_menu_and_creates_one() {
    let mut h = Harness::open(Setup::default());
    h.click(&glyph(Icon::Drama));
    assert_eq!(h.screen.open_menu(), Some(&OpenMenu::Interpretations));
    assert!(h.shows("New interpretation"));
    assert_eq!(h.count("Interpretation 1"), 2, "the chip, and its menu row");
    h.click("New interpretation");
    assert!(h.screen.open_menu().is_none(), "the menu closes first");
    let Dialog::Prompt(prompt) = h.dialog() else {
        panic!("a prompt");
    };
    assert_eq!(prompt.title, "New interpretation");
    assert_eq!(prompt.initial.as_deref(), Some("Interpretation 2"));
    h.answer(DialogResult::Text(Some("Tender".into())));
    h.settle();
    let names: Vec<String> = h.practice().versions.iter().map(|v| v.name.clone()).collect();
    assert_eq!(names, ["Interpretation 1", "Tender"]);
    assert!(h.shows("Tender"), "the chip shows the new interpretation");
}

// detail_arrange_journey: 'the "New take" button adds an empty vocal lane'.
#[test]
fn the_new_take_button_adds_an_empty_vocal_lane() {
    let mut h = Harness::open(Setup::default());
    h.click("New take");
    h.settle();
    assert_eq!(h.practice().vocal_tracks.len(), 2);
    assert!(h.shows("TAKE 2"));
}

// ----------------------------------------------------- the catch-up guard

// playhead_repaint_test: 'a reopen behind a landing save shows the workspace
// but takes no input' — and a file dropped on it is said, not taken.
#[test]
fn a_reopen_behind_a_landing_save_shows_the_workspace_but_takes_no_input() {
    let finalizer = Arc::new(RecordingFinalizer::for_testing());
    let repo = MemRepo::with(practice(vec![take("t1", "Take 1")]));
    let mut first = Harness::open(Setup {
        repo: Some(Arc::clone(&repo)),
        finalizer: Some(Arc::clone(&finalizer)),
        ..Setup::default()
    });
    // A save that is still landing when the page goes.
    let gate = Gate::new();
    repo.s().save_gate = Some(Arc::clone(&gate));
    first.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ToggleMute,
    });
    first.screen.dispose(&first.services);
    assert!(first.screen.lingers(), "the save outlives the page");

    let mut second = Harness::open(Setup {
        repo: Some(Arc::clone(&repo)),
        finalizer: Some(Arc::clone(&finalizer)),
        settle: false,
        ..Setup::default()
    });
    second.pump_until(|h| h.vm().practice().is_some());
    assert!(second.vm().catching_up());
    assert!(second.shows(FINISHING_SAVE));
    assert!(second.shows("TAKE 1"), "the workspace shows beneath");
    let lane = lane_at(&second, "TAKE 1");
    second.drop_at(lane, &["/src/a.wav"]);
    assert_eq!(second.screen.snack(), Some(WAIT_FOR_SAVE));
    assert_eq!(second.files.imports.load(Ordering::SeqCst), 0);

    gate.open();
    first.settle();
    assert!(!first.screen.lingers(), "the save landed");
    second.pump_until(|h| !h.vm().catching_up());
    second.settle();
    assert!(!second.shows(FINISHING_SAVE));
    assert!(second.track("t1").muted, "the reopen shows the landed save");
}

// --------------------------------------------------------------- leaving

// A page left with a save in flight lingers until it lands (Dart's futures
// outlived the route).
#[test]
fn a_page_left_mid_save_lingers_until_the_save_lands() {
    let mut h = Harness::open(Setup::default());
    let gate = Gate::new();
    h.repo.s().save_gate = Some(Arc::clone(&gate));
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ToggleSolo,
    });
    h.screen.dispose(&h.services);
    assert!(h.screen.lingers());
    gate.open();
    h.settle();
    assert!(!h.screen.lingers());
    assert!(h.repo.stored().vocal_tracks[0].soloed);
}

// A fader dragged when the page goes owes its commit (Dart's fader committed
// from its `dispose`): the live level is saved.
#[test]
fn a_fader_held_when_the_page_goes_commits_its_level() {
    let mut h = Harness::open(Setup::default());
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::Volume(0.25),
    });
    assert_eq!(h.repo.s().saves, 0, "a live edit saves nothing");
    h.screen.dispose(&h.services);
    let teardown = std::mem::take(&mut h.screen.teardown);
    for effect in teardown {
        h.run(effect);
    }
    h.settle();
    assert_eq!(h.repo.stored().vocal_tracks[0].volume, 0.25);
    assert!(!h.screen.lingers());
}

// A fader dragged on a lane that leaves the screen (its interpretation
// switched away) commits too.
#[test]
fn a_fader_held_on_a_lane_that_goes_commits_its_level() {
    let mut p = practice(vec![take("t1", "Take 1")]);
    p.versions
        .push(Arc::new(TakeVersion::new("v2", "Interpretation 2", at())));
    let mut h = Harness::open(Setup {
        practice: p,
        ..Setup::default()
    });
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::Volume(0.4),
    });
    h.send(Message::OpenInterpretations);
    h.send(Message::InterpretationMenu(InterpretationAction::SetActive(
        "v2".into(),
    )));
    h.settle();
    let t1 = h
        .repo
        .stored()
        .vocal_tracks
        .iter()
        .find(|t| t.id == "t1")
        .cloned()
        .expect("t1");
    assert_eq!(t1.volume, 0.4);
    assert!(h.screen.owed.is_empty());
}

/// The take-flow screen: a backing and `take1`, the mic armed by REC.
fn take_flow(ready: Option<&Arc<Gate>>, analyze: TakeAnalyzer) -> (Harness, Arc<FakeMic>) {
    let finalizer = Arc::new(RecordingFinalizer::for_testing());
    let mic = FakeMic::new(&finalizer, ready);
    let mut p = practice(vec![take("take1", "Take 1")]);
    p.accompaniment = Some(Arc::new(backing()));
    let h = Harness::open(Setup {
        practice: p,
        mic: Some(Arc::clone(&mic) as Arc<dyn MicRecorder>),
        finalizer: Some(finalizer),
        analyze,
        ..Setup::default()
    });
    (h, mic)
}

fn take_ids(p: &SongPractice) -> Vec<String> {
    p.vocal_tracks.iter().map(|t| t.id.clone()).collect()
}

// song_practice_take_flow_test: 'Back while REC is still arming cancels the
// take before the page leaves'. In REC's arm window the page popped at once:
// the mic's handshake could still land during the exit and arm a take (and
// play the backing) on a page that was leaving — beside the empty "Take N"
// REC had saved to record into.
#[test]
fn back_while_rec_is_still_arming_cancels_the_take_before_the_page_leaves() {
    let ready = Gate::new();
    let (mut h, _mic) = take_flow(Some(&ready), ok_analyze());
    h.send(Message::Transport(detail_transport_bar::Message::StartRecording));
    h.pump_until(|h| h.repo.stored().vocal_tracks.len() == 2);
    assert!(h.vm().recording().is_starting_take(), "REC made a take");

    h.click(&glyph(Icon::ChevronLeft));
    assert_eq!(h.leaves(), 0, "not before the start is cancelled");
    ready.open(); // the handshake lands
    h.settle();
    assert!(!h.vm().is_recording(), "a take armed on a leaving page");
    assert_eq!(h.leaves(), 1);
    assert_eq!(take_ids(&h.repo.stored()), ["take1"]);
}

// song_practice_take_flow_test: 'two back-arrow taps in REC's arm window
// leave the page once' — each queued a pop of its own, and the page left
// one page too far.
#[test]
fn two_back_taps_in_recs_arm_window_leave_the_page_once() {
    let ready = Gate::new();
    let (mut h, _mic) = take_flow(Some(&ready), ok_analyze());
    h.send(Message::Transport(detail_transport_bar::Message::StartRecording));
    h.pump_until(|h| h.repo.stored().vocal_tracks.len() == 2);
    h.click(&glyph(Icon::ChevronLeft));
    // The page is still there (nothing visibly happened): tap Back again.
    h.click(&glyph(Icon::ChevronLeft));
    ready.open();
    h.settle();
    assert_eq!(h.leaves(), 1);
}

// song_practice_take_flow_test: 'a second Back while the take lands leaves
// the page once (systemBack: false)'.
#[test]
fn a_second_back_while_the_take_lands_leaves_the_page_once() {
    let analyze = GatedAnalyze::new();
    analyze.gated.store(false, Ordering::SeqCst);
    let (mut h, _mic) = take_flow(None, analyze.analyzer());
    h.send(Message::Transport(detail_transport_bar::Message::StartRecording));
    h.pump_until(|h| h.vm().is_recording());
    // The finalize's waveform analysis takes a while (a long take).
    analyze.gated.store(true, Ordering::SeqCst);
    h.click(&glyph(Icon::ChevronLeft));
    h.pump_until(|_| analyze.parked() == 1);
    assert_eq!(h.leaves(), 0, "the take is still landing");
    h.click(&glyph(Icon::ChevronLeft));
    analyze.release_last();
    h.settle();
    assert_eq!(h.leaves(), 1);
    assert_eq!(h.repo.stored().vocal_tracks.len(), 2, "the take landed");
}

// The quit hook: a quit while a take records asks the page (on the quit's
// thread, through the waker) to stop it, and returns once it has landed.
#[test]
fn a_quit_while_recording_stops_the_take_and_waits_for_it_to_land() {
    let (mut h, _mic) = take_flow(None, ok_analyze());
    let wakes = h.screen.quit_feed.subscribe();
    h.send(Message::Transport(detail_transport_bar::Message::StartRecording));
    h.pump_until(|h| h.vm().is_recording());
    let finalizer = Arc::clone(&h.finalizer);
    let quit = thread::spawn(move || finalizer.finalize_all());
    // The waker rings (its first ring is the subscription's own).
    let mut wakes = iced::futures::executor::block_on_stream(wakes);
    assert_eq!(wakes.next(), Some(()));
    h.pump_until(|h| h.vm().recording().has_quit_requests());
    assert_eq!(wakes.next(), Some(()), "the quit rang");
    h.send(Message::QuitRequests);
    h.pump_until(|_| quit.is_finished());
    h.settle();
    assert!(!h.vm().is_recording());
    assert_eq!(h.repo.stored().vocal_tracks.len(), 2, "the take was saved");
}

// ---------------------------------------- a fader's drag as lanes change

/// The page with buses `ids` (their names `BUS <id>`).
fn with_buses(ids: &[&str]) -> Harness {
    let mut p = practice(vec![take("t1", "Take 1")]);
    p.fx_buses = ids
        .iter()
        .map(|id| Arc::new(FxBus::new(*id, format!("Bus {id}"), TrackFx::flat())))
        .collect();
    Harness::open(Setup {
        practice: p,
        ..Setup::default()
    })
}

/// Where `name`'s timeline fader sits: in its header's bottom row, right of
/// M and S.
fn fader_at(h: &Harness, name: &str) -> Point {
    let label = h.find(name).unwrap_or_else(|| panic!("{name}")).bounds();
    Point::new(label.x + 90.0, label.y + TRACK_ROW_HEIGHT - 26.0)
}

// kept_subtree_fresh_build_test: 'a bus fader, a bus deleted above it' —
// unkeyed, a bus deleted above one whose fader was being dragged handed the
// rest of the drag to the next.
#[test]
fn a_bus_fader_keeps_its_drag_when_a_bus_above_it_is_deleted() {
    let mut h = with_buses(&["a", "b"]);
    let mut ui = LiveUi::new(h.size);
    let at = fader_at(&h, "BUS B");
    let mut messages = ui.run(
        h.screen.view(),
        [
            live_ui::move_to(at.x, at.y),
            live_ui::press(),
            live_ui::move_to(at.x + 10.0, at.y),
        ],
    );
    assert!(
        messages.iter().any(|m| matches!(
            m,
            Message::SetBusVolumeLive { bus_id, .. } if bus_id == "b"
        )),
        "{messages:?}"
    );
    h.feed(std::mem::take(&mut messages));
    // Bus a goes (sync, say) while b's fader is held.
    let fx = h.screen.vm.mixer_do(|m, host| m.delete_bus(host, "a"));
    let mut step = Step::new();
    h.screen.absorb(fx, &mut step);
    h.absorb(step);
    h.settle();
    let messages = ui.run(
        h.screen.view(),
        [live_ui::move_to(at.x + 20.0, at.y - TRACK_ROW_HEIGHT)],
    );
    assert!(
        messages.iter().all(|m| !matches!(
            m,
            Message::SetBusVolumeLive { bus_id, .. } if bus_id != "b"
        )),
        "the drag stays b's: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| matches!(
            m,
            Message::SetBusVolumeLive { bus_id, .. } if bus_id == "b"
        )),
        "{messages:?}"
    );
}

// kept_subtree_fresh_build_test: 'a take fader, a take deleted above it'.
#[test]
fn a_take_fader_keeps_its_drag_when_a_take_above_it_is_deleted() {
    let mut h = Harness::open(Setup {
        practice: practice(vec![take("t1", "Take 1"), take("t2", "Take 2")]),
        ..Setup::default()
    });
    let mut ui = LiveUi::new(h.size);
    let at = fader_at(&h, "TAKE 2");
    let messages = ui.run(
        h.screen.view(),
        [
            live_ui::move_to(at.x, at.y),
            live_ui::press(),
            live_ui::move_to(at.x + 10.0, at.y),
        ],
    );
    assert!(
        messages.iter().any(|m| matches!(
            m,
            Message::Row { track_id, message: track_row::Message::Volume(_) } if track_id == "t2"
        )),
        "{messages:?}"
    );
    h.feed(messages);
    let fx = h.screen.vm.delete_track("t1");
    let mut step = Step::new();
    h.screen.absorb(fx, &mut step);
    h.absorb(step);
    h.settle();
    let messages = ui.run(
        h.screen.view(),
        [live_ui::move_to(at.x + 20.0, at.y - TRACK_ROW_HEIGHT)],
    );
    assert!(
        messages.iter().any(|m| matches!(
            m,
            Message::Row { track_id, message: track_row::Message::Volume(_) } if track_id == "t2"
        )),
        "the drag stays Take 2's: {messages:?}"
    );
}

// kept_subtree_fresh_build_test: 'the master fader, a bus added above it'.
#[test]
fn the_master_fader_keeps_its_drag_when_a_bus_is_added_above_it() {
    let mut h = with_buses(&["a"]);
    let mut ui = LiveUi::new(h.size);
    let master = h.find("MASTER").expect("the master lane").bounds();
    let at = Point::new(master.x + 60.0, master.y + TRACK_ROW_HEIGHT - 28.0);
    let messages = ui.run(
        h.screen.view(),
        [
            live_ui::move_to(at.x, at.y),
            live_ui::press(),
            live_ui::move_to(at.x + 10.0, at.y),
        ],
    );
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::SetMasterVolumeLive(_))),
        "{messages:?}"
    );
    h.feed(messages);
    h.send(Message::NewBus);
    h.settle();
    let messages = ui.run(
        h.screen.view(),
        [live_ui::move_to(at.x + 20.0, at.y + TRACK_ROW_HEIGHT)],
    );
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::SetMasterVolumeLive(_))),
        "the drag stays the master's: {messages:?}"
    );
}
