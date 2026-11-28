//! Port of `ui/technique_library/view_model/technique_detail_view_model.dart`:
//! the view model of a single technique's page — view/edit its text fields,
//! manage its audio/video example clips, compute the reference "ground
//! truth" analysis from a selected example, and run an ephemeral practice
//! recording to compare against it.
//!
//! Media edits made while editing are STAGED (added/removed/renamed/recorded
//! in a draft) and only persisted when the form is saved — the same buffered
//! model as the text fields, so nothing touches the stored technique until
//! the user explicitly saves. Cancelling discards the draft (and any files
//! copied for it).
//!
//! Implements [`AnalysisSource`] so the same bottom-dock analysis views
//! (spectrogram / pitch / timbre / vibrato / register / range / dynamics)
//! render here, switching between the reference and practice subjects — and
//! showing the live mic in real time while a recording is in progress.
//!
//! # How Dart's async maps here
//!
//! The view model is plain state with no iced types. Every Dart method that
//! awaited I/O runs here up to that `await` and returns a [`Next`]: the
//! [`Effect`]s to run and the [`Answer`]s owed so far. An
//! [`Effect::Run`] carries a [`Job`] — the I/O itself — which the screen
//! runs on a blocking worker ([`Job::run`]) and hands back as a [`Done`] to
//! [`TechniqueDetailViewModel::land`], where the Dart code after the
//! `await` runs (its generation and dispose checks, its `finally`); landing
//! returns the next [`Next`]. The flows are kept as continuations, in Dart's
//! order: an import copies one file per job and checks for a cancel between
//! them, a commit's clean-up deletes one file per job, as Dart's loops
//! awaited one at a time. A Dart method's return value is the [`Answer`] it
//! settles with (`save`'s `bool`, a start's error message).
//!
//! Dart's `notifyListeners` bumps [`TechniqueDetailViewModel::revision`]
//! (never after dispose); its `position` notifier is
//! [`TechniqueDetailViewModel::position`], read each frame; its two signals
//! for the example players are effects: `micArmed` is
//! [`Effect::PauseExamples`], `analysisScrub` is [`Effect::SeekAnalyzed`].
//! `remoteApplied` is the screen's: it hands each event to
//! [`TechniqueDetailViewModel::on_remote_applied`].
//!
//! # The recorder's timers and the quit
//!
//! The shared [`MicRecordingController`] (the [`MicRecorder`] trait is the
//! tests' fake seam) runs its 30 ms capture pump and 50 ms level/device
//! watch as ticks: while [`TechniqueDetailViewModel::next_tick_due`] is
//! `Some`, the screen subscribes to
//! `iced::time::every(MicRecordingController::TICK_PERIOD)` and calls
//! [`TechniqueDetailViewModel::tick`], which also delivers the device events
//! (an input lost or at its length limit stops the take; a degraded input is
//! a notice). A quit while a take is armed calls the controller's quit
//! finalize on a finalizer thread: it is queued for the view model and
//! waits; the queue keeps the tick due, and the tick runs Dart's
//! `quitFinalize` (the example take committed, the practice take cleaned
//! up), answering the waiting quit once that has settled.
//!
//! # What the detail screen calls
//!
//! Creation: [`TechniqueDetailViewModel::new`] over a
//! [`TechniqueDetailDeps`] (`from_services` with the app's save panel for
//! exports), running the returned [`Next`] (the first load). Then, from its
//! `update`:
//!
//! - lands every job: [`TechniqueDetailViewModel::land`];
//! - after any change, asks [`TechniqueDetailViewModel::ensure_reference_analysis`]
//!   (Dart's post-frame call: safe to call repeatedly);
//! - editing: [`TechniqueDetailViewModel::can_edit`] (checked at tap time
//!   too), then [`TechniqueDetailViewModel::begin_media_draft`] and
//!   [`TechniqueDetailViewModel::begin_edit`] before seeding the form from
//!   [`TechniqueDetailViewModel::technique`] as of that tap; Save builds the
//!   edited technique from the form and calls
//!   [`TechniqueDetailViewModel::save`] (on [`Answer::Saved`]`(true)` it
//!   tears the form down and calls [`TechniqueDetailViewModel::end_edit`]);
//!   Cancel calls [`TechniqueDetailViewModel::discard_media_draft`] (on
//!   [`Answer::DraftDiscarded`]`(true)` the same). The edit surface is
//!   frozen while [`TechniqueDetailViewModel::saving`]; Save is blocked while
//!   `saving || saving_example || importing_media`; Cancel's discard prompt
//!   is due when the form is dirty or [`TechniqueDetailViewModel::has_media_draft`],
//!   `is_recording_example`, `saving_example`, `starting` or
//!   `importing_media`;
//! - the examples: [`TechniqueDetailViewModel::effective_media`],
//!   `add_media`, `remove_media` (its prompt reads `is_unsaved_media`),
//!   `rename_media`, `export_media`, `start_example_recording` /
//!   `stop_example_recording` (with `is_recording_example`, `saving_example`,
//!   `starting`, `can_record_example`, `record_level`), the inline analysis
//!   (`reference_media` is the analyzed example, `expanded_example_id`,
//!   `toggle_example_expanded`), and for the players `mic_live` (their
//!   `play_blocked`), `set_playhead_from_playback`, and the two effects
//!   above;
//! - the analysis card: [`AnalysisSource`] plus `analysis_dim` /
//!   `set_analysis_dim`, `analysis_subject` / `show_analysis_subject`,
//!   `reference_analysis*`, `retry_reference_analysis`, `practice_analysis*`,
//!   `start_practice` / `stop_practice` / `clear_practice`,
//!   `is_recording_practice`, `starting_practice`, `can_record_practice`,
//!   and [`TechniqueDetailViewModel::seek`] for a scrub;
//! - `recording_notice` is a one-shot toast: shown, then
//!   `clear_recording_notice`;
//! - `delete` (on [`Answer::Deleted`]`(true)` back to the library);
//! - the recorder's ticks (above) and remote changes;
//! - when the page goes: [`TechniqueDetailViewModel::end_edit`] (the form's
//!   teardown) and [`TechniqueDetailViewModel::dispose`], whose jobs it runs
//!   with [`Job::detach`] (nothing waits for them). A landing that arrives
//!   after the dispose still lands, as Dart's futures ran on: route it here
//!   if the page can.

mod jobs;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::spectrogram::{AnalysisFraming, Spectrogram};
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::technique_media::TechniqueMedia;
use vocara_model::models::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_num::clamp_i64;
use vocara_model::util::id::new_id;
use vocara_model::util::lru_bytes_cache::{analysis_cache_budget, LruBytesCache};
use vocara_model::util::{dart_path, dart_string};
use vocara_services::app_settings_service::AppSettingsService;
use vocara_services::audio_exporter::{AudioExporter, PickError};
use vocara_services::audio_file_service::AudioFileError;
use vocara_services::engine::media::MediaError;
use vocara_services::graph_audio_playback_service::GraphAudioPlaybackService;
use vocara_services::media_analysis_service::{MediaAnalyses, MediaAnalysisService};
use vocara_services::mic_permission;
use vocara_services::mic_recording_controller::{
    delete_temp_recording_dir, MicEvent, MicRecorder, MicRecordingController, MicRecordingFailure,
    MicRecordingOptions, MicRecordingStart, MicRecordingStop, QuitFinalize, RecordingPathStrategy,
};
use vocara_services::monitor_service::{LiveMonitor, MonitorService};
use vocara_services::playback_engine::PlaybackEngine;
use vocara_services::recording_finalizer::{Finalizer, RecordingFinalizer};
use vocara_services::technique_media_service::TechniqueMediaFiles;
use vocara_store::db::AssetStore;
use vocara_store::repositories::TechniqueRepository;
use vocara_store::sync::sync_engine::RemoteApplied;
use vocara_store::sync::sync_hashes::Loaded;
// `IndexMap<String, String>`, the base hashes' type, without naming
// `indexmap` (not a dependency of the app).
use vocara_store::sync::sync_row::FieldClocks as BaseHashes;

use crate::services::Services;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::util::command::{Command, Command0};
use crate::util::ActionError;

pub use jobs::{Answer, CreateEngine, Done, Effect, Job, JobKind, Next};
use jobs::{EngineSlot, Keep, Probe, QuitRequests, Value};

/// Which clip the analysis card is showing: the technique's reference
/// example ("ground truth") or the user's just-recorded practice take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TechniqueAnalysisSubject {
    Reference,
    Practice,
}

/// A cause to say for a reference example's failed analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FailureCause {
    text: String,
    /// Whether another try can succeed.
    retryable: bool,
}

/// What the view model is built over (Dart's constructor parameters).
pub struct TechniqueDetailDeps {
    pub technique_id: String,
    pub repository: Arc<dyn TechniqueRepository>,
    pub media: Arc<dyn TechniqueMediaFiles>,
    /// Copies an example out of the app (its save panel is the app's).
    pub exporter: Arc<AudioExporter>,
    /// The reference and practice analyses (default: the engine's decode).
    pub analysis: Arc<dyn MediaAnalyses>,
    /// Where the takes record from (the input chosen in Settings).
    pub app_settings: Option<Arc<AppSettingsService>>,
    /// The quit registry an armed take and an in-flight save hold the app's
    /// exit on (the app-wide instance unless a test injects its own).
    pub finalizer: Arc<RecordingFinalizer>,
    /// Test seam: when set, the view model records through it instead of
    /// building the real controller (whose start path talks to the native
    /// engine and the OS permission dialog). A fake bypasses the finalizer
    /// registry, so a test drives the quit path with
    /// [`TechniqueDetailViewModel::quit_finalize`].
    pub recorder: Option<Arc<dyn MicRecorder>>,
    /// Builds the page's engine on its first take (test seam).
    pub create_engine: CreateEngine,
}

impl std::fmt::Debug for TechniqueDetailDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TechniqueDetailDeps")
            .field("technique_id", &self.technique_id)
            .finish_non_exhaustive()
    }
}

impl TechniqueDetailDeps {
    /// The dependencies with Dart's defaults for the rest: the engine's
    /// analysis, no settings, the app-wide finalizer, the real recorder over
    /// a page engine of its own.
    pub fn new(
        technique_id: impl Into<String>,
        repository: Arc<dyn TechniqueRepository>,
        media: Arc<dyn TechniqueMediaFiles>,
        exporter: Arc<AudioExporter>,
    ) -> Self {
        TechniqueDetailDeps {
            technique_id: technique_id.into(),
            repository,
            media,
            exporter,
            analysis: Arc::new(MediaAnalysisService::new()),
            app_settings: None,
            finalizer: RecordingFinalizer::instance(),
            recorder: None,
            create_engine: Arc::new(|| Arc::new(GraphAudioPlaybackService::new())),
        }
    }

    /// The page's dependencies from the app's services; exports write where
    /// [pick_save_location] (the app's save panel) answers.
    pub fn from_services(
        technique_id: impl Into<String>,
        services: &Services,
        pick_save_location: impl Fn(&str) -> Result<Option<String>, PickError> + Send + Sync + 'static,
    ) -> Self {
        let finalizer = RecordingFinalizer::instance();
        let exporter = AudioExporter::new(services.audio_files.clone(), pick_save_location)
            .with_finalizer(Arc::clone(&finalizer));
        TechniqueDetailDeps {
            app_settings: Some(services.app_settings.clone()),
            finalizer,
            ..TechniqueDetailDeps::new(
                technique_id,
                services.technique_repo.clone(),
                services.technique_media.clone(),
                Arc::new(exporter),
            )
        }
    }
}

/// A flow's continuation once a job lands.
type Cont = Box<dyn FnOnce(&mut TechniqueDetailViewModel, Value, &mut Next) + Send>;

/// What a sub-flow hands its caller back.
type Then<T> = Box<dyn FnOnce(&mut TechniqueDetailViewModel, T, &mut Next) + Send>;

/// A recording start listed with the quit registry until it settles, which
/// is what the update waits on (`self_update::work_in_flight`); taken off
/// the list when dropped, however the start ends. A quit meanwhile cancels
/// the start, as Cancel does, and waits for nothing: a take not armed yet
/// has nothing to save, and a start at the permission prompt would hold the
/// quit for its whole budget. So it begins no work a quit must budget for
/// (`starts_work` false), where the song page's start drops an empty take.
struct StartListed {
    finalizer: Arc<RecordingFinalizer>,
    cancel_for_quit: Finalizer,
}

impl StartListed {
    fn new(finalizer: &Arc<RecordingFinalizer>, recorder: &Arc<dyn MicRecorder>) -> Self {
        let cancel_for_quit = {
            let recorder = Arc::clone(recorder);
            Finalizer::new(move || recorder.invalidate_pending_start())
        };
        finalizer.register(&cancel_for_quit, false);
        StartListed {
            finalizer: Arc::clone(finalizer),
            cancel_for_quit,
        }
    }
}

impl Drop for StartListed {
    fn drop(&mut self) {
        self.finalizer.unregister(&self.cancel_for_quit);
    }
}

/// Why a recording start threw rather than answered.
struct StartThrew {
    /// Dart's `e is FileSystemException`.
    file_system: bool,
    /// Dart's `e.runtimeType`, for the log.
    type_name: &'static str,
}

/// Why an import was refused.
enum ImportFailure {
    /// A refusal in words the user can act on (Dart's `FormatException`).
    Format(String),
    /// A copy that failed.
    Failed,
}

/// An `addMedia` call working through its files.
struct ImportRun {
    queue: VecDeque<String>,
    generation: u64,
    now: DateTime,
    added: Vec<TechniqueMedia>,
    skipped: usize,
    reason: Option<String>,
}

/// An analysis's outcome on the worker: the analysis, or why it threw
/// (`None` when it panicked or failed with nothing to say).
type Analyzed = Result<Option<TechniqueAnalysis>, Option<MediaError>>;

/// ViewModel for a single technique's detail page (see the module docs).
pub struct TechniqueDetailViewModel {
    technique_id: String,
    repository: Arc<dyn TechniqueRepository>,
    media_service: Arc<dyn TechniqueMediaFiles>,
    analysis: Arc<dyn MediaAnalyses>,
    exporter: Arc<AudioExporter>,
    /// The quit registry an armed take and an in-flight [`save`] hold the
    /// app's exit on.
    ///
    /// [`save`]: TechniqueDetailViewModel::save
    finalizer: Arc<RecordingFinalizer>,

    /// The shared mic-recording core (permission → temp WAV → engine →
    /// capture/level pumps → quit finalizer). The engine stays owned by this
    /// view model: created lazily on the first take, disposed in
    /// [`dispose`](TechniqueDetailViewModel::dispose); the controller
    /// borrows it per take.
    recorder: Arc<dyn MicRecorder>,
    engines: Arc<EngineSlot>,
    monitor: Arc<MonitorService>,
    quit: Arc<QuitRequests>,
    probe: Arc<Probe>,

    /// Loads the technique (runs once on creation).
    load: Command0<Loaded<Technique>>,
    technique: Option<Technique>,

