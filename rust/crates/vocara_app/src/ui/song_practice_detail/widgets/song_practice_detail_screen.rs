//! Port of `ui/song_practice_detail/widgets/song_practice_detail_screen.dart`:
//! the detail view of one song practice — a dark studio workspace: the
//! transport on top, the arrange in the centre (Logic-style track-header
//! lanes under the ruler: the backing, the vocal takes, the FX buses and the
//! master), the inspector on the left, the lyrics / notes / techniques panel
//! on the right and the analysis + mixer dock at the bottom.
//!
//! # What the screen owns, and how it drives the view model
//!
//! - The [`SongPracticeDetailViewModel`] (its module doc is the API guide).
//!   Every [`Effect`] it hands back runs here: the jobs, analyses, the mic's
//!   start (its take setup handed back on the UI thread), the recording's
//!   file work and finalize, the timers and the permission prompts run off
//!   the UI thread as tasks and come back as this screen's messages, which
//!   the matching answer method (`job_done`, `analysis_job_done`,
//!   `start_done` / `take_setup`, `recording_job_done`, `finalize_ran`,
//!   `finalize_retry_due`, ...) continues; a [`Effect::Toast`] is the snack
//!   bar, [`Effect::Leave`] pops the page and a [`Effect::Reply`] resumes
//!   what the Dart screen awaited (the imports of a multi-file drop, one
//!   after the other; the technique list; the lyrics import and Save).
//! - What the Dart widgets kept in `State` and the views now borrow: the
//!   lyrics panel's state ([`LyricsPanelState`]), the dock's Spectrogram
//!   image ([`VoiceprintState`]), a [`LaneImage`] per take whose spectrogram
//!   lane is out, the menu the screen holds open, the open note editor and
//!   technique picker, the lane a reorder drag carries, which drop zone a
//!   dragged file hovers.
//!
//! After every message the screen does what Dart's rebuild and its
//! post-frame callbacks did: it drains the engine's events, asks the cache
//! again for analysis the dock and the inspector show and the cache let go,
//! reads the FX panel's analyzer input and gain reduction, syncs the
//! spectrogram images and the lyrics panel, and commits a drag whose
//! control left the screen mid-gesture (Dart's `dispose` committed it).
//!
//! Its subscription carries the frame tick while the transport plays
//! (`drain_engine_events`), the engine's events while it does not, the 30 ms
//! capture pump, the mic's level tick, the monitor meter and the device
//! watch while each is active, the separation's progress, a quit's requests
//! (the waker set with `set_quit_waker` wakes it; the app's quit guard waits
//! for them), the library's remote-applied events and the snack bar's time.
//! The dialogs — every
//! confirmation and name prompt — go through the app's dialog stack
//! ([`Update::dialog`]). Its keyboard shortcuts (Space, Delete and
//! Backspace, Escape) are a widget around the page (`shortcuts`), which
//! hears only the keys nothing on the page took.
//!
//! # Leaving
//!
//! Back finalizes a take in flight first (`stop_before_leaving`, the page
//! leaving on its [`Effect::Leave`]), then pops — or goes Home when the page
//! is the root of the stack (a deep link). While loading, or when the
//! practice could not be loaded, a bar with the same back chevron is all
//! there is. When the page leaves the stack the view model is disposed, and
//! the page lingers ([`SongPracticeDetailScreen::lingers`]) until
//! `has_work_in_flight` reads false: what was in flight then (a stopped take
//! landing, a queued save, a guarded edit) outlives the page, as Dart's
//! futures did, and still gets its answers.
//!
//! # Differences from Flutter
//!
//! - The page draws its snack bar itself, over its bottom edge, while it is
//!   on top; once it leaves (or is covered) the app keeps it up over the
//!   page on top for the time it has left (`app::messenger`), as Dart's
//!   `ScaffoldMessenger`, above the pages, did — the case that matters is a
//!   take's finalize toasting as Back leaves. A new one replaces the one
//!   showing, as `clearSnackBars` did, once that one has left; each fades
//!   in and out as Material's floating snack bar did, its time counted from
//!   the end of its entrance (`ui::snack`). Its timer is a subscription.
//! - The note editor and the technique picker are drawn over the page, under
//!   the page's toolbar (Dart's dialog routes covered it); the app's studio
//!   dialogs cover the whole window as before. They come in and go as their
//!   `DialogRoute`s did (`confirm_dialog::DialogMotion`); a closed one stays
//!   drawn, taking no input, until it has faded out.
//! - There is no system Back (no `PopScope`): the transport's chevron is the
//!   page's only way back, and it runs the leave guard.
//! - A multi-file drop still importing when the page leaves stops with the
//!   file in flight (Dart's loop went on calling the disposed view model).

mod feeds;
mod shortcuts;
mod update;
mod view;

