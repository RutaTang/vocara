//! The fakes the Dart tests built (a repository, a media service, an
//! analysis, a recorder), on the services' seams, and a rig that runs the
//! view model's jobs on the test thread.
//!
//! The Dart tests parked a fake on a completer to hold an `await` open.
//! Here the rig holds a job kind back ([`Rig::hold`]) — the job has not run,
//! so the fake has not been called — or runs a job and keeps its landing
//! ([`Rig::run_one`], then [`Rig::land`]) where the Dart fake did its work
//! first and parked after.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::models::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_path;
use vocara_services::audio_exporter::AudioExporter;
use vocara_services::audio_file_service::{AudioFileError, AudioFileService};
use vocara_services::engine::media::MediaError;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::media_analysis_service::MediaAnalyses;
use vocara_services::mic_recording_controller::{
    DestinationError, MicEvent, MicRecorder, MicRecordingStart, MicRecordingStop, PendingStart,
    RecordingPathStrategy, StopFinalizeGate, StopFinalizeHold,
};
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::technique_media_service::{TechniqueMediaFiles, TechniqueMediaService};
use vocara_store::repositories::TechniqueRepository;
use vocara_store::sync::sync_engine::RemoteApplied;
use vocara_store::sync::sync_hashes::Loaded;

use super::super::{
    Answer, BaseHashes, Done, Effect, Job, JobKind, Next, TechniqueDetailDeps,
    TechniqueDetailViewModel,
};

/// The technique id most Dart tests used.
pub const ID: &str = "T1000000000000000000000000";

/// `DateTime(2026, 6, 20)`.
pub fn june20() -> DateTime {
    DateTime::local_ymd(2026, 6, 20)
}

/// An audio example `id` at `path`.
pub fn example(id: &str, path: &str) -> TechniqueMedia {
    TechniqueMedia {
        id: id.to_string(),
        path: path.to_string(),
        kind: TechniqueMediaKind::Audio,
        label: String::new(),
        added_at: june20(),
    }
}

/// "Twang" with [media].
pub fn twang(id: &str, media: Vec<TechniqueMedia>) -> Technique {
    Technique {
        media,
        ..Technique::new(id, "Twang", june20(), june20())
    }
}