    /// Per-row content hashes of the persisted technique as THIS view model
    /// last loaded or saved it — the BASE side of the repository's three-way
    /// save (see [`Loaded`]). Captured by the load, handed to every save, and
    /// replaced by the fresh map each successful save returns, so a stale
    /// in-memory technique can never revert or tombstone rows that changed
    /// underneath it (e.g. applied by sync).
    base_hashes: BaseHashes,

    // --- Sync freshness -------------------------------------------------------
    /// True while [`save`](TechniqueDetailViewModel::save) is writing (the
    /// commit plus its file cleanup); a remote-applied reload must not
    /// interleave with it (the save returns the fresh aggregate + base
    /// itself).
    saving: bool,

    /// Bumped when a save or a newer remote reload starts so an older
    /// in-flight reload's result is discarded (stale-read-completes-last
    /// race).
    reload_epoch: u64,

    /// True for the lifetime of the editor's form — from `begin_edit` until
    /// the controllers are torn down. The reload must not advance past the
    /// snapshot those controllers were seeded from; see
    /// [`on_remote_applied`](TechniqueDetailViewModel::on_remote_applied).
    editing: bool,

    /// A remote change arrived while the reload was skipped, so it has to be
    /// replayed once the edit session ends or the read view keeps showing
    /// pre-remote content with nothing pending to correct it.
    missed_remote: bool,

    /// True while example files are being copied into the sandbox.
    importing_media: bool,

    // --- Staged media draft (committed on save) ------------------------------
    /// Example clips added (recorded or imported) during the current edit but
    /// not yet persisted. Their files already live in the sandbox; they are
    /// committed on save and their files are deleted if the edit is
    /// cancelled.
    draft_adds: Vec<TechniqueMedia>,

    /// Ids of already-persisted examples staged for removal; their files are
    /// deleted only once the technique-without-them persists.
    draft_removes: HashSet<String>,

    /// Staged caption changes (media id -> new label), applied on save.
    draft_labels: HashMap<String, String>,

    /// Bumped whenever the media draft is reset/discarded, so an in-flight
    /// import (add_media / stop_example_recording) that resolves after the
    /// draft was abandoned deletes its just-copied file instead of appending
    /// an orphan clip to the cleared draft (and mutating selection behind the
    /// read view).
    draft_generation: u64,

    // --- Reference analysis (ground truth) -----------------------------------
    /// Id of the example whose analysis is shown. `None` falls back to the
    /// first audio example.
    ///
    /// An id, not a path: two examples with identical bytes resolve to the
    /// SAME pooled file once saved, and a path then named both of them — both
    /// cards opened with the analysis and a playhead-linked player, and the
    /// second could never be selected on its own. The analysis caches stay
    /// keyed by path: an analysis is a property of the file's bytes, shared
    /// correctly.
    selected_example_id: Option<String>,

    /// Whether the selected example's inline analysis is collapsed. Only the
    /// selected example can be expanded (accordion), so this one flag
    /// suffices.
    analysis_collapsed: bool,

    /// Cached analysis per example path, so switching back to an example is
    /// instant and a changed reference recomputes while an unchanged one
    /// doesn't.
    ///
    /// Byte-budgeted against the app-wide analysis budget, and disposed with
    /// this view model: each entry carries a whole spectrogram (a 10-minute
    /// reference is ~44 MB of it), so an unbounded map pinned every example
    /// the user opened for as long as the screen lived, on top of whatever
    /// the practice screen was holding.
    ref_cache: LruBytesCache<TechniqueAnalysis>,

    /// Paths whose analysis was attempted but produced nothing (undecodable
    /// file).
    ref_failed: HashSet<String>,

    /// Per-path cause for a [`ref_failed`](Self::ref_failed) entry when
    /// there is one to say (the decode-length cap, a format the engine cannot
    /// decode, a read error): the failure hint shows it instead of the
    /// generic "couldn't analyze" copy.
    ref_failed_cause: HashMap<String, FailureCause>,

    /// Paths whose analysis is currently in flight.
    loading_paths: HashSet<String>,

    /// In-flight analyses whose clip a commit moved to a new (pool) path: the
    /// old path -> the path the result lands under (see
    /// `remap_media_after_commit`).
    in_flight_moves: HashMap<String, String>,

    // --- Analysis subject (reference vs practice) ----------------------------
    subject: TechniqueAnalysisSubject,

    // --- Mic recording (shared controller) -----------------------------------
    recording_example: bool,

    /// One-shot user notice from an async recording event (e.g. the input
    /// device was disconnected mid-record). The view peeks it, surfaces it as
    /// a toast, then clears it.
    recording_notice: Option<String>,

    // --- Ephemeral practice recording ----------------------------------------
    practice_temp_path: Option<String>,
    practice: Option<TechniqueAnalysis>,

    /// Bumped whenever the reference changes (`reset_to_reference_subject`)
    /// or the practice take is cleared, so a take whose reference changed
    /// while it was recording or being analyzed knows it is stale — recorded
    /// against a now-replaced reference — and is discarded instead of pairing
    /// mismatched clips.
    practice_generation: u64,

    /// The [`practice_generation`](Self::practice_generation) the take now
    /// recording was ARMED under: the reference a take is recorded against
    /// is the one selected when capture starts, so the snapshot is taken
    /// then, not at Stop (see `stop_practice`).
    practice_take_generation: u64,

    practice_loading: bool,
    practice_failed: bool,

    /// A user-presentable cause for the failed practice analysis (today: the
    /// decode-length cap), or `None` when only the generic copy applies.
    /// Mirrors `reference_analysis_failure_message` for the practice subject.
    practice_failed_message: Option<String>,

    /// Whether the start pending is a practice take's.
    practice_starting: bool,

    // --- Record a persisted example clip --------------------------------------
    example_temp_path: Option<String>,
    saving_example: bool,

    // --- AnalysisSource --------------------------------------------------------
    /// The shared playhead (subject-local).
    position: Duration,

    /// Which analysis dimension (Spectrogram / Power / …) is shown. Held here
    /// so it persists when the panel moves between cards or in/out of the
    /// live slot.
    analysis_dim: usize,

    /// Set in [`dispose`](TechniqueDetailViewModel::dispose); guards async
    /// continuations (reference analysis, recording finalize, draft cleanup)
    /// that resume after the view model is gone from notifying.
    disposed: bool,

    revision: u64,
    next_job: u64,
    conts: HashMap<u64, Cont>,
}

impl std::fmt::Debug for TechniqueDetailViewModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TechniqueDetailViewModel")
            .field("technique_id", &self.technique_id)
            .field("loaded", &self.technique.is_some())
            .field("saving", &self.saving)
            .field("editing", &self.editing)
            .field("revision", &self.revision)
            .field("jobs_waiting", &self.conts.len())
            .finish_non_exhaustive()
    }
}

/// Resident payload of one reference analysis: the two typed-data members
/// dominate; the LTAS, profile and register map are bounded summaries.
fn analysis_bytes(a: &TechniqueAnalysis) -> usize {
    a.spectrogram.db.len() * 4 + a.metrics.frames * VoiceMetrics::BYTES_PER_FRAME
}

/// Dart's `e.runtimeType` for a media file failure, for the log.
fn file_error_type(e: &AudioFileError) -> &'static str {
    match e {
        AudioFileError::InvalidId(_) => "ArgumentError",
        AudioFileError::Format(_) => "FormatException",
        AudioFileError::Io(_) => "FileSystemException",
    }
}

/// Whether an analysis refusal carries a cause the user is told (Dart's
/// `e is MediaTooLongException || e is MediaUnsupportedException`).
fn presentable(e: &MediaError) -> bool {
    matches!(e, MediaError::TooLong { .. }) || e.is_unsupported()
}

/// Runs an analysis on the worker, a panic taken as a throw with nothing to
/// say (Dart's isolate failure).
fn analyze_caught(run: impl FnOnce() -> Result<Option<TechniqueAnalysis>, MediaError>) -> Analyzed {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Ok(analysis)) => Ok(analysis),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

/// Dart's `File(path).exists()`: a file is there (a folder is not one).
fn file_exists(path: &str) -> bool {
    Path::new(path).is_file()
}