/// The page's message for what its snack bars report.
fn snack_message(event: SnackEvent) -> Message {
    match event {
        SnackEvent::Expired(seq) => Message::SnackExpired(seq),
        SnackEvent::Frame(at) => Message::SnackFrame(at),
        SnackEvent::Swiped(seq) => Message::SnackSwiped(seq),
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod through_the_app;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iced::{Element, Subscription, Task};
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_services::mic_permission;
use vocara_services::playback_engine::PlaybackEngine;
use vocara_store::sync::sync_engine::RemoteApplied;

use self::feeds::{EngineFeed, WakeFeed};
use self::view::panel_data;
use super::analysis_lanes::{self, LaneImage};
use super::bottom_dock::{self, SummaryData};
use super::detail_transport_bar;
use super::fx_panel;
use super::left_panel::{self, InspectorMenu};
use super::lyrics_panel::{self, LyricsPanelState, NoteRef};
use super::note_editor::{self, NoteEditor};
use super::ruler_bar;
use super::technique_picker::{self, TechniquePicker};
use super::track_context_menu::{BusAction, ClipAction, TrackAction};
use super::track_row;
use super::vocal_tract_view::{self, VocalTractState};
use super::voiceprint_view::VoiceprintState;
use crate::app::library::RemoteAppliedBroadcast;
use crate::services::Services;
use crate::ui::core::motion;
use crate::ui::core::widgets::confirm_dialog::{DialogMotion, DialogResult};
use crate::ui::core::widgets::spectrogram_painter::{release_spectrogram_images, RenderRequest};
use crate::ui::snack::{SnackEvent, SnackQueue};
use crate::ui::song_practice_detail::view_model::analysis_cache::{self, EqInputColumn};
use crate::ui::song_practice_detail::view_model::interpretations_controller::VersionsOp;
use crate::ui::song_practice_detail::view_model::recording_controller::{
    self, RecordingController, RecordingOp, StartDone, StartJob, StartOutcome, TakeSetupRequest,
};
use crate::ui::song_practice_detail::view_model::song_practice_detail_view_model::{
    Call, Deps, Effect, JobDone, Options, SongPracticeDetailViewModel,
};
use crate::ui::{Screen, SnackBar, Update};
use crate::util::busy_gate::BusyOutcome;
use vocara_services::self_update::SelfUpdate;

// ------------------------------------------------------------------ constants

/// What the engine's decoder reads (symphonia's features in the engine's
/// Cargo.toml). Opus and WMA were offered too, with no decoder for either:
/// each drop failed with "could not be converted for playback".
pub const AUDIO_EXTENSIONS: [&str; 10] = [
    ".wav", ".mp3", ".m4a", ".aac", ".flac", ".ogg", ".oga", ".aif", ".aiff", ".caf",
];

/// The least height the timeline (and the side panels beside it) keeps
/// however tall the dock is dragged: the ruler, the scrollbar and most of a
/// lane.
pub const MIN_TIMELINE_HEIGHT: f32 = 120.0;

/// How long a toast shows.
pub const SNACK_DURATION: Duration = Duration::from_millis(2800);

/// The capture pump's period.
pub const CAPTURE_PUMP_PERIOD: Duration = Duration::from_millis(30);

/// How often a running separation's progress is taken in (Dart's
/// `onProgress` stream, polled).
pub const SEPARATION_PROGRESS_PERIOD: Duration = Duration::from_millis(100);

/// Over a load that failed.
pub const NOT_LOADED: &str = "Could not load this practice.";

/// A drop with nothing the engine can play.
pub const NO_AUDIO_IN_DROP: &str = "No supported audio files in the drop.";

/// A drop beside every drop zone (on the transport, the inspector, the
/// dock): where audio goes.
pub const DROP_ON_A_LANE: &str = "Drop audio on the backing track or a take.";

/// A drop on the workspace while it catches up on a save.
pub const WAIT_FOR_SAVE: &str = "Wait for your last save to finish.";

/// A drop on the separation overlay.
pub const WAIT_FOR_SEPARATION: &str = "Wait for the separation to finish.";

/// REC while an update installs: Vocara relaunches in seconds.
pub const RECORD_AFTER_THE_UPDATE: &str =
    "Vocara is installing an update and will relaunch. Record once it's back.";

/// Over the workspace while it catches up on a save.
pub const FINISHING_SAVE: &str = "Finishing your last save…";

/// `_isAudioPath`: whether `path` names a file the engine can decode, by its
/// extension (case-insensitive).
pub fn is_audio_path(path: &str) -> bool {
    let Some(dot) = path.rfind('.') else {
        return false;
    };
    let ext = path[dot..].to_lowercase();
    AUDIO_EXTENSIONS.contains(&ext.as_str())
}

/// The toast after a drop on a set backing slot became vocal takes.
pub fn backing_set_toast(slot: &str) -> String {
    format!("The {slot} is already set, so the drop was added as vocal takes.")
}

// ------------------------------------------------------------------ messages

/// A drop target of the lane list: each has its own hover highlight, so
/// dragging over one region never lights up the others.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DropZone {
    /// The backing region: fill the empty backing slot (per the
    /// Accompaniment / Original toggle); any extra files become new takes.
    Backing,
    /// The (empty) vocal-takes area: each file becomes a new take.
    NewTake,
    /// A take's lane: each file becomes a new clip on that take.
    Take(String),
}

/// The menu the screen holds open (Dart pushed a popup route).
#[derive(Debug, Clone, PartialEq)]
pub enum OpenMenu {
    /// A lane's menu (its track id).
    Track(String),
    /// A clip's menu.
    Clip { track_id: String, clip_id: String },
    /// An FX bus lane's menu.
    Bus(String),
    /// The interpretation dropdown.
    Interpretations,
    /// One of the inspector's chips.
    Inspector(InspectorMenu),
}

/// A row of the interpretation dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterpretationAction {
    /// Show and play this interpretation (`versions.setActive`).
    SetActive(String),
    /// "New interpretation": ask for a name, then create it.
    Create,
    /// "Rename": ask for a new name for the active one.
    Rename,
    /// "Delete": ask, then delete the active one.
    Delete,
}