/// Base hashes from pairs.
pub fn hashes(pairs: &[(&str, &str)]) -> BaseHashes {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A remote page touching technique [id].
pub fn remote(id: &str) -> RemoteApplied {
    RemoteApplied {
        rows: Vec::new(),
        practice_ids: Default::default(),
        technique_ids: [id.to_string()].into_iter().collect(),
    }
}

/// The Dart tests' canned `_analysis()`: four frames, none active.
pub fn canned_analysis() -> TechniqueAnalysis {
    let flat = |v: f32| vec![v; 4];
    let metrics = VoiceMetrics {
        frames: 4,
        hop: 512,
        fft_size: 2048,
        sample_rate: 44100,
        ring: flat(0.0),
        tilt: flat(0.0),
        cpp: flat(0.0),
        loudness: flat(-20.0),
        f0: flat(200.0),
        f1: flat(f32::NAN),
        f2: flat(f32::NAN),
        f3: flat(f32::NAN),
        h1h2: flat(f32::NAN),
        active: vec![0; 4],
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: -6.0,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    };
    TechniqueAnalysis {
        metrics: Arc::new(metrics),
        spectrogram: Arc::new(Spectrogram {
            frames: 2,
            bins: 2,
            max_hz: 5000.0,
            sample_rate: 44100,
            fft_size: 2048,
            hop: 512,
            db: vec![0.0; 4],
            min_db: -80.0,
            max_db: 0.0,
        }),
        ltas: Arc::new(Ltas {
            db: vec![0.0; 4],
            bin_hz: 21.5,
            max_hz: 5000.0,
            max_db: -20.0,
            alpha_ratio: -12.0,
            singers_formant_db: 3.0,
            frame_count: 2,
        }),
        profile: Arc::new(VoiceProfile {
            low_midi: Some(48),
            high_midi: Some(72),
            tess_lo_midi: Some(55),
            tess_hi_midi: Some(65),
            passaggio_midi: None,
            passaggio_jump_db: 0.0,
            soft_db: -30.0,
            loud_db: -10.0,
            median_db: -20.0,
        }),
        register: Arc::new(RegisterMap {
            score: vec![0.0; 4],
            chest_pct: 50.0,
            mix_pct: 30.0,
            head_pct: 20.0,
            voiced_frames: 4,
        }),
        duration_ms: 1000,
    }
}

// --- The repository ------------------------------------------------------------

/// What a save does with what it was sent.
#[derive(Default)]
pub enum SaveMode {
    /// Stores it and reports it re-read, with the stored hashes.
    #[default]
    Store,
    /// Stores it with every example under `<pool>/<id>.wav` (the drift
    /// repository's hydrate through the asset pool) and reports that.
    Pool(String),
    /// Stores it pooled, but reports its re-read failed: what was sent comes
    /// back, staging paths and all.
    PoolRereadFails(String),
    /// Commits a merge of what it was sent (a concurrent writer's row kept)
    /// but reports its re-read failed.
    CommitWithoutReread(fn(Technique) -> Technique),
}

#[derive(Default)]
pub struct RepoState {
    pub technique: Option<Technique>,
    pub stored_hashes: BaseHashes,
    pub loads: usize,
    /// Loads still to fail before the store answers again.
    pub load_failures: usize,
    /// Loads fail for good (a technique deleted underneath).
    pub unreadable: bool,
    pub base_saves: usize,
    pub saved_with_base: Vec<BaseHashes>,
    pub saved: Vec<Technique>,
    pub save_mode: SaveMode,
    /// The next save fails (and clears the flag).
    pub fail_next_save: bool,
    /// Like `resolveOrImport`: a save refuses media whose file is gone.
    pub require_files: bool,
    pub deletes: usize,
}

/// A technique repository a test edits as another device would.
#[derive(Default)]
pub struct FakeRepo {
    st: Mutex<RepoState>,
}

impl FakeRepo {
    pub fn new(technique: Technique) -> Arc<FakeRepo> {
        Arc::new(FakeRepo {
            st: Mutex::new(RepoState {
                technique: Some(technique),
                ..RepoState::default()
            }),
        })
    }

    pub fn st(&self) -> MutexGuard<'_, RepoState> {
        self.st.lock().unwrap()
    }

    pub fn technique(&self) -> Technique {
        self.st().technique.clone().unwrap()
    }

    /// Sync applying a remote edit: the stored row and its hash move on.
    pub fn remote_edit(&self, edit: impl FnOnce(&mut Technique), hash: &str) {
        let mut st = self.st();
        edit(st.technique.as_mut().unwrap());
        st.stored_hashes = hashes(&[("t1", hash)]);
    }
}

fn store_error<T>(message: &str) -> vocara_store::Result<T> {
    Err(vocara_store::Error::State(message.to_string()))
}

fn pooled(t: &Technique, pool: &str) -> Technique {
    Technique {
        media: t
            .media
            .iter()
            .map(|m| TechniqueMedia {
                path: dart_path::join(pool, &format!("{}.wav", m.id)),
                ..m.clone()
            })
            .collect(),
        ..t.clone()
    }
}

impl TechniqueRepository for FakeRepo {
    fn list_techniques(&self) -> vocara_store::Result<Vec<Technique>> {
        Ok(self.st().technique.clone().into_iter().collect())
    }

    fn get_technique(&self, _id: &str) -> vocara_store::Result<Technique> {
        match self.st().technique.clone() {
            Some(t) => Ok(t),
            None => store_error("not found"),
        }
    }

