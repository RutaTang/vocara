//! The song practice screen's coordinator (port of
//! `ui/song_practice_detail/view_model/song_practice_detail_view_model.dart`):
//! the composition root of the screen's five controllers and the single
//! owner of the practice model, the selection, persistence and the audio
//! engine / monitor lifecycle.
//!
//! The feature areas live in the five controllers this view model
//! constructs: [`PlaybackController`] (transport, loop, zoom, playhead),
//! [`RecordingController`] (take capture, input monitoring, device watch),
//! [`MixerController`] (everything the engine hears),
//! [`InterpretationsController`] (`versions`) and [`AnalysisCache`]
//! (compute caches, dock, live view). What remains here besides composition:
//! load and save, the remote-applied refresh, audio imports, source
//! separation, export, track/clip STRUCTURE edits, notes, technique markers,
//! lyrics, toasts, and the shared [`BusyGate`].
//!
//! # How the screen drives it
//!
//! The view model is plain state with no iced types (see `rust/PORTING.md`,
//! "View models"). Every Dart `notifyListeners` — its own and each
//! controller's, which Dart chained into its own — moves
//! [`SongPracticeDetailViewModel::revision`]; the views compare it between
//! builds.
//!
//! Every action returns the work it starts as [`Effect`]s, in order
//! (`#[must_use]`). The screen runs each:
//!
//! | Effect | Run it | Hand the result to |
//! |---|---|---|
//! | [`Effect::Run`] | [`Job::run`] on a blocking worker | [`SongPracticeDetailViewModel::job_done`] |
//! | [`Effect::Analysis`] | [`AnalysisJob::run`] on a worker | [`SongPracticeDetailViewModel::analysis_job_done`] |
//! | [`Effect::AnalysisMicPermission`] | `mic_permission::request` on a worker | [`SongPracticeDetailViewModel::analysis_mic_permission_answered`] |
//! | [`Effect::LaneHealRetry`] | a timer | [`SongPracticeDetailViewModel::lane_heal_retry_due`] |
//! | [`Effect::StartRecording`] | [`StartJob::run`] on a worker, handing its [`TakeSetupRequest`] to [`SongPracticeDetailViewModel::take_setup`] on the UI thread | [`SongPracticeDetailViewModel::start_done`] |
//! | [`Effect::RecordingWork`] | [`RecordingJob::run`] on a worker | [`SongPracticeDetailViewModel::recording_job_done`] |
//! | [`Effect::FinalizeTake`] | [`BusyRun::into_task`] | [`SongPracticeDetailViewModel::finalize_ran`] |
//! | [`Effect::FinalizeRetry`] | a timer | [`SongPracticeDetailViewModel::finalize_retry_due`] |
//! | [`Effect::MonitorMicPermission`] | `mic_permission::request` on a worker | [`SongPracticeDetailViewModel::monitor_permission_answered`] |
//! | [`Effect::InterpretationsCleanUp`] | [`BusyRun::into_task`] | [`SongPracticeDetailViewModel::interpretations_cleaned_up`] |
//! | [`Effect::Toast`] | show it (Dart's `toasts` stream) | — |
//! | [`Effect::Leave`] | pop the page (the answer to [`SongPracticeDetailViewModel::stop_before_leaving`]) | — |
//! | [`Effect::Reply`] | the answer to a call the screen awaits (see [`Reply`]) | — |
//!
//! Each answer method returns the next effects. A Dart `await` on a
//! repository, file, edit or engine call is a [`Job`]; the code after it runs
//! in [`SongPracticeDetailViewModel::job_done`], with every generation,
//! identity and `_disposed` guard Dart had at that point. Dart's save chain
//! (`_enqueueSave`) is a queue: one save runs at a time, the next starts
//! (its synchronous start, as Dart's `then` ran it) before the finished
//! one's caller resumes, and each holds the quit and is listed as landing on
//! the practice from the moment it is queued. A save queued with none
//! running starts as the call that queued it ends (Dart chained it on a
//! completed tail: a microtask), so it sends the model that call left.
//!
//! # The API
//!
//! Construction: [`SongPracticeDetailViewModel::new`]`(`[`Deps`]`,
//! practice_id, `[`Options`]`) -> (Self, Vec<Effect>)` — the load and the
//! separation probe start at once.
//!
//! State (Dart's getters, snake_cased): `revision`, `practice_id`,
//! `practice`, `app_settings`, `monitor`, `load_running`, `load_error`,
//! `busy`, `error_message`, `catching_up`, `separation_progress`,
//! `is_separating`, `separation_available`, `can_cancel_separation`,
//! `can_reseparate`, `selected_track_id`, `selected_clip_id`,
//! `selected_bus_id`, `selected_bus`, `selected_track`, `track_by_id`,
//! `selected_track_notes`, `selected_track_techniques`, `all_notes`,
//! `active_backing`, `lyrics_open`, `inspector_open`, `right_tab`, `lyrics`,
//! `has_lyrics`, `lyrics_mode`, `lyrics_edit_session`, `lyrics_draft`,
//! `lyrics_focus`, `sync_index`, `current_lyric_index(position_ms)`,
//! `lyrics_as_text`, `position`, `is_recording`, `take_in_flight`,
//! `take_holds_screen`, `is_moving_live(track_id)`, `record_target_id`,
//! `has_work_in_flight`, `is_disposed`; the controllers (`playback`,
//! `mixer`, `analysis`, `recording`, `versions`) and their host-bound reads
//! (`with_playback`, `with_mixer`, `with_analysis`); and the view model is
//! the dock's [`AnalysisSource`].
//!
//! Actions returning `Vec<Effect>`: `select_track`, `select_clip`,
//! `set_play_reference`, `toggle_inspector_panel`, `stamp_next_lyric`,
//! `stamp_lyric_at`, `clear_lyric_times`, `separate_backing`,
//! `export_track_audio`, `export_clip_audio`, `new_take`, `delete_track`,
//! `delete_clip`, `commit_track_move`, `move_vocal_track_to_lane_of`,
//! `rename_track`, `set_track_color`, `duplicate_track`,
//! `split_track_at_playhead`, `merge_track_down`, `reanalyze_track`,
//! `add_note`, `add_note_at_playhead`, `update_note_content`, `delete_note`,
//! `add_technique`, `delete_technique_marker`, `stop_recording`,
//! `stop_before_leaving`, `commit_fx`, `on_remote_applied`,
//! `refuse_structural_edit` (`(refused, effects)`), and the controller
//! actions `playback_do`, `mixer_do`, `analysis_do`, `recording_do`,
//! `versions_do`. Returning a [`Started`]: `add_accompaniment`,
//! `add_reference`, `add_vocal_take`, `add_clip_to_take`,
//! `import_lyrics_from_file`, `set_lyrics_text`, `import_lyrics`,
//! `apply_edited_lyrics`, `available_techniques`. Plain: `select_bus`,
//! `toggle_lyrics_panel`, `set_right_tab`, `set_lyrics_mode`,
//! `set_lyrics_draft`, `set_lyrics_focus`, `seek`, `seek_to_lyric`,
//! `move_clip_start_live`, `cancel_separation`, `dispose`.
//!
//! A call whose Dart future the screen awaits for its value returns a
//! [`Started`]: its [`Call`] comes back in a [`Reply`] when it is done (the
//! imports, so a multi-file drop runs one file after the other; the lyrics
//! writes, whose editor stays open unless they landed; the technique list).
//!
//! # The controllers
//!
//! The controllers are reached through the view model, which is their host
//! (Dart's constructor closures): [`SongPracticeDetailViewModel::playback`]
//! and its siblings for host-free reads, `with_*` for the reads that take
//! a host, and `*_do` for every action, which routes the controller's
//! effects (a [`SaveRequest`] becomes a save, a recording's sibling calls
//! run in place). The backing A/B switch is
//! [`SongPracticeDetailViewModel::set_play_reference`] (it keeps the
//! selection on the backing shown), never the mixer's own.
//!
//! # Ticks
//!
//! While each is active the screen calls: [`SongPracticeDetailViewModel::drain_engine_events`]
//! on every frame (the engine's position and playing streams),
//! [`SongPracticeDetailViewModel::pump_capture`] every 30 ms while
//! [`AnalysisCache::capture_pump_active`], [`SongPracticeDetailViewModel::mic_tick`]
//! while [`RecordingController::next_mic_tick_due`] is `Some`,
//! [`SongPracticeDetailViewModel::monitor_meter_tick`] and
//! [`SongPracticeDetailViewModel::device_watch_tick`] at the recording
//! controller's periods, [`SongPracticeDetailViewModel::separation_progress_tick`]
//! while [`SongPracticeDetailViewModel::is_separating`], and
//! [`SongPracticeDetailViewModel::serve_quit_requests`] when the waker set
//! with [`SongPracticeDetailViewModel::set_quit_waker`] fires.
//!
//! # Leaving
//!
//! [`SongPracticeDetailViewModel::dispose`] tears the screen down as Dart's
//! did (recording first, an in-flight separation cancelled, the engine
//! detached and disposed). What is in flight then outlives the page, as
//! Dart's futures did: keep the view model and keep answering its effects
//! until [`SongPracticeDetailViewModel::has_work_in_flight`] reads false
//! (a stopped take landing, a queued save, a guarded edit). Toasts after
//! dispose are dropped.

mod edits;
mod hosts;
mod jobs;
mod lyrics;
mod persist;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use vocara_model::models::audio_track::{
    lane_visible_spans, AudioClip, AudioTrack, AudioTrackKind,
};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::models::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_services::app_settings_service::AppSettingsService;
use vocara_services::audio_edit_service::{AudioEditService, AudioEdits};
use vocara_services::audio_exporter::{AudioExporter, PickError};
use vocara_services::audio_file_service::AudioFiles;
use vocara_services::engine::media::{probe_media_duration, MediaError};
use vocara_services::mic_recording_controller::MicRecorder;
use vocara_services::monitor_service::MonitorService;
use vocara_services::playback_engine_factory::create_playback_engine;
use vocara_services::recording_finalizer::{Landing, RecordingFinalizer};
use vocara_services::source_separation_service::SourceSeparationService;
use vocara_services::spectrogram_service::SpectrogramAnalyses;
use vocara_services::vocal_tract::VocalTractTrack;
use vocara_services::waveform_service::{self, WaveformResult, DEFAULT_TARGET_BUCKETS};
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};
use vocara_store::sync::sync_engine::RemoteApplied;