/// What a studio dialog the screen asked was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    DeleteTrack(String),
    DeleteClip { track_id: String, clip_id: String },
    RenameTrack(String),
    Separate(String),
    RemoveTechnique { track_id: String, marker_id: String },
    RenameBus(String),
    DeleteBus(String),
    NewInterpretation,
    RenameInterpretation(String),
    DeleteInterpretation(String),
    ClearLyricTimes,
    ImportLyrics(PathBuf),
    SaveEmptyLyrics(lyrics_panel::SaveTicket),
    DeleteNote { track_id: String, note_id: String },
}

/// A result that is cloneable (the screen's messages are) but landed once:
/// a second landing of the same report finds nothing.
pub struct Landed<T>(Arc<Mutex<Option<T>>>);

impl<T> Landed<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    /// The value, the first time.
    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl<T> Clone for Landed<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: fmt::Debug> fmt::Debug for Landed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_tuple("Landed").field(&*slot).finish()
    }
}

/// What the song practice page reacts to.
#[derive(Debug, Clone)]
pub enum Message {
    /// The back chevron of the bar shown while there is no workspace.
    Back,
    /// The transport bar.
    Transport(detail_transport_bar::Message),
    /// The ruler.
    Ruler(ruler_bar::Message),
    /// A lane's row (the track it shows).
    Row {
        track_id: String,
        message: track_row::Message,
    },
    /// A take's fold-out analysis lanes.
    Lanes {
        track_id: String,
        message: analysis_lanes::Message,
    },
    /// The backing header's Accompaniment ⇄ Original toggle
    /// (`vm.setPlayReference`).
    SetPlayReference(bool),
    /// "+ New take" (`vm.newTake`).
    NewTake,
    /// The interpretation dropdown was pressed.
    OpenInterpretations,
    /// "+ New bus" (`vm.mixer.createBus`).
    NewBus,
    /// An FX bus lane's header was tapped (`vm.selectBus`).
    SelectBus(String),
    /// An FX bus lane's ⋯ or right-click.
    OpenBusMenu(String),
    /// A bus lane's M (`vm.mixer.toggleBusMute`).
    ToggleBusMute(String),
    /// A bus lane's S (`vm.mixer.toggleBusSolo`).
    ToggleBusSolo(String),
    /// A bus lane's fader (`vm.mixer.setBusVolumeLive`).
    SetBusVolumeLive {
        bus_id: String,
        volume: f64,
    },
    /// A bus lane's fader released (`vm.commitFx`).
    CommitFx,
    /// The master lane's fader (`vm.mixer.setMasterVolumeLive`).
    SetMasterVolumeLive(f64),
    /// The master lane's fader released (`vm.mixer.commitMasterVolume`).
    CommitMasterVolume,
    /// A horizontal swipe over the lanes, or the scrollbar's thumb dragged:
    /// pan the view by this many ms (`vm.playback.panView`).
    PanView(i64),
    /// Files dropped on a drop zone.
    Drop {
        zone: DropZone,
        paths: Vec<PathBuf>,
    },
    /// A dragged file entered (or left) a drop zone.
    DropHover {
        zone: DropZone,
        hover: bool,
    },
    /// A drop hint was clicked: browse for audio for its zone.
    Browse(DropZone),
    /// The open panel answered (`at_ms`: where a take menu's "Add audio
    /// clip at playhead" was asked).
    Picked {
        zone: DropZone,
        paths: Vec<PathBuf>,
        at_ms: Option<i64>,
    },
    /// A drop on an overlay that takes none: say why.
    RefusedDrop(&'static str),
    /// A row of the open lane menu.
    TrackMenu(TrackAction),
    /// A row of the open clip menu.
    ClipMenu(ClipAction),
    /// A row of the open bus menu.
    BusMenu(BusAction),
    /// A row of the interpretation dropdown.
    InterpretationMenu(InterpretationAction),
    /// A tap outside the open menu, or Escape.
    DismissMenu,
    /// Delete or Backspace: delete the selected clip, asked first as the
    /// clip menu's Delete is.
    DeleteSelectedClip,
    /// Escape with no menu open: let go of the selected clip (its take
    /// stays selected: the inspector always shows a lane).
    DeselectClip,
    /// The inspector (left panel).
    Inspector(left_panel::Message),
    /// The inspector's FX panel.
    Fx(fx_panel::Message),
    /// The right panel and the lyrics Focus view.
    Lyrics(lyrics_panel::Message),
    /// The bottom dock.
    Dock(bottom_dock::Message),
    /// The open note editor.
    NoteEditor(note_editor::Message),
    /// The open technique picker.
    TechniquePicker(technique_picker::Message),
    /// A studio dialog the screen asked closed.
    Answered(Asked, DialogResult),
    /// The separation overlay's Cancel (`vm.cancelSeparation`).
    CancelSeparation,

    // The view model's work, landed.
    Job(Landed<JobDone>),
    AnalysisJob(analysis_cache::JobDone),
    AnalysisMicPermission(bool),
    LaneHealRetry,
    TakeSetup(TakeSetupRequest),
    StartDone(StartDone),
    RecordingJob(recording_controller::JobDone),
    Finalized(RecordingOp, BusyOutcome<String>),
    FinalizeRetry(RecordingOp),
    MonitorPermission(bool),
    CleanedUp(VersionsOp, BusyOutcome<String>),

    // Ticks and feeds.
    /// A frame while the transport plays.
    Frame,
    /// The engine added a position or playing event.
    EngineEvent,
    PumpCapture,
    MicTick,
    MonitorMeterTick,
    DeviceWatchTick,
    SeparationProgressTick,
    /// A quit asks something of the recording.
    QuitRequests,
    /// Sync applied remote changes (`Persistence.remoteApplied`).
    RemoteApplied(RemoteApplied),
    /// The showing snack bar's time is up.
    SnackExpired(u64),
    /// A frame while the snack bar enters or leaves.
    SnackFrame(Instant),
    /// Snack bar `n` was swiped away.
    SnackSwiped(u64),
    /// A frame while the note editor or the picker comes in or goes.
    DialogFrame(Instant),
    /// A finished side effect.
    Done,
}

// ------------------------------------------------------------- the state

/// What a reply the screen waits for resumes.
#[derive(Debug)]
enum Awaited {
    /// The next file of a drop (its run).
    Drop(u64),
    /// The technique list for the picker opened `seq`.
    Techniques(u64),
    /// A lyrics file's import (`_importDrop`).
    LyricsImport(lyrics_panel::ImportTicket),
    /// The lyrics editor's Save.
    LyricsSave(lyrics_panel::SaveTicket),
}

/// A drop (or a browse) importing its files one after the other.
#[derive(Debug)]
struct DropRun {
    id: u64,
    kind: RunKind,
    queue: std::collections::VecDeque<String>,
}

#[derive(Debug)]
enum RunKind {
    /// `_dropBacking`: the first file filled the empty backing slot unless
    /// `filled_slot` is false (the slot was set: every file is a take).
    Backing {
        filled_slot: bool,
        files: usize,
        /// Whether the slot's name and the take count were read: once the
        /// backing (if any) landed, as Dart read them after its first await.
        counted: bool,
        slot: &'static str,
        before: usize,
    },
    /// `_dropNewTake`.
    NewTake,
    /// `_dropClipToTake`: where the next clip lands.
    Clip { track_id: String, at_ms: i64 },
}

/// Where an open note editor's result goes.
#[derive(Debug, Clone)]
enum NoteTarget {
    Add {
        track_id: String,
        start_ms: i64,
        end_ms: Option<i64>,
    },
    Edit {
        track_id: String,
        note_id: String,
    },
}

/// The open technique picker and where its technique lands.
struct OpenPicker {
    picker: TechniquePicker,
    seq: u64,
    track_id: String,
    start_ms: i64,
    end_ms: Option<i64>,
}

/// A live edit whose release has not come yet: its commit is owed by the
/// control that made it, or by the screen when the control leaves the
/// screen mid-gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owed {
    /// A take's fader or pan knob (`commitTrackVolume`), on the control
    /// showing this take.
    TrackVolume(Source),
    /// A bus's fader, pan, a send level or an FX edit (`commitFx`).
    Fx(Source),
    /// The master fader (`commitMasterVolume`).
    MasterVolume(Source),
}