    fn load_technique(&self, _id: &str) -> vocara_store::Result<Loaded<Technique>> {
        let mut st = self.st();
        st.loads += 1;
        if st.load_failures > 0 {
            st.load_failures -= 1;
            return store_error("read failed");
        }
        if st.unreadable {
            return store_error("simulated read failure");
        }
        match st.technique.clone() {
            Some(t) => Ok(Loaded::new(t, st.stored_hashes.clone())),
            None => store_error("not found"),
        }
    }

    fn save_technique(&self, technique: &Technique) -> vocara_store::Result<Technique> {
        self.st().technique = Some(technique.clone());
        Ok(technique.clone())
    }

    fn save_technique_with_base(
        &self,
        technique: &Technique,
        base_hashes: &BaseHashes,
    ) -> vocara_store::Result<Loaded<Technique>> {
        let mut st = self.st();
        st.base_saves += 1;
        st.saved_with_base.push(base_hashes.clone());
        st.saved.push(technique.clone());
        if st.fail_next_save {
            st.fail_next_save = false;
            return store_error("simulated write failure");
        }
        if st.require_files {
            for m in &technique.media {
                if !Path::new(&m.path).exists() {
                    return store_error("missing file");
                }
            }
        }
        let t = technique.clone();
        match &st.save_mode {
            SaveMode::Store => {
                st.technique = Some(t.clone());
                Ok(Loaded::new(t, st.stored_hashes.clone()))
            }
            SaveMode::Pool(pool) => {
                let stored = pooled(&t, pool);
                st.technique = Some(stored.clone());
                Ok(Loaded::new(stored, BaseHashes::default()))
            }
            SaveMode::PoolRereadFails(pool) => {
                st.technique = Some(pooled(&t, pool));
                Ok(Loaded {
                    aggregate: t,
                    base_hashes: BaseHashes::default(),
                    sent_hashes: Some(BaseHashes::default()),
                    reread: false,
                })
            }
            SaveMode::CommitWithoutReread(merge) => {
                st.technique = Some(merge(t.clone()));
                st.stored_hashes = hashes(&[("t1", "MERGED-HASH")]);
                Ok(Loaded {
                    aggregate: t,
                    base_hashes: hashes(&[("t1", "SENT-HASH")]),
                    sent_hashes: Some(hashes(&[("t1", "SENT-HASH")])),
                    reread: false,
                })
            }
        }
    }

    fn delete_technique(&self, _id: &str) -> vocara_store::Result<()> {
        let mut st = self.st();
        st.deletes += 1;
        st.technique = None;
        Ok(())
    }
}

// --- The media service -----------------------------------------------------------

/// Where an import puts its copy.
pub enum ImportTo {
    /// The real service's copy into the sandbox.
    Real(TechniqueMediaService),
    /// `/sandbox/<basename>` (no disk).
    Sandbox,
    /// `/pool/<technique id>-<n>.wav` (no disk).
    Pool,
}

pub struct FakeMedia {
    pub import_to: ImportTo,
    /// Every import refused in these words (the size cap).
    pub refuse: Option<String>,
    /// Every delete fails.
    pub fail_deletes: bool,
    pub imports: AtomicUsize,
    pub deletes: AtomicUsize,
    pub deleted: Mutex<Vec<String>>,
}

impl FakeMedia {
    fn with(import_to: ImportTo) -> FakeMedia {
        FakeMedia {
            import_to,
            refuse: None,
            fail_deletes: false,
            imports: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
            deleted: Mutex::new(Vec::new()),
        }
    }

    /// The real service over storage rooted at [root].
    pub fn real(root: &Path) -> FakeMedia {
        FakeMedia::with(ImportTo::Real(TechniqueMediaService::new(Arc::new(
            LocalStorageService::with_root(root.to_path_buf()),
        ))))
    }

    pub fn sandbox() -> FakeMedia {
        FakeMedia::with(ImportTo::Sandbox)
    }

    pub fn pool() -> FakeMedia {
        FakeMedia::with(ImportTo::Pool)
    }

    pub fn deleted(&self) -> Vec<String> {
        self.deleted.lock().unwrap().clone()
    }
}