use super::analysis_cache::{
    self, AnalysisCache, AnalysisCacheOptions, AnalysisEffect, AnalysisHost, AnalysisJob,
};
use super::analysis_source::AnalysisSource;
use super::interpretations_controller::{
    InterpretationsController, InterpretationsEffect, InterpretationsHost, VersionsOp,
};
use super::mixer_controller::{EngineHandle, MixerController, MixerHost, SaveRequest};
use super::playback_controller::{PlaybackController, PlaybackHost};
use super::recording_controller::{
    self, RecordingController, RecordingControllerOptions, RecordingEffect, RecordingHost,
    RecordingJob, RecordingOp, RecordingSettings, StartDone, StartJob, TakeAnalyzer,
    TakeSetupRequest,
};
use crate::util::busy_gate::{BusyGate, BusyOutcome, BusyRun};
use crate::util::ActionError;

pub use super::analysis_cache::{AnalysisLane, DockTab};
pub use jobs::{Job, JobDone, JobKind};

use hosts::{AnalysisView, MixerView, PlaybackView, RecordingView, VersionsView};
use jobs::{Failure, ProgressCell};
use persist::{CatchUp, QueuedSave, RunningSave, SaveThen};

// --- Public types ---------------------------------------------------------------

/// The tabs of the right-side panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RightTab {
    Lyrics,
    Notes,
    Techniques,
}

/// The lyrics panel's interaction mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LyricsMode {
    Read,
    Edit,
    Sync,
}

/// A note/technique range drag shorter than this (ms) collapses to a point —
/// so a stray wobble while tapping doesn't create a tiny range.
pub const MIN_MARKER_RANGE_MS: i64 = 120;

/// Orders `start_ms`/`end_ms` and drops a `None` or too-short span to a point
/// marker (end `None`). Returns the normalized (start, end), with start never
/// negative. Pure — unit-tested. The editors title a marker by it too, so a
/// drag too short to keep as a range is not announced as one.
pub fn normalize_marker_range(start_ms: i64, end_ms: Option<i64>) -> (i64, Option<i64>) {
    let Some(end_ms) = end_ms else {
        return (start_ms.max(0), None);
    };
    let mut s = start_ms.min(end_ms);
    let e = if start_ms < end_ms { end_ms } else { start_ms };
    if s < 0 {
        s = 0;
    }
    if e - s < MIN_MARKER_RANGE_MS {
        return (s, None);
    }
    (s, Some(e))
}

/// Clamps a note's range so its span doesn't extend past `boundary_ms` (e.g.
/// a split point or a deleted clip's edge), collapsing to a point when the
/// clamped span is shorter than the minimum. A point note is returned
/// unchanged.
pub fn clamp_note_end(n: &PracticeNote, boundary_ms: i64) -> PracticeNote {
    match n.end_ms {
        Some(end) if end > boundary_ms => PracticeNote {
            end_ms: if boundary_ms - n.position_ms < MIN_MARKER_RANGE_MS {
                None
            } else {
                Some(boundary_ms)
            },
            ..n.clone()
        },
        _ => n.clone(),
    }
}

/// The parts of `deleted`'s span that no clip in `remaining` still covers:
/// where the lane falls silent once it goes, in timeline order. A take's
/// notes are the lane's, at absolute times, so only what lies there goes
/// with the clip — a punch-in deleted from over its original took the notes
/// the original still played under.
pub fn uncovered_spans(deleted: &AudioClip, remaining: &[AudioClip]) -> Vec<(i64, i64)> {
    let mut spans = vec![(deleted.start_ms, deleted.end_ms())];
    for c in remaining {
        let mut next = Vec::new();
        for (s, e) in spans {
            if c.end_ms() <= s || c.start_ms >= e {
                next.push((s, e));
            } else {
                if c.start_ms > s {
                    next.push((s, c.start_ms));
                }
                if c.end_ms() < e {
                    next.push((c.end_ms(), e));
                }
            }
        }
        spans = next;
    }
    spans.sort_by_key(|span| span.0);
    spans
}

/// The clip of `clips` (a lane's, in its order) heard at `ms`: the topmost
/// one covering it ([`lane_visible_spans`]), `None` where the lane is
/// silent. A note or technique marker lies on the audio of the clip heard
/// at its start, so a range that runs on over the next clip goes with the
/// one it starts on.
pub fn clip_heard_at(clips: &[AudioClip], ms: i64) -> Option<usize> {
    let spans: Vec<(i64, i64)> = clips.iter().map(|c| (c.start_ms, c.end_ms())).collect();
    lane_visible_spans(&spans)
        .into_iter()
        .find(|v| ms >= v.start_ms && ms < v.end_ms)
        .map(|v| v.clip)
}

/// What rides a clip being dragged: the notes and technique markers that
/// lay on its audio when the drag began ([`clip_heard_at`]), as they were
/// then, and the clip's start then. Each live move puts them where they
/// were, moved by the clip's travel since: a note the clip passes over is
/// not swept along, and a drag back returns everything where it was. The
/// notes stayed at their times while the clip moved away, so its delete,
/// which takes "the notes inside it", took other audio's notes and left its
/// own.
struct ClipRiders {
    track_id: String,
    clip_id: String,
    start_ms: i64,
    notes: Vec<PracticeNote>,
    markers: Vec<TechniqueMarker>,
}

impl ClipRiders {
    /// What lies on `clip_id`'s audio on `track` as it is now.
    fn of(track: &AudioTrack, clip_id: &str) -> Option<ClipRiders> {
        let index = track.clips.iter().position(|c| c.id == clip_id)?;
        let on_clip = |ms: i64| clip_heard_at(&track.clips, ms) == Some(index);
        Some(ClipRiders {
            track_id: track.id.clone(),
            clip_id: clip_id.to_string(),
            start_ms: track.clips[index].start_ms,
            notes: track
                .notes
                .iter()
                .filter(|n| on_clip(n.position_ms))
                .cloned()
                .collect(),
            markers: track
                .technique_markers
                .iter()
                .filter(|m| on_clip(m.position_ms))
                .cloned()
                .collect(),
        })
    }

    /// `notes` with the riders moved `delta` from where they were, a range
    /// held inside `timeline_ms`.
    fn moved_notes(
        &self,
        notes: &[PracticeNote],
        delta: i64,
        timeline_ms: i64,
    ) -> Vec<PracticeNote> {
        notes
            .iter()
            .map(|n| match self.notes.iter().find(|r| r.id == n.id) {
                Some(r) => clamp_note_end(
                    &PracticeNote {
                        position_ms: r.position_ms + delta,
                        end_ms: r.end_ms.map(|e| e + delta),
                        ..n.clone()
                    },
                    timeline_ms,
                ),
                None => n.clone(),
            })
            .collect()
    }

    /// Like [`ClipRiders::moved_notes`], for technique markers.
    fn moved_markers(
        &self,
        markers: &[TechniqueMarker],
        delta: i64,
        timeline_ms: i64,
    ) -> Vec<TechniqueMarker> {
        markers
            .iter()
            .map(|m| match self.markers.iter().find(|r| r.id == m.id) {
                Some(r) => clamp_marker_end(
                    &TechniqueMarker {
                        position_ms: r.position_ms + delta,
                        end_ms: r.end_ms.map(|e| e + delta),
                        ..m.clone()
                    },
                    timeline_ms,
                ),
                None => m.clone(),
            })
            .collect()
    }
}

/// Like [`clamp_note_end`], for a technique marker.
pub fn clamp_marker_end(m: &TechniqueMarker, boundary_ms: i64) -> TechniqueMarker {
    match m.end_ms {
        Some(end) if end > boundary_ms => TechniqueMarker {
            end_ms: if boundary_ms - m.position_ms < MIN_MARKER_RANGE_MS {
                None
            } else {
                Some(boundary_ms)
            },
            ..m.clone()
        },
        _ => m.clone(),
    }
}

/// A call whose completion the screen awaits (see [`Reply`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Call(u64);

/// A started call: its [`Call`] and the work it starts.
#[must_use = "run the effects; the call's Reply comes back among them or later ones"]
#[derive(Debug)]
pub struct Started {
    pub call: Call,
    pub effects: Vec<Effect>,
}

/// The answer to a call the screen awaits (Dart's future completing).
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// The call ([`SongPracticeDetailViewModel::add_accompaniment`],
    /// [`SongPracticeDetailViewModel::add_reference`],
    /// [`SongPracticeDetailViewModel::add_vocal_take`],
    /// [`SongPracticeDetailViewModel::import_lyrics_from_file`]) is done.
    Done(Call),
    /// [`SongPracticeDetailViewModel::add_clip_to_take`] is done: where the
    /// placed clip ends, `None` when nothing was added.
    ClipAdded { call: Call, end_ms: Option<i64> },
    /// A lyrics write ([`SongPracticeDetailViewModel::set_lyrics_text`],
    /// [`SongPracticeDetailViewModel::import_lyrics`],
    /// [`SongPracticeDetailViewModel::apply_edited_lyrics`]) is done:
    /// whether it landed (the editor keeps its draft if not).
    LyricsSaved { call: Call, landed: bool },
    /// [`SongPracticeDetailViewModel::available_techniques`]: the library,
    /// or `None` when it could not be read.
    Techniques {
        call: Call,
        techniques: Option<Vec<Technique>>,
    },
}

/// Work the screen does for the view model (see the module doc).
pub enum Effect {
    Run(Job),
    Analysis(AnalysisJob),
    AnalysisMicPermission,
    LaneHealRetry { after: Duration },
    StartRecording(StartJob),
    RecordingWork(RecordingJob),
    FinalizeTake(RecordingOp, BusyRun<String>),
    FinalizeRetry { op: RecordingOp, after: Duration },
    MonitorMicPermission,
    InterpretationsCleanUp(VersionsOp, BusyRun<String>),
    Toast(String),
    Leave,
    Reply(Reply),
}

impl fmt::Debug for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Effect::Run(job) => f.debug_tuple("Run").field(job).finish(),
            Effect::Analysis(job) => f.debug_tuple("Analysis").field(job).finish(),
            Effect::AnalysisMicPermission => f.write_str("AnalysisMicPermission"),
            Effect::LaneHealRetry { after } => f
                .debug_struct("LaneHealRetry")
                .field("after", after)
                .finish(),
            Effect::StartRecording(job) => f.debug_tuple("StartRecording").field(job).finish(),
            Effect::RecordingWork(_) => f.write_str("RecordingWork"),
            Effect::FinalizeTake(op, _) => f.debug_tuple("FinalizeTake").field(op).finish(),
            Effect::FinalizeRetry { op, after } => f
                .debug_struct("FinalizeRetry")
                .field("op", op)
                .field("after", after)
                .finish(),
            Effect::MonitorMicPermission => f.write_str("MonitorMicPermission"),
            Effect::InterpretationsCleanUp(op, _) => {
                f.debug_tuple("InterpretationsCleanUp").field(op).finish()
            }
            Effect::Toast(m) => f.debug_tuple("Toast").field(m).finish(),
            Effect::Leave => f.write_str("Leave"),
            Effect::Reply(r) => f.debug_tuple("Reply").field(r).finish(),
        }
    }
}