/// Which control made a live edit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A timeline lane (a take's or a bus's id) or the master lane.
    Lane(Option<String>),
    /// The inspector's strip for this channel (and its FX panel).
    Inspector(String),
    /// The dock's mixer desk.
    Mixer(Option<String>),
}

/// What the views borrow that the view model hands out as owned values,
/// as of the last update.
#[derive(Default)]
struct Shown {
    /// `vm.versions.vocalTakes`.
    vocal_takes: Vec<Arc<AudioTrack>>,
    /// `_orderedTracks`: the active backing, then the active takes.
    mixer_tracks: Vec<Arc<AudioTrack>>,
    notes: Vec<PracticeNote>,
    techniques: Vec<TechniqueMarker>,
    all_notes: Vec<NoteRef>,
    summary: SummaryData,
    /// The FX panel's EQ analyzer input, for the selected take.
    eq_input: Option<EqInputColumn>,
    /// The FX panel's compressor gain reduction.
    gain_reduction_db: f64,
}

/// A spectrogram render's destination.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Render {
    /// The dock's Spectrogram tab.
    Dock,
    /// A take's spectrogram lane.
    Lane(String),
}

/// What handling one message asked for: the view model's work and the
/// spectrogram renders, to run off the UI thread, and the rest of the
/// update.
struct Step {
    work: Vec<Effect>,
    renders: Vec<(Render, RenderRequest)>,
    /// Open panels to show (`_pickAudioPaths`): a drop zone, and where a
    /// take menu's clip lands.
    picks: Vec<(DropZone, Option<i64>)>,
    update: Update<Message>,
}