impl TechniqueMediaFiles for FakeMedia {
    fn import_media(
        &self,
        technique_id: &str,
        source_path: &str,
    ) -> Result<String, AudioFileError> {
        let n = self.imports.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(reason) = &self.refuse {
            return Err(AudioFileError::Format(reason.clone()));
        }
        match &self.import_to {
            ImportTo::Real(service) => service.import_media(technique_id, source_path),
            ImportTo::Sandbox => Ok(format!("/sandbox/{}", dart_path::basename(source_path))),
            ImportTo::Pool => Ok(format!("/pool/{technique_id}-{n}.wav")),
        }
    }

    fn delete_media(&self, path: &str) -> Result<(), AudioFileError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        if self.fail_deletes {
            return Err(AudioFileError::Io(std::io::Error::other(
                "injected delete failure",
            )));
        }
        self.deleted.lock().unwrap().push(path.to_string());
        match &self.import_to {
            ImportTo::Real(service) => service.delete_media(path),
            _ => Ok(()),
        }
    }

    fn delete_technique_media(&self, technique_id: &str) -> Result<(), AudioFileError> {
        if self.fail_deletes {
            return Err(AudioFileError::Io(std::io::Error::other(
                "injected delete failure",
            )));
        }
        match &self.import_to {
            ImportTo::Real(service) => service.delete_technique_media(technique_id),
            _ => Ok(()),
        }
    }
}

// --- The analysis -----------------------------------------------------------------

/// What an analysis call does once admitted.
#[derive(Clone, Default)]
pub enum Outcome {
    /// Nothing decodes (`null`).
    #[default]
    Nothing,
    /// The same canned instance every call (identity tells a carried
    /// analysis from a recomputed one).
    Fixed(TechniqueAnalysis),
    /// A fresh canned analysis per call.
    Fresh,
    /// Every call refuses with this.
    Refuse(MediaError),
}

#[derive(Default)]
pub struct AnalysisState {
    pub outcome: Outcome,
    /// Thrown by the next call only (then the outcome applies again).
    pub throw_next: Option<MediaError>,
    /// The next call panics (an isolate that died).
    pub panic_next: bool,
    /// Calls for a path containing this refuse with the error.
    pub refuse_paths: Option<(String, MediaError)>,
    /// Every path asked for, in order.
    pub asked: Vec<String>,
    /// The paths decoded (wanted when admitted).
    pub decoded: Vec<String>,
    /// The paths dropped as unwanted.
    pub skipped: Vec<String>,
}

/// An analysis that asks `wanted` when admitted, as the real service does.
#[derive(Default)]
pub struct FakeAnalysis {
    st: Mutex<AnalysisState>,
}

impl FakeAnalysis {
    pub fn new(outcome: Outcome) -> Arc<FakeAnalysis> {
        let fake = FakeAnalysis::default();
        fake.st().outcome = outcome;
        Arc::new(fake)
    }

    pub fn st(&self) -> MutexGuard<'_, AnalysisState> {
        self.st.lock().unwrap()
    }

    pub fn calls(&self) -> usize {
        self.st().asked.len()
    }
}