/// Resolves where an export is written, given the suggested file name (the
/// platform save panel in the app).
pub type SaveLocationPicker = Arc<dyn Fn(&str) -> Result<Option<String>, PickError> + Send + Sync>;

/// Probes a media file's declared duration (the import admission gate).
pub type ProbeDuration = Arc<dyn Fn(&str) -> Result<Option<Duration>, MediaError> + Send + Sync>;

/// The view model's collaborators (Dart's required constructor parameters).
#[derive(Clone)]
pub struct Deps {
    pub repository: Arc<dyn SongPracticeRepository>,
    pub technique_repository: Arc<dyn TechniqueRepository>,
    pub audio_files: Arc<dyn AudioFiles>,
    pub spectrograms: Arc<dyn SpectrogramAnalyses>,
    pub separation: Arc<dyn SourceSeparationService>,
    pub app_settings: Arc<AppSettingsService>,
}

/// The view model's optional parts (Dart's optional and test-only
/// constructor parameters).
#[derive(Default)]
pub struct Options {
    /// The playback engine (default: a new native graph engine). Owned here:
    /// created, attached to the settings, disposed.
    pub engine: Option<EngineHandle>,
    pub monitor: Option<Arc<MonitorService>>,
    /// The clip edits (default: an [`AudioEditService`] over the files).
    pub edits: Option<Arc<dyn AudioEdits>>,
    /// The quit's registry (default: the app-wide instance).
    pub finalizer: Option<Arc<RecordingFinalizer>>,
    /// The export's save panel (default: `file_panels`' save panel).
    pub save_location_picker: Option<SaveLocationPicker>,
    /// The import admission gate's probe. Injectable because the real probe
    /// is a direct call into the engine, which plain unit tests cannot make.
    pub probe_duration: Option<ProbeDuration>,
    /// The waveform analysis (Dart's `WaveformService.analyze`; default
    /// [`waveform_service::analyze`]). Its error is the user-presentable
    /// message.
    pub analyze: Option<TakeAnalyzer>,
    /// A recorder to drive instead of the real one (Dart's `mic`).
    pub mic: Option<Arc<dyn MicRecorder>>,
    /// The recording controller's other seams (its `mic` and `analyze` are
    /// the two above).
    pub recording: RecordingControllerOptions,
    /// The analysis cache's seams.
    pub analysis: AnalysisCacheOptions,
    /// The recording controller's settings (default: the app settings).
    pub recording_settings: Option<Arc<dyn RecordingSettings>>,
}

/// What [`SongPracticeDetailViewModel::all_notes`] lists: a note with its
/// owning lane.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteEntry {
    pub track_id: String,
    pub track_name: String,
    pub note: PracticeNote,
}

/// What was selected before an empty take selected itself (Dart's
/// `({String? track, String? bus, int picks})` record, compared by
/// identity: `token`).
#[derive(Clone, Debug)]
struct SelectionBefore {
    token: u64,
    track: Option<String>,
    bus: Option<String>,
    picks: u64,
}

// --- Core state -------------------------------------------------------------------

/// The view model's own state: everything but the controllers, so a host
/// can borrow it beside them.
pub(super) struct Core {
    repository: Arc<dyn SongPracticeRepository>,
    technique_repository: Arc<dyn TechniqueRepository>,
    audio_files: Arc<dyn AudioFiles>,
    edits: Arc<dyn AudioEdits>,
    separation: Arc<dyn SourceSeparationService>,
    app_settings: Arc<AppSettingsService>,
    /// Exports takes, clips and backings (its save panel is injectable so
    /// tests can drive an export without the platform panel).
    exporter: Arc<AudioExporter>,
    probe_duration: ProbeDuration,
    analyze: TakeAnalyzer,
    finalizer: Arc<RecordingFinalizer>,
    /// The playback engine — owned here (created, attached to settings,
    /// disposed); controllers only borrow it.
    engine: EngineHandle,
    monitor: Arc<MonitorService>,
    practice_id: String,

    revision: u64,
    /// Set in dispose. Guards the work that resumes after the view model is
    /// gone (a save finishing while the route is popped mid-record) from
    /// touching what the page owned.
    disposed: bool,
    out: Vec<Effect>,
    next_id: u64,

    /// The one busy/error latch (spinner + structural-edit mutex).
    gate: BusyGate,
    /// What the gate's holder is listed under as landing on the practice
    /// (Dart's `around`): every guarded edit (an import, a split, a merge, a
    /// take's finalize) outlives this screen if the user leaves mid-way, and
    /// writes when it ends: listed from its start, a screen reopened
    /// meanwhile catches up on it. Listed only at its closing save, the
    /// reopen missed it and later saved over it.
    gate_landing: Option<Landing>,
    /// The gate is the catch-up's, which is not listed: it waits for
    /// everything landing on this practice, itself included if it were.
    gate_unlisted: bool,

    practice: Option<Arc<SongPractice>>,
    /// The model as last read from or written to the store (a load, a
    /// save's re-read, a revert). Any other practice holds a change not
    /// saved yet.
    persisted: Option<Arc<SongPractice>>,
    /// Per-row content hashes of the persisted practice as THIS view model
    /// last loaded or saved it — the BASE side of the repository's three-way
    /// save. Captured by the load, handed to every save, and replaced by the
    /// fresh map each successful save (and the failed-save reload) returns,
    /// so a stale in-memory aggregate can never revert or tombstone rows
    /// that changed underneath it (e.g. applied by sync).
    base_hashes: IndexMap<String, String>,

    load_running: bool,
    load_error: Option<String>,
    /// A remote change a reload skipped because the load was still reading;
    /// replayed once the load is done.
    missed_remote: bool,
    /// True while a save is writing/re-reading; a remote-applied reload must
    /// not interleave its own load with it (the save already returns the
    /// fresh aggregate + base).
    saving: bool,
    /// Bumped whenever a save or a newer remote reload starts, so an older
    /// in-flight reload's result is discarded instead of clobbering fresher
    /// state (the classic stale-read-completes-last race).
    reload_epoch: u64,
    /// True while this screen shows the practice but waits for a save of an
    /// earlier screen on it to land (see `catch_up`).
    catching_up: bool,
    catch_up: Option<CatchUp>,

    /// 0..1 while a stem separation runs; `None` when idle.
    separation_progress: Option<f64>,
    separation_cell: Option<Arc<ProgressCell>>,
    /// Whether the separation engine is usable; gates "Separate".
    separation_available: bool,
    /// Set once the separated backing is being saved: too late to cancel.
    separation_committing: bool,
    /// Cancel was pressed during the current separation.
    separation_cancelled: bool,

    selected_track_id: Option<String>,
    /// The selected clip within the selected lane (for per-clip actions
    /// like delete). `None` when the lane is selected via its header.
    selected_clip_id: Option<String>,
    /// The bus whose strip is selected for FX editing in the inspector.
    selected_bus_id: Option<String>,
    /// Bumped by every selection the user makes (even one re-picking what is
    /// selected): a restore of a selection taken before an await stands down
    /// once the user picked something since.
    user_selections: u64,
    /// Set when switching the backing lane to an empty slot moved a
    /// selection on it to `movedTo` (the slot has no lane to select):
    /// switching straight back puts it on the backing again, rather than
    /// losing it to a glance.
    backing_detour: Option<Option<String>>,
    /// What was selected before `create_empty_vocal_take` selected its take.
    selection_before_empty_take: Option<SelectionBefore>,

    lyrics_open: bool,
    inspector_open: bool,
    right_tab: RightTab,
    lyrics_mode: LyricsMode,
    lyrics_edit_session: u64,
    lyrics_draft: Option<String>,
    lyrics_focus: bool,
    /// The tap-to-time cursor and the lyrics it was placed on (see
    /// `lyrics.rs`). Read through a getter that re-seeds it, as Dart's did:
    /// interior mutability keeps that getter `&self`.
    sync_index: Cell<usize>,
    sync_words: RefCell<Option<Vec<LyricLine>>>,
    /// The earliest line stamped (or 0 for a cleared or new text) since the
    /// lyrics were last saved with nothing newer behind them; `None` when
    /// nothing is pending.
    unsaved_from: Option<usize>,
    /// Bumped by every revert to the disk's model.
    reverts: u64,

    /// Takes with a clip moved live and not yet committed.
    moving_live: HashSet<String>,
    /// What rides the clip dragged now, from its first live move to the
    /// commit.
    clip_riders: Option<ClipRiders>,
    /// Where a split or merge moved a note or technique marker, by its old
    /// (take, id): both give what they move to another take a fresh id. A
    /// note editor or marker menu opened before such an edit landed still
    /// names the old pair, and its edit found nothing there and was lost
    /// without a word. An edit whose save fails drops its entries again.
    moved_marks: HashMap<(String, String), (String, String)>,

    save_queue: VecDeque<QueuedSave>,
    running_save: Option<RunningSave>,
    /// A save was queued with none running: it starts as the call ends.
    save_kick: bool,
    pending: HashMap<u64, persist::Pending>,
}