/// "Recording N" with a bounded N, so an absurd caption typed by hand can't
/// overflow the numbering (the same guard "Take N" numbering carries):
/// `^Recording (\d{1,9})$`.
fn recording_number(label: &str) -> Option<u64> {
    let digits = label.strip_prefix("Recording ")?;
    if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The message the screen toasts for a failed recording start.
fn start_failure_message(failure: MicRecordingFailure) -> String {
    match failure {
        MicRecordingFailure::PermissionDenied => mic_permission::DENIED_MESSAGE.to_string(),
        MicRecordingFailure::EngineUnavailable => {
            "The audio engine is unavailable. Reconnect an audio device and try again.".into()
        }
        MicRecordingFailure::FileCreateFailed => {
            "Couldn't create the recording file. Check your free disk space.".into()
        }
        MicRecordingFailure::InputNotDelivering => "The microphone did not start. Try again, or \
             pick a different input device in Settings."
            .into(),
    }
}

/// The notice for a take that finalized CLEANLY but whose recording ring
/// overflowed mid-take (writer thread or disk stalled): the WAV is valid yet
/// missing [dropped_frames] frames as silent holes. The clip is kept — most
/// of it is fine — but it must never be presented as gapless. Mirrors the
/// song recording flow's dropped-frames toast.
fn dropped_frames_message(dropped_frames: u64) -> String {
    // The recording ring runs at 48 kHz, so frames convert to milliseconds
    // directly (the same math as the song flow's toast).
    let dropped_ms = dropped_frames * 1000 / 48000;
    format!(
        "Recording dropped about {dropped_ms} ms of audio while the disk was busy. \
         The clip has small gaps."
    )
}

/// A representative time on [a]: the loudest clearly-voiced frame — so the
/// playhead-pinned panels (Power / Vowel / Timbre) open on a real sung
/// moment instead of the (usually silent) clip onset. Middle of the clip if
/// none.
fn representative_time(a: &TechniqueAnalysis) -> Duration {
    let m = &a.metrics;
    let mut best_fr: Option<usize> = None;
    let mut best = -1e30_f64;
    for fr in 0..m.frames {
        let loudness = f64::from(m.loudness[fr]);
        if m.active[fr] == 1 && m.f0[fr].is_finite() && loudness > best {
            best = loudness;
            best_fr = Some(fr);
        }
    }
    let ms = match best_fr {
        None => a.duration_ms / 2,
        Some(fr) => (m.time_of_frame(fr as i64) * 1000.0).round() as i64,
    };
    Duration::from_millis(ms.max(0) as u64)
}

impl TechniqueDetailViewModel {
    /// The view model, and the load it starts at once (Dart's
    /// `load = Command0(_load)..execute()`).
    pub fn new(deps: TechniqueDetailDeps) -> (Self, Next) {
        let TechniqueDetailDeps {
            technique_id,
            repository,
            media,
            exporter,
            analysis,
            app_settings,
            finalizer,
            recorder,
            create_engine,
        } = deps;
        let quit = Arc::new(QuitRequests::default());
        let monitor = Arc::new(MonitorService::default());
        let engines = Arc::new(EngineSlot::new(create_engine, app_settings));
        let recorder =
            recorder.unwrap_or_else(|| Self::build_recorder(&engines, &monitor, &quit, &finalizer));
        let load_repo = Arc::clone(&repository);
        let load_id = technique_id.clone();
        let mut vm = TechniqueDetailViewModel {
            technique_id,
            repository,
            media_service: media,
            analysis,
            exporter,
            finalizer,
            recorder,
            engines,
            monitor,
            quit,
            probe: Arc::new(Probe::default()),
            // Load through the base-hash path: the hashes captured here are
            // what every later save three-way merges against (see
            // `base_hashes`).
            load: Command0::new(move || {
                load_repo
                    .load_technique(&load_id)
                    .map_err(|e| e.to_string())
            }),
            technique: None,
            base_hashes: BaseHashes::default(),
            saving: false,
            reload_epoch: 0,
            editing: false,
            missed_remote: false,
            importing_media: false,
            draft_adds: Vec::new(),
            draft_removes: HashSet::new(),
            draft_labels: HashMap::new(),
            draft_generation: 0,
            selected_example_id: None,
            analysis_collapsed: false,
            ref_cache: LruBytesCache::shared(analysis_cache_budget(), analysis_bytes),
            ref_failed: HashSet::new(),
            ref_failed_cause: HashMap::new(),
            loading_paths: HashSet::new(),
            in_flight_moves: HashMap::new(),
            subject: TechniqueAnalysisSubject::Reference,
            recording_example: false,
            recording_notice: None,
            practice_temp_path: None,
            practice: None,
            practice_generation: 0,
            practice_take_generation: 0,
            practice_loading: false,
            practice_failed: false,
            practice_failed_message: None,
            practice_starting: false,
            example_temp_path: None,
            saving_example: false,
            position: Duration::ZERO,
            analysis_dim: 0,
            disposed: false,
            revision: 0,
            next_job: 0,
            conts: HashMap::new(),
        };
        let mut next = Next::default();
        vm.execute_load(&mut next);
        (vm, next)
    }

    /// The real recording controller: it borrows the page's engine per take,
    /// feeds the page's monitor, watches the input device, and registers the
    /// page's quit finalize for the armed window.
    fn build_recorder(
        engines: &Arc<EngineSlot>,
        monitor: &Arc<MonitorService>,
        quit: &Arc<QuitRequests>,
        finalizer: &Arc<RecordingFinalizer>,
    ) -> Arc<dyn MicRecorder> {
        let controller_cell: Arc<OnceLock<Weak<MicRecordingController>>> = Arc::default();
        let on_quit: QuitFinalize = {
            let quit = Arc::clone(quit);
            let controller_cell = Arc::clone(&controller_cell);
            Arc::new(move || {
                // Dart's `quitFinalize` does nothing without a take
                // recording: answered at once, not queued for a tick.
                let recording = controller_cell
                    .get()
                    .and_then(Weak::upgrade)
                    .is_some_and(|c| c.is_recording());
                if recording {
                    quit.request_and_wait();
                }
            })
        };
        let acquire = {
            let engines = Arc::clone(engines);
            move || engines.acquire()
        };
        let controller = Arc::new(MicRecordingController::new(
            acquire,
            MicRecordingOptions {
                monitor: Some(Arc::clone(monitor) as Arc<dyn LiveMonitor>),
                on_quit_finalize: Some(on_quit),
                // This view model is the only device-event consumer on the
                // technique screen, so the controller polls while recording.
                watch_input_device: true,
                finalizer: Some(Arc::clone(finalizer)),
                ..MicRecordingOptions::default()
            },
        ));
        let _ = controller_cell.set(Arc::downgrade(&controller));
        controller
    }

    // --- The async machinery ----------------------------------------------------

    /// Queues [work] as a job of [kind]; [then] runs when it lands, with its
    /// value (or why there is none). [keep]'s guards are held by both until
    /// they are done.
    fn spawn<T, W, C>(&mut self, next: &mut Next, kind: JobKind, keep: &Keep, work: W, then: C)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        C: FnOnce(&mut Self, Result<T, String>, &mut Next) + Send + 'static,
    {
        self.next_job += 1;
        let id = self.next_job;
        let kept = keep.clone();
        self.conts.insert(
            id,
            Box::new(move |vm, value, next| {
                let _kept = kept;
                let value = value.and_then(|v| {
                    v.downcast::<T>()
                        .map(|v| *v)
                        .map_err(|_| "a job reported the wrong value".to_string())
                });
                then(vm, value, next);
            }),
        );
        self.push_job(next, id, kind, keep, work);
    }

    /// Queues [work] as a job nothing waits for (Dart's `unawaited`), its
    /// value dropped as it lands.
    fn spawn_detached<T, W>(&mut self, next: &mut Next, kind: JobKind, keep: &Keep, work: W)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
    {
        self.next_job += 1;
        let id = self.next_job;
        self.push_job(next, id, kind, keep, work);
    }

    fn push_job<T, W>(&mut self, next: &mut Next, id: u64, kind: JobKind, keep: &Keep, work: W)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
    {
        let held = keep.clone();
        next.effects.push(Effect::Run(Job {
            id,
            kind,
            work: Box::new(move || {
                let value = work();
                drop(held);
                Box::new(value) as Box<dyn std::any::Any + Send>
            }),
        }));
    }

    /// Lands what a job reported: the rest of its Dart method after the
    /// `await`. A report nothing waits for (a detached job, or one landed
    /// already) changes nothing.
    pub fn land(&mut self, done: Done) -> Next {
        let mut next = Next::default();
        if let Some(cont) = self.conts.remove(&done.id) {
            cont(self, done.take(), &mut next);
        }
        next
    }

    /// Dart's `notifyListeners`: a no-op once disposed.
    fn notify(&mut self) {
        if self.disposed {
            return;
        }
        self.revision += 1;
        self.publish_reference();
    }

    /// What the reference analyses' `wanted` reads, as of now.
    fn publish_reference(&self) {
        self.probe.set_reference(
            self.reference_media().map(|m| m.path),
            &self.in_flight_moves,
        );
    }

    /// Bumped on every Dart `notifyListeners` (never after dispose).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether a job's landing is still awaited.
    pub fn has_pending_jobs(&self) -> bool {
        !self.conts.is_empty()
    }

    // --- Quit --------------------------------------------------------------------

    /// Dart's `quitFinalize`: what a quit runs while a take is in flight.
    /// The recording controller calls it on a quit (see the module docs); a
    /// test, whose fake recorder bypasses the finalizer registry, calls it
    /// directly.
    pub fn quit_finalize(&mut self) -> Next {
        let mut next = Next::default();
        self.quit_finalize_with(Keep::default(), &mut next);
        next
    }

    fn quit_finalize_with(&mut self, keep: Keep, next: &mut Next) {
        if self.is_recording_example() {
            // The same backstop as `stop_example_recording`: an airborne save
            // owns the take.
            if self.saving {
                return;
            }
            // Stopping only STAGES the clip into the in-memory media draft,
            // which dies with the exiting process — and the imported file,
            // referenced by no row, would be swept as an orphan at the next
            // boot. Commit the captured clip itself so the recording survives
            // the quit (mirrors the song screen's finalizer, which persists
            // its take). Held for the quit from the stop on, like `save`'s:
            // the stop unregisters this finalizer, so a close-button run cut
            // off mid-stage or mid-commit left the exit request's run nothing
            // to wait on, and the clip was lost.
            // Held as `save` holds it, from the stop to the commit's end: the
            // window stays usable while the quit waits, and a Save meanwhile
            // ran a second commit against the same base, while Cancel →
            // Discard deleted the clip being committed.
            self.saving = true;
            self.notify();
            let stopped = self.recorder.stop();
            let keep = keep.with(self.recorder.begin_stop_finalize());
            let commit_keep = keep.clone();
            self.stage_stopped_example(
                stopped,
                keep,
                next,
                Box::new(move |vm, staged, next| {
                    let finish: Then<()> = Box::new(|vm, (), _| {
                        vm.saving = false;
                        vm.notify();
                    });
                    match staged {
                        Some(staged) => {
                            vm.persist_example_at_quit(staged, commit_keep, next, finish)
                        }
                        None => finish(vm, (), next),
                    }
                }),
            );
        } else if self.is_recording_practice() {
            // A practice take is ephemeral: the dying process would never
            // show the analysis result, so skip the full DSP pass and only
            // clean up — quit must not block on a discarded compute.
            self.stop_practice_with(true, keep, next);
        }
    }

    // --- Recorder ticks -------------------------------------------------------------

    /// When the screen should call [`tick`](Self::tick) next: while a take is
    /// armed (the recorder's pumps), and at once while a quit waits on the
    /// view model.
    pub fn next_tick_due(&self) -> Option<Instant> {
        if self.quit.pending() {
            return Some(Instant::now());
        }
        self.recorder.next_tick_due()
    }

    /// The recorder's timers (see the module docs): answers a quit waiting
    /// on the view model, then fires the recorder's due timers, handling the
    /// device events they report as Dart's callbacks did.
    pub fn tick(&mut self) -> Next {
        let mut next = Next::default();
        for answer in self.quit.drain() {
            self.quit_finalize_with(Keep::default().with(answer), &mut next);
        }
        let recorder = Arc::clone(&self.recorder);
        recorder.tick(&mut |event| self.on_mic_event(event, &mut next));
        next
    }

    fn on_mic_event(&mut self, event: MicEvent, next: &mut Next) {
        match event {
            MicEvent::InputDeviceLost => self.on_input_device_lost(next),
            MicEvent::RecordLimitReached => self.on_record_limit_reached(next),
            MicEvent::RecordWriteFailed => self.on_record_write_failed(next),
            MicEvent::DeviceNotice(message) => self.on_device_notice(message),
            MicEvent::LevelTick(_) => {}
        }
    }

    /// The mic vanished mid-record (unplugged, Bluetooth profile flip): stop
    /// the recording gracefully — finalizing KEEPS what was captured —
    /// instead of letting capture die silently. This view model is the only
    /// device-event consumer on the technique screen, so the controller
    /// polls while recording.
    fn on_input_device_lost(&mut self, next: &mut Next) {
        self.recording_notice =
            Some("Recording stopped: the input device was disconnected.".to_string());
        if self.recording_example {
            self.stop_example_recording_into(next, false);
        } else {
            self.stop_practice_with(false, Keep::default(), next);
        }
    }

    /// The take reached the longest recording a WAV header can describe:
    /// stop it so everything captured up to the limit is saved, instead of
    /// leaving a take that looks like it is still recording while nothing
    /// more is stored.
    fn on_record_limit_reached(&mut self, next: &mut Next) {
        // A practice take is never saved (only analyzed), so it is told no
        // such thing.
        self.recording_notice = Some(if self.recording_example {
            "Recording stopped at the six-hour maximum length. \
             Everything up to that point was saved."
                .to_string()
        } else {
            "Recording stopped at the six-hour maximum length.".to_string()
        });
        if self.recording_example {
            self.stop_example_recording_into(next, false);
        } else {
            self.stop_practice_with(false, Keep::default(), next);
        }
    }

    /// A write to the take's WAV failed mid-take (a full disk, a revoked
    /// permission): stop it now, as at the limit, instead of recording on
    /// into nothing until Stop. The stop's dirty path then keeps what landed
    /// if it can be read, and says so.
    fn on_record_write_failed(&mut self, next: &mut Next) {
        // A practice take is never saved (only analyzed), so it is told no
        // such thing.
        self.recording_notice = Some(if self.recording_example {
            "Recording stopped: the example could not be written to disk, which may be \
             full. What was recorded up to then is being saved."
                .to_string()
        } else {
            "Recording stopped: the take could not be written to disk, which may be full."
                .to_string()
        });
        if self.recording_example {
            self.stop_example_recording_into(next, false);
        } else {
            self.stop_practice_with(false, Keep::default(), next);
        }
    }

    /// A non-fatal input degradation (echo cancellation unavailable, or the
    /// selected mic fell back to the system default): recording continues,
    /// but the user must know — both were silently dropped before.
    fn on_device_notice(&mut self, message: &str) {
        self.recording_notice = Some(message.to_string());
        self.notify();
    }

    // --- Load and sync freshness ------------------------------------------------------

    /// The technique id this page shows.
    pub fn technique_id(&self) -> &str {
        &self.technique_id
    }

    /// The load command's state.
    pub fn load(&self) -> &Command<Loaded<Technique>> {
        &self.load
    }

    /// The technique, as last loaded or saved (and, while a save commits,
    /// as it is being saved).
    pub fn technique(&self) -> Option<&Technique> {
        self.technique.as_ref()
    }

    /// `load.execute()`: a read of the technique, unless one runs already.
    fn execute_load(&mut self, next: &mut Next) {
        let Some(run) = self.load.execute() else {
            return;
        };
        self.spawn(
            next,
            JobKind::Load,
            &Keep::default(),
            move || run.run(),
            |vm, outcome, next| {
                let outcome = outcome.unwrap_or_else(|panic| Err(ActionError::Panicked(panic)));
                match &outcome {
                    Ok(loaded) => {
                        vm.technique = Some(loaded.aggregate.clone());
                        vm.base_hashes = loaded.base_hashes.clone();
                        vm.notify();
                    }
                    // The failure must reach the screen too: it watches only
                    // this view model, and the command notifies just its own
                    // listeners, so without this the spinner (keyed on
                    // load.running) is never rebuilt away.
                    Err(ActionError::Failed(_)) => vm.notify(),
                    // Dart's action threw before its own notify.
                    Err(ActionError::Panicked(_)) => {}
                }
                vm.load.finish(outcome);
                // A remote change that landed while a load read is owed once
                // it is done (the command notifies as it finishes, its
                // running flag already clear).
                if !vm.load.running() {
                    vm.replay_missed_remote(next);
                }
            },
        );
    }

    /// Whether an edit session may open now. Not while a practice take is
    /// recording or its start is still pending: the edit view has no
    /// practice recorder (so no Stop for the take), `save` refuses while the
    /// mic is hot (every Save would fail), and Cancel's
    /// `discard_media_draft` would abort the take with no prompt. The user
    /// stops the take first. The screen disables Edit on this and also
    /// checks it at tap time, since a tap can land between a start going
    /// pending and the redraw that announces it.
    pub fn can_edit(&self) -> bool {
        !self.is_recording_practice() && !self.recorder.is_starting()
    }

    /// Open an edit session. Pairs with [`end_edit`](Self::end_edit); the
    /// screen calls this when it seeds the form's controllers, so the flag's
    /// lifetime is exactly theirs.
    pub fn begin_edit(&mut self) {
        self.editing = true;
        // A change missed before this session stays owed (replayed once it
        // and any hold end): cleared here, one missed while an abandoned
        // import still ran was dropped by an Edit reopened before the copy
        // ended.
        // The editor's example card has no Reference/Practice switch and only
        // the reference drives (and is driven by) the inline player, so a
        // practice subject left over from the read view would show the
        // take's analysis under the example with no way back. The take itself
        // is kept for the read view's switch; a take still being analyzed
        // lands without touching the subject (see `stop_practice`).
        self.show_analysis_subject(TechniqueAnalysisSubject::Reference);
    }

    /// Close the edit session (cancel, successful save, or the screen going
    /// away). Idempotent. Replays a reload the session had to skip.
    pub fn end_edit(&mut self) -> Next {
        let mut next = Next::default();
        if !self.editing {
            return next;
        }
        self.editing = false;
        self.replay_missed_remote(&mut next);
        next
    }

    /// Replays a reload skipped while a hold was on (see
    /// `holds_editor_state`), once nothing pins the read view any more.
    /// Outside an edit session the hold is a practice take: stopping it used
    /// to replay nothing, so a remote change landing mid-take left the read
    /// view stale with nothing pending to correct it — and a later Edit
    /// seeded the form from that stale text.
    /// An import or an example save that outlived its edit session ended (or
    /// failed): tell the screen, and replay a remote change skipped while it
    /// ran. end_edit's replay found it still busy and skipped again, and
    /// nothing else replayed it: the read view kept the old text, and the
    /// next edit, seeded from it against the stale base, wrote it over the
    /// remote change.
    fn busy_ended(&mut self, next: &mut Next) {
        self.notify();
        self.replay_missed_remote(next);
    }

    fn replay_missed_remote(&mut self, next: &mut Next) {
        if self.editing || !self.missed_remote {
            return;
        }
        self.missed_remote = false;
        self.reload_from_remote(next);
    }

    /// Freshness-only reload when sync applied remote rows of THIS
    /// technique. Skipped whenever an edit session holds state this reload
    /// would talk over (a media draft, an import, a recording, a save in
    /// flight) — skipping is always SAFE: the next save three-way merges
    /// against the base hashes, so a missed reload can never revert the
    /// synced-in change.
    ///
    /// An open EDIT SESSION skips it too, and that one is not merely safe
    /// but REQUIRED. The screen seeds its text controllers once, at
    /// `begin_edit`, and never re-seeds them; a save then submits all eight
    /// fields from those controllers. Advancing the base hashes to the remote
    /// row while the form still holds pre-remote text makes
    /// `desired != base` even though the user typed nothing, which is exactly
    /// the condition the repository's keep-theirs branch requires to be
    /// FALSE — so a later save (a media-only one, most plausibly) rewrites
    /// the stale text over the remote change under a fresh stamp, and wins
    /// downstream too. Re-seeding the controllers instead would fight the
    /// caret and discard typed text.
    pub fn on_remote_applied(&mut self, event: &RemoteApplied) -> Next {
        let mut next = Next::default();
        if !event.technique_ids.contains(self.technique_id.as_str()) {
            return next;
        }
        self.reload_from_remote(&mut next);
        next
    }

    /// Whether a reload would talk over state the user is holding. Records
    /// the skip in `missed_remote` so `end_edit` can replay it.
    fn holds_editor_state(&mut self) -> bool {
        let holds = self.saving
            || self.editing
            || self.importing_media
            || self.saving_example()
            || self.has_media_draft()
            || self.recorder.is_recording();
        if holds {
            self.missed_remote = true;
        }
        holds
    }

    /// The reload body: shared by the sync event and by `end_edit`'s replay.
    fn reload_from_remote(&mut self, next: &mut Next) {
        if self.disposed {
            return;
        }
        if self.load.running() {
            // Owed after the load (see `execute_load`): dropped, a rename
            // landing after the first read stayed invisible, Edit and Cancel
            // included.
            self.missed_remote = true;
            return;
        }
        if self.technique.is_none() {
            // The first load failed (a passing read error): the change is a
            // reason to read again, not one owed to a load that has already
            // ended.
            self.execute_load(next);
            return;
        }
        if self.holds_editor_state() {
            return;
        }
        self.reload_epoch += 1;
        let epoch = self.reload_epoch;
        let repository = Arc::clone(&self.repository);
        let id = self.technique_id.clone();
        self.spawn(
            next,
            JobKind::Reload,
            &Keep::default(),
            move || repository.load_technique(&id).map_err(|e| e.to_string()),
            move |vm, result, _next| {
                if vm.disposed || epoch != vm.reload_epoch {
                    return;
                }
                if vm.holds_editor_state() {
                    return;
                }
                let Ok(Ok(loaded)) = result else {
                    // Still owed (e.g. deleted remotely, or a read that
                    // failed like the save's own re-read did): the next
                    // replay point asks again, where clearing it here left
                    // the read view on what a save had sent.
                    vm.missed_remote = true;
                    return;
                };
                let reference_before = vm.reference_media_id();
                // An example can come back under another path (the pool's,
                // for one this editor still knew by its staging path): the
                // same bytes by id, so its analysis follows it, as after a
                // commit.
                let old_path_by_id = vm.persisted_paths_by_id();
                vm.technique = Some(loaded.aggregate);
                vm.base_hashes = loaded.base_hashes;
                vm.remap_media_after_commit(&old_path_by_id, reference_before.as_deref());
                vm.notify();
            },
        );
    }

    /// The persisted examples' paths by id.
    fn persisted_paths_by_id(&self) -> HashMap<String, String> {
        self.technique
            .iter()
            .flat_map(|t| &t.media)
            .map(|m| (m.id.clone(), m.path.clone()))
            .collect()
    }

    /// True while [`save`](Self::save) runs. The screen disables the WHOLE
    /// edit surface while this holds: Save (a second commit would race the
    /// first on the same base hashes), Cancel (a discard would delete staged
    /// media files the airborne commit is persisting rows for), the text
    /// fields and the media add/remove/rename/record controls (the commit
    /// snapshotted the form and the draft when it started, so edits made
    /// during the await would be silently thrown away when the save lands
    /// and edit mode closes). The media mutation methods also return early
    /// on this flag — the UI disabling is the affordance, the view model
    /// guard is the invariant.
    pub fn saving(&self) -> bool {
        self.saving
    }

    /// True while example files are being copied into the sandbox.
    pub fn importing_media(&self) -> bool {
        self.importing_media
    }

    // --- Staged media draft (committed on save) ---------------------------------------

    /// True when the media draft differs from what's persisted, so Cancel
    /// knows to warn about unsaved example changes.
    pub fn has_media_draft(&self) -> bool {
        !self.draft_adds.is_empty()
            || !self.draft_removes.is_empty()
            || !self.draft_labels.is_empty()
    }

    fn labelled(&self, m: &TechniqueMedia) -> TechniqueMedia {
        match self.draft_labels.get(&m.id) {
            None => m.clone(),
            Some(label) => TechniqueMedia {
                label: label.clone(),
                ..m.clone()
            },
        }
    }

    /// The example list as the user currently sees it: persisted media
    /// (minus staged removals, with staged captions) followed by staged
    /// additions. In the read view the draft is empty, so this is just the
    /// technique's media.
    ///
    /// A staged addition is listed from the draft for as long as the draft
    /// holds it, even when the technique already carries the same clip: the
    /// commit's optimistic aggregate includes every staged addition (and so
    /// does the just-committed one, until `save` clears the draft), and
    /// listing both showed each staged clip twice — with a second audio
    /// player — for the whole commit. Keeping the DRAFT copy (not the
    /// aggregate's) leaves the list exactly as it was before the save: same
    /// order, and the sandbox path the selection still points at until
    /// `save` remaps it. A failed save reverts the aggregate, so nothing else
    /// is needed to restore the list.
    pub fn effective_media(&self) -> Vec<TechniqueMedia> {
        let base = self.technique.as_ref().map_or(&[][..], |t| &t.media[..]);
        let staged: HashSet<&str> = self.draft_adds.iter().map(|m| m.id.as_str()).collect();
        let mut out = Vec::new();
        for m in base {
            if self.draft_removes.contains(&m.id) || staged.contains(m.id.as_str()) {
                continue;
            }
            out.push(self.labelled(m));
        }
        for m in &self.draft_adds {
            out.push(self.labelled(m));
        }
        out
    }

    /// Resets the draft when an edit session opens (defensive: there
    /// shouldn't be a stale one).
    pub fn begin_media_draft(&mut self) {
        self.draft_adds.clear();
        self.draft_removes.clear();
        self.draft_labels.clear();
    }

    /// Drops the media draft and deletes any files copied for staged
    /// additions (they were never committed). Called when an edit is
    /// cancelled. Answers ([`Answer::DraftDiscarded`]) whether it ran: a save
    /// in flight holds it off, and the edit must then stay open — closed, the
    /// draft lived on in the read view.
    pub fn discard_media_draft(&mut self) -> Next {
        let mut next = Next::default();
        // Never during an in-flight save: the commit is persisting rows for
        // the exact files this would delete (the screen disables Cancel while
        // saving, but this method is callable directly). The save's own
        // success path clears the draft; its failure path leaves the edit
        // session — and this draft — alive for the user to retry or cancel.
        if self.saving {
            next.answers.push(Answer::DraftDiscarded(false));
            return next;
        }
        // Invalidate any in-flight import: a clip that finishes copying after
        // this must be treated as an orphan (deleted), not appended to the
        // now-cleared draft (which would strand its file and show a phantom
        // clip in the read view).
        self.draft_generation += 1;
        // Also invalidate a PENDING recording start (still awaiting
        // permission or the temp dir: is_recording is false, so the teardown
        // below can't see it). Without this, cancelling during that window
        // would arm a hot mic in the read view with no stop affordance.
        self.recorder.invalidate_pending_start();
        // Releasing edit must release the mic: stop and discard any in-flight
        // example recording (otherwise the mic + capture/level timers run on
        // until the route is popped).
        if self.recorder.is_recording() {
            self.abort_recorder(
                &mut next,
                Box::new(|vm, (), next| {
                    vm.recording_example = false;
                    vm.example_temp_path = None;
                    vm.discard_draft_files(next);
                }),
            );
        } else {
            self.discard_draft_files(&mut next);
        }
        next
    }

    fn discard_draft_files(&mut self, next: &mut Next) {
        let reference_before = self.reference_media_id();
        let orphans: VecDeque<TechniqueMedia> = std::mem::take(&mut self.draft_adds).into();
        self.draft_removes.clear();
        self.draft_labels.clear();
        self.discard_orphans(orphans, reference_before, next);
    }

    fn discard_orphans(
        &mut self,
        mut orphans: VecDeque<TechniqueMedia>,
        reference_before: Option<String>,
        next: &mut Next,
    ) {
        let Some(m) = orphans.pop_front() else {
            self.fix_selection_after_media_change(reference_before.as_deref());
            self.notify();
            next.answers.push(Answer::DraftDiscarded(true));
            return;
        };
        self.forget_path(&m.path);
        // Logged, not thrown: one failed delete stopped the loop with the
        // draft already cleared — no notify, the rest of the files left, and
        // Cancel stuck in edit mode behind an unhandled error.
        let work = self.delete_media_logged(m.path);
        self.spawn(
            next,
            JobKind::DeleteMedia,
            &Keep::default(),
            work,
            move |vm, _, next| vm.discard_orphans(orphans, reference_before, next),
        );
    }

    /// Forgets everything path-keyed about a clip that is gone.
    fn forget_path(&mut self, path: &str) {
        self.ref_cache.remove(path);
        self.ref_failed.remove(path);
        self.ref_failed_cause.remove(path);
        self.loading_paths.remove(path);
    }

    /// Dart's `await _recorder.abort()`: the take stops at once (the
    /// controller's abort stops before it awaits anything), its file — the
    /// take's temp folder, the view model's destination — is discarded on a
    /// worker, and [then] runs after.
    fn abort_recorder(&mut self, next: &mut Next, then: Then<()>) {
        let stopped = self.recorder.stop();
        match stopped {
            None => then(self, (), next),
            Some(stopped) => self.spawn(
                next,
                JobKind::DeleteTake,
                &Keep::default(),
                move || delete_temp_recording_dir(Some(&stopped.path)),
                move |vm, _, next| then(vm, (), next),
            ),
        }
    }

    // --- Reference analysis (ground truth) ------------------------------------------

    /// All analyzable (audio) examples, in display order. Video clips aren't
    /// analyzed.
    pub fn audio_examples(&self) -> Vec<TechniqueMedia> {
        self.effective_media()
            .into_iter()
            .filter(|m| !m.is_video())
            .collect()
    }

    /// The example clip used as the reference "ground truth": the selected
    /// audio example, or the first one. `None` when the technique has no
    /// audio example.
    pub fn reference_media(&self) -> Option<TechniqueMedia> {
        let mut audio = self.audio_examples();
        if audio.is_empty() {
            return None;
        }
        if let Some(i) = audio
            .iter()
            .position(|m| Some(&m.id) == self.selected_example_id.as_ref())
        {
            return Some(audio.swap_remove(i));
        }
        Some(audio.swap_remove(0))
    }

    fn reference_media_id(&self) -> Option<String> {
        self.reference_media().map(|m| m.id)
    }

    fn reference_path(&self) -> Option<String> {
        self.reference_media().map(|m| m.path)
    }

    /// The id of the example whose inline analysis is currently expanded, or
    /// `None` when collapsed.
    pub fn expanded_example_id(&self) -> Option<String> {
        if self.analysis_collapsed {
            None
        } else {
            self.reference_media_id()
        }
    }

    /// Toggles a card's inline analysis (by media id): collapse/expand the
    /// selected one, or select-and-expand a different one. While a practice
    /// take records — or its start is still pending, when it arms into this
    /// same card — its Stop lives in the selected card: collapsing that card
    /// hid it (until re-expanded), so the card stays open.
    pub fn toggle_example_expanded(&mut self, media_id: &str) -> Next {
        let mut next = Next::default();
        if self.reference_media_id().as_deref() == Some(media_id) {
            if (self.is_recording_practice() || self.recorder.is_starting())
                && !self.analysis_collapsed
            {
                return next;
            }
            self.analysis_collapsed = !self.analysis_collapsed;
            self.notify();
        } else {
            self.analysis_collapsed = false;
            next = self.select_example(media_id);
        }
        next
    }

    /// Switches which example's analysis is shown (and which drives the
    /// playhead). Drops any practice take, since it was compared against the
    /// old reference. Refused while one records: it is compared against the
    /// example it armed under, so switching mid-take only threw it away at
    /// Stop.
    pub fn select_example(&mut self, media_id: &str) -> Next {
        if self.reference_media_id().as_deref() == Some(media_id) {
            return Next::default();
        }
        if self.is_recording_practice() {
            self.recording_notice = Some("Stop the practice take to switch examples.".to_string());
            self.notify();
            return Next::default();
        }
        self.selected_example_id = Some(media_id.to_string());
        self.reset_to_reference_subject();
        self.notify();
        self.ensure_reference_analysis()
    }

    /// A new/different reference example became selected: drop any practice
    /// take (it was recorded against the OLD reference, so pairing it with
    /// the new one in the gap table would mislead) and point the analysis
    /// views at the reference.
    fn reset_to_reference_subject(&mut self) {
        self.practice = None;
        self.practice_failed = false;
        self.practice_failed_message = None;
        // Invalidate any in-flight stop_practice() analysis: its take was
        // recorded against the reference we're replacing. Its "Analyzing…"
        // goes with it: left up, it stood for a take that no longer exists
        // and kept Record disabled until the discarded pass had run to its
        // end.
        self.bump_practice_generation();
        self.practice_loading = false;
        self.subject = TechniqueAnalysisSubject::Reference;
        self.sync_position_to_subject();
    }

    fn bump_practice_generation(&mut self) {
        self.practice_generation += 1;
        self.probe.set_practice_generation(self.practice_generation);
    }

    fn selected_reference(&self) -> Option<TechniqueAnalysis> {
        let path = self.reference_path()?;
        self.ref_cache.get(&path)
    }

    /// The reference analysis of [`reference_media`](Self::reference_media),
    /// or `None` until computed.
    pub fn reference_analysis(&self) -> Option<TechniqueAnalysis> {
        self.selected_reference()
    }

    pub fn reference_analysis_loading(&self) -> bool {
        self.reference_path()
            .is_some_and(|p| self.loading_paths.contains(&p))
    }

    /// True when analysis of the selected clip was attempted but produced
    /// nothing.
    pub fn reference_analysis_failed(&self) -> bool {
        self.reference_path()
            .is_some_and(|p| self.ref_failed.contains(&p))
    }

    /// A user-presentable cause for the current reference's failed analysis,
    /// or `None` when only the generic copy applies.
    pub fn reference_analysis_failure_message(&self) -> Option<String> {
        let path = self.reference_path()?;
        self.ref_failed_cause.get(&path).map(|c| c.text.clone())
    }

    /// Whether the selected example's failed analysis may yet succeed: one
    /// that failed with no stated cause (a decode or isolate hiccup) or could
    /// not read its file. One refused for what the clip is (too long, a
    /// format the engine cannot decode) fails the same way again.
    pub fn can_retry_reference_analysis(&self) -> bool {
        self.reference_path().is_some_and(|p| {
            self.ref_failed.contains(&p)
                && self.ref_failed_cause.get(&p).is_none_or(|c| c.retryable)
        })
    }

    /// Forgets the selected example's failed analysis and runs it again: the
    /// failure was latched for the life of the page, with no way to ask
    /// again short of reopening it.
    pub fn retry_reference_analysis(&mut self) -> Next {
        if !self.can_retry_reference_analysis() {
            return Next::default();
        }
        if let Some(path) = self.reference_path() {
            self.ref_failed.remove(&path);
            self.ref_failed_cause.remove(&path);
        }
        self.ensure_reference_analysis()
    }

    /// Computes the reference analysis from
    /// [`reference_media`](Self::reference_media) if it hasn't been done for
    /// the current clip. Safe to call repeatedly (e.g. after every update).
    pub fn ensure_reference_analysis(&mut self) -> Next {
        let mut next = Next::default();
        let Some(media) = self.reference_media() else {
            return next;
        };
        let path = media.path;
        if self.ref_cache.contains_key(&path)
            || self.ref_failed.contains(&path)
            || self.loading_paths.contains(&path)
        {
            return next;
        }
        if AssetStore::asset_id_of(&path).is_some() {
            // Synced in, its file not here yet (the player above says so):
            // the decode failed as a disk error, with "Try again" that could
            // not work. Once the file lands, a reload moves the example to
            // its real path, which is analyzed afresh.
            self.ref_failed.insert(path.clone());
            self.ref_failed_cause.insert(
                path,
                FailureCause {
                    text: "This clip's file isn't on this device yet.".to_string(),
                    retryable: false,
                },
            );
            self.notify();
            return next;
        }
        if !TechniqueMedia::is_analyzable(&path) {
            // It plays (through the media player), but the engine has no
            // decoder for it: said, and never decoded — a decode only failed
            // the same way again, with "Couldn't analyze" offering a retry
            // that could never work.
            self.ref_failed.insert(path.clone());
            self.ref_failed_cause.insert(
                path,
                FailureCause {
                    text: "This clip plays, but its format can't be analyzed.".to_string(),
                    retryable: false,
                },
            );
            self.notify();
            return next;
        }
        self.loading_paths.insert(path.clone());
        self.notify();

        let analysis = Arc::clone(&self.analysis);
        let probe = Arc::clone(&self.probe);
        let analyzed = path.clone();
        self.spawn(
            &mut next,
            JobKind::AnalyzeReference,
            &Keep::default(),
            move || {
                let skipped = std::cell::Cell::new(false);
                // Asked when the app-wide analysis gate admits the call: one
                // queued behind other computes while the user clicked on to
                // another example (or left the screen) is dropped before its
                // decode, instead of delaying the analyses still wanted
                // behind a full pass for nothing.
                let wanted = || {
                    let wanted = probe.reference_wanted(&analyzed);
                    skipped.set(!wanted);
                    wanted
                };
                let result = analyze_caught(|| analysis.analyze(&analyzed, Some(&wanted)));
                (skipped.get(), result)
            },
            move |vm, outcome, next| vm.reference_analyzed(path, outcome, next),
        );
        next
    }

    /// The rest of `ensureReferenceAnalysis` once its analysis returned.
    fn reference_analyzed(
        &mut self,
        path: String,
        outcome: Result<(bool, Analyzed), String>,
        _next: &mut Next,
    ) {
        // analyze() can THROW (native decode / isolate-spawn failure), not
        // just return null. Without the `finally` below the path would stay in
        // loading_paths forever — the spinner sticks AND the contains-guard
        // above blocks every retry. Always clear the loading flag and latch
        // failure.
        let (skipped, result) = outcome.unwrap_or((false, Err(None)));
        let landed = match result {
            Ok(result) => {
                // The screen can be gone by now (analysis outlives a quick
                // back-out); its cache has left the shared analysis budget and
                // nothing will read these latches again. The cache drops the
                // write on its own, so this is about not doing pointless work,
                // not about correctness.
                if self.disposed {
                    self.end_reference_analysis(&path);
                    return;
                }
                // Where the result lands: [path], unless a commit moved the
                // clip to its pool path while this was in flight (see
                // `remap_media_after_commit`).
                let landed = self
                    .in_flight_moves
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| path.clone());
                if skipped {
                    // Nothing to latch: selecting the example again analyzes
                    // it.
                } else if let Some(result) = result {
                    self.ref_cache.insert(landed.clone(), result);
                } else {
                    self.ref_failed.insert(landed.clone());
                }
                landed
            }
            Err(e) => {
                let landed = self
                    .in_flight_moves
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| path.clone());
                self.ref_failed.insert(landed.clone());
                // Keep the cause when it is user-presentable (the
                // decode-length cap), so the failure hint can say WHY instead
                // of the generic copy.
                if let Some(e) = e.filter(presentable) {
                    self.ref_failed_cause.insert(
                        landed.clone(),
                        FailureCause {
                            text: e.to_string(),
                            retryable: e == MediaError::Read,
                        },
                    );
                }
                landed
            }
        };
        self.end_reference_analysis(&path);
        // Place the playhead if this is still the shown reference.
        if self.reference_path().as_deref() == Some(landed.as_str())
            && self.subject == TechniqueAnalysisSubject::Reference
        {
            self.sync_position_to_subject();
        }
        self.notify();
    }

    /// `ensureReferenceAnalysis`'s `finally`.
    fn end_reference_analysis(&mut self, path: &str) {
        self.loading_paths.remove(path);
        if let Some(moved) = self.in_flight_moves.remove(path) {
            self.loading_paths.remove(&moved);
        }
    }

    // --- Analysis subject (reference vs practice) ---------------------------------

    pub fn analysis_subject(&self) -> TechniqueAnalysisSubject {
        self.subject
    }

    /// Switches the analysis card between the reference and the practice
    /// take.
    pub fn show_analysis_subject(&mut self, subject: TechniqueAnalysisSubject) {
        if self.subject == subject {
            return;
        }
        self.subject = subject;
        self.sync_position_to_subject();
        self.notify();
    }

    fn active_analysis(&self) -> Option<TechniqueAnalysis> {
        match self.subject {
            TechniqueAnalysisSubject::Practice => self.practice.clone(),
            TechniqueAnalysisSubject::Reference => self.selected_reference(),
        }
    }

    // --- Mic recording (shared controller) ----------------------------------------------

    /// The engine for the next take (see `EngineSlot::acquire`); the
    /// recording controller calls it per take. Public for the tests (Dart's
    /// `@visibleForTesting`).
    pub fn acquire_engine(&self) -> Arc<GraphAudioPlaybackService> {
        self.engines.acquire()
    }

    /// True during the example/practice record START window — after the
    /// user tapped Record but before the permission + temp-dir waits resolve
    /// and is_recording flips true. Cancel must treat this as dirty so the
    /// pending start isn't left to arm a hot mic in the read view after
    /// teardown.
    pub fn starting(&self) -> bool {
        self.recorder.is_starting()
    }

    /// The one-shot notice to toast, if any.
    pub fn recording_notice(&self) -> Option<&str> {
        self.recording_notice.as_deref()
    }

    /// The notice was shown (Dart's `clearRecordingNotice`, which notified
    /// nothing).
    pub fn clear_recording_notice(&mut self) {
        self.recording_notice = None;
    }

    /// Whether a take (practice or example) is arming or recording: what the
    /// example players must not play into.
    pub fn mic_live(&self) -> bool {
        self.recorder.is_recording() || self.recorder.is_starting()
    }

    /// True while recording the ephemeral practice take.
    pub fn is_recording_practice(&self) -> bool {
        self.recorder.is_recording() && !self.recording_example
    }

    /// True while recording a clip to save as a persisted example.
    pub fn is_recording_example(&self) -> bool {
        self.recorder.is_recording() && self.recording_example
    }

    /// Live input level (0..1) while recording, for the meter (shared by both
    /// the practice and the example recorder).
    pub fn record_level(&self) -> f64 {
        self.recorder.level()
    }

    /// Bumped whenever the recorder's level or state moves (the meter's
    /// redraw key; Dart's `recordLevel` notifier).
    pub fn record_level_revision(&self) -> u64 {
        self.recorder.revision()
    }

    /// Runs the recorder's start, announcing both edges of its pending
    /// window. The recorder's `is_starting` flips synchronously at the call
    /// and can then pend behind the permission prompt and the mic open, but
    /// the screen only sees what this view model notifies: without these,
    /// Save and the example cards looked live for the whole window (a Save
    /// tap toasted "Couldn't save", a collapsed card armed the take with its
    /// Stop hidden), and a failed start left them looking busy until
    /// something else redrew.
    fn start_recorder(&mut self, prefix: &str, next: &mut Next, then: Then<MicRecordingStart>) {
        // Before the start, not once it resolves: by then capture is running
        // (the start waits for recorded frames), so a clip still playing on
        // speakers was already in the take's opening — longest on a cold
        // engine.
        next.effects.push(Effect::PauseExamples);
        let pending = self
            .recorder
            .begin_start(RecordingPathStrategy::temp_dir(prefix));
        // Listed with the quit registry from the press until the start
        // settles, its landing included. The take lists itself only once it
        // arms, after the permission prompt, the mic's open and its first
        // frames: an update's last look before its swap, landing in that
        // window, saw no work, and the relaunch's quit cut the take short.
        let listed = StartListed::new(&self.finalizer, &self.recorder);
        self.notify();
        self.spawn(
            next,
            JobKind::StartRecording,
            &Keep::default().with(listed),
            move || {
                pending.run().map_err(|e| StartThrew {
                    file_system: e.downcast_ref::<std::io::Error>().is_some(),
                    type_name: if e.downcast_ref::<std::io::Error>().is_some() {
                        "FileSystemException"
                    } else {
                        "Error"
                    },
                })
            },
            move |vm, outcome, next| {
                let threw = match outcome {
                    Ok(Ok(r)) => {
                        // A remote change held off while it pended is owed
                        // now, as after a failed import (see `busy_ended`):
                        // left, the page kept the old text, and an edit
                        // seeded from it saved over the change.
                        if !r.started() {
                            vm.busy_ended(next);
                        }
                        then(vm, r, next);
                        return;
                    }
                    Ok(Err(threw)) => threw,
                    Err(_panic) => StartThrew {
                        file_system: false,
                        type_name: "panic",
                    },
                };
                // Thrown rather than answered (the take's temp folder could
                // not be made, the engine would not open): told nothing, the
                // screen kept showing the start pending, its button disabled.
                // Said as the failure it is.
                DiagnosticsLog::instance().log(
                    "technique",
                    &format!("recording start threw: {}", threw.type_name),
                );
                vm.busy_ended(next);
                then(
                    vm,
                    MicRecordingStart::Failed(if threw.file_system {
                        MicRecordingFailure::FileCreateFailed
                    } else {
                        MicRecordingFailure::EngineUnavailable
                    }),
                    next,
                );
            },
        );
    }

    // --- Ephemeral practice recording ---------------------------------------------------

    /// The analysis of the latest practice recording (ephemeral), or `None`.
    pub fn practice_analysis(&self) -> Option<&TechniqueAnalysis> {
        self.practice.as_ref()
    }

    pub fn practice_analysis_loading(&self) -> bool {
        self.practice_loading
    }

    pub fn practice_analysis_failed(&self) -> bool {
        self.practice_failed
    }

    pub fn practice_analysis_failure_message(&self) -> Option<&str> {
        self.practice_failed_message.as_deref()
    }

    /// Whether a practice take can be recorded right now. `!saving` keeps the
    /// mic from arming while a save is committing (the edit surface is
    /// frozen then; see `save`).
    pub fn can_record_practice(&self) -> bool {
        !self.recorder.is_recording()
            && !self.practice_loading
            && !self.saving_example
            && !self.saving
            && !self.recorder.is_starting()
    }

    /// Whether the start pending (see [`starting`](Self::starting)) is a
    /// practice take's: its button says so and takes no taps, as the
    /// example's does.
    pub fn starting_practice(&self) -> bool {
        self.practice_starting && self.recorder.is_starting()
    }

    /// Starts an ephemeral practice recording. Answers
    /// ([`Answer::PracticeStart`]) an error to show, or `None`.
    pub fn start_practice(&mut self) -> Next {
        let mut next = Next::default();
        if !self.can_record_practice() {
            next.answers.push(Answer::PracticeStart(None));
            return next;
        }
        self.practice_starting = true;
        self.start_recorder(
            "vocara_practice",
            &mut next,
            Box::new(|vm, r, next| {
                vm.practice_starting = false;
                if let Some(failure) = r.failure() {
                    next.answers
                        .push(Answer::PracticeStart(Some(start_failure_message(failure))));
                    return;
                }
                // Not started with no failure: the start was invalidated
                // mid-wait (dispose or a generation bump). Nothing armed;
                // leave state untouched.
                let Some(path) = r.path() else {
                    next.answers.push(Answer::PracticeStart(None));
                    return;
                };
                vm.practice_temp_path = Some(path.to_string());
                vm.practice_take_generation = vm.practice_generation;
                vm.recording_example = false;
                vm.practice_failed = false;
                vm.practice_failed_message = None;
                vm.notify();
                next.answers.push(Answer::PracticeStart(None));
            }),
        );
        next
    }

    /// Stops the practice recording, analyzes it, and shows it on the card.
    /// The recording file is deleted afterwards (never persisted).
    pub fn stop_practice(&mut self) -> Next {
        let mut next = Next::default();
        self.stop_practice_with(false, Keep::default(), &mut next);
        next
    }

    /// [skip_analysis] is the quit-finalize path: stop the hardware and
    /// clean up the temp dir WITHOUT the analysis pass — the process is
    /// exiting, so its result would never be shown (an ephemeral take
    /// persists nothing either). [`stop_practice`](Self::stop_practice) with
    /// it is Dart's `stopPractice(skipAnalysis: true)`.
    pub fn stop_practice_skipping_analysis(&mut self) -> Next {
        let mut next = Next::default();
        self.stop_practice_with(true, Keep::default(), &mut next);
        next
    }

    fn stop_practice_with(&mut self, skip_analysis: bool, keep: Keep, next: &mut Next) {
        if !self.is_recording_practice() {
            return;
        }
        let stopped = self.recorder.stop();
        // False = the writer hit an IO error during the take, so the WAV on
        // disk is missing, damaged, or incomplete; handled conservatively
        // below.
        let recording_clean = stopped.as_ref().is_none_or(|s| s.clean);
        // > 0 = the recording ring overflowed mid-take: the WAV finalized fine
        // but is time-compressed (frames missing as silent holes) — never
        // present it as clean.
        let dropped_frames = stopped.as_ref().map_or(0, |s| s.dropped_frames);
        // The take was the hold keeping remote reloads off the read view; a
        // quit has no view left to refresh.
        if !skip_analysis {
            self.replay_missed_remote(next);
        }
        let Some(path) = self.practice_temp_path.clone() else {
            self.notify();
            return;
        };
        if skip_analysis {
            // Only the cleanup from the `finally` below matters at quit.
            self.practice_temp_path = None;
            self.spawn_detached(next, JobKind::DeleteTake, &keep, move || {
                delete_temp_recording_dir(Some(&path))
            });
            return;
        }
        // The take was recorded against the reference selected when it
        // ARMED. A reference change since then (the example cards stay live
        // while recording, so opening another one mid-take is one tap)
        // already dropped the previous take and pointed the views at the new
        // reference; analyzing this one would pair it in the gap table with a
        // reference it was never recorded against. A snapshot taken HERE
        // accepted exactly that, while the same tap landing just after Stop
        // discarded the take — the verdict hung on which side of Stop the tap
        // fell. Discard it, and say why: the user just pressed Stop and
        // expects a result.
        let generation = self.practice_take_generation;
        if self.practice_generation != generation {
            self.practice_temp_path = None;
            self.spawn(
                next,
                JobKind::DeleteTake,
                &keep,
                move || delete_temp_recording_dir(Some(&path)),
                |vm, _, _| {
                    vm.recording_notice = Some(
                        "Practice take discarded: the reference example changed while recording."
                            .to_string(),
                    );
                    vm.notify();
                },
            );
            return;
        }
        if recording_clean && dropped_frames > 0 {
            // The take is kept and analyzed anyway; the notice only stops it
            // from masquerading as gapless. A dirty stop surfaces its own
            // notice below.
            self.recording_notice = Some(dropped_frames_message(dropped_frames));
        }
        // The temp path is this call's own from here on: a generation bump
        // mid-analysis lets a new take arm, whose path the cleanup below must
        // not clear.
        self.practice_temp_path = None;
        self.practice_loading = true;
        // The previous take goes now, not when this one's analysis lands:
        // kept, its gap table and quality warning showed under "Analyzing
        // your take…" as if they were this take's.
        self.practice = None;
        self.practice_failed = false;
        self.practice_failed_message = None;
        self.subject = TechniqueAnalysisSubject::Practice;
        // The same generation guards the analysis: if it changes during
        // analyze() (the user selected/added/recorded a different reference,
        // or cleared the take), the result is discarded rather than shown
        // against the wrong reference.
        self.notify();

        // analyze() can THROW (native decode / isolate failure). Without the
        // cleanup running whatever happened, practice_loading would stay true
        // forever — can_record_practice is then stuck false, blocking EVERY
        // future practice recording, and the temp dir leaks. A pass whose
        // take was dropped meanwhile (the reference changed, the take was
        // cleared, the screen left) stops before or after its decode instead
        // of holding an analysis slot for a result nobody will see.
        let analysis = Arc::clone(&self.analysis);
        let probe = Arc::clone(&self.probe);
        self.spawn(
            next,
            JobKind::AnalyzePractice,
            &keep,
            move || {
                let ours = || probe.practice_wanted(generation);
                let result = analyze_caught(|| {
                    if recording_clean {
                        analysis.analyze(&path, Some(&ours))
                    } else if file_exists(&path) {
                        // Dirty stop: analyze the partial WAV only if it's
                        // still on disk; a missing or undecodable file falls
                        // out as a failed take below.
                        analysis.analyze(&path, Some(&ours))
                    } else {
                        Ok(None)
                    }
                });
                // The `finally`: the take's temp folder goes whatever
                // happened.
                delete_temp_recording_dir(Some(&path));
                result
            },
            move |vm, outcome, _next| {
                vm.practice_analyzed(generation, recording_clean, outcome.unwrap_or(Err(None)));
            },
        );
    }

    /// The rest of `stopPractice` once its analysis returned.
    fn practice_analyzed(&mut self, generation: u64, recording_clean: bool, outcome: Analyzed) {
        let ours = self.practice_generation == generation;
        match outcome {
            Ok(result) => {
                if ours {
                    if !recording_clean {
                        self.recording_notice = Some(if result.is_none() {
                            // A practice take is analyzed, never saved.
                            "Recording failed. The take could not be analyzed.".to_string()
                        } else {
                            "Recording hit a disk error. The take may be incomplete.".to_string()
                        });
                    }
                    self.practice_failed = result.is_none();
                    self.practice = result;
                    // A failed take stays the subject: its panel is where the
                    // failure is said (switched back to the reference,
                    // nothing on screen said it).
                }
                // else: the reference changed mid-analysis;
                // reset_to_reference_subject already dropped this take — keep
                // it discarded (the temp file is still deleted), and say
                // nothing of a recording failure an analysis skipped as
                // unwanted would only seem to show.
            }
            Err(e) => {
                if ours {
                    if !recording_clean {
                        self.recording_notice =
                            Some("Recording failed. The take could not be analyzed.".to_string());
                    }
                    self.practice = None;
                    self.practice_failed = true;
                    // Keep the cause when it is user-presentable (the
                    // decode-length cap), mirroring the reference-analysis
                    // failure hint.
                    if let Some(e) = e.filter(presentable) {
                        self.practice_failed_message = Some(e.to_string());
                    }
                }
            }
        }
        // A bump already cleared the flag, and a take armed since may have
        // set it again for its own analysis.
        if self.practice_generation == generation {
            self.practice_loading = false;
            self.sync_position_to_subject();
        }
        self.notify();
    }

    /// Discards the current practice take (and its analysis).
    pub fn clear_practice(&mut self) {
        if self.practice.is_none() && !self.practice_failed && !self.practice_loading {
            return;
        }
        self.practice = None;
        self.practice_failed = false;
        self.practice_failed_message = None;
        // Invalidate any in-flight stop_practice() analysis so it doesn't
        // resurrect the take the user just discarded. Not while a NEW take is
        // recording: no live take's analysis can be in flight then (a take
        // only arms with none loading), and the bump would void that take at
        // its Stop although its reference never changed (see
        // `stop_practice`).
        if !self.is_recording_practice() {
            self.bump_practice_generation();
            self.practice_loading = false;
        }
        if self.subject == TechniqueAnalysisSubject::Practice {
            self.subject = TechniqueAnalysisSubject::Reference;
        }
        self.sync_position_to_subject();
        self.notify();
    }

    // --- Record a persisted example clip ---------------------------------------------

    /// True while the just-recorded example clip is being imported into the
    /// draft OR while a recording start is pending (permission/temp-dir
    /// waits). The screen's save-blocked reads this, so both windows disable
    /// Save: saving during a pending start would close the edit and let the
    /// mic arm in the read view with no stop affordance.
    pub fn saving_example(&self) -> bool {
        self.saving_example || self.recorder.is_starting()
    }

    /// Whether an example clip can be recorded right now. `!saving` is part
    /// of the in-flight-save invariant: a take started during the commit
    /// would stage its clip into a draft the save has already snapshotted,
    /// so the clip would be silently dropped when the save lands (see
    /// `save`).
    pub fn can_record_example(&self) -> bool {
        !self.recorder.is_recording()
            && !self.saving_example
            && !self.importing_media
            && !self.saving
            && !self.recorder.is_starting()
    }

    /// Records a clip to add as an audio example (staged in the draft) — so
    /// a singer can demo the technique themselves and re-record until happy,
    /// seeing each take's analysis. The example is only persisted when the
    /// form is saved. Answers ([`Answer::ExampleStart`]) an error to surface,
    /// or `None`.
    pub fn start_example_recording(&mut self) -> Next {
        let mut next = Next::default();
        if !self.can_record_example() {
            next.answers.push(Answer::ExampleStart(None));
            return next;
        }
        self.start_recorder(
            "vocara_example",
            &mut next,
            Box::new(|vm, r, next| {
                if let Some(failure) = r.failure() {
                    next.answers
                        .push(Answer::ExampleStart(Some(start_failure_message(failure))));
                    return;
                }
                // Not started with no failure: the start was invalidated
                // mid-wait (dispose or a generation bump). Nothing armed;
                // leave state untouched.
                let Some(path) = r.path() else {
                    next.answers.push(Answer::ExampleStart(None));
                    return;
                };
                vm.example_temp_path = Some(path.to_string());
                vm.recording_example = true;
                vm.notify();
                next.answers.push(Answer::ExampleStart(None));
            }),
        );
        next
    }

    /// Stops the example recording and imports it into the sandbox as a
    /// STAGED example (added to the draft, not persisted until save), then
    /// deletes the temp. The just-recorded example becomes the selected
    /// reference so its analysis shows immediately. Answers
    /// ([`Answer::ExampleStop`]) the staged clip, or `None` when nothing was
    /// staged (not recording, the take failed, or the draft was abandoned
    /// mid-import).
    ///
    /// Not quit-guarded past the stop, deliberately: the clip only lands in
    /// the in-memory draft, which a quit discards like any unsaved edit, so
    /// holding the exit for the import would persist nothing. A Save while
    /// the take records runs this same tail and its commit under the quit
    /// gate instead (see `save`).
    pub fn stop_example_recording(&mut self) -> Next {
        let mut next = Next::default();
        self.stop_example_recording_into(&mut next, true);
        next
    }

    fn stop_example_recording_into(&mut self, next: &mut Next, answer: bool) {
        let answer_none = |next: &mut Next| {
            if answer {
                next.answers.push(Answer::ExampleStop(None));
            }
        };
        if !self.is_recording_example() {
            answer_none(next);
            return;
        }
        // Backstop for the in-flight-save invariant (the screen's controls
        // are disabled then): stopping here would stage a clip into the draft
        // the airborne commit already snapshotted, silently dropping it when
        // the save lands. The recording keeps running; save() itself aborts
        // it on success.
        if self.saving {
            answer_none(next);
            return;
        }
        let stopped = self.recorder.stop();
        self.stage_stopped_example(
            stopped,
            Keep::default(),
            next,
            Box::new(move |_, staged, next| {
                if answer {
                    next.answers.push(Answer::ExampleStop(staged));
                }
            }),
        );
    }

    /// The tail of `stop_example_recording` after the hardware [stopped]:
    /// import the take into the draft (see there). Split out so `save` can
    /// run it, together with its commit, inside one quit-gate bracket.
    fn stage_stopped_example(
        &mut self,
        stopped: Option<MicRecordingStop>,
        keep: Keep,
        next: &mut Next,
        then: Then<Option<TechniqueMedia>>,
    ) {
        // False = the writer hit an IO error during the take, so the WAV on
        // disk is missing, damaged, or incomplete; handled conservatively
        // below.
        let recording_clean = stopped.as_ref().is_none_or(|s| s.clean);
        // > 0 = the recording ring overflowed mid-take: the WAV finalized fine
        // but is time-compressed (frames missing as silent holes) — never
        // present it as clean.
        let dropped_frames = stopped.as_ref().map_or(0, |s| s.dropped_frames);
        self.recording_example = false;
        let Some(path) = self.example_temp_path.clone() else {
            self.notify();
            then(self, None, next);
            return;
        };
        if recording_clean && dropped_frames > 0 {
            // The clip is still staged below; the notice only stops it from
            // masquerading as gapless. A dirty stop surfaces its own notice
            // instead.
            self.recording_notice = Some(dropped_frames_message(dropped_frames));
        }
        self.saving_example = true;
        // Snapshot the draft generation: a Cancel/discard (or dispose) while
        // the clip is being copied means it must be deleted, not staged into
        // the cleared draft.
        let generation = self.draft_generation;
        self.notify();

        if recording_clean {
            self.import_take(path, None, generation, keep, next, then);
            return;
        }
        // Dirty stop: stage the clip only if the partial WAV is still present
        // and decodable, so no example points at unusable audio. The partial
        // analysis seeds the reference cache after the import below.
        let analysis = Arc::clone(&self.analysis);
        let checked = path.clone();
        let import_keep = keep.clone();
        self.spawn(
            next,
            JobKind::ValidateExample,
            &keep,
            move || {
                let partial = if file_exists(&checked) {
                    analyze_caught(|| analysis.analyze(&checked, None))
                        .ok()
                        .flatten()
                } else {
                    None
                };
                if partial.is_none() {
                    delete_temp_recording_dir(Some(&checked));
                }
                partial
            },
            move |vm, partial, next| {
                let Some(partial) = partial.ok().flatten() else {
                    vm.example_temp_path = None;
                    vm.recording_notice =
                        Some("Recording failed. The example could not be saved.".to_string());
                    vm.saving_example = false;
                    vm.busy_ended(next);
                    then(vm, None, next);
                    return;
                };
                vm.recording_notice =
                    Some("Recording hit a disk error. The example may be incomplete.".to_string());
                vm.import_take(path, Some(partial), generation, import_keep, next, then);
            },
        );
    }

    /// The import half of `stage_stopped_example`.
    fn import_take(
        &mut self,
        path: String,
        partial: Option<TechniqueAnalysis>,
        generation: u64,
        keep: Keep,
        next: &mut Next,
        then: Then<Option<TechniqueMedia>>,
    ) {
        let media = Arc::clone(&self.media_service);
        let id = self.technique_id.clone();
        let source = path;
        self.spawn(
            next,
            JobKind::Import,
            &keep,
            move || {
                let dest = media.import_media(&id, &source);
                // Import succeeded: the clip lives in the sandbox now, the
                // temp is a duplicate. (Import failed: the temp WAV is the
                // ONLY copy of the take, so it must NOT be deleted.)
                if dest.is_ok() {
                    delete_temp_recording_dir(Some(&source));
                }
                dest.map_err(|_| ())
            },
            move |vm, dest, next| {
                let Ok(Ok(dest)) = dest else {
                    // Import failed (copy error, disk full): the temp WAV is
                    // kept on disk for recovery and the failure surfaced.
                    // Clearing example_temp_path keeps dispose()'s
                    // abandoned-recording cleanup away from the kept file.
                    vm.example_temp_path = None;
                    vm.recording_notice = Some(
                        "Couldn't save the example recording. The take was kept in a \
                         temporary folder."
                            .to_string(),
                    );
                    vm.saving_example = false;
                    vm.busy_ended(next);
                    then(vm, None, next);
                    return;
                };
                let added = TechniqueMedia {
                    id: new_id(),
                    kind: TechniqueMedia::kind_for_path(&dest),
                    path: dest,
                    // Captioned now: the file is named by a generated id here
                    // and by its content hash once pooled, so the uncaptioned
                    // fallback (the file's basename) would show the user a
                    // hash.
                    label: vm.next_recording_label(),
                    added_at: DateTime::now(),
                };
                // Kept set until the file was actually gone (the job deleted
                // it), so a dispose mid-save still cleaned it up.
                vm.example_temp_path = None;
                if vm.draft_generation != generation || vm.disposed {
                    // Draft abandoned (Cancel) or view model disposed
                    // mid-save; the copied clip is an orphan — delete it
                    // rather than staging it behind the read view.
                    vm.delete_media_quietly(added.path, next);
                    vm.saving_example = false;
                    vm.busy_ended(next);
                    then(vm, None, next);
                    return;
                }
                // A dirty stop already validated the partial take: reuse its
                // analysis so the reference view doesn't decode the clip a
                // second time.
                if let Some(partial) = partial {
                    vm.ref_cache.insert(added.path.clone(), partial);
                }
                vm.draft_adds.push(added.clone());
                vm.selected_example_id = Some(added.id.clone());
                // The just-recorded example becomes the reference; drop a
                // stale practice take that was compared against the previous
                // reference (matches select_example).
                vm.reset_to_reference_subject();
                // Expand the just-recorded example so its analysis shows
                // immediately.
                vm.analysis_collapsed = false;
                vm.saving_example = false;
                vm.notify();
                then(vm, Some(added), next);
            },
        );
    }

    /// The caption a just-recorded example is staged under: "Recording N",
    /// one past the highest such caption the technique already shows, so
    /// takes recorded across edit sessions keep counting up.
    fn next_recording_label(&self) -> String {
        let highest = self
            .effective_media()
            .iter()
            .filter_map(|m| recording_number(dart_string::trim(&m.label)))
            .max()
            .unwrap_or(0);
        format!("Recording {}", highest + 1)
    }

    /// Quit-time commit of the example [staged] by `stop_example_recording`:
    /// the persisted technique plus the captured clip, and nothing else — the
    /// rest of the unsaved draft (removals, captions, earlier additions)
    /// stays uncommitted, exactly as it does when the app quits with no
    /// recording in flight. Only the take the finalizer just stopped must not
    /// die with the process.
    fn persist_example_at_quit(
        &mut self,
        staged: TechniqueMedia,
        keep: Keep,
        next: &mut Next,
        then: Then<()>,
    ) {
        let Some(current) = self.technique.clone() else {
            then(self, (), next);
            return;
        };
        // The commit resolves the clip's sandbox path to a pool path; remap
        // the analysis caches like `save` does, so the state stays coherent
        // if the exit is ever abandoned after the finalizer ran.
        let mut old_path_by_id: HashMap<String, String> = current
            .media
            .iter()
            .map(|m| (m.id.clone(), m.path.clone()))
            .collect();
        old_path_by_id.insert(staged.id.clone(), staged.path.clone());
        let reference_before = self.reference_media_id();
        let mut media = current.media.clone();
        media.push(staged.clone());
        self.commit(
            Technique { media, ..current },
            &keep,
            next,
            Box::new(move |vm, ok, next| {
                if !ok {
                    // The clip stays staged; the finalizer has no UI to retry
                    // from, so the loss risk is at least recorded.
                    DiagnosticsLog::instance().log("technique", "quit-time example commit failed");
                    then(vm, (), next);
                    return;
                }
                vm.draft_adds.retain(|m| m.id != staged.id);
                vm.remap_media_after_commit(&old_path_by_id, reference_before.as_deref());
                then(vm, (), next);
            }),
        );
    }

    // --- AnalysisSource and the playhead -------------------------------------------------

    /// Which analysis dimension (Spectrogram / Power / …) is shown.
    pub fn analysis_dim(&self) -> usize {
        self.analysis_dim
    }

    pub fn set_analysis_dim(&mut self, index: usize) {
        if self.analysis_dim == index {
            return;
        }
        self.analysis_dim = index;
        self.notify();
    }

    /// The analysis was scrubbed to [to]: the playhead moves there (clamped
    /// to the subject), and — the inline player plays the reference clip —
    /// the analyzed example's player follows ([`Effect::SeekAnalyzed`]). A
    /// practice take has no player to move.
    pub fn seek(&mut self, to: Duration) -> Next {
        let mut next = Next::default();
        let max_ms = self.active_analysis().map_or(0, |a| a.duration_ms);
        let clamped = clamp_ms(to, max_ms);
        self.position = clamped;
        if self.subject == TechniqueAnalysisSubject::Reference {
            next.effects.push(Effect::SeekAnalyzed(clamped));
        }
        next
    }

    /// Moves the analysis playhead to follow the example clip's audio player
    /// while it plays — only when the reference is shown (the inline player
    /// plays the reference clip), and not while recording (the live feed
    /// owns the views).
    pub fn set_playhead_from_playback(&mut self, to: Duration) {
        if self.recorder.is_recording() {
            return;
        }
        if self.subject != TechniqueAnalysisSubject::Reference {
            return;
        }
        let max_ms = self.selected_reference().map_or(0, |a| a.duration_ms);
        if max_ms <= 0 {
            return;
        }
        self.position = clamp_ms(to, max_ms);
    }

    /// Moves the playhead to a representative moment of the active subject.
    fn sync_position_to_subject(&mut self) {
        // Reached from analysis continuations that land after the page may
        // have gone: nothing reads the playhead then.
        if self.disposed {
            return;
        }
        self.position = self
            .active_analysis()
            .map_or(Duration::ZERO, |a| representative_time(&a));
    }

    // --- Teardown ---------------------------------------------------------------------

    /// The page is gone (Dart's `dispose`): later notifies are no-ops, the
    /// recorder is torn down, the analysis cache leaves the shared budget,
    /// and the returned jobs clean up what the page leaves (run them with
    /// [`Job::detach`]). Idempotent.
    pub fn dispose(&mut self) -> Next {
        let mut next = Next::default();
        if self.disposed {
            return next;
        }
        self.disposed = true;
        self.probe.dispose();
        // A quit waiting on this page has nothing left to wait for.
        self.quit.close();
        // Tears down a recording that is running OR still starting (a pending
        // start is invalidated and arms nothing) and unregisters the quit
        // finalizer.
        self.recorder.dispose();
        // The analysis budget is app-wide, so a cache that never leaves it
        // keeps this technique's references resident for the life of the
        // process.
        self.ref_cache.dispose();
        // Clean up any abandoned recording (Stop never pressed before
        // leaving).
        for path in [
            self.practice_temp_path.clone(),
            self.example_temp_path.clone(),
        ]
        .into_iter()
        .flatten()
        {
            self.spawn_detached(
                &mut next,
                JobKind::DeleteTake,
                &Keep::default(),
                move || delete_temp_recording_dir(Some(&path)),
            );
        }
        // Delete files copied for staged additions that were never committed
        // — but NEVER while a save is airborne: the commit is persisting rows
        // for exactly these files, and deleting them here (the route can pop
        // during the repository wait) would leave committed rows pointing at
        // nothing. The save's own completion path clears the draft it owns;
        // if it fails after this dispose, the files leak as orphans —
        // strictly better than dangling rows.
        if !self.saving {
            for path in self
                .draft_adds
                .iter()
                .map(|m| m.path.clone())
                .collect::<Vec<_>>()
            {
                self.delete_media_quietly(path, &mut next);
            }
        }
        if let Some(engine) = self.engines.take() {
            self.spawn_detached(
                &mut next,
                JobKind::DisposeEngine,
                &Keep::default(),
                move || PlaybackEngine::dispose(&*engine),
            );
        }
        next
    }

    /// Fire-and-forget deletion of an orphaned media copy (Dart's
    /// `_deleteMediaQuietly`): nothing waits for it, and an IO failure is
    /// logged, never escapes.
    fn delete_media_quietly(&mut self, path: String, next: &mut Next) {
        let work = self.delete_media_logged(path);
        self.spawn_detached(next, JobKind::DeleteMedia, &Keep::default(), work);
    }

    /// Deletes an orphaned media copy, logging an IO failure instead of
    /// throwing it: the file is only ever left for the boot sweep.
    fn delete_media_logged(&self, path: String) -> impl FnOnce() + Send + 'static {
        let media = Arc::clone(&self.media_service);
        move || {
            let failed = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                media.delete_media(&path)
            })) {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(file_error_type(&e)),
                Err(_) => Some("panic"),
            };
            if let Some(kind) = failed {
                // No path in the log: the diagnostics log's contract forbids
                // media paths (they name user content). The error TYPE is
                // enough to diagnose.
                DiagnosticsLog::instance()
                    .log("technique", &format!("draft media delete failed: {kind}"));
            }
        }
    }

    // --- Save, delete, and the media mutations ----------------------------------------

    /// Saves edited text fields plus the staged media draft. Answers
    /// ([`Answer::Saved`]) whether it persisted. An example take still
    /// recording is finalized BY the save — stopped, staged and committed
    /// with the rest — so Save keeps the take and releases the mic.
    pub fn save(&mut self, edited: Technique) -> Next {
        let mut next = Next::default();
        // Re-entrancy guard (the UI also disables Save, this is the
        // backstop): a second commit racing the first would run against the
        // same, now-stale base hashes — the three-way save would see the
        // first commit's rows as concurrent edits and could double-apply or
        // mis-merge. Report "did not persist"; the button re-enables when the
        // in-flight save resolves.
        if self.saving {
            next.answers.push(Answer::Saved(false));
            return next;
        }
        // The REVERSE races of the in-flight-save freeze: a save entering
        // while a media import / example save / recording start is mid-wait
        // would snapshot effective_media BEFORE that operation lands — its
        // result would then be silently thrown away when this save clears
        // the draft and the screen leaves edit mode. The UI disables Save for
        // all of these; this is the invariant backstop for a same-frame
        // click. A recording EXAMPLE take is the exception: this save stops
        // and stages it itself, before taking that snapshot. (A practice take
        // never reaches an edit session; see `can_edit`.)
        let finalize_take = self.is_recording_example();
        if self.importing_media
            || self.saving_example()
            || (self.recorder.is_recording() && !finalize_take)
        {
            next.answers.push(Answer::Saved(false));
            return next;
        }
        // Held across the take's import (when there is one), the commit AND
        // its cleanup below, not just the repository write: until the
        // successful commit's draft is cleared, a Cancel →
        // discard_media_draft would delete staged files whose rows just
        // persisted. Holding it from the import on also keeps Cancel and the
        // form frozen while the take is staged — the screen built [edited]
        // from the form before calling, and a Cancel → Discard landing
        // mid-import used to tear the form down under a save that then
        // committed anyway.
        self.saving = true;
        let end: Then<bool> = Box::new(|vm, ok, next| {
            // Clear BEFORE the notify so the redraw that shows the outcome
            // also re-enables Save/Cancel (and a failing repository can't
            // strand the UI saving forever).
            vm.saving = false;
            vm.notify();
            next.answers.push(Answer::Saved(ok));
        });
        if !finalize_take {
            // Held against quit like the take path below: the user pressed
            // Save, and a Cmd+Q landing during the pool import or the commit
            // must wait for it rather than exit with the edit (and its staged
            // files, swept as orphans at the next boot) silently lost.
            let keep = Keep::default().with(self.finalizer.begin_hold());
            self.commit_edit(edited, keep, &mut next, end);
            return next;
        }
        // stop() unregisters the armed-window quit finalizer, yet the take is
        // safe only once the commit below lands: until then it is a temp WAV
        // no boot recovery scans plus an in-memory draft entry. Hold the quit
        // gate over the import AND the commit (as the song flow does over its
        // finalize tail), entered synchronously after the stop so nothing
        // separates the two registrations.
        let stopped = self.recorder.stop();
        let keep = Keep::default().with(self.recorder.begin_stop_finalize());
        let commit_keep = keep.clone();
        self.stage_stopped_example(
            stopped,
            keep,
            &mut next,
            Box::new(move |vm, _staged, next| vm.commit_edit(edited, commit_keep, next, end)),
        );
        next
    }

    /// The commit half of `save` (which holds `saving` around it): fold the
    /// draft into [edited], persist it, and on success clean up after the
    /// draft.
    fn commit_edit(&mut self, edited: Technique, keep: Keep, next: &mut Next, then: Then<bool>) {
        // Fold the staged media into the technique being saved.
        let media = self.effective_media();
        // A successful commit re-reads the aggregate with every media file
        // resolved through the asset pool, so a clip staged from the sandbox
        // comes back under a DIFFERENT path. Remember each clip's pre-commit
        // path by id (ids are stable across the commit) for the remap below.
        let old_path_by_id: HashMap<String, String> = media
            .iter()
            .map(|m| (m.id.clone(), m.path.clone()))
            .collect();
        // Files of examples staged for removal — delete only after the
        // technique without them persists.
        let removed_paths: VecDeque<String> = self
            .technique
            .iter()
            .flat_map(|t| &t.media)
            .filter(|m| self.draft_removes.contains(&m.id))
            .map(|m| m.path.clone())
            .collect();
        let cleanup_keep = keep.clone();
        self.commit(
            Technique { media, ..edited },
            &keep,
            next,
            Box::new(move |vm, ok, next| {
                if !ok {
                    then(vm, false, next);
                    return;
                }
                // A successful save ends the edit session (the screen leaves
                // edit mode): invalidate a recording start still pending
                // behind its waits, and stop an example recording that armed
                // while the commit was in flight — in the read view a live
                // example mic would have no stop affordance.
                vm.recorder.invalidate_pending_start();
                let after_abort: Then<()> = Box::new(move |vm, (), next| {
                    // The removal is committed whatever happens to the files:
                    // a failed delete leaves an orphan for the boot sweep,
                    // not a failed save.
                    vm.delete_removed(
                        removed_paths,
                        cleanup_keep,
                        next,
                        Box::new(move |vm, (), next| {
                            // The staged additions are now part of the
                            // persisted technique; clear the draft so their
                            // files aren't treated as orphans.
                            let reference_before = vm.reference_media_id();
                            vm.draft_adds.clear();
                            vm.draft_removes.clear();
                            vm.draft_labels.clear();
                            // The analysis caches key on paths the commit
                            // just rewrote to pool paths; carry them onto the
                            // persisted aggregate so the shown example's
                            // analysis isn't recomputed (or its failure
                            // verdict forgotten).
                            vm.remap_media_after_commit(
                                &old_path_by_id,
                                reference_before.as_deref(),
                            );
                            then(vm, true, next);
                        }),
                    );
                });
                if vm.recorder.is_recording() && vm.recording_example {
                    vm.abort_recorder(
                        next,
                        Box::new(move |vm, (), next| {
                            vm.recording_example = false;
                            vm.example_temp_path = None;
                            after_abort(vm, (), next);
                        }),
                    );
                } else {
                    after_abort(vm, (), next);
                }
            }),
        );
    }

    /// Deletes [paths] one at a time, as Dart's loop awaited each, then runs
    /// [then].
    fn delete_removed(
        &mut self,
        mut paths: VecDeque<String>,
        keep: Keep,
        next: &mut Next,
        then: Then<()>,
    ) {
        let Some(path) = paths.pop_front() else {
            then(self, (), next);
            return;
        };
        let work = self.delete_media_logged(path);
        let rest_keep = keep.clone();
        self.spawn(
            next,
            JobKind::DeleteMedia,
            &keep,
            work,
            move |vm, _, next| {
                vm.delete_removed(paths, rest_keep, next, then);
            },
        );
    }

    /// Deletes this technique and its example files. Answers
    /// ([`Answer::Deleted`]) whether it persisted (the caller navigates back
    /// on success).
    pub fn delete(&mut self) -> Next {
        let mut next = Next::default();
        let repository = Arc::clone(&self.repository);
        let media = Arc::clone(&self.media_service);
        let id = self.technique_id.clone();
        self.spawn(
            &mut next,
            JobKind::Delete,
            &Keep::default(),
            move || {
                if repository.delete_technique(&id).is_err() {
                    return false;
                }
                // The technique is gone once its row is: a file left behind by
                // a failed cleanup is an orphan the boot sweep collects, not a
                // failed delete — thrown, it escaped unhandled and the page
                // stayed on a deleted technique.
                let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    media.delete_technique_media(&id)
                }));
                let failed = match cleanup {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(file_error_type(&e)),
                    Err(_) => Some("panic"),
                };
                if let Some(kind) = failed {
                    // No path in the log (see `delete_media_logged`).
                    DiagnosticsLog::instance().log(
                        "technique",
                        &format!("media cleanup after delete failed: {kind}"),
                    );
                }
                true
            },
            |_, ok, next| next.answers.push(Answer::Deleted(ok.unwrap_or(false))),
        );
        next
    }

    /// Imports [source_paths] as example clips, copying each into the
    /// sandbox and staging it in the draft (persisted on save). Unsupported
    /// files are skipped. Reentrancy-guarded.
    pub fn add_media(&mut self, source_paths: Vec<String>) -> Next {
        let mut next = Next::default();
        if self.importing_media {
            return next;
        }
        if self.technique.is_none() {
            return next;
        }
        // In-flight-save invariant (the UI also disables the add zone; this
        // is the backstop): the commit already snapshotted effective_media,
        // so a clip staged now would be thrown away when the save lands and
        // the screen leaves edit mode — silent loss plus an orphaned copied
        // file.
        if self.saving {
            return next;
        }

        let supported: VecDeque<String> = source_paths
            .iter()
            .filter(|p| TechniqueMedia::is_supported_media(p))
            .cloned()
            .collect();
        if supported.is_empty() {
            if !source_paths.is_empty() {
                self.recording_notice =
                    Some("No supported audio or video files in the drop.".to_string());
                self.notify();
            }
            return next;
        }

        self.importing_media = true;
        // Snapshot the draft generation: if the draft is discarded (Cancel)
        // or the view model disposed while these copies are in flight, they
        // must be deleted, not folded into the cleared draft.
        let generation = self.draft_generation;
        self.notify();

        // Unsupported files are already excluded; count both for one summary
        // notice.
        let skipped = source_paths.len() - supported.len();
        self.import_next(
            ImportRun {
                queue: supported,
                generation,
                now: DateTime::now(),
                added: Vec::new(),
                skipped,
                // Why a copy was refused, when the service says it in words
                // the user can act on (the size cap): the count alone hid why
                // a lone file failed.
                reason: None,
            },
            &mut next,
        );
        next
    }

    /// One file of `add_media`'s loop, or the loop's end.
    fn import_next(&mut self, mut run: ImportRun, next: &mut Next) {
        // Cancelled or left part way: the rest are not copied (each file up
        // to gigabytes, deleted again below).
        let source = if self.draft_generation != run.generation || self.disposed {
            None
        } else {
            run.queue.pop_front()
        };
        let Some(source) = source else {
            self.end_import(run, next);
            return;
        };
        let media = Arc::clone(&self.media_service);
        let id = self.technique_id.clone();
        let copied = source.clone();
        self.spawn(
            next,
            JobKind::Import,
            &Keep::default(),
            move || {
                media.import_media(&id, &copied).map_err(|e| match e {
                    AudioFileError::Format(message) => ImportFailure::Format(message),
                    _ => ImportFailure::Failed,
                })
            },
            move |vm, dest, next| {
                match dest {
                    Ok(Ok(dest)) => run.added.push(TechniqueMedia {
                        id: new_id(),
                        kind: TechniqueMedia::kind_for_path(&dest),
                        path: dest,
                        // The user's own file name: the sandbox copy is named
                        // by a generated id and the pooled file by its content
                        // hash, so the uncaptioned fallback (the file's
                        // basename) showed a hash.
                        label: dart_path::basename_without_extension(&source),
                        added_at: run.now,
                    }),
                    Ok(Err(ImportFailure::Format(message))) => {
                        run.skipped += 1;
                        run.reason.get_or_insert(message);
                    }
                    // A file that failed to copy; the rest still import.
                    Ok(Err(ImportFailure::Failed)) | Err(_) => run.skipped += 1,
                }
                vm.import_next(run, next);
            },
        );
    }

    /// The rest of `add_media` once its loop ended.
    fn end_import(&mut self, run: ImportRun, next: &mut Next) {
        // The draft was abandoned (Cancel) or the view model disposed while
        // importing: the copies are orphans — delete them and don't touch the
        // (cleared) draft.
        if self.draft_generation != run.generation || self.disposed {
            self.importing_media = false;
            for m in run.added {
                self.delete_media_quietly(m.path, next);
            }
            // The screen last saw importing_media true (its spinner, and
            // Save/add/Record disabled) and the user may already be in a NEW
            // edit session: tell it the import is over, or that busy state
            // sticks there.
            self.busy_ended(next);
            return;
        }

        if run.skipped > 0 {
            self.recording_notice = Some(if run.skipped == 1 {
                run.reason
                    .clone()
                    .unwrap_or_else(|| "1 file couldn't be imported.".to_string())
            } else {
                format!("{} files couldn't be imported.", run.skipped)
            });
        }

        self.draft_adds.extend(run.added.iter().cloned());
        self.importing_media = false;
        // Select + expand the first newly added audio example so its analysis
        // shows.
        if let Some(m) = run.added.iter().find(|m| !m.is_video()) {
            self.selected_example_id = Some(m.id.clone());
            // The selected reference changed; drop a stale practice take (see
            // select_example).
            self.reset_to_reference_subject();
            self.analysis_collapsed = false;
        }
        self.notify();
    }

    /// Copies example [media_id] out of the app to a place the user picks,
    /// as a take's export does: a recorded example was otherwise reachable
    /// only by digging through the app's container, its file named by a
    /// hash. The outcome is said through `recording_notice`; a cancelled
    /// panel is silent.
    pub fn export_media(&mut self, media_id: &str) -> Next {
        let mut next = Next::default();
        let Some(m) = self
            .effective_media()
            .into_iter()
            .find(|x| x.id == media_id)
        else {
            return next;
        };
        let Some(technique) = self.technique.as_ref() else {
            return next;
        };
        let suggested = AudioExporter::file_name(&technique.name, &m.display_label(), &m.path, "");
        let medium = if m.is_video() { "video" } else { "audio" };
        let exporter = Arc::clone(&self.exporter);
        self.spawn(
            &mut next,
            JobKind::Export,
            &Keep::default(),
            move || exporter.export(&m.path, &suggested, "example", medium),
            |vm, said, _| {
                let Ok(Some(said)) = said else {
                    return;
                };
                if vm.disposed {
                    return;
                }
                vm.recording_notice = Some(said);
                vm.notify();
            },
        );
        next
    }

    /// Whether example [media_id] was added in this edit and not saved yet:
    /// removing it deletes it at once (see `remove_media`), with nothing for
    /// Cancel to bring back.
    pub fn is_unsaved_media(&self, media_id: &str) -> bool {
        self.draft_adds.iter().any(|m| m.id == media_id)
    }

    /// Removes one example clip. A staged (not-yet-saved) addition is dropped
    /// and its file deleted now; a persisted example is staged for removal
    /// (its file is deleted on save).
    pub fn remove_media(&mut self, media_id: &str) -> Next {
        let mut next = Next::default();
        // In-flight-save invariant (backstop behind the disabled UI): a
        // removal staged now is invisible to the airborne commit and gets
        // cleared with the draft when the save lands — the user's removal
        // silently un-happens. Worse, removing a staged ADDITION would delete
        // a file the commit is persisting a row for.
        if self.saving {
            return next;
        }
        let reference_before = self.reference_media_id();
        if let Some(index) = self.draft_adds.iter().position(|m| m.id == media_id) {
            let m = self.draft_adds.remove(index);
            self.draft_labels.remove(media_id);
            self.forget_path(&m.path);
            self.fix_selection_after_media_change(reference_before.as_deref());
            self.notify();
            let work = self.delete_media_logged(m.path);
            self.spawn_detached(&mut next, JobKind::DeleteMedia, &Keep::default(), work);
            return next;
        }
        if self
            .technique
            .as_ref()
            .is_some_and(|t| t.media.iter().any(|m| m.id == media_id))
        {
            self.draft_removes.insert(media_id.to_string());
            self.draft_labels.remove(media_id);
            self.fix_selection_after_media_change(reference_before.as_deref());
            self.notify();
        }
        next
    }

    /// Updates the caption on one example clip (staged; applied on save).
    /// An empty caption puts back the one the clip has: the name it was
    /// added with (the user's file name, or its "Recording N"), unless a
    /// caption was saved over it since. Staged as empty, it read "Audio
    /// example": the pooled file is named by its content hash, so there is
    /// no file name left to fall back to.
    pub fn rename_media(&mut self, media_id: &str, label: &str) {
        // In-flight-save invariant (backstop behind the disabled UI): the
        // commit already snapshotted the labels, so a caption staged now
        // vanishes when the successful save clears the draft.
        if self.saving {
            return;
        }
        let label = dart_string::trim(label);
        if label.is_empty() {
            self.draft_labels.remove(media_id);
        } else {
            self.draft_labels
                .insert(media_id.to_string(), label.to_string());
        }
        self.notify();
    }

    /// Carries the path-keyed state (reference-analysis cache and failure
    /// latches) across a successful commit. The commit re-reads the
    /// aggregate with every media file resolved through the asset pool, so a
    /// clip staged from the sandbox comes back under a different path while
    /// its id stays stable; without the remap the just-computed analysis is
    /// orphaned under the old path. (The selection is keyed by id, so it
    /// needs no carrying.) [old_path_by_id] maps each committed clip's id to
    /// its pre-commit path; [reference_before] is the reference shown before
    /// the commit (see `fix_selection_after_media_change`).
    fn remap_media_after_commit(
        &mut self,
        old_path_by_id: &HashMap<String, String>,
        reference_before: Option<&str>,
    ) {
        let media = self
            .technique
            .as_ref()
            .map(|t| t.media.clone())
            .unwrap_or_default();
        for m in media {
            let Some(old) = old_path_by_id.get(&m.id) else {
                continue;
            };
            // A placeholder for bytes not on this device yet: its verdict was
            // on no bytes at all, and carried to the file that landed it kept
            // that file from ever being analyzed.
            if *old == m.path || AssetStore::asset_id_of(old).is_some() {
                continue;
            }
            // An analysis still in flight under the old path lands under the
            // new one (see `ensure_reference_analysis`), and the new path
            // reads as loading meanwhile: keyed on the old path alone, the
            // saved example looked unanalyzed, the view started a second full
            // pass over the same bytes, and the first parked a dead entry in
            // the shared cache.
            if self.loading_paths.contains(old) && !self.loading_paths.contains(&m.path) {
                self.loading_paths.insert(m.path.clone());
                self.in_flight_moves.insert(old.clone(), m.path.clone());
            }
            if let Some(cached) = self.ref_cache.remove(old) {
                self.ref_cache.insert(m.path.clone(), cached);
            }
            // The pool copy is byte-identical to the staged file, so a
            // latched failure verdict transfers — recomputing it is doomed.
            if self.ref_failed.remove(old) {
                self.ref_failed.insert(m.path.clone());
            }
            if let Some(cause) = self.ref_failed_cause.remove(old) {
                self.ref_failed_cause.insert(m.path.clone(), cause);
            }
        }
        // Re-normalize like every other media-mutating path (e.g. the
        // selected example was staged for removal by this very save).
        self.fix_selection_after_media_change(reference_before);
    }

    /// Re-selects a valid example after the current one was removed.
    /// [reference_before] is `reference_media`'s id as it was before the
    /// media changed.
    ///
    /// The practice take is dropped only when the reference ITSELF moved.
    /// This used to compare the raw selection instead, and an example nobody
    /// had tapped — the first one, the reference by fallback, which the page
    /// opens on — went from none to its own id here: read as a reference
    /// change, it dropped the take on Edit then Cancel, on a text-only Save
    /// and on every remote change to the technique, and a remote change
    /// replayed at Stop discarded the take being analyzed without a word.
    fn fix_selection_after_media_change(&mut self, reference_before: Option<&str>) {
        let audio = self.audio_examples();
        if audio.is_empty() {
            self.selected_example_id = None;
        } else if !audio
            .iter()
            .any(|m| Some(&m.id) == self.selected_example_id.as_ref())
        {
            self.selected_example_id = Some(audio[0].id.clone());
        }
        if self.reference_media_id().as_deref() != reference_before {
            // The reference example changed — drop the practice take,
            // matching select_example.
            self.reset_to_reference_subject();
        }
    }

    /// Applies [technique] optimistically, persists it, reverts on failure;
    /// [then] gets whether the save persisted (callers gate file deletion on
    /// it).
    fn commit(&mut self, technique: Technique, keep: &Keep, next: &mut Next, then: Then<bool>) {
        // A starting save supersedes any in-flight remote-applied reload (its
        // read predates this write) and blocks new ones until the fresh base
        // is captured below. `save` already holds `saving` here, so this
        // optimistic notify also disables the header's Save/Cancel.
        self.reload_epoch += 1;
        let previous = self.technique.replace(technique.clone());
        self.notify();
        // Three-way save against the base captured at load / the previous
        // save, so a stale technique can't revert or tombstone concurrent
        // (synced-in) changes; the returned fresh base replaces ours for the
        // next save.
        let repository = Arc::clone(&self.repository);
        let base = self.base_hashes.clone();
        self.spawn(
            next,
            JobKind::Commit,
            keep,
            move || {
                repository
                    .save_technique_with_base(&technique, &base)
                    .map_err(|e| e.to_string())
            },
            move |vm, result, next| {
                let ok = match result {
                    Ok(Ok(loaded)) => {
                        // Committed, but its re-read failed: what was sent
                        // stands in for the store, which may hold rows a
                        // concurrent writer kept, and pool paths where this
                        // has its clips' sandbox ones. A reload is owed, as
                        // for a remote change skipped meanwhile — replayed
                        // once nothing holds the editor (a save ends its edit
                        // session).
                        if !loaded.reread {
                            vm.missed_remote = true;
                        }
                        vm.technique = Some(loaded.aggregate);
                        vm.base_hashes = loaded.base_hashes;
                        true
                    }
                    _ => {
                        // Revert to the previous aggregate; the base hashes
                        // stay as they are — they still describe the last
                        // state this session loaded or saved, which is what
                        // the reverted model matches.
                        vm.technique = previous;
                        false
                    }
                };
                vm.notify();
                then(vm, ok, next);
            },
        );
    }
}