impl MediaAnalyses for FakeAnalysis {
    fn analyze(
        &self,
        path: &str,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> Result<Option<TechniqueAnalysis>, MediaError> {
        let (throw, panic) = {
            let mut st = self.st();
            st.asked.push(path.to_string());
            (st.throw_next.take(), std::mem::take(&mut st.panic_next))
        };
        if panic {
            panic!("isolate died");
        }
        if let Some(e) = throw {
            return Err(e);
        }
        if wanted.is_some_and(|w| !w()) {
            self.st().skipped.push(path.to_string());
            return Ok(None);
        }
        let mut st = self.st();
        st.decoded.push(path.to_string());
        if let Some((part, e)) = &st.refuse_paths {
            if path.contains(part.as_str()) {
                return Err(e.clone());
            }
        }
        match &st.outcome {
            Outcome::Nothing => Ok(None),
            Outcome::Fixed(a) => Ok(Some(a.clone())),
            Outcome::Fresh => Ok(Some(canned_analysis())),
            Outcome::Refuse(e) => Err(e.clone()),
        }
    }
}

// --- The recorder -------------------------------------------------------------------

/// How the next start resolves.
#[derive(Clone, Default)]
pub enum StartMode {
    /// Arms in a fresh temp dir of its own, with a WAV stub in it (the view
    /// model deletes a take's whole parent dir on cleanup).
    #[default]
    TempDir,
    /// Arms in `<dir>/practice_take/take.wav`.
    In(PathBuf),
    /// Arms at a path in a folder that does not exist (no disk).
    At(String),
    /// Resolves to this outcome.
    Resolve(MicRecordingStart),
    /// Throws as a file system failure (the temp folder could not be made).
    Throw,
}

pub struct RecState {
    pub recording: bool,
    pub starting: bool,
    pub path: Option<String>,
    pub clean: bool,
    pub dropped_frames: u64,
    pub mode: StartMode,
    pub takes: u8,
    pub stops: usize,
    pub disposed: bool,
    /// Events the next tick reports.
    pub events: Vec<MicEvent>,
}

/// The Dart tests' recorder fakes in one: a start resolves as [`StartMode`]
/// says when its job runs; the stop-finalize gate is the real one.
pub struct FakeRecorder {
    st: Arc<Mutex<RecState>>,
    gate: StopFinalizeGate,
    /// Whether the live monitor reads as fed while recording.
    pub feeds_monitor: bool,
}

impl FakeRecorder {
    pub fn new(finalizer: &Arc<RecordingFinalizer>) -> Arc<FakeRecorder> {
        Arc::new(FakeRecorder {
            st: Arc::new(Mutex::new(RecState {
                recording: false,
                starting: false,
                path: None,
                clean: true,
                dropped_frames: 0,
                mode: StartMode::TempDir,
                takes: 0,
                stops: 0,
                disposed: false,
                events: Vec::new(),
            })),
            gate: StopFinalizeGate::new(Arc::clone(finalizer)),
            feeds_monitor: false,
        })
    }

    pub fn st(&self) -> MutexGuard<'_, RecState> {
        self.st.lock().unwrap()
    }
}

fn resolve_start(st: &Mutex<RecState>) -> Result<MicRecordingStart, DestinationError> {
    let mut st = st.lock().unwrap();
    st.starting = false;
    let mode = st.mode.clone();
    let armed = |st: &mut RecState, path: String| {
        st.path = Some(path.clone());
        st.recording = true;
        Ok(MicRecordingStart::Started(path))
    };
    match mode {
        StartMode::TempDir => {
            st.takes += 1;
            let dir = tempfile::Builder::new()
                .prefix("vocara_fake_take")
                .tempdir()
                .unwrap()
                .keep();
            let path = dir.join("take.wav");
            std::fs::write(&path, [1, 2, 3, st.takes]).unwrap();
            armed(&mut st, path.to_string_lossy().into_owned())
        }
        StartMode::In(dir) => {
            let d = dir.join("practice_take");
            std::fs::create_dir_all(&d).unwrap();
            let path = d.join("take.wav");
            std::fs::write(&path, [1, 2, 3]).unwrap();
            armed(&mut st, path.to_string_lossy().into_owned())
        }
        StartMode::At(path) => armed(&mut st, path),
        StartMode::Resolve(outcome) => {
            st.recording = outcome.started();
            st.path = outcome.path().map(str::to_string);
            Ok(outcome)
        }
        StartMode::Throw => Err(Box::new(std::io::Error::other("createTemp failed"))),
    }
}

impl MicRecorder for FakeRecorder {
    fn level(&self) -> f64 {
        0.0
    }

    fn revision(&self) -> u64 {
        0
    }

    fn is_recording(&self) -> bool {
        self.st().recording
    }

    fn is_starting(&self) -> bool {
        self.st().starting
    }