impl Core {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn notify(&mut self) {
        if !self.disposed {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// One-shot user feedback (errors, guards, confirmations), shown by the
    /// view as a transient toast. Dropped once the screen is gone.
    fn toast(&mut self, message: &str) {
        if !self.disposed {
            self.out.push(Effect::Toast(message.to_string()));
        }
    }

    fn reply(&mut self, reply: Reply) {
        self.out.push(Effect::Reply(reply));
    }

    fn job(
        &mut self,
        kind: JobKind,
        pending: persist::Pending,
        work: impl FnOnce() -> jobs::Output + Send + 'static,
    ) {
        let id = self.id();
        self.pending.insert(id, pending);
        self.out.push(Effect::Run(Job::new(id, kind, work)));
    }

    fn push_analysis(&mut self, effects: Vec<AnalysisEffect>) {
        for e in effects {
            match e {
                AnalysisEffect::Run(job) => self.out.push(Effect::Analysis(job)),
                AnalysisEffect::RequestMicPermission => {
                    self.out.push(Effect::AnalysisMicPermission)
                }
                AnalysisEffect::Toast(m) => self.toast(&m),
                AnalysisEffect::LaneHealRetry { after } => {
                    self.out.push(Effect::LaneHealRetry { after })
                }
            }
        }
    }

    /// `_practice = next`: a new identity, as every Dart assignment was.
    fn set_practice(&mut self, practice: SongPractice) {
        self.practice = Some(Arc::new(practice));
    }

    fn is_current(&self, sent: &Arc<SongPractice>) -> bool {
        self.practice.as_ref().is_some_and(|p| Arc::ptr_eq(p, sent))
    }

    fn track_by_id(&self, track_id: &str) -> Option<Arc<AudioTrack>> {
        let p = self.practice.as_deref()?;
        if let Some(a) = p.accompaniment.as_ref().filter(|a| a.id == track_id) {
            return Some(Arc::clone(a));
        }
        if let Some(r) = p.reference.as_ref().filter(|r| r.id == track_id) {
            return Some(Arc::clone(r));
        }
        p.vocal_tracks.iter().find(|t| t.id == track_id).cloned()
    }

    fn update_track(&mut self, track_id: &str, update: &dyn Fn(&AudioTrack) -> AudioTrack) {
        let Some(p) = self.practice.as_deref() else {
            return;
        };
        let mut next = p.clone();
        if let Some(a) = p.accompaniment.as_ref().filter(|a| a.id == track_id) {
            next.accompaniment = Some(Arc::new(update(a)));
        } else if let Some(r) = p.reference.as_ref().filter(|r| r.id == track_id) {
            next.reference = Some(Arc::new(update(r)));
        } else {
            next.vocal_tracks = p
                .vocal_tracks
                .iter()
                .map(|t| {
                    if t.id == track_id {
                        Arc::new(update(t))
                    } else {
                        Arc::clone(t)
                    }
                })
                .collect();
        }
        self.set_practice(next);
    }

    /// Whether any lane of the model on screen, in any interpretation,
    /// plays `path`.
    fn holds_audio(&self, path: &str) -> bool {
        self.practice
            .as_deref()
            .is_some_and(|p| p.references_audio(path))
    }

    fn all_tracks(&self) -> Vec<Arc<AudioTrack>> {
        let Some(p) = self.practice.as_deref() else {
            return Vec::new();
        };
        let mut out: Vec<Arc<AudioTrack>> = Vec::new();
        out.extend(p.accompaniment.iter().cloned());
        out.extend(p.reference.iter().cloned());
        // Only the active interpretation's takes are loaded into
        // playback/mixing.
        out.extend(p.active_vocal_tracks());
        out
    }

    fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        self.track_by_id(self.selected_track_id.as_deref()?)
    }

    /// `SongPractice::next_take_number` — the numbering shared with boot
    /// recovery, so a "Take N (recovered)" counts here and vice versa.
    fn next_take_number(&self) -> i64 {
        self.practice.as_deref().map_or(1, |p| p.next_take_number())
    }

    /// Appends a vocal take of `path` to `version_id`, or to the
    /// interpretation on screen when that is `None` or no longer exists.
    fn append_vocal(
        &mut self,
        path: &str,
        display_name: &str,
        analysis: &WaveformResult,
        start_ms: i64,
        version_id: Option<&str>,
    ) -> String {
        let Some(practice) = self.practice.as_deref() else {
            return String::new();
        };
        let target = match version_id {
            Some(v) if practice.versions.iter().any(|x| x.id == v) => Some(v.to_string()),
            _ => practice.active_version().map(|v| v.id.clone()),
        };
        let mut track = AudioTrack::new(
            new_id(),
            AudioTrackKind::Vocal,
            display_name,
            DateTime::now(),
        )
        .with_audio(path, start_ms, analysis.duration_ms, analysis.peaks.clone());
        // Resolved, like create_empty_vocal_take: a recording or import must
        // land in an interpretation that is shown.
        track.version_id = target;
        let id = track.id.clone();
        let mut next = practice.clone();
        next.vocal_tracks.push(Arc::new(track));
        self.set_practice(next);
        id
    }

    /// Selects the lane a finished recording landed on and, when `clip_id`
    /// is `Some`, the punched-in clip itself. A `None` clip (new-take case)
    /// leaves the clip selection untouched, matching the pre-controller flow.
    fn select_recorded_clip(&mut self, track_id: &str, clip_id: Option<&str>) {
        self.selected_track_id = Some(track_id.to_string());
        if let Some(clip) = clip_id {
            self.selected_clip_id = Some(clip.to_string());
        }
        // Mutually exclusive with a bus (see select_bus): a bus still
        // selected from before the take kept owning the inspector, beside a
        // second highlighted lane.
        self.selected_bus_id = None;
    }

    /// Selects a just-created/duplicated bus. Bus and track selection are
    /// mutually exclusive (the inspector owns one or the other) — the
    /// track/clip selection is cleared so two lanes never highlight at once.
    fn select_bus_after_mutation(&mut self, bus_id: &str) {
        self.selected_bus_id = Some(bus_id.to_string());
        self.selected_track_id = None;
        self.selected_clip_id = None;
    }

    /// Defaults the timeline length to fit the content (plus a little tail)
    /// on first open, and grows it — tail included — if the content has
    /// outgrown it, but never shrinks it, so moving a clip can't rescale the
    /// timeline. Grown to the content alone, a new practice (opened on the
    /// 20 s placeholder) ended exactly where its first import did, and no
    /// clip could be dragged to end past the backing.
    fn ensure_timeline(&mut self) {
        let Some(practice) = self.practice.as_deref() else {
            return;
        };
        let content = practice.content_end_ms();
        let with_tail = content + (content * 15 / 100).clamp(2000, 30000);
        let mut dur = practice.timeline_duration_ms;
        if dur <= 0 {
            dur = if content <= 0 { 20000 } else { with_tail };
        } else if dur < content {
            dur = with_tail;
        }
        if dur != practice.timeline_duration_ms {
            let next = SongPractice {
                timeline_duration_ms: dur,
                ..practice.clone()
            };
            self.set_practice(next);
        }
    }

    /// Keeps the selection on a lane that exists and is on screen. Returns
    /// whether it moved the selected take, so a caller can fetch the
    /// analysis the dock and inspector show for it (they only compute when
    /// asked).
    fn ensure_selection(&mut self, play_reference: bool) -> bool {
        let Some(practice) = self.practice.clone() else {
            return false;
        };
        // A selected bus that is gone (a failed create reverted, another
        // device deleted it) selected nothing at all, for good: an empty
        // inspector and no lane highlighted.
        if self.selected_bus_id.is_some()
            && practice
                .bus_by_id(self.selected_bus_id.as_deref())
                .is_none()
        {
            self.selected_bus_id = None;
        }
        // A selected bus owns the inspector; track and bus selection are
        // mutually exclusive, so don't force a track back in (that would
        // highlight two lanes at once after creating/duplicating a bus).
        if self.selected_bus_id.is_some() {
            return false;
        }
        // The backing lane shows one backing at a time: the hidden one was a
        // candidate too, and a practice holding only the original reopened
        // with its (silent, hidden) lane selected under the inspector.
        let backing = if play_reference {
            practice.reference.as_ref()
        } else {
            practice.accompaniment.as_ref()
        };
        let mut ids: Vec<String> = Vec::new();
        if let Some(b) = backing {
            ids.push(b.id.clone());
        }
        ids.extend(practice.active_vocal_tracks().iter().map(|t| t.id.clone()));
        if let Some(sel) = self.selected_track_id.clone() {
            if ids.contains(&sel) {
                if let Some(clip) = self.selected_clip_id.clone() {
                    if self
                        .track_by_id(&sel)
                        .and_then(|t| t.clip_by_id(&clip).cloned())
                        .is_none()
                    {
                        self.selected_clip_id = None;
                    }
                }
                return false;
            }
        }
        let next = ids.first().cloned();
        let moved = next != self.selected_track_id;
        self.selected_track_id = next;
        self.selected_clip_id = None;
        moved
    }

    /// Keeps the busy gate's landing listing in step with whoever holds the
    /// gate (see `gate_landing`).
    fn sync_gate_landing(&mut self) {
        if self.gate.busy() {
            if self.gate_landing.is_none() && !self.gate_unlisted {
                self.gate_landing = Some(self.finalizer.begin_landing(&self.practice_id));
            }
        } else {
            self.gate_landing = None;
            self.gate_unlisted = false;
        }
    }

    /// Takes the busy gate for a guarded edit of the view model's own (Dart's
    /// `_busyGate.run`): `false` when an action already holds it.
    fn take_gate(&mut self) -> bool {
        let taken = self.gate.run(|| Ok(())).is_some();
        if taken {
            self.sync_gate_landing();
            self.notify();
        }
        taken
    }

    /// Hands the gate back as a guarded edit ends: a failure is kept as the
    /// gate's error and said (Dart's `onError`).
    fn finish_gate(&mut self, outcome: Result<(), Failure>) {
        let error = self.gate.finish(
            outcome
                .as_ref()
                .map(|_| ())
                .map_err(|f| ActionError::Failed(f.raw())),
        );
        self.sync_gate_landing();
        self.notify();
        if error.is_some() {
            if let Err(f) = outcome {
                self.toast(&f.friendly());
            }
        }
    }
}

/// `recording.isRecording || isStartingTake || isStopping`.
fn take_in_flight(recording: &RecordingController) -> bool {
    recording.is_recording() || recording.is_starting_take() || recording.is_stopping()
}

/// What a take in flight the user has nothing left to stop makes them wait
/// for; `None` while there is a take to stop. Stop pressed while REC still
/// armed (or a start that failed, dropping its empty take) read "Stop
/// recording first", and pressing Stop again did nothing.
fn take_wait(recording: &RecordingController) -> Option<&'static str> {
    if recording.is_recording() {
        return None;
    }
    if recording.is_stopping() {
        return Some("Wait for the take to finish saving.");
    }
    if recording.is_cancelling_start() {
        return Some("Wait for the recording to stop.");
    }
    None
}

/// Something a mixer action returns: nothing, or the save it asks for.
pub trait MixerOutcome {
    fn save_request(self) -> Option<SaveRequest>;
}

impl MixerOutcome for () {
    fn save_request(self) -> Option<SaveRequest> {
        None
    }
}

impl MixerOutcome for SaveRequest {
    fn save_request(self) -> Option<SaveRequest> {
        Some(self)
    }
}

impl MixerOutcome for Option<SaveRequest> {
    fn save_request(self) -> Option<SaveRequest> {
        self
    }
}