impl Step {
    fn new() -> Self {
        Self {
            work: Vec::new(),
            renders: Vec::new(),
            picks: Vec::new(),
            update: Update::none(),
        }
    }

    fn task(&mut self, task: Task<Message>) {
        let update = std::mem::replace(&mut self.update, Update::none());
        self.update = update.and_task(task);
    }

    fn into_update(self) -> Update<Message> {
        let mut update = self.update;
        for effect in self.work {
            update = update.and_task(work_task(effect));
        }
        for (target, render) in self.renders {
            update = update.and_task(render.into_task(move |done| rendered(&target, done)));
        }
        for (zone, at_ms) in self.picks {
            update = update.and_task(pick_audio(zone, at_ms));
        }
        update
    }
}

/// A finished render, as the message of its destination.
fn rendered(
    target: &Render,
    done: crate::ui::core::widgets::spectrogram_painter::Rendered,
) -> Message {
    match target {
        Render::Dock => Message::Dock(bottom_dock::Message::Voiceprint(
            super::voiceprint_view::Message::ImageRendered(done),
        )),
        Render::Lane(id) => Message::Lanes {
            track_id: id.clone(),
            message: analysis_lanes::Message::ImageRendered(done),
        },
    }
}

/// A worker that never answered (the runtime shut down under it).
fn lost(what: &str, error: &str) -> Message {
    DiagnosticsLog::instance().log("song", &format!("{what} lost: {error}"));
    Message::Done
}

/// Runs one of the view model's effects off the UI thread; what it reports
/// comes back as the message its answer method takes.
fn work_task(effect: Effect) -> Task<Message> {
    use crate::util::blocking;
    match effect {
        Effect::Run(job) => Task::perform(blocking(move || job.run()), |r| match r {
            Ok(done) => Message::Job(Landed::new(done)),
            Err(e) => lost("job", &e),
        }),
        Effect::Analysis(job) => Task::perform(blocking(move || job.run()), |r| match r {
            Ok(done) => Message::AnalysisJob(done),
            Err(e) => lost("analysis", &e),
        }),
        Effect::AnalysisMicPermission => Task::perform(blocking(mic_permission::request), |r| {
            Message::AnalysisMicPermission(r.unwrap_or(false))
        }),
        Effect::LaneHealRetry { after } => {
            Task::perform(tokio::time::sleep(after), |_| Message::LaneHealRetry)
        }
        Effect::StartRecording(job) => start_task(job),
        Effect::RecordingWork(job) => Task::perform(blocking(move || job.run()), |r| match r {
            Ok(done) => Message::RecordingJob(done),
            Err(e) => lost("recording work", &e),
        }),
        Effect::FinalizeTake(op, run) => run.into_task(move |o| Message::Finalized(op, o)),
        Effect::FinalizeRetry { op, after } => {
            Task::perform(tokio::time::sleep(after), move |_| {
                Message::FinalizeRetry(op)
            })
        }
        Effect::MonitorMicPermission => Task::perform(blocking(mic_permission::request), |r| {
            Message::MonitorPermission(r.unwrap_or(false))
        }),
        Effect::InterpretationsCleanUp(op, run) => {
            run.into_task(move |o| Message::CleanedUp(op, o))
        }
        // Handled where they come up.
        Effect::Toast(_) | Effect::Leave | Effect::Reply(_) => Task::none(),
    }
}