    fn feeding_monitor(&self) -> bool {
        self.feeds_monitor && self.st().recording
    }

    fn invalidate_pending_start(&self) {
        self.st().starting = false;
    }

    fn begin_start(&self, _destination: RecordingPathStrategy) -> PendingStart {
        self.st().starting = true;
        let st = Arc::clone(&self.st);
        PendingStart::from_fn(move || resolve_start(&st))
    }

    fn tick(&self, on_event: &mut dyn FnMut(MicEvent)) {
        let events = std::mem::take(&mut self.st().events);
        for event in events {
            on_event(event);
        }
    }

    fn next_tick_due(&self) -> Option<Instant> {
        let st = self.st();
        (st.recording || !st.events.is_empty()).then(Instant::now)
    }

    fn stop(&self) -> Option<MicRecordingStop> {
        let mut st = self.st();
        if !st.recording {
            return None;
        }
        st.recording = false;
        st.stops += 1;
        Some(MicRecordingStop {
            path: st
                .path
                .clone()
                .unwrap_or_else(|| "/nonexistent/take.wav".to_string()),
            clean: st.clean,
            dropped_frames: st.dropped_frames,
        })
    }

    fn abort(&self) -> Result<(), DestinationError> {
        self.st().recording = false;
        Ok(())
    }

    fn begin_stop_finalize(&self) -> StopFinalizeHold {
        self.gate.begin()
    }

    fn dispose(&self) {
        self.st().disposed = true;
    }
}

// --- The rig ---------------------------------------------------------------------------

/// A built view model and its collaborators.
pub struct Setup {
    pub repo: Arc<FakeRepo>,
    pub media: Arc<FakeMedia>,
    pub analysis: Arc<FakeAnalysis>,
    pub recorder: Arc<FakeRecorder>,
    pub finalizer: Arc<RecordingFinalizer>,
    pub tmp: tempfile::TempDir,
    pub id: String,
    pub exporter: Option<Arc<AudioExporter>>,
}

impl Setup {
    /// Fakes over [technique]: the real media service in a temp root, no
    /// analysis, a temp-dir recorder, a private finalizer.
    pub fn new(technique: Technique) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let finalizer = Arc::new(RecordingFinalizer::for_testing());
        Setup {
            id: technique.id.clone(),
            repo: FakeRepo::new(technique),
            media: Arc::new(FakeMedia::real(tmp.path())),
            analysis: FakeAnalysis::new(Outcome::Nothing),
            recorder: FakeRecorder::new(&finalizer),
            finalizer,
            tmp,
            exporter: None,
        }
    }

    pub fn media(mut self, media: FakeMedia) -> Setup {
        self.media = Arc::new(media);
        self
    }

    pub fn analysis(mut self, outcome: Outcome) -> Setup {
        self.analysis = FakeAnalysis::new(outcome);
        self
    }

    pub fn exporter(mut self, exporter: AudioExporter) -> Setup {
        self.exporter = Some(Arc::new(exporter));
        self
    }

    /// The temp root.
    pub fn root(&self) -> &Path {
        self.tmp.path()
    }

    pub fn deps(&self) -> TechniqueDetailDeps {
        let exporter = self.exporter.clone().unwrap_or_else(|| {
            Arc::new(
                AudioExporter::new(
                    Arc::new(AudioFileService::new(Arc::new(
                        LocalStorageService::with_root(self.root().join("app")),
                    ))),
                    |_| Ok(None),
                )
                .with_finalizer(Arc::clone(&self.finalizer)),
            )
        });
        TechniqueDetailDeps {
            analysis: self.analysis.clone(),
            finalizer: Arc::clone(&self.finalizer),
            recorder: Some(self.recorder.clone()),
            ..TechniqueDetailDeps::new(
                self.id.clone(),
                self.repo.clone(),
                self.media.clone(),
                exporter,
            )
        }
    }

    /// The view model, its first load queued (not run).
    pub fn rig(&self) -> Rig {
        let (vm, next) = TechniqueDetailViewModel::new(self.deps());
        Rig::from_parts(vm, next)
    }

    /// The view model with its first load landed.
    pub fn loaded(&self) -> Rig {
        let mut rig = self.rig();
        rig.settle();
        assert!(rig.vm.technique().is_some(), "loaded");
        rig
    }
}