/// `create_bus` / `duplicate_bus`: the new bus (selected by the mixer) and
/// its save.
impl MixerOutcome for Option<(String, SaveRequest)> {
    fn save_request(self) -> Option<SaveRequest> {
        self.map(|(_, save)| save)
    }
}

/// Something an analysis action returns: nothing, or its effects.
pub trait AnalysisOutcome {
    fn effects(self) -> Vec<AnalysisEffect>;
}

impl AnalysisOutcome for () {
    fn effects(self) -> Vec<AnalysisEffect> {
        Vec::new()
    }
}

impl AnalysisOutcome for Vec<AnalysisEffect> {
    fn effects(self) -> Vec<AnalysisEffect> {
        self
    }
}

// --- The view model -------------------------------------------------------------

/// ViewModel for the multi-track detail view (see the module docs).
pub struct SongPracticeDetailViewModel {
    c: Core,
    playback: PlaybackController,
    mixer: MixerController,
    analysis: AnalysisCache,
    recording: RecordingController,
    /// Interpretations. (Named `versions` — `interpretations` is the list.)
    versions: InterpretationsController,
}

impl fmt::Debug for SongPracticeDetailViewModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SongPracticeDetailViewModel")
            .field("practice_id", &self.c.practice_id)
            .field("loaded", &self.c.practice.is_some())
            .field("busy", &self.c.gate.busy())
            .finish_non_exhaustive()
    }
}

impl SongPracticeDetailViewModel {
    /// The screen's view model over `deps`, and the work it starts at once:
    /// the load (Dart's `load = Command0(_load)..execute()`) and the
    /// separation engine's availability probe. Remote-applied events reach it
    /// through [`SongPracticeDetailViewModel::on_remote_applied`].
    pub fn new(
        deps: Deps,
        practice_id: impl Into<String>,
        options: Options,
    ) -> (Self, Vec<Effect>) {
        let practice_id = practice_id.into();
        let Options {
            engine,
            monitor,
            edits,
            finalizer,
            save_location_picker,
            probe_duration,
            analyze,
            mic,
            recording: mut recording_options,
            analysis: analysis_options,
            recording_settings,
        } = options;
        let engine = engine.unwrap_or_else(|| EngineHandle::Graph(create_playback_engine()));
        let finalizer = finalizer.unwrap_or_else(RecordingFinalizer::instance);
        let monitor = monitor.unwrap_or_else(|| Arc::new(MonitorService::default()));
        let edits: Arc<dyn AudioEdits> =
            edits.unwrap_or_else(|| Arc::new(AudioEditService::new(Arc::clone(&deps.audio_files))));
        let analyze: TakeAnalyzer = analyze.unwrap_or_else(|| {
            Arc::new(|path: &str| {
                waveform_service::analyze(path, DEFAULT_TARGET_BUCKETS).map_err(|e| e.0)
            })
        });
        let picker: SaveLocationPicker = save_location_picker.unwrap_or_else(|| {
            Arc::new(|suggested: &str| {
                Ok(vocara_services::file_panels::FileDialog::new()
                    .set_file_name(suggested)
                    .save_file()
                    .map(|path| path.to_string_lossy().into_owned()))
            })
        });
        let exporter = Arc::new(
            AudioExporter::new(Arc::clone(&deps.audio_files), move |name: &str| {
                picker(name)
            })
            .with_finalizer(Arc::clone(&finalizer)),
        );
        let probe_duration: ProbeDuration =
            probe_duration.unwrap_or_else(|| Arc::new(probe_media_duration));
        // Adopt the persisted output-device choice and let the global
        // settings panel drive this engine live. The native engine is the
        // only one with device support.
        if let Some(g) = engine.graph() {
            deps.app_settings.attach_engine(Arc::clone(g));
        }
        let transport = engine.shared();
        let graph = engine.graph().cloned();
        let playback = PlaybackController::new(Arc::clone(&transport));
        let mixer = MixerController::new(engine.clone());
        let analysis = AnalysisCache::with_options(
            Arc::clone(&deps.spectrograms),
            Arc::clone(&monitor),
            graph.clone(),
            analysis_options,
        );
        recording_options.mic = mic.or(recording_options.mic);
        recording_options.analyze = Some(Arc::clone(&analyze));
        let settings: Arc<dyn RecordingSettings> = recording_settings
            .unwrap_or_else(|| Arc::clone(&deps.app_settings) as Arc<dyn RecordingSettings>);
        let recording = RecordingController::with_options(
            Arc::clone(&transport),
            graph,
            Arc::clone(&deps.audio_files),
            settings,
            practice_id.clone(),
            Arc::clone(&finalizer),
            recording_options,
        );
        let versions = InterpretationsController::new(transport, Arc::clone(&deps.audio_files));
        let c = Core {
            repository: deps.repository,
            technique_repository: deps.technique_repository,
            audio_files: deps.audio_files,
            edits,
            separation: deps.separation,
            app_settings: deps.app_settings,
            exporter,
            probe_duration,
            analyze,
            finalizer,
            engine,
            monitor,
            practice_id,
            revision: 0,
            disposed: false,
            out: Vec::new(),
            next_id: 0,
            gate: BusyGate::new(),
            gate_landing: None,
            gate_unlisted: false,
            practice: None,
            persisted: None,
            base_hashes: IndexMap::new(),
            load_running: false,
            load_error: None,
            missed_remote: false,
            saving: false,
            reload_epoch: 0,
            catching_up: false,
            catch_up: None,
            separation_progress: None,
            separation_cell: None,
            separation_available: false,
            separation_committing: false,
            separation_cancelled: false,
            selected_track_id: None,
            selected_clip_id: None,
            selected_bus_id: None,
            user_selections: 0,
            backing_detour: None,
            selection_before_empty_take: None,
            lyrics_open: true,
            inspector_open: true,
            right_tab: RightTab::Lyrics,
            lyrics_mode: LyricsMode::Read,
            lyrics_edit_session: 0,
            lyrics_draft: None,
            lyrics_focus: false,
            sync_index: Cell::new(0),
            sync_words: RefCell::new(None),
            unsaved_from: None,
            reverts: 0,
            moving_live: HashSet::new(),
            clip_riders: None,
            moved_marks: HashMap::new(),
            save_queue: VecDeque::new(),
            running_save: None,
            save_kick: false,
            pending: HashMap::new(),
        };
        let mut vm = SongPracticeDetailViewModel {
            c,
            playback,
            mixer,
            analysis,
            recording,
            versions,
        };
        vm.start_load();
        // Probe the separation engine once so the UI can hide "Separate"
        // when no engine is present (e.g. the dev sidecar isn't installed).
        let separation = Arc::clone(&vm.c.separation);
        vm.c.job(JobKind::Probe, persist::Pending::Probe, move || {
            jobs::Output::Available(separation.is_available())
        });
        let effects = vm.finish();
        (vm, effects)
    }

    /// The effects gathered so far.
    /// The effects gathered so far, once the save a call queued with none
    /// running has started: Dart chained it on a completed tail, so its body
    /// ran as a microtask — after the call that queued it, reading the model
    /// as that call left it.
    fn finish(&mut self) -> Vec<Effect> {
        while std::mem::take(&mut self.c.save_kick) {
            if self.c.running_save.is_none() {
                self.start_next_save();
            }
        }
        std::mem::take(&mut self.c.out)
    }

    fn next_call(&mut self) -> Call {
        Call(self.c.id())
    }

    fn started(&mut self, call: Call) -> Started {
        Started {
            call,
            effects: self.finish(),
        }
    }

    // --- Notifications and state ------------------------------------------------

    /// Bumped on every notification of the view model and of each of its
    /// controllers (Dart chained theirs into its own); the views compare it
    /// between builds. Never moves after [`SongPracticeDetailViewModel::dispose`].
    pub fn revision(&self) -> u64 {
        self.c
            .revision
            .wrapping_add(self.playback.revision())
            .wrapping_add(self.mixer.revision())
            .wrapping_add(self.analysis.revision())
            .wrapping_add(self.recording.revision())
            .wrapping_add(self.versions.revision())
    }

    pub fn practice_id(&self) -> &str {
        &self.c.practice_id
    }

    pub fn practice(&self) -> Option<&SongPractice> {
        self.c.practice.as_deref()
    }

    /// The global audio settings (devices, record/monitor gains) — used by
    /// the mixer's mic strip.
    pub fn app_settings(&self) -> &Arc<AppSettingsService> {
        &self.c.app_settings
    }

    pub fn monitor(&self) -> &MonitorService {
        &self.c.monitor
    }

    /// Whether the load (Dart's `load.running`) is still in flight.
    pub fn load_running(&self) -> bool {
        self.c.load_running
    }

    /// Why the load failed, once it has (Dart's `load.error`).
    pub fn load_error(&self) -> Option<&str> {
        self.c.load_error.as_deref()
    }

    /// The one busy/error latch (spinner + structural-edit mutex).
    pub fn busy(&self) -> bool {
        self.c.gate.busy()
    }

    pub fn error_message(&self) -> Option<&str> {
        self.c.gate.error_message()
    }

    /// 0..1 while a stem separation runs; `None` when idle. Drives a
    /// progress bar — separation can take minutes, unlike the ~1s import
    /// busy spinner.
    pub fn separation_progress(&self) -> Option<f64> {
        self.c.separation_progress
    }

    pub fn is_separating(&self) -> bool {
        self.c.separation_progress.is_some()
    }

    /// Whether the separation engine is usable; gates the "Separate" action.
    pub fn separation_available(&self) -> bool {
        self.c.separation_available
    }

    /// Whether Cancel can still stop the separation.
    pub fn can_cancel_separation(&self) -> bool {
        self.is_separating() && !self.c.separation_committing
    }

    /// True while this screen shows the practice but waits for a save of an
    /// earlier screen on it to land. Takes no edits meanwhile: the screen
    /// dims its workspace, and the busy gate refuses recording, imports and
    /// structural edits.
    pub fn catching_up(&self) -> bool {
        self.c.catching_up
    }

    /// Whether anything started here is still in flight: a job not answered
    /// yet, a save queued or running, a guarded edit, a stopped take
    /// landing. The page keeps the view model (and runs its effects) until
    /// this reads false.
    pub fn has_work_in_flight(&self) -> bool {
        !self.c.pending.is_empty()
            || self.c.running_save.is_some()
            || !self.c.save_queue.is_empty()
            || self.c.save_kick
            || self.c.gate.busy()
            || self.recording.is_stopping()
            || self.recording.is_start_in_flight()
    }

    // --- The controllers --------------------------------------------------------

    pub fn playback(&self) -> &PlaybackController {
        &self.playback
    }