/// `_pickAudioPaths`: the open panel, filtered to the same formats the drop
/// path accepts so both entry paths take identical files. Multi-select:
/// every zone that accepts a drop of many files accepts a browse of many
/// files. Nothing on cancel. What it picked comes back as
/// [`Message::Picked`].
fn pick_audio(zone: DropZone, at_ms: Option<i64>) -> Task<Message> {
    let extensions: Vec<&str> = AUDIO_EXTENSIONS.iter().map(|e| &e[1..]).collect();
    let picking = vocara_services::file_panels::AsyncFileDialog::new()
        .add_filter("Audio", &extensions)
        .pick_files();
    Task::future(picking).map(move |files| Message::Picked {
        zone: zone.clone(),
        paths: files
            .unwrap_or_default()
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect(),
        at_ms,
    })
}

/// The mic's start on a worker: its take setup comes back to the UI thread
/// as [`Message::TakeSetup`] while the worker waits for the answer, then
/// its outcome as [`Message::StartDone`].
fn start_task(job: StartJob) -> Task<Message> {
    use iced::futures::SinkExt;
    let op = job.op();
    Task::run(
        iced::stream::channel(
            4,
            move |mut out: iced::futures::channel::mpsc::Sender<Message>| async move {
                let mut setup = out.clone();
                let done = crate::util::blocking(move || {
                    job.run(move |request| {
                        let _ = setup.try_send(Message::TakeSetup(request));
                    })
                })
                .await
                .unwrap_or_else(|message| StartDone {
                    op,
                    outcome: StartOutcome::Threw {
                        setup_failed: false,
                        message,
                    },
                });
                let _ = out.send(Message::StartDone(done)).await;
            },
        ),
        |m| m,
    )
}

/// The song practice page (see the module docs).
pub struct SongPracticeDetailScreen {
    /// The practice it shows (the route's id).
    practice_id: String,
    /// The view model (boxed: every page sits in one enum).
    vm: Box<SongPracticeDetailViewModel>,
    /// What the sync engine announces after applying remote rows.
    remote_applied: Arc<RemoteAppliedBroadcast>,
    /// Where the quit's waker wakes the page.
    quit_feed: Arc<WakeFeed>,
    /// The playback engine, whose events wake the page while it is paused.
    engine: Arc<dyn PlaybackEngine>,
    /// The app's self-update: REC is refused while it installs (see
    /// `RECORD_AFTER_THE_UPDATE`).
    self_update: Arc<SelfUpdate>,
    /// Set once the page left the stack.
    disposed: bool,
    /// The work the page's teardown started (the saves of the commits it
    /// owed), for the app to run ([`SongPracticeDetailScreen::take_teardown`]).
    teardown: Vec<Effect>,
    shown: Shown,
    lyrics: LyricsPanelState,
    voiceprint: VoiceprintState,
    /// The dock's Vocal tract tab: its view, its camera and the pose on its
    /// springs (kept for the session).
    vocal_tract: VocalTractState,
    lane_images: HashMap<String, LaneImage>,
    /// The image a lane is handed before its own is made.
    no_image: LaneImage,
    menu: Option<OpenMenu>,
    note_editor: Option<(NoteEditor, NoteTarget)>,
    picker: Option<OpenPicker>,
    /// The open note editor's and picker's route animations.
    editor_motion: DialogMotion,
    picker_motion: DialogMotion,
    /// A closed note editor or picker, drawn as it goes.
    closing_editor: Option<Box<(NoteEditor, DialogMotion)>>,
    closing_picker: Option<Box<(TechniquePicker, DialogMotion)>>,
    /// The last frame's time, what their drawing reads.
    dialog_now: Instant,
    picker_seq: u64,
    /// Studio dialogs this page asked that have not answered yet.
    dialogs_open: usize,
    awaiting: HashMap<Call, Awaited>,
    runs: Vec<DropRun>,
    next_run: u64,
    /// The drop zone a dragged file hovers.
    drop_hover: Option<DropZone>,
    /// The lane (track id) a reorder drag carries.
    lane_drag: Option<String>,
    /// The lane the dragged one hovers.
    drop_target: Option<String>,
    /// The take whose clip a move-drag is moving.
    moving: Option<String>,
    owed: Vec<Owed>,
    /// The snack bars (one at a time: each replaces the one up).
    snacks: SnackQueue,
}