/// [to] in milliseconds, clamped to `0..=max_ms`.
fn clamp_ms(to: Duration, max_ms: i64) -> Duration {
    let ms = i64::try_from(to.as_millis()).unwrap_or(i64::MAX);
    Duration::from_millis(clamp_i64(ms, 0, max_ms.max(0)) as u64)
}

impl AnalysisSource for TechniqueDetailViewModel {
    fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        let a = self.active_analysis()?;
        // A non-empty audio file path makes the clip carry duration_ms (the
        // time axis for the timbre / register / pitch / dynamics panels). No
        // view reads the path itself.
        let created_at = DateTime::from_millis_since_epoch(0, false).unwrap_or_else(DateTime::now);
        Some(Arc::new(
            AudioTrack::new(
                "technique-analysis",
                AudioTrackKind::Vocal,
                match self.subject {
                    TechniqueAnalysisSubject::Practice => "Your practice",
                    TechniqueAnalysisSubject::Reference => "Reference",
                },
                created_at,
            )
            .with_audio("technique-analysis.wav", 0, a.duration_ms, Vec::new()),
        ))
    }

    /// The subject axis: the synthetic [`selected_track`] wraps exactly one
    /// clip placed at 0, so the subject span is trivially the track span —
    /// the clip's own 0..duration_ms.
    ///
    /// [`selected_track`]: AnalysisSource::selected_track
    fn analysis_start_ms(&self) -> i64 {
        0
    }

    fn analysis_duration_ms(&self) -> i64 {
        self.active_analysis().map_or(0, |a| a.duration_ms)
    }

    fn selected_metrics(&self) -> Option<Arc<VoiceMetrics>> {
        self.active_analysis().map(|a| a.metrics)
    }

    fn selected_spectrogram(&self) -> Option<Arc<Spectrogram>> {
        self.active_analysis().map(|a| a.spectrogram)
    }

    fn selected_profile(&self) -> Option<Arc<VoiceProfile>> {
        self.active_analysis().map(|a| a.profile)
    }

    fn selected_ltas(&self) -> Option<Arc<Ltas>> {
        self.active_analysis().map(|a| a.ltas)
    }

    fn selected_register(&self) -> Option<Arc<RegisterMap>> {
        self.active_analysis().map(|a| a.register)
    }

    fn spectrogram_loading(&self) -> bool {
        match self.subject {
            TechniqueAnalysisSubject::Practice => self.practice_loading,
            TechniqueAnalysisSubject::Reference => self.reference_analysis_loading(),
        }
    }

    /// The halves come from one compute here.
    fn metrics_loading(&self) -> bool {
        self.spectrogram_loading()
    }

    /// True while a recording is in progress (the controller feeds the
    /// monitor then), switching the analysis views to the live mic feed.
    fn live_monitor(&self) -> bool {
        self.recorder.feeding_monitor()
    }

    fn monitor(&self) -> &MonitorService {
        &self.monitor
    }

    fn position(&self) -> Duration {
        self.position
    }

    fn revision(&self) -> u64 {
        self.revision
    }
}

impl TechniqueDetailViewModel {
    /// The shared playhead (subject-local); see [`AnalysisSource::position`].
    pub fn position(&self) -> Duration {
        self.position
    }

    /// True while a recording feeds the live monitor; see
    /// [`AnalysisSource::live_monitor`].
    pub fn live_monitor(&self) -> bool {
        self.recorder.feeding_monitor()
    }

    /// The live monitor the analysis views read while recording.
    pub fn monitor(&self) -> &MonitorService {
        &self.monitor
    }

    /// The active subject's metrics; see [`AnalysisSource::selected_metrics`].
    pub fn selected_metrics(&self) -> Option<Arc<VoiceMetrics>> {
        self.active_analysis().map(|a| a.metrics)
    }
}