    pub fn mixer(&self) -> &MixerController {
        &self.mixer
    }

    pub fn analysis(&self) -> &AnalysisCache {
        &self.analysis
    }

    pub fn recording(&self) -> &RecordingController {
        &self.recording
    }

    pub fn versions(&self) -> &InterpretationsController {
        &self.versions
    }

    fn record_target(&mut self) -> Option<String> {
        if !self.recording.is_recording() {
            return None;
        }
        let host = RecordingView {
            c: &mut self.c,
            position: self.playback.position(),
            live_monitor: self.analysis.live_monitor(),
        };
        self.recording.record_target_id(&host)
    }

    /// Reads the transport through its host.
    pub fn with_playback<R>(
        &self,
        f: impl FnOnce(&PlaybackController, &dyn PlaybackHost) -> R,
    ) -> R {
        let host = PlaybackView {
            c: &self.c,
            recording: &self.recording,
        };
        f(&self.playback, &host)
    }

    /// Reads the mixer through its host.
    pub fn with_mixer<R>(&self, f: impl FnOnce(&MixerController, &dyn MixerHost) -> R) -> R {
        // The frozen record target is all the mixer reads of it, and only
        // while a take records.
        let target = if self.recording.is_recording() {
            self.recording.record_target_id(&ReadRecordingHost(&self.c))
        } else {
            None
        };
        let host = MixerView {
            c: &self.c,
            recording: &self.recording,
            target,
            position: self.playback.position(),
        };
        f(&self.mixer, &host)
    }

    /// Reads the analysis cache through its host.
    pub fn with_analysis<R>(&self, f: impl FnOnce(&AnalysisCache, &dyn AnalysisHost) -> R) -> R {
        let host = AnalysisView {
            c: &self.c,
            playback: &self.playback,
            recording: &self.recording,
        };
        f(&self.analysis, &host)
    }

    /// The take REC will record into (the recording controller's
    /// `recordTargetId`).
    pub fn record_target_id(&self) -> Option<String> {
        self.recording.record_target_id(&ReadRecordingHost(&self.c))
    }

    /// Runs a transport action.
    #[must_use]
    pub fn playback_do(
        &mut self,
        f: impl FnOnce(&mut PlaybackController, &mut dyn PlaybackHost),
    ) -> Vec<Effect> {
        let mut host = PlaybackView {
            c: &mut self.c,
            recording: &self.recording,
        };
        f(&mut self.playback, &mut host);
        self.finish()
    }

    /// Runs a mixer action; the save it asks for joins the save chain. Not
    /// for the backing A/B switch: that is
    /// [`SongPracticeDetailViewModel::set_play_reference`].
    #[must_use]
    pub fn mixer_do<O: MixerOutcome>(
        &mut self,
        f: impl FnOnce(&mut MixerController, &mut dyn MixerHost) -> O,
    ) -> Vec<Effect> {
        let save = self.mixer_call(f).save_request();
        if save.is_some() {
            self.save(SaveThen::Nothing);
        }
        self.finish()
    }

    fn mixer_call<R>(
        &mut self,
        f: impl FnOnce(&mut MixerController, &mut dyn MixerHost) -> R,
    ) -> R {
        let target = self.record_target();
        let mut host = MixerView {
            c: &mut self.c,
            recording: &self.recording,
            target,
            position: self.playback.position(),
        };
        f(&mut self.mixer, &mut host)
    }

    /// Runs an analysis action.
    #[must_use]
    pub fn analysis_do<O: AnalysisOutcome>(
        &mut self,
        f: impl FnOnce(&mut AnalysisCache, &dyn AnalysisHost) -> O,
    ) -> Vec<Effect> {
        self.analysis_call(|a, h| f(a, h).effects());
        self.finish()
    }

    fn analysis_call(
        &mut self,
        f: impl FnOnce(&mut AnalysisCache, &dyn AnalysisHost) -> Vec<AnalysisEffect>,
    ) {
        let host = AnalysisView {
            c: &self.c,
            playback: &self.playback,
            recording: &self.recording,
        };
        let effects = f(&mut self.analysis, &host);
        self.c.push_analysis(effects);
    }

    /// Runs a recording action (REC, the input monitor toggle, ...).
    #[must_use]
    pub fn recording_do(
        &mut self,
        f: impl FnOnce(&mut RecordingController, &mut dyn RecordingHost) -> Vec<RecordingEffect>,
    ) -> Vec<Effect> {
        self.recording_call(f);
        self.finish()
    }

    fn recording_call(
        &mut self,
        f: impl FnOnce(&mut RecordingController, &mut dyn RecordingHost) -> Vec<RecordingEffect>,
    ) {
        let mut host = RecordingView {
            c: &mut self.c,
            position: self.playback.position(),
            live_monitor: self.analysis.live_monitor(),
        };
        let effects = f(&mut self.recording, &mut host);
        self.c.sync_gate_landing();
        self.apply_recording(effects);
    }

    /// Runs an interpretations action (switch, create, rename, delete).
    #[must_use]
    pub fn versions_do(
        &mut self,
        f: impl FnOnce(
            &mut InterpretationsController,
            &mut dyn InterpretationsHost,
        ) -> Vec<InterpretationsEffect>,
    ) -> Vec<Effect> {
        self.versions_apply(f);
        self.finish()
    }

    fn versions_call<R>(
        &mut self,
        f: impl FnOnce(&mut InterpretationsController, &mut dyn InterpretationsHost) -> R,
    ) -> R {
        let play_reference = self.mixer.play_reference();
        let mut host = VersionsView {
            c: &mut self.c,
            analysis: &mut self.analysis,
            playback: &self.playback,
            recording: &self.recording,
            play_reference,
        };
        let r = f(&mut self.versions, &mut host);
        self.c.sync_gate_landing();
        r
    }

    fn versions_apply(
        &mut self,
        f: impl FnOnce(
            &mut InterpretationsController,
            &mut dyn InterpretationsHost,
        ) -> Vec<InterpretationsEffect>,
    ) {
        let effects = self.versions_call(f);
        self.apply_versions(effects);
    }

    /// Applies the recording controller's effects in order (see its module
    /// doc): the sibling calls in place, the rest as work.
    fn apply_recording(&mut self, effects: Vec<RecordingEffect>) {
        for effect in effects {
            match effect {
                RecordingEffect::StopLiveView => {
                    self.analysis_call(|a, h| a.set_live_monitor(false, h));
                }
                RecordingEffect::Start(job) => self.c.out.push(Effect::StartRecording(job)),
                RecordingEffect::CreateEmptyTake(op) => {
                    self.create_empty_vocal_take(edits::EmptyTakeFor::Recording(op));
                }
                RecordingEffect::DiscardEmptyTake(op, id) => self.discard_empty_take(op, id),
                RecordingEffect::Resync(op) => {
                    self.resync(persist::ResyncThen::Recording(op));
                }
                RecordingEffect::SetRecordingPosition(at) => {
                    let host = PlaybackView {
                        c: &self.c,
                        recording: &self.recording,
                    };
                    self.playback.set_recording_position(&host, at);
                }
                RecordingEffect::EnsureLoopRange => self.ensure_loop_range(),
                RecordingEffect::Run(job) => self.c.out.push(Effect::RecordingWork(job)),
                RecordingEffect::Finalize(op, run) => {
                    self.c.out.push(Effect::FinalizeTake(op, run));
                }
                RecordingEffect::RetryFinalize { op, after } => {
                    self.c.out.push(Effect::FinalizeRetry { op, after });
                }
                RecordingEffect::Save(op) => self.save(SaveThen::Recording(op)),
                RecordingEffect::TrackAudioChanged(id) => {
                    self.analysis_call(|a, h| a.on_track_audio_changed(&id, h));
                }
                RecordingEffect::GateError(e) => {
                    let friendly = match e {
                        // The finalize's only failure is its analysis, whose
                        // message is the user's to read.
                        ActionError::Failed(message) => message,
                        ActionError::Panicked(_) => jobs::SOMETHING_WENT_WRONG.to_string(),
                    };
                    self.c.toast(&friendly);
                }
                RecordingEffect::RequestMicPermission => {
                    self.c.out.push(Effect::MonitorMicPermission);
                }
                RecordingEffect::Leave => self.c.out.push(Effect::Leave),
            }
        }
        self.c.sync_gate_landing();
    }

    /// Applies the interpretations controller's effects in order.
    fn apply_versions(&mut self, effects: Vec<InterpretationsEffect>) {
        for effect in effects {
            match effect {
                InterpretationsEffect::Resync(op) => self.resync(persist::ResyncThen::Versions(op)),
                InterpretationsEffect::Save(op) => self.save(SaveThen::Versions(op)),
                InterpretationsEffect::EnsureSelectedAnalysis => {
                    self.analysis_call(|a, h| a.ensure_selected_spectrogram(h));
                }
                InterpretationsEffect::CleanUp(op, run) => {
                    self.c.out.push(Effect::InterpretationsCleanUp(op, run));
                }
            }
        }
    }

    fn ensure_loop_range(&mut self) {
        let host = PlaybackView {
            c: &mut self.c,
            recording: &self.recording,
        };
        self.playback.ensure_loop_range(&host);
    }

    /// Whether the dock or the left inspector shows the selected take's
    /// analysis (they only compute when asked).
    fn shows_selected_analysis(&self) -> bool {
        (self.analysis.dock_open() && self.analysis.dock_shows_take()) || self.c.inspector_open
    }

    fn ensure_selected_spectrogram(&mut self) {
        self.analysis_call(|a, h| a.ensure_selected_spectrogram(h));
    }

    /// [`Core::ensure_selection`], fetching the newly selected take's
    /// analysis when the dock or the inspector shows it (as `select_track`
    /// does): a take deleted from under the selection left an open dock
    /// blank on its replacement.
    fn ensure_selection_analyzed(&mut self) {
        let play_reference = self.mixer.play_reference();
        if self.c.ensure_selection(play_reference) && self.shows_selected_analysis() {
            self.ensure_selected_spectrogram();
        }
    }

    /// Drops `path`'s analysis once no lane plays it any more. Two lanes over
    /// identical bytes share one pooled path, so deleting, splitting or
    /// merging one of them evicted the other's analysis too — and disowned a
    /// compute still in flight for it, leaving that lane's dock with nothing
    /// to show until it was selected again. Its bytes are unchanged, so it
    /// stays valid.
    fn evict_unused(&mut self, path: &str) {
        if self.c.holds_audio(path) {
            return;
        }
        self.analysis.evict_analysis(path, false);
    }

    // --- Answers --------------------------------------------------------------------