/// Runs the view model's jobs on the test thread, in the order queued, and
/// keeps what it asks of the screen.
pub struct Rig {
    pub vm: TechniqueDetailViewModel,
    queue: VecDeque<Job>,
    held: HashSet<JobKind>,
    pub answers: Vec<Answer>,
    /// `micArmed` signals seen.
    pub pauses: usize,
    /// `analysisScrub` requests seen.
    pub seeks: Vec<Duration>,
}

impl Rig {
    /// A rig over [vm], with what its creation asked taken in.
    pub fn from_parts(vm: TechniqueDetailViewModel, next: Next) -> Rig {
        let mut rig = Rig {
            vm,
            queue: VecDeque::new(),
            held: HashSet::new(),
            answers: Vec::new(),
            pauses: 0,
            seeks: Vec::new(),
        };
        rig.apply(next);
        rig
    }

    /// Takes in what a call asked.
    pub fn apply(&mut self, next: Next) {
        for effect in next.effects {
            match effect {
                Effect::Run(job) => self.queue.push_back(job),
                Effect::PauseExamples => self.pauses += 1,
                Effect::SeekAnalyzed(to) => self.seeks.push(to),
            }
        }
        self.answers.extend(next.answers);
    }

    /// A call on the view model; what it asks is taken in.
    pub fn call(&mut self, f: impl FnOnce(&mut TechniqueDetailViewModel) -> Next) {
        let next = f(&mut self.vm);
        self.apply(next);
    }

    /// A call, then everything it started that is not held runs.
    pub fn call_settled(&mut self, f: impl FnOnce(&mut TechniqueDetailViewModel) -> Next) {
        self.call(f);
        self.settle();
    }

    /// Lands a report.
    pub fn land(&mut self, done: Done) {
        let next = self.vm.land(done);
        self.apply(next);
    }

    /// Runs and lands every queued job not held, until none is left.
    pub fn settle(&mut self) {
        while let Some(i) = self
            .queue
            .iter()
            .position(|j| !self.held.contains(&j.kind()))
        {
            let job = self.queue.remove(i).unwrap();
            let done = job.run();
            self.land(done);
        }
    }

    /// Jobs of [kind] wait (Dart's completer not yet completed).
    pub fn hold(&mut self, kind: JobKind) {
        self.held.insert(kind);
    }

    /// Lets [kind] run again, and settles.
    pub fn release(&mut self, kind: JobKind) {
        self.held.remove(&kind);
        self.settle();
    }

    /// Queued jobs of [kind].
    pub fn queued(&self, kind: JobKind) -> usize {
        self.queue.iter().filter(|j| j.kind() == kind).count()
    }

    /// Runs the first queued job of [kind] without landing it.
    pub fn run_one(&mut self, kind: JobKind) -> Done {
        let i = self
            .queue
            .iter()
            .position(|j| j.kind() == kind)
            .unwrap_or_else(|| panic!("no {kind:?} job queued"));
        self.queue.remove(i).unwrap().run()
    }

    /// The answers so far, taken.
    pub fn take_answers(&mut self) -> Vec<Answer> {
        std::mem::take(&mut self.answers)
    }

    /// The last answer, taken with the rest.
    pub fn last_answer(&mut self) -> Answer {
        self.take_answers().pop().expect("an answer")
    }

    /// Runs the page's teardown and its clean-up jobs.
    pub fn dispose(&mut self) {
        let next = self.vm.dispose();
        self.apply(next);
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // The Dart tests' addTearDown(vm.dispose): the clean-up runs.
        if !std::thread::panicking() {
            self.dispose();
            self.settle();
        }
    }
}

/// A file with [bytes] at [dir]/[name].
pub fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}