impl fmt::Debug for SongPracticeDetailScreen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SongPracticeDetailScreen")
            .field("practice_id", &self.practice_id)
            .field("vm", &self.vm)
            .field("disposed", &self.disposed)
            .finish_non_exhaustive()
    }
}

/// The view model's collaborators from the app's services (the route
/// builder's `context.read`s).
pub fn deps_of(services: &Services) -> Deps {
    Deps {
        repository: Arc::clone(&services.song_repo),
        technique_repository: Arc::clone(&services.technique_repo),
        audio_files: Arc::clone(&services.audio_files),
        spectrograms: Arc::clone(&services.spectrograms),
        separation: Arc::clone(&services.separation),
        app_settings: Arc::clone(&services.app_settings),
    }
}

impl SongPracticeDetailScreen {
    /// The page for practice `practice_id` over the app's services, its load
    /// started: the native graph engine, the app-wide recording finalizer,
    /// the platform save panel.
    pub fn new(practice_id: String, services: &Services) -> (Self, Task<Message>) {
        Self::with_options(practice_id, services, deps_of(services), Options::default())
    }

    /// The page over the given view model dependencies and options (a
    /// test's fakes), its load started.
    pub fn with_options(
        practice_id: String,
        services: &Services,
        deps: Deps,
        options: Options,
    ) -> (Self, Task<Message>) {
        let (screen, step) = Self::open(practice_id, services, deps, options);
        (screen, step.into_update().task)
    }

    fn open(
        practice_id: String,
        services: &Services,
        deps: Deps,
        options: Options,
    ) -> (Self, Step) {
        let (mut vm, effects) =
            SongPracticeDetailViewModel::new(deps, practice_id.clone(), options);
        let quit_feed = Arc::new(WakeFeed::default());
        let waker = Arc::clone(&quit_feed);
        vm.set_quit_waker(move || waker.wake());
        let engine = Arc::clone(vm.playback().engine());
        let mut screen = Self {
            practice_id,
            vm: Box::new(vm),
            remote_applied: Arc::clone(&services.library.remote_applied),
            quit_feed,
            engine,
            self_update: Arc::clone(&services.self_update),
            disposed: false,
            teardown: Vec::new(),
            shown: Shown::default(),
            lyrics: LyricsPanelState::new(),
            voiceprint: VoiceprintState::new(),
            vocal_tract: VocalTractState::new(),
            lane_images: HashMap::new(),
            no_image: LaneImage::new(),
            menu: None,
            note_editor: None,
            picker: None,
            editor_motion: DialogMotion::shown(),
            picker_motion: DialogMotion::shown(),
            closing_editor: None,
            closing_picker: None,
            dialog_now: Instant::now(),
            picker_seq: 0,
            dialogs_open: 0,
            awaiting: HashMap::new(),
            runs: Vec::new(),
            next_run: 0,
            drop_hover: None,
            lane_drag: None,
            drop_target: None,
            moving: None,
            owed: Vec::new(),
            snacks: SnackQueue::new(),
        };
        let mut step = Step::new();
        screen.absorb(effects, &mut step);
        screen.refresh(&mut step);
        (screen, step)
    }

    /// The practice the page shows.
    pub fn practice_id(&self) -> &str {
        &self.practice_id
    }

    /// The view model.
    pub fn view_model(&self) -> &SongPracticeDetailViewModel {
        &self.vm
    }

    /// The snack bar showing, if any.
    pub fn snack(&self) -> Option<&str> {
        self.snacks.current().map(|s| s.text.as_str())
    }

    /// The showing snack bar's number ([`Message::SnackExpired`] takes it).
    pub fn snack_seq(&self) -> Option<u64> {
        self.snacks.current_seq()
    }

    /// The menu the page holds open, if any.
    pub fn open_menu(&self) -> Option<&OpenMenu> {
        self.menu.as_ref()
    }

    /// The open note editor, if any.
    pub fn note_editor(&self) -> Option<&NoteEditor> {
        self.note_editor.as_ref().map(|(e, _)| e)
    }

    /// The open technique picker, if any.
    pub fn technique_picker(&self) -> Option<&TechniquePicker> {
        self.picker.as_ref().map(|p| &p.picker)
    }

    /// Whether the page, gone from the stack, must be kept: its view model
    /// still has work in flight that outlives it (see the module docs).
    pub fn lingers(&self) -> bool {
        self.disposed && self.vm.has_work_in_flight()
    }

    /// The work [`Screen::dispose`] started (a drag's commit owed by a
    /// control that left with the page, and its save), for the app to run:
    /// what it reports comes back to the lingering page.
    pub fn take_teardown(&mut self) -> Task<Message> {
        let mut step = Step::new();
        step.work = std::mem::take(&mut self.teardown);
        step.into_update().task
    }
}