    /// An analysis job's result.
    #[must_use]
    pub fn analysis_job_done(&mut self, done: analysis_cache::JobDone) -> Vec<Effect> {
        self.analysis_call(|a, h| a.job_done(done, h));
        self.finish()
    }

    /// The live view's microphone permission answer.
    #[must_use]
    pub fn analysis_mic_permission_answered(&mut self, granted: bool) -> Vec<Effect> {
        self.analysis_call(|a, h| a.mic_permission_answered(granted, h));
        self.finish()
    }

    /// The lane-heal retry's timer fired.
    pub fn lane_heal_retry_due(&mut self) {
        self.analysis.lane_heal_retry_due();
    }

    /// A start reached its take setup (on the UI thread).
    #[must_use]
    pub fn take_setup(&mut self, request: TakeSetupRequest) -> Vec<Effect> {
        self.recording_call(|r, h| r.take_setup(request, h));
        self.finish()
    }

    /// The mic's start came back.
    #[must_use]
    pub fn start_done(&mut self, done: StartDone) -> Vec<Effect> {
        self.recording_call(|r, h| r.start_done(done, h));
        self.finish()
    }

    /// A recording file job ran.
    #[must_use]
    pub fn recording_job_done(&mut self, done: recording_controller::JobDone) -> Vec<Effect> {
        self.recording_call(|r, h| r.job_done(done, h));
        self.finish()
    }

    /// The finalize's guarded read of the take landed.
    #[must_use]
    pub fn finalize_ran(&mut self, op: RecordingOp, outcome: BusyOutcome<String>) -> Vec<Effect> {
        self.recording_call(|r, h| r.finalize_ran(op, outcome, h));
        self.finish()
    }

    /// The finalize's wait for the busy gate is over.
    #[must_use]
    pub fn finalize_retry_due(&mut self, op: RecordingOp) -> Vec<Effect> {
        self.recording_call(|r, h| r.finalize_retry_due(op, h));
        self.finish()
    }

    /// The input monitor's microphone permission answer.
    #[must_use]
    pub fn monitor_permission_answered(&mut self, granted: bool) -> Vec<Effect> {
        self.recording_call(|r, h| r.monitor_permission_answered(granted, h));
        self.finish()
    }

    /// An interpretation delete's guarded clean-up landed: the gate is handed
    /// back, its error said.
    #[must_use]
    pub fn interpretations_cleaned_up(
        &mut self,
        op: VersionsOp,
        outcome: BusyOutcome<String>,
    ) -> Vec<Effect> {
        let error = self.versions_call(|v, h| v.cleaned_up(op, outcome, h));
        self.c.notify();
        // The clean-up's failures are file system errors, which Dart's
        // `_friendlyError` said as "Something went wrong.".
        if error.is_some() {
            self.c.toast(jobs::SOMETHING_WENT_WRONG);
        }
        self.finish()
    }

    // --- Ticks ---------------------------------------------------------------------

    /// Delivers the engine's position and playing events (every frame).
    pub fn drain_engine_events(&mut self) -> bool {
        let host = PlaybackView {
            c: &self.c,
            recording: &self.recording,
        };
        self.playback.drain_engine_events(&host)
    }

    /// The live view's 30 ms capture pump.
    pub fn pump_capture(&mut self) {
        self.analysis.pump_capture();
    }

    /// The mic's level tick, while a take is armed.
    #[must_use]
    pub fn mic_tick(&mut self) -> Vec<Effect> {
        let effects = self.recording.mic_tick();
        self.apply_recording(effects);
        self.finish()
    }

    /// The 50 ms monitor meter.
    pub fn monitor_meter_tick(&mut self) {
        self.recording.monitor_meter_tick();
    }

    /// The 1 s device watch.
    #[must_use]
    pub fn device_watch_tick(&mut self) -> Vec<Effect> {
        self.recording_call(|r, h| r.device_watch_tick(h));
        self.finish()
    }

    /// Lets the screen know when a quit asks something of the recording
    /// (see [`RecordingController::set_quit_waker`]).
    pub fn set_quit_waker(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        self.recording.set_quit_waker(wake);
    }

    /// Serves a quit's requests (see
    /// [`RecordingController::serve_quit_requests`]).
    #[must_use]
    pub fn serve_quit_requests(&mut self) -> Vec<Effect> {
        let effects = self.recording.serve_quit_requests();
        self.apply_recording(effects);
        self.finish()
    }

    /// Takes in a running separation's progress (Dart's `onProgress`).
    pub fn separation_progress_tick(&mut self) {
        if self.c.separation_progress.is_none() {
            return;
        }
        if let Some(v) = self.c.separation_cell.as_ref().and_then(|cell| cell.take()) {
            self.c.separation_progress = Some(v);
            self.c.notify();
        }
    }

    // --- Selection ------------------------------------------------------------------

    pub fn selected_track_id(&self) -> Option<&str> {
        self.c.selected_track_id.as_deref()
    }

    pub fn selected_clip_id(&self) -> Option<&str> {
        self.c.selected_clip_id.as_deref()
    }

    pub fn selected_bus_id(&self) -> Option<&str> {
        self.c.selected_bus_id.as_deref()
    }

    pub fn selected_bus(&self) -> Option<&Arc<FxBus>> {
        self.c
            .practice
            .as_deref()?
            .bus_by_id(self.c.selected_bus_id.as_deref())
    }

    pub fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        self.c.selected_track()
    }

    pub fn track_by_id(&self, track_id: &str) -> Option<Arc<AudioTrack>> {
        self.c.track_by_id(track_id)
    }

    fn user_selected(&mut self) {
        self.c.user_selections += 1;
        // A choice of their own ends a backing-lane detour (see
        // set_play_reference).
        self.c.backing_detour = None;
    }

    #[must_use]
    pub fn select_track(&mut self, track_id: &str) -> Vec<Effect> {
        self.select_track_inner(track_id);
        self.finish()
    }

    fn select_track_inner(&mut self, track_id: &str) {
        self.user_selected();
        if self.c.selected_track_id.as_deref() == Some(track_id)
            && self.c.selected_clip_id.is_none()
            && self.c.selected_bus_id.is_none()
        {
            return;
        }
        self.c.selected_track_id = Some(track_id.to_string());
        self.c.selected_clip_id = None;
        self.c.selected_bus_id = None;
        self.c.notify();
        // The dock and the left inspector both read the selected take's
        // analysis.
        if self.shows_selected_analysis() {
            self.ensure_selected_spectrogram();
        }
    }

    /// Selects a specific clip (and its lane), so per-clip actions target it.
    #[must_use]
    pub fn select_clip(&mut self, track_id: &str, clip_id: &str) -> Vec<Effect> {
        self.user_selected();
        if self.c.selected_track_id.as_deref() == Some(track_id)
            && self.c.selected_clip_id.as_deref() == Some(clip_id)
            && self.c.selected_bus_id.is_none()
        {
            return self.finish();
        }
        self.c.selected_track_id = Some(track_id.to_string());
        self.c.selected_clip_id = Some(clip_id.to_string());
        self.c.selected_bus_id = None;
        self.c.notify();
        if self.shows_selected_analysis() {
            self.ensure_selected_spectrogram();
        }
        self.finish()
    }

    /// Selecting a bus takes over the inspector FX panel; selecting a track
    /// clears it.
    pub fn select_bus(&mut self, bus_id: Option<&str>) {
        self.user_selected();
        if self.c.selected_bus_id.as_deref() == bus_id {
            return;
        }
        self.c.selected_bus_id = bus_id.map(str::to_string);
        // Bus and track selection are mutually exclusive (the inspector owns
        // one or the other) — selecting a bus must clear the track/clip
        // selection, or two lanes highlight at once. create_bus/duplicate_bus
        // already do this.
        if bus_id.is_some() {
            self.c.selected_track_id = None;
            self.c.selected_clip_id = None;
        }
        self.c.notify();
    }

    /// Notes pinned to the currently selected track, sorted by time.
    pub fn selected_track_notes(&self) -> Vec<PracticeNote> {
        let Some(track) = self.selected_track() else {
            return Vec::new();
        };
        let mut notes = track.notes.clone();
        notes.sort_by_key(|n| n.position_ms);
        notes
    }

    /// The selected take's technique markers, sorted by position — for the
    /// right panel's Techniques tab.
    pub fn selected_track_techniques(&self) -> Vec<TechniqueMarker> {
        let Some(track) = self.selected_track() else {
            return Vec::new();
        };
        let mut markers = track.technique_markers.clone();
        markers.sort_by_key(|m| m.position_ms);
        markers
    }

    /// All notes across every track, with their owning track, sorted by time.
    pub fn all_notes(&self) -> Vec<NoteEntry> {
        let Some(practice) = self.c.practice.as_deref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut collect = |t: &AudioTrack| {
            for n in &t.notes {
                out.push(NoteEntry {
                    track_id: t.id.clone(),
                    track_name: t.display_name.clone(),
                    note: n.clone(),
                });
            }
        };
        if let Some(acc) = &practice.accompaniment {
            collect(acc);
        }
        // The reference lane too: update_track routes note updates to it, so
        // reference notes (e.g. arriving via sync) must show up here —
        // mirroring SongPractice::note_count / active_note_count.
        if let Some(r) = &practice.reference {
            collect(r);
        }
        for v in practice.active_vocal_tracks() {
            collect(&v);
        }
        out.sort_by_key(|e| e.note.position_ms);
        out
    }

    /// Switches the backing lane between the accompaniment and the original
    /// (see [`MixerController::set_play_reference`]), keeping a selection on
    /// that lane on the backing now shown.
    #[must_use]
    pub fn set_play_reference(&mut self, value: bool) -> Vec<Effect> {
        // The segment already shown, tapped again: nothing changes (a clip
        // selected on the backing stays, a detour stays pending).
        if self.mixer.play_reference() == value {
            return self.finish();
        }
        let detour = self.c.backing_detour.take();
        let before = if self.c.selected_bus_id.is_none() {
            self.c.selected_track_id.clone()
        } else {
            None
        };
        let on_backing = before
            .as_deref()
            .and_then(|b| self.c.track_by_id(b))
            .is_some_and(|t| t.is_backing());
        self.mixer_call(|m, h| m.set_play_reference(h, value));
        let shown = self.active_backing().map(|b| b.id.clone());
        // Still where the detour left it (any pick of the user's own ends
        // it).
        let back = detour.as_ref().is_some_and(|moved_to| {
            self.c.selected_bus_id.is_none()
                && self.c.selected_clip_id.is_none()
                && self.c.selected_track_id == *moved_to
        });
        if let Some(shown) = &shown {
            if on_backing || back {
                self.select_track_inner(shown);
                return self.finish();
            }
        }
        let moved = self.c.selected_track_id.clone();
        self.ensure_selection_analyzed();
        if on_backing && shown.is_none() && self.c.selected_track_id != moved {
            self.c.backing_detour = Some(self.c.selected_track_id.clone());
        }
        self.c.notify();
        self.finish()
    }

    /// The backing track shown and played (the mixer's `activeBacking`).
    pub fn active_backing(&self) -> Option<Arc<AudioTrack>> {
        let p = self.c.practice.as_deref()?;
        if self.mixer.play_reference() {
            p.reference.clone()
        } else {
            p.accompaniment.clone()
        }
    }

    // --- Panels (left inspector / right tabs) ------------------------------------

    pub fn lyrics_open(&self) -> bool {
        self.c.lyrics_open
    }

    pub fn toggle_lyrics_panel(&mut self) {
        self.c.lyrics_open = !self.c.lyrics_open;
        self.c.notify();
    }

    pub fn inspector_open(&self) -> bool {
        self.c.inspector_open
    }

    #[must_use]
    pub fn toggle_inspector_panel(&mut self) -> Vec<Effect> {
        self.c.inspector_open = !self.c.inspector_open;
        self.c.notify();
        // The inspector shows analysis, so opening it must trigger the
        // compute the dock would otherwise be responsible for.
        if self.c.inspector_open {
            self.ensure_selected_spectrogram();
        }
        self.finish()
    }

    pub fn right_tab(&self) -> RightTab {
        self.c.right_tab
    }

    pub fn set_right_tab(&mut self, tab: RightTab) {
        if self.c.right_tab == tab {
            return;
        }
        self.c.right_tab = tab;
        self.c.notify();
    }

    // --- Cross-cutting delegates ----------------------------------------------------

    /// The shared playhead.
    pub fn position(&self) -> Duration {
        self.playback.position()
    }

    /// Moves the backing (see [`PlaybackController::seek`]).
    pub fn seek(&mut self, to: Duration) {
        let host = PlaybackView {
            c: &mut self.c,
            recording: &self.recording,
        };
        self.playback.seek(&host, to);
    }

    // Cross-cutting recording guard: read by the transport bar, the screen's
    // back path and the lyrics import — a genuinely view-model-level
    // condition, not a mixer/playback one.
    pub fn is_recording(&self) -> bool {
        self.recording.is_recording()
    }

    /// A take owns the model's structure from REC's arm until its finalize
    /// has committed. [`SongPracticeDetailViewModel::is_recording`] alone
    /// misses both ends: the arm window (permission, take setup, the mic's
    /// ready handshake) and the stop window (pause, head trim, analysis —
    /// seconds for a long take), during both of which the busy gate is free.
    /// Every structural pre-guard refuses it.
    pub fn take_in_flight(&self) -> bool {
        take_in_flight(&self.recording)
    }

    /// Whether leaving the screen must first end a take: one recording, one
    /// still in REC's arm window, or one a Back is already landing.
    pub fn take_holds_screen(&self) -> bool {
        self.is_recording() || self.recording.is_starting_take() || self.recording.is_leaving()
    }

    /// Cross-cutting stop: invoked from the transport bar.
    #[must_use]
    pub fn stop_recording(&mut self) -> Vec<Effect> {
        let effects = self.recording.stop_recording();
        self.c.sync_gate_landing();
        self.apply_recording(effects);
        self.finish()
    }

    /// The screen's back path: finalize (or cancel) the take in flight before
    /// leaving the route. The page leaves on [`Effect::Leave`]; a Back
    /// pressed while an earlier one is still doing that gets none: only the
    /// first may leave.
    #[must_use]
    pub fn stop_before_leaving(&mut self) -> Vec<Effect> {
        let effects = self.recording.stop_before_leaving();
        self.apply_recording(effects);
        self.finish()
    }

    /// The one shared "persist pending live FX/level edits" commit.
    #[must_use]
    pub fn commit_fx(&mut self) -> Vec<Effect> {
        self.mixer_do(|m, _| m.commit_fx())
    }

    /// Whether a structural edit (a delete, a separation) has to wait, saying
    /// why when it does. Asked before a confirm dialog opens as well: the
    /// dialog asked the user to confirm an edit that was then refused
    /// without a word, the take still there after "Delete".
    #[must_use]
    pub fn refuse_structural_edit(&mut self) -> (bool, Vec<Effect>) {
        let refused = self.refuses_structural_edit();
        (refused, self.finish())
    }

    fn refuses_structural_edit(&mut self) -> bool {
        let why = if let Some(wait) = take_wait(&self.recording) {
            wait
        } else if self.take_in_flight() {
            "Stop recording first."
        } else if self.is_separating() {
            "Wait for the separation to finish."
        } else if self.busy() {
            "Wait for the current edit to finish."
        } else {
            return false;
        };
        self.c.toast(why);
        true
    }

    /// Whether a clip of `track_id` is being moved live. Nothing asks for its
    /// analysis meanwhile (see `AnalysisHost::is_moving_live`): each tick
    /// re-keys a multi-clip lane, and its lanes, the inspector and the dock
    /// merged every position the clip passed through, caching each; the
    /// commit computes the one it lands on.
    pub fn is_moving_live(&self, track_id: &str) -> bool {
        self.c.moving_live.contains(track_id)
    }

    // --- Remote changes -------------------------------------------------------------

    /// Freshness reload when sync applied remote rows belonging to THIS
    /// practice. Heavily guarded: skipped while anything is in flight (busy
    /// gate, recording, separating, saving) — skipping is always SAFE
    /// because every save three-way merges against the base hashes; this
    /// reload only makes the remote edit visible sooner and refreshes the
    /// base so the next save resolves against it.
    #[must_use]
    pub fn on_remote_applied(&mut self, event: &RemoteApplied) -> Vec<Effect> {
        if event.practice_ids.contains(&self.c.practice_id) {
            self.reload_if_unchanged(persist::ReloadThen::Nothing);
        }
        self.finish()
    }

    // --- Dispose --------------------------------------------------------------------

    /// Tears the screen down (once). ORDER MATTERS. Recording first: it
    /// closes an in-flight take's WAV handle, unregisters the quit finalizer
    /// and stops the device-watch/meter timers that poll the engine — all
    /// before the engine goes away. Work in flight carries on (see the module
    /// doc).
    pub fn dispose(&mut self) {
        // Once: a second call disposed the controllers again.
        if self.c.disposed {
            return;
        }
        self.c.disposed = true;
        self.recording.dispose();
        // Cancel an in-flight separation so its subprocess is killed rather
        // than orphaned when the user leaves mid-separation.
        if self.is_separating() {
            self.c.separation.cancel();
        }
        self.analysis.dispose(); // stops the live-capture pump
        self.versions.dispose();
        self.mixer.dispose();
        self.playback.dispose(); // drops the position/playing subscriptions
        if let Some(g) = self.c.engine.graph() {
            self.c.app_settings.detach_engine(g);
        }
        self.c.engine.transport().dispose();
        self.c.monitor.stop();
    }

    pub fn is_disposed(&self) -> bool {
        self.c.disposed
    }
}

/// The recording host for a read: `record_target_id` reads only the model
/// and the selection.
struct ReadRecordingHost<'a>(&'a Core);

impl RecordingHost for ReadRecordingHost<'_> {
    fn practice(&self) -> Option<&SongPractice> {
        self.0.practice.as_deref()
    }

    fn selected_track_id(&self) -> Option<&str> {
        self.0.selected_track_id.as_deref()
    }

    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>> {
        self.0.track_by_id(id)
    }

    // Never reached through the shared borrow `record_target_id` takes.
    fn update_track(&mut self, _id: &str, _update: &dyn Fn(&AudioTrack) -> AudioTrack) {}

    fn append_vocal(&mut self, _path: &str, _analysis: &WaveformResult, _start_ms: i64) -> String {
        String::new()
    }

    fn select_recorded_clip(&mut self, _track_id: &str, _clip_id: Option<&str>) {}

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn busy_gate(&mut self) -> &mut BusyGate {
        unreachable!("a read-only recording host lends no gate")
    }

    fn live_monitor(&self) -> bool {
        false
    }

    fn toast(&mut self, _message: &str) {}

    fn friendly_error(&self, _error: &str) -> String {
        jobs::SOMETHING_WENT_WRONG.to_string()
    }
}

// AnalysisSource contract: this view model is what the dock and inspector
// analysis views read (shared with the technique screen's source). The
// compute + caches live in the analysis cache; playhead/seek in playback.
impl AnalysisSource for SongPracticeDetailViewModel {
    fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        self.c.selected_track()
    }

    fn analysis_start_ms(&self) -> i64 {
        self.with_analysis(|a, h| a.analysis_start_ms(h))
    }

    fn analysis_duration_ms(&self) -> i64 {
        self.with_analysis(|a, h| a.analysis_duration_ms(h))
    }

    fn selected_metrics(&self) -> Option<Arc<VoiceMetrics>> {
        self.with_analysis(|a, h| a.selected_metrics(h))
    }

    fn selected_spectrogram(&self) -> Option<Arc<Spectrogram>> {
        self.with_analysis(|a, h| a.selected_spectrogram(h))
    }

    fn selected_profile(&self) -> Option<Arc<VoiceProfile>> {
        self.with_analysis(|a, h| a.selected_profile(h))
    }

    fn selected_ltas(&self) -> Option<Arc<Ltas>> {
        self.with_analysis(|a, h| a.selected_ltas(h))
    }

    fn selected_register(&self) -> Option<Arc<RegisterMap>> {
        self.with_analysis(|a, h| a.selected_register(h))
    }

    fn spectrogram_loading(&self) -> bool {
        self.with_analysis(|a, h| a.spectrogram_loading(h))
    }

    fn metrics_loading(&self) -> bool {
        self.with_analysis(|a, h| a.metrics_loading(h))
    }

    fn live_monitor(&self) -> bool {
        self.analysis.live_monitor()
    }

    fn monitor(&self) -> &MonitorService {
        &self.c.monitor
    }

    fn position(&self) -> Duration {
        self.playback.position()
    }

    fn revision(&self) -> u64 {
        SongPracticeDetailViewModel::revision(self)
    }

    fn selected_vocal_tract(&self) -> Option<Arc<VocalTractTrack>> {
        self.with_analysis(|a, h| a.selected_vocal_tract(h))
    }

    fn vocal_tract_loading(&self) -> bool {
        self.with_analysis(|a, h| a.vocal_tract_loading(h))
    }
}