impl Screen for SongPracticeDetailScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        self.handle(message).into_update()
    }

    fn view(&self) -> Element<'_, Message> {
        self.page()
    }

    fn subscription(&self, _on_top: bool) -> Subscription<Message> {
        if self.disposed {
            return Subscription::none();
        }
        let vm = &self.vm;
        let recording = vm.recording();
        let mut subscriptions = vec![
            self.remote_applied
                .subscription()
                .map(Message::RemoteApplied),
            feeds::wakes(&self.quit_feed).map(|()| Message::QuitRequests),
        ];
        // The playhead moves every frame while playing; paused, the engine's
        // own events wake the page (a seek, the flip to playing).
        if vm.playback().is_playing() {
            subscriptions.push(iced::window::frames().map(|_| Message::Frame));
        } else {
            subscriptions.push(
                feeds::engine_events(EngineFeed(Arc::clone(&self.engine)))
                    .map(|()| Message::EngineEvent),
            );
        }
        if vm.analysis().capture_pump_active() {
            subscriptions
                .push(iced::time::every(CAPTURE_PUMP_PERIOD).map(|_| Message::PumpCapture));
        }
        if recording.next_mic_tick_due().is_some() {
            subscriptions.push(
                iced::time::every(RecordingController::MIC_TICK_PERIOD).map(|_| Message::MicTick),
            );
        }
        if recording.monitor_meter_active() {
            subscriptions.push(
                iced::time::every(RecordingController::MONITOR_METER_PERIOD)
                    .map(|_| Message::MonitorMeterTick),
            );
        }
        if recording.device_watch_active() {
            subscriptions.push(
                iced::time::every(RecordingController::DEVICE_WATCH_PERIOD)
                    .map(|_| Message::DeviceWatchTick),
            );
        }
        if vm.is_separating() {
            subscriptions.push(
                iced::time::every(SEPARATION_PROGRESS_PERIOD)
                    .map(|_| Message::SeparationProgressTick),
            );
        }
        subscriptions.push(self.snacks.subscription().map(snack_message));
        subscriptions.push(
            motion::frames(self.wants_dialog_frames(Instant::now())).map(Message::DialogFrame),
        );
        // The Vocal tract tab's figure moving to the latest estimate.
        subscriptions.push(
            motion::frames(self.vocal_tract_shown() && self.vocal_tract.animating()).map(|at| {
                Message::Dock(bottom_dock::Message::VocalTract(
                    vocal_tract_view::Message::Frame(at),
                ))
            }),
        );
        // A lyrics list scrolling to a revealed line.
        subscriptions.push(
            motion::frames(self.lyrics.wants_scroll_frames())
                .map(|at| Message::Lyrics(lyrics_panel::Message::ScrollFrame(at))),
        );
        Subscription::batch(subscriptions)
    }

    /// The page left the stack: a drag whose release will never come is
    /// committed, the view model disposed (recording first, an in-flight
    /// separation cancelled, the engine detached and disposed), and the
    /// cached spectrogram images let go. What the view model still has in
    /// flight keeps the page lingering.
    fn dispose(&mut self, _services: &Services) {
        if self.disposed {
            return;
        }
        let mut step = Step::new();
        self.settle_owed(true, &mut step);
        self.disposed = true;
        self.vm.dispose();
        self.menu = None;
        self.note_editor = None;
        self.picker = None;
        self.closing_editor = None;
        self.closing_picker = None;
        self.runs.clear();
        self.awaiting.clear();
        self.snacks = SnackQueue::new();
        self.voiceprint.clear();
        self.vocal_tract.clear();
        self.lane_images.clear();
        release_spectrogram_images();
        // The commits' saves land on the lingering page.
        self.teardown = step.work;
    }

    /// The snack bar showing, for the time it has left; it replaced the
    /// ones before it.
    fn take_snack_bars(&mut self) -> Vec<SnackBar> {
        self.snacks.take(Instant::now())
    }
}

/// The selected track as the practice holds it (borrowed for the views).
fn selected_track_ref<'a>(
    vm: &SongPracticeDetailViewModel,
    practice: &'a SongPractice,
) -> Option<&'a Arc<AudioTrack>> {
    track_ref(practice, vm.selected_track_id()?)
}

/// Lane `id` as the practice holds it.
fn track_ref<'a>(practice: &'a SongPractice, id: &str) -> Option<&'a Arc<AudioTrack>> {
    practice
        .accompaniment
        .iter()
        .chain(practice.reference.iter())
        .chain(practice.vocal_tracks.iter())
        .find(|t| t.id == id)
}

/// `Duration.inMilliseconds`.
fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}
