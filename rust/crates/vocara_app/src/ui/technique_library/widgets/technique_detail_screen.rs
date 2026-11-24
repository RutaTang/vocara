//! Port of `ui/technique_library/widgets/technique_detail_screen.dart`: a
//! single technique's page — view or edit its fields and manage its
//! audio/video example clips, each paired with its analysis (and, in the
//! read view, a practice recorder to compare against it).
//!
//! The edit form's state is the screen's (not a child widget's) so the
//! Save/Cancel actions live in the top header. Media is managed in both
//! modes through the view model; while editing, its changes are staged in
//! the view model's draft and persisted with the text on Save.
//!
//! # What the screen owns, and how it drives the view model
//!
//! - The [`TechniqueDetailViewModel`]. Each [`Job`] it asks for runs on a
//!   blocking worker ([`job_task`]) and comes back as [`Message::Landed`],
//!   which [`TechniqueDetailViewModel::land`] continues; the view model's
//!   [`Answer`]s are the Dart screen's `await`s resuming (a save that
//!   persisted closes the editor, a delete that persisted goes back to the
//!   library, a failed start or save says so).
//! - The examples' players ([`ExamplePlayers`], over a
//!   [`MediaPlayerFactory`]) and the examples section's state
//!   ([`MediaSectionState`]). The view model's `micArmed` and
//!   `analysisScrub` signals ([`Effect::PauseExamples`],
//!   [`Effect::SeekAnalyzed`]) are applied to the players as they come.
//! - The analysis card's Spectrogram image ([`VoiceprintState`]).
//!
//! After every message the screen does what Dart's rebuild did: it syncs the
//! players with the examples it shows, asks
//! [`TechniqueDetailViewModel::ensure_reference_analysis`] while the
//! analysis panel is built (the panel's post-frame call), syncs the
//! Spectrogram image, and toasts a pending recording notice once.
//!
//! Its subscription carries the recorder's ticks while
//! [`TechniqueDetailViewModel::next_tick_due`] asks for them (the capture
//! pump, the level and device watch, and a quit's `quitFinalize` handed
//! over from the finalizer thread — the app's quit guard waits on it), the
//! players' poll, the library's remote-applied events, and the showing snack
//! bar's time. The dialogs —
//! delete, unsaved changes, an example's removal and rename — go through the
//! app's dialog stack ([`Update::dialog`]).
//!
//! # Differences from Flutter
//!
//! The page draws its snack bars itself, over its bottom edge, while it is
//! on top; once it leaves (or is covered) the app keeps them up over the page
//! on top (`app::messenger`), as Dart's `ScaffoldMessenger`, above every
//! page, did. A new one waits behind the ones up, as `showSnackBar` queued
//! it; the timer is a subscription. The
//! header ignores its buttons for a double-tap's span after it switches
//! modes, as Dart's `IgnorePointer` did, by dropping their messages. iced has
//! no ellipsis (the title clips) and no letter spacing (the title's 0.3, the
//! captions' 0.6); no italic Inter face is bundled, so "Empty content" asks
//! for italic and draws upright. The prose fields are iced text editors that
//! grow from their minimum to their maximum lines, then scroll, as Flutter's
//! did.

#[cfg(test)]
mod tests;
#[cfg(test)]
mod through_the_app;

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::alignment::Vertical;
use iced::widget::text::Wrapping;
use iced::widget::{
    button, center, column, container, opaque, scrollable, stack, text, text_editor, Column, Row,
    Space,
};
use iced::{widget, Color, Element, Font, Length, Padding, Shadow, Subscription, Task, Vector};
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::TechniqueMedia;
use vocara_model::util::dart_string;
use vocara_services::audio_exporter::PickError;
use vocara_services::media_player::{MediaPlayerFactory, SystemMediaPlayers};
use vocara_services::mic_recording_controller::MicRecordingController;
use vocara_store::sync::sync_engine::RemoteApplied;

use super::media::example_players::ExamplePlayers;
use super::media::technique_media_section::{
    self as media_section, rename_dialog, sync_players, AnalysisBuilder, Event as MediaEvent,
    MediaSectionState, TechniqueMediaSection,
};
use super::studio_field::{parse_comma_list, studio_field, studio_prose_field};
use super::technique_analysis_section as analysis;
use crate::app::library::RemoteAppliedBroadcast;
use crate::router::{Navigation, Route};
use crate::services::Services;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, bare_button, boxed, font, Studio};
use crate::ui::core::widgets::confirm_dialog::ConfirmDialog;
use crate::ui::core::widgets::spectrogram_painter::{release_spectrogram_images, RenderRequest};
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::song_practice_detail::widgets::voiceprint_view::{
    self as voiceprint, VoiceprintState,
};
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, Done, Effect, Job, Next, TechniqueDetailDeps, TechniqueDetailViewModel,
};
use crate::ui::{Screen, SnackBar, Update};

/// The header's title while editing.
pub const EDIT_TITLE: &str = "Edit technique";
/// The header's title before the technique has loaded.
pub const TITLE: &str = "Technique";
/// Over a load that failed.
pub const NOT_LOADED: &str = "This technique could not be loaded.";
/// The snack bar for a Save with the name cleared.
pub const NAME_REQUIRED: &str = "Give the technique a name.";
/// The snack bar after a save that did not persist.
pub const SAVE_FAILED: &str = "Couldn't save. Please try again.";
/// The snack bar after a delete that did not happen.
pub const DELETE_FAILED: &str = "Couldn't delete the technique.";
/// The unsaved-changes dialog's title.
pub const DISCARD_TITLE: &str = "Discard unsaved changes?";
/// The read view's placeholder for a field with nothing in it.
pub const EMPTY_CONTENT: &str = "Empty content";

/// How long the header ignores taps after it switches between edit and view
/// (Flutter's `kDoubleTapTimeout`).
pub const HEADER_SETTLE: Duration = Duration::from_millis(300);

/// How long a snack bar shows (Material's default, 4 s).
pub const SNACK_DURATION: Duration = Duration::from_millis(4000);

/// The edit form's Name field (autofocused when the form opens).
pub const NAME_FIELD: widget::Id = LineField::Name.id();

/// A single-line field of the edit form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineField {
    Name,
    Aliases,
    Tags,
}

impl LineField {
    /// The field's widget id.
    pub const fn id(self) -> widget::Id {
        widget::Id::new(match self {
            LineField::Name => "technique-detail-name",
            LineField::Aliases => "technique-detail-aliases",
            LineField::Tags => "technique-detail-tags",
        })
    }
}

/// A prose field of the edit form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProseField {
    Summary,
    Description,
    Cues,
    Exercises,
    Acoustic,
}

impl ProseField {
    /// The field's widget id.
    pub const fn id(self) -> widget::Id {
        widget::Id::new(match self {
            ProseField::Summary => "technique-detail-summary",
            ProseField::Description => "technique-detail-description",
            ProseField::Cues => "technique-detail-cues",
            ProseField::Exercises => "technique-detail-exercises",
            ProseField::Acoustic => "technique-detail-acoustic",
        })
    }
}

/// What the technique page reacts to.
#[derive(Debug, Clone)]
pub enum Message {
    /// The header's back chevron.
    Back,
    /// The unsaved-changes dialog Back asked closed.
    BackConfirmed(bool),
    /// The header's Edit.
    Edit,
    /// The header's Cancel (editing).
    Cancel,
    /// The unsaved-changes dialog Cancel asked closed.
    CancelConfirmed(bool),
    /// The header's Save (editing).
    Save,
    /// The header's Delete.
    Delete,
    /// The delete confirmation closed.
    DeleteConfirmed(bool),
    /// The header's settle time after a mode switch is up.
    HeaderSettled(u64),
    /// A single-line field of the edit form changed.
    Field(LineField, String),
    /// A prose field of the edit form was edited.
    Prose(ProseField, text_editor::Action),
    /// The examples section.
    Media(media_section::Message),
    /// An example's removal confirmation closed.
    RemoveConfirmed { id: String, confirmed: bool },
    /// The analysis panel.
    Analysis(analysis::Message),
    /// A view model job reported.
    Landed(Done),
    /// The recorder's timers.
    Tick,
    /// Sync applied remote changes (`Persistence.remoteApplied`).
    RemoteApplied(RemoteApplied),
    /// The showing snack bar's time is up.
    SnackExpired(u64),
}

/// The edit form: the text as typed, and the technique it was seeded from
/// (for the dirty comparison). It lives from Edit to the editor's teardown,
/// exactly as long as the view model's edit session.
pub struct EditForm {
    /// The technique the form was seeded from (`_editBase`).
    base: Technique,
    name: String,
    aliases: String,
    tags: String,
    summary: text_editor::Content,
    description: text_editor::Content,
    cues: text_editor::Content,
    exercises: text_editor::Content,
    acoustic: text_editor::Content,
    dirty: bool,
}

impl std::fmt::Debug for EditForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditForm")
            .field("name", &self.name)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

impl EditForm {
    /// `_seedForm(t)`.
    fn seed(t: &Technique) -> Self {
        Self {
            base: t.clone(),
            name: t.name.clone(),
            aliases: t.aliases.join(", "),
            tags: t.tags.join(", "),
            summary: text_editor::Content::with_text(&t.summary),
            description: text_editor::Content::with_text(&t.description),
            cues: text_editor::Content::with_text(&t.cues),
            exercises: text_editor::Content::with_text(&t.exercises),
            acoustic: text_editor::Content::with_text(&t.acoustic_signature),
            dirty: false,
        }
    }

    /// The name as typed.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the text differs from what it was seeded with.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    fn prose(&mut self, field: ProseField) -> &mut text_editor::Content {
        match field {
            ProseField::Summary => &mut self.summary,
            ProseField::Description => &mut self.description,
            ProseField::Cues => &mut self.cues,
            ProseField::Exercises => &mut self.exercises,
            ProseField::Acoustic => &mut self.acoustic,
        }
    }

    /// `_recomputeDirty`.
    fn recompute_dirty(&mut self) {
        let t = &self.base;
        self.dirty = self.name != t.name
            || self.aliases != t.aliases.join(", ")
            || self.tags != t.tags.join(", ")
            || self.summary.text() != t.summary
            || self.description.text() != t.description
            || self.cues.text() != t.cues
            || self.exercises.text() != t.exercises
            || self.acoustic.text() != t.acoustic_signature;
    }

    /// The technique Save sends: `base` with the form's fields, trimmed
    /// (the aliases and tags parsed), and `name` (already trimmed).
    fn build(&self, base: &Technique, name: String) -> Technique {
        let trimmed = |c: &text_editor::Content| dart_string::trim(&c.text()).to_string();
        Technique {
            name,
            aliases: parse_comma_list(&self.aliases),
            tags: parse_comma_list(&self.tags),
            summary: trimmed(&self.summary),
            description: trimmed(&self.description),
            cues: trimmed(&self.cues),
            exercises: trimmed(&self.exercises),
            acoustic_signature: trimmed(&self.acoustic),
            ..base.clone()
        }
    }
}

/// What the page shows of the examples, as of its last update: the section
/// and the cards borrow it.
#[derive(Debug, Default)]
struct Shown {
    media: Vec<TechniqueMedia>,
    analyzed_id: Option<String>,
    expanded_id: Option<String>,
}

impl Shown {
    /// The edit view shows the staged draft (added/removed/renamed examples
    /// not yet saved); the read view the technique's own.
    fn of(vm: &TechniqueDetailViewModel, editing: bool) -> Self {
        let media = if editing {
            vm.effective_media()
        } else {
            vm.technique().map(|t| t.media.clone()).unwrap_or_default()
        };
        Self {
            media,
            analyzed_id: vm.reference_media().map(|m| m.id),
            expanded_id: vm.expanded_example_id(),
        }
    }
}

/// The examples section as the page shows it: editable (add/drop zone,
/// record, per-clip controls, frozen while the save commits) while editing;
/// play-only in the read view (export still offered: it changes nothing).
fn section_of<'a>(
    vm: &TechniqueDetailViewModel,
    shown: &'a Shown,
    editing: bool,
) -> TechniqueMediaSection<'a> {
    let mut s = TechniqueMediaSection::new(&shown.media);
    s.analyzed_id = shown.analyzed_id.as_deref();
    s.expanded_id = shown.expanded_id.as_deref();
    s.expandable = true;
    s.live_monitor = vm.live_monitor();
    s.mic_live = vm.mic_live();
    s.exportable = true;
    if editing {
        s.importing = vm.importing_media();
        s.enabled = !vm.saving();
        s.recordable = true;
        s.is_recording = vm.is_recording_example();
        s.saving_recording = vm.saving_example();
        s.starting_recording = vm.starting();
        s.can_record = vm.can_record_example();
        s.record_level = vm.record_level() as f32;
    } else {
        s.editable = false;
    }
    s
}

/// What handling one message asked for: the view model's jobs and the
/// Spectrogram renders, to run off the UI thread, and the rest of the
/// update.
struct Step {
    jobs: Vec<Job>,
    renders: Vec<RenderRequest>,
    update: Update<Message>,
}

impl Step {
    fn new() -> Self {
        Self {
            jobs: Vec::new(),
            renders: Vec::new(),
            update: Update::none(),
        }
    }

    fn task(&mut self, task: Task<Message>) {
        let update = std::mem::replace(&mut self.update, Update::none());
        self.update = update.and_task(task);
    }

    fn into_update(self) -> Update<Message> {
        let mut update = self.update;
        for job in self.jobs {
            update = update.and_task(job_task(job));
        }
        for render in self.renders {
            update = update.and_task(render.into_task(rendered));
        }
        update
    }
}

/// A finished Spectrogram render, as the page's message.
fn rendered(done: crate::ui::core::widgets::spectrogram_painter::Rendered) -> Message {
    Message::Analysis(analysis::Message::Voiceprint(
        voiceprint::Message::ImageRendered(done),
    ))
}

/// Runs a view model job on a blocking thread; what it reports comes back as
/// [`Message::Landed`] (its failure, if the worker was lost).
pub fn job_task(job: Job) -> Task<Message> {
    let lost = job.lost("the worker was lost");
    Task::perform(crate::util::blocking(move || job.run()), move |joined| {
        Message::Landed(joined.unwrap_or(lost))
    })
}

/// The platform save panel an example's export asks (rfd, on the export's
/// worker; on macOS it runs on the main thread for it): the destination, or
/// `None` on cancel. Blocking.
fn save_panel(suggested_name: &str) -> Result<Option<String>, PickError> {
    Ok(rfd::FileDialog::new()
        .set_file_name(suggested_name)
        .save_file()
        .map(|path| path.to_string_lossy().into_owned()))
}

/// The technique page.
pub struct TechniqueDetailScreen {
    /// The technique it shows (the route's id).
    pub technique_id: String,
    /// The view model (boxed: every page sits in one enum).
    vm: Box<TechniqueDetailViewModel>,
    /// Builds the examples' players.
    player_factory: Arc<dyn MediaPlayerFactory>,
    /// The examples' players, kept by example.
    players: ExamplePlayers,
    /// The examples section's hover and drag.
    media: MediaSectionState,
    /// The analysis card's Spectrogram image.
    voiceprint: VoiceprintState,
    /// What the sync engine announces after applying remote rows.
    remote_applied: Arc<RemoteAppliedBroadcast>,
    /// Whether the page shows the edit form (`_editing`).
    editing: bool,
    /// Set for a double-tap's span after the header switches between edit
    /// and view. Its buttons trade places there (view's Delete sits where
    /// Save was), so a double-click on Save, the save quick, opened the
    /// Delete dialog; a double-click on Edit cancelled the edit it opened.
    header_settling: bool,
    /// Tags the settle timer, so a stale one ends nothing.
    settle_seq: u64,
    /// The edit form, while editing (boxed: every page sits in one enum).
    form: Option<Box<EditForm>>,
    /// The examples as last shown.
    shown: Shown,
    /// The snack bars: the first shows, the rest wait their turn.
    snacks: VecDeque<String>,
    /// Tags the showing snack bar's timer.
    snack_seq: u64,
    /// Since when the first has shown.
    snack_since: Instant,
}

impl std::fmt::Debug for TechniqueDetailScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TechniqueDetailScreen")
            .field("technique_id", &self.technique_id)
            .field("vm", &self.vm)
            .field("editing", &self.editing)
            .field("form", &self.form)
            .finish_non_exhaustive()
    }
}

impl TechniqueDetailScreen {
    /// The page for technique `technique_id` over the app's services, its
    /// first load started: exports write where the platform save panel
    /// answers, the examples play on this build's players.
    pub fn new(technique_id: String, services: &Services) -> (Self, Task<Message>) {
        let deps = TechniqueDetailDeps::from_services(technique_id, services, save_panel);
        Self::with_deps(deps, services, Arc::new(SystemMediaPlayers))
    }

    /// The page over the given view model dependencies and players (a
    /// test's fakes), its first load started.
    pub fn with_deps(
        deps: TechniqueDetailDeps,
        services: &Services,
        players: Arc<dyn MediaPlayerFactory>,
    ) -> (Self, Task<Message>) {
        let (screen, step) = Self::open(deps, services, players);
        (screen, step.into_update().task)
    }

    fn open(
        deps: TechniqueDetailDeps,
        services: &Services,
        player_factory: Arc<dyn MediaPlayerFactory>,
    ) -> (Self, Step) {
        let technique_id = deps.technique_id.clone();
        let (vm, next) = TechniqueDetailViewModel::new(deps);
        let mut screen = Self {
            technique_id,
            vm: Box::new(vm),
            player_factory,
            players: ExamplePlayers::new(),
            media: MediaSectionState::new(),
            voiceprint: VoiceprintState::new(),
            remote_applied: services.library.remote_applied.clone(),
            editing: false,
            header_settling: false,
            settle_seq: 0,
            form: None,
            shown: Shown::default(),
            snacks: VecDeque::new(),
            snack_seq: 0,
            snack_since: Instant::now(),
        };
        let mut step = Step::new();
        screen.absorb(next, &mut step);
        screen.refresh(&mut step);
        (screen, step)
    }

    /// The view model.
    pub fn view_model(&self) -> &TechniqueDetailViewModel {
        &self.vm
    }

    /// Whether the page shows the edit form.
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// The edit form, while editing.
    pub fn form(&self) -> Option<&EditForm> {
        self.form.as_deref()
    }

    /// The snack bar showing, if any.
    pub fn snack(&self) -> Option<&str> {
        self.snacks.front().map(String::as_str)
    }

    /// The examples' players.
    pub fn players(&self) -> &ExamplePlayers {
        &self.players
    }

    /// Whether the edit form is what the page shows (`_editing && _name !=
    /// null`).
    fn edit_view(&self) -> bool {
        self.editing && self.form.is_some()
    }

    /// `_isDirtyEditing`.
    fn dirty_editing(&self) -> bool {
        self.editing && self.form.as_deref().is_some_and(EditForm::dirty)
    }

    /// While true Save is disabled: a media import is mid-flight (a just-
    /// stopped example still importing, OR a drag-drop/browse import
    /// copying) — the clip isn't in the draft yet, so saving now would
    /// persist the technique without it and orphan the copied file — OR the
    /// technique save itself is in flight (a double-click would run two
    /// commits against the same stale base hashes).
    fn save_blocked(&self) -> bool {
        self.vm.saving() || self.vm.saving_example() || self.vm.importing_media()
    }

    /// While true Cancel is disabled: the save is committing (or still
    /// staging the example take it finalizes), and Cancel → Discard would
    /// delete the staged media files the airborne commit is persisting rows
    /// for (committed rows pointing at deleted files). It also keeps the
    /// edit form alive until the save resolves either way.
    fn cancel_blocked(&self) -> bool {
        self.vm.saving()
    }

    /// Whether the analysis panel is built: inside the expanded example's
    /// card (not while editing with the mic live), or — recording an example
    /// in the editor, where the new clip has no card yet — under the record
    /// control.
    fn analysis_shown(&self) -> bool {
        let live = self.vm.live_monitor();
        let editing = self.edit_view();
        let in_card = self
            .shown
            .expanded_id
            .as_ref()
            .is_some_and(|id| self.shown.media.iter().any(|m| &m.id == id))
            && (!live || !editing);
        in_card || (editing && live)
    }

    // ------------------------------------------------------------ update

    fn handle(&mut self, message: Message) -> Step {
        let mut step = Step::new();
        // The header ignores taps for a double-tap's span after it switches.
        let header = matches!(
            message,
            Message::Back | Message::Edit | Message::Cancel | Message::Save | Message::Delete
        );
        if header && self.header_settling {
            self.refresh(&mut step);
            return step;
        }
        match message {
            Message::Back => self.back(&mut step),
            Message::BackConfirmed(confirmed) => {
                if confirmed {
                    step.update.navigation = Some(Navigation::PopOrGo(Route::Techniques));
                }
            }
            Message::Edit => self.enter_edit(&mut step),
            Message::Cancel => self.cancel_edit(&mut step),
            Message::CancelConfirmed(confirmed) => {
                if confirmed {
                    self.discard(&mut step);
                }
            }
            Message::Save => self.save(&mut step),
            Message::Delete => self.delete(&mut step),
            Message::DeleteConfirmed(confirmed) => {
                if confirmed {
                    let next = self.vm.delete();
                    self.absorb(next, &mut step);
                }
            }
            Message::HeaderSettled(seq) => {
                if seq == self.settle_seq {
                    self.header_settling = false;
                }
            }
            Message::Field(field, value) => {
                // Frozen while the save commits (the fields take no input
                // then; a late edit is dropped too).
                if let Some(form) = self.form.as_mut().filter(|_| !self.vm.saving()) {
                    match field {
                        LineField::Name => form.name = value,
                        LineField::Aliases => form.aliases = value,
                        LineField::Tags => form.tags = value,
                    }
                    form.recompute_dirty();
                }
            }
            Message::Prose(field, action) => {
                if let Some(form) = self.form.as_mut().filter(|_| !self.vm.saving()) {
                    form.prose(field).perform(action);
                    form.recompute_dirty();
                }
            }
            Message::Media(m) => self.media_message(m, &mut step),
            Message::RemoveConfirmed { id, confirmed } => {
                if confirmed {
                    let next = self.vm.remove_media(&id);
                    self.absorb(next, &mut step);
                }
            }
            Message::Analysis(m) => {
                let allow_practice = !self.edit_view();
                let next = analysis::update(&mut self.vm, &mut self.voiceprint, m, allow_practice);
                self.absorb(next, &mut step);
            }
            Message::Landed(done) => {
                let next = self.vm.land(done);
                self.absorb(next, &mut step);
            }
            Message::Tick => {
                let next = self.vm.tick();
                self.absorb(next, &mut step);
            }
            Message::RemoteApplied(event) => {
                let next = self.vm.on_remote_applied(&event);
                self.absorb(next, &mut step);
            }
            Message::SnackExpired(seq) => {
                if seq == self.snack_seq
                    && self.snacks.pop_front().is_some()
                    && !self.snacks.is_empty()
                {
                    self.snack_shows();
                }
            }
        }
        self.refresh(&mut step);
        step
    }

    /// Takes in what a view model call asked: its jobs to run, its signals
    /// to the players, and its answers (the Dart screen's awaits resuming).
    fn absorb(&mut self, next: Next, step: &mut Step) {
        for effect in next.effects {
            match effect {
                Effect::Run(job) => step.jobs.push(job),
                Effect::PauseExamples => self.players.pause_all(),
                Effect::SeekAnalyzed(to) => {
                    if let Some(analyzed) = self.vm.reference_media() {
                        self.players.seek_analyzed(&analyzed.id, to);
                    }
                }
            }
        }
        for answer in next.answers {
            self.answer(answer, step);
        }
    }

    fn answer(&mut self, answer: Answer, step: &mut Step) {
        match answer {
            // `_record` / `_RecordRow._start`: an error from the start is
            // said.
            Answer::PracticeStart(Some(error)) | Answer::ExampleStart(Some(error)) => {
                self.snack_into(&error);
            }
            Answer::PracticeStart(None) | Answer::ExampleStart(None) | Answer::ExampleStop(_) => {}
            Answer::Saved(ok) => {
                if !ok {
                    // Stay in edit so the staged draft (and the files copied
                    // for it) isn't stranded; the user can retry. Leaving now
                    // would orphan those files.
                    self.snack_into(SAVE_FAILED);
                    return;
                }
                self.close_editor(step);
            }
            // Held off by a save that began while the dialog was open (a
            // quit committing the example): the edit stays open on the
            // draft, as Cancel is not offered during a save.
            Answer::DraftDiscarded(ran) => {
                if ran {
                    self.close_editor(step);
                }
            }
            Answer::Deleted(ok) => {
                if ok {
                    step.update.navigation = Some(Navigation::PopOrGo(Route::Techniques));
                } else {
                    self.snack_into(DELETE_FAILED);
                }
            }
        }
    }

    /// After every message, what Dart's rebuild did: the examples shown and
    /// their players, the analysis panel's post-frame analysis and its
    /// Spectrogram image, and the one-shot recording notice.
    fn refresh(&mut self, step: &mut Step) {
        self.shown = Shown::of(&self.vm, self.edit_view());
        sync_players(&mut self.players, &self.shown.media, &*self.player_factory);
        if self.analysis_shown() {
            // Compute the reference analysis lazily once the example clip is
            // known.
            let next = self.vm.ensure_reference_analysis();
            self.absorb(next, step);
            if self.vm.analysis_dim() == 0 {
                step.renders.extend(self.voiceprint.sync_request(&*self.vm));
            } else {
                self.voiceprint.clear();
            }
        } else {
            self.voiceprint.clear();
        }
        // Surface a one-shot async recording notice (e.g. the input device
        // was lost), then clear it so it shows exactly once.
        if let Some(notice) = self.vm.recording_notice().map(str::to_string) {
            self.vm.clear_recording_notice();
            self.snack_into(&notice);
        }
    }

    /// `_setEditing`: switches the mode and has the header settle.
    fn set_editing(&mut self, editing: bool, step: &mut Step) {
        self.editing = editing;
        self.header_settling = true;
        self.settle_seq += 1;
        let seq = self.settle_seq;
        step.task(Task::perform(
            async { tokio::time::sleep(HEADER_SETTLE).await },
            move |_| Message::HeaderSettled(seq),
        ));
    }

    /// `_disposeForm`: the single point BOTH exits funnel through (cancel,
    /// successful save) and also the screen's own dispose — so the view
    /// model's edit flag has exactly the form's lifetime, which is the
    /// invariant it protects.
    fn dispose_form(&mut self) -> Next {
        if self.form.take().is_none() {
            return Next::default();
        }
        self.vm.end_edit()
    }

    /// The editor's exit: the form torn down, back to the read view.
    fn close_editor(&mut self, step: &mut Step) {
        let next = self.dispose_form();
        self.absorb(next, step);
        self.set_editing(false, step);
    }

    /// `_handleBack`.
    fn back(&mut self, step: &mut Step) {
        if self.dirty_editing() {
            step.update.dialog = discard_dialog(Message::BackConfirmed).dialog;
            return;
        }
        step.update.navigation = Some(Navigation::PopOrGo(Route::Techniques));
    }

    /// `_enterEdit`.
    fn enter_edit(&mut self, step: &mut Step) {
        // A practice take recording (or still starting) would be stranded in
        // the editor, which has no practice Stop. See
        // `TechniqueDetailViewModel::can_edit`.
        if !self.vm.can_edit() || self.editing {
            return;
        }
        // Seed from the technique as of THIS tap, not the one the header was
        // last built with: a click landing after a remote reload but before
        // the next frame used to seed the pre-reload text while the base
        // hashes already described the remote row, and a Save with nothing
        // typed then wrote the old text back over the remote change.
        let Some(t) = self.vm.technique().cloned() else {
            return;
        };
        self.vm.begin_media_draft();
        // Tell the view model the form is open BEFORE seeding it: from here
        // until the form's teardown, the base hashes must stay pinned to this
        // snapshot, or a remote change landing mid-edit disarms the save's
        // keep-theirs branch.
        self.vm.begin_edit();
        self.form = Some(Box::new(EditForm::seed(&t)));
        self.set_editing(true, step);
        // `autofocus: true` on the Name field.
        step.task(widget::operation::focus(NAME_FIELD));
    }

    /// `_cancelEdit`.
    fn cancel_edit(&mut self, step: &mut Step) {
        if !self.edit_view() || self.cancel_blocked() {
            return;
        }
        let vm = &self.vm;
        // Dirty if text fields OR the staged example edits changed OR a
        // recording is in flight OR a just-stopped example is still being
        // imported. saving_example matters because stop_example_recording
        // clears is_recording_example and only adds the clip to the media
        // draft AFTER its import — so during that window all the other flags
        // read false and Cancel would skip the discard confirm, tear down the
        // session, then strand the imported file + lose the recording.
        // Prompting keeps the session alive until the import lands (so
        // discard_media_draft can clean it up) or the user keeps editing.
        let dirty = self.form.as_deref().is_some_and(EditForm::dirty)
            || vm.has_media_draft()
            || vm.is_recording_example()
            || vm.saving_example()
            || vm.starting()
            // A drag-drop/browse import in flight: add_media only folds the
            // copied files into the draft AFTER its copies, so like
            // saving_example the other flags read false meanwhile and Cancel
            // would skip the confirm + tear the session down mid-copy.
            || vm.importing_media();
        if dirty {
            step.update.dialog = discard_dialog(Message::CancelConfirmed).dialog;
            return;
        }
        self.discard(step);
    }

    /// The rest of `_cancelEdit` once the user chose to discard.
    fn discard(&mut self, step: &mut Step) {
        if !self.edit_view() {
            return;
        }
        let next = self.vm.discard_media_draft();
        self.absorb(next, step);
    }

    /// `_save`.
    fn save(&mut self, step: &mut Step) {
        if !self.edit_view() || self.save_blocked() {
            return;
        }
        let (Some(base), Some(form)) = (self.vm.technique(), self.form.as_deref()) else {
            return;
        };
        let name = dart_string::trim(&form.name).to_string();
        // Said, not quietly put back: Save kept the old name and closed the
        // editor as if the cleared one had been saved.
        if name.is_empty() {
            self.snack_into(NAME_REQUIRED);
            return;
        }
        // Built from the form BEFORE the save runs: an in-flight example
        // recording is finalized by the view model's save itself (stopped,
        // imported and committed under the quit gate, with Cancel and the
        // form frozen throughout). The media comes from the view model's
        // draft at commit time, so a clip staged by that stop is still saved.
        let built = form.build(base, name);
        let next = self.vm.save(built);
        self.absorb(next, step);
    }

    /// `_delete`: confirm, then delete.
    fn delete(&mut self, step: &mut Step) {
        if self.editing {
            return;
        }
        let Some(t) = self.vm.technique() else {
            return;
        };
        let dialog = ConfirmDialog::new(format!("Delete \"{}\"?", t.name))
            .body("This removes the technique and its example clips. This cannot be undone.");
        step.update.dialog =
            Update::dialog(dialog, |r| Message::DeleteConfirmed(r.confirmed())).dialog;
    }

    /// A message of the examples section: the callback it stands for.
    fn media_message(&mut self, message: media_section::Message, step: &mut Step) {
        let editing = self.edit_view();
        let section = section_of(&self.vm, &self.shown, editing);
        let Some(event) =
            self.media
                .update(message, &section, &mut self.players, &*self.player_factory)
        else {
            return;
        };
        let next = match event {
            MediaEvent::AddPaths(paths) => self.vm.add_media(paths),
            MediaEvent::Browse => {
                step.task(media_section::browse().map(Message::Media));
                return;
            }
            MediaEvent::Remove(id) => {
                // `_removeMedia`. One added in this edit goes at once: Cancel
                // would not bring it back (and discards it anyway).
                let body = if self.vm.is_unsaved_media(&id) {
                    "This example hasn't been saved yet. Removing it deletes it."
                } else {
                    "It's removed from this technique when you save. Cancel the edit to keep it."
                };
                let dialog = ConfirmDialog::new("Remove this example?")
                    .body(body)
                    .confirm_label("Remove");
                step.update.dialog = Update::dialog(dialog, move |r| Message::RemoveConfirmed {
                    id,
                    confirmed: r.confirmed(),
                })
                .dialog;
                return;
            }
            MediaEvent::Rename { id, current } => {
                let (dialog, on_result) = rename_dialog(id, &current);
                step.update.dialog =
                    Update::dialog(dialog, move |r| Message::Media(on_result(r))).dialog;
                return;
            }
            MediaEvent::Renamed { id, label } => {
                self.vm.rename_media(&id, &label);
                return;
            }
            MediaEvent::Export(id) => self.vm.export_media(&id),
            MediaEvent::ToggleExpand(id) => self.vm.toggle_example_expanded(&id),
            MediaEvent::StartRecord => self.vm.start_example_recording(),
            MediaEvent::StopRecord => self.vm.stop_example_recording(),
            MediaEvent::Position(at) => {
                self.vm.set_playhead_from_playback(at);
                return;
            }
        };
        self.absorb(next, step);
    }

    /// Shows `text` in a snack bar (now, or after the ones before it).
    fn snack_into(&mut self, text: &str) {
        self.snacks.push_back(text.to_string());
        if self.snacks.len() == 1 {
            self.snack_shows();
        }
    }

    /// The first snack bar shows from now, for [`SNACK_DURATION`] (timed by
    /// the subscription).
    fn snack_shows(&mut self) {
        self.snack_seq += 1;
        self.snack_since = Instant::now();
    }

    // -------------------------------------------------------------- view

    fn header(&self) -> Element<'_, Message> {
        let vm = &self.vm;
        let technique = vm.technique();
        let title = if self.editing {
            EDIT_TITLE
        } else {
            technique.map_or(TITLE, |t| t.name.as_str())
        };
        let mut bar = Row::new().height(Length::Fill).align_y(Vertical::Center);
        // The back chevron only exists in view mode; editing is exited
        // through Cancel/Save, so there's no "back" affordance to confuse it.
        if !self.editing {
            bar = bar
                .push(
                    button(
                        container(icons::icon(Icon::ChevronLeft, 20.0, Studio::TEXT_SECONDARY))
                            .center_x(32)
                            .center_y(32),
                    )
                    .padding(0)
                    .style(bare_button)
                    .on_press(Message::Back),
                )
                .push(Space::new().width(6));
        } else {
            bar = bar.push(Space::new().width(8));
        }
        // Filling (not shrinking with a spacer) so the title takes the
        // middle and the action buttons sit flush against the right edge.
        bar = bar
            .push(
                container(
                    text(title)
                        .size(18)
                        .font(font::BOLD)
                        .color(Studio::TEXT_PRIMARY)
                        .wrapping(Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
            )
            .push(Space::new().width(12));
        if self.editing {
            bar = bar
                .push(studio_button("Cancel", Message::Cancel).enabled(!self.cancel_blocked()))
                .push(Space::new().width(8))
                .push(
                    studio_button("Save", Message::Save)
                        .icon(Icon::Check)
                        .variant(StudioButtonVariant::Accent)
                        .enabled(!self.save_blocked()),
                );
        } else if technique.is_some() {
            // Shown but disabled while a practice take is recording (or
            // starting): the editor has no practice Stop to end it.
            bar = bar
                .push(
                    studio_button("Edit", Message::Edit)
                        .icon(Icon::Pencil)
                        .enabled(vm.can_edit()),
                )
                .push(Space::new().width(8))
                .push(
                    studio_button("Delete", Message::Delete)
                        .icon(Icon::Trash2)
                        .variant(StudioButtonVariant::Danger),
                );
        }
        column![
            container(bar)
                .width(Length::Fill)
                .height(55)
                .padding(Padding {
                    left: 12.0,
                    right: 16.0,
                    ..Padding::ZERO
                })
                .style(theme::fill(Studio::SURFACE)),
            rule(),
        ]
        .into()
    }

    fn content(&self) -> Element<'_, Message> {
        let vm = &self.vm;
        if vm.load().running() && vm.technique().is_none() {
            return center(spinner(36.0, 4.0, Studio::TEXT_SECONDARY)).into();
        }
        let Some(t) = vm.technique() else {
            return center(text(NOT_LOADED).size(13).color(Studio::TEXT_SECONDARY)).into();
        };
        match self.form.as_deref().filter(|_| self.editing) {
            Some(form) => self.edit_form(form),
            None => self.viewer(t),
        }
    }

    /// The examples block: each example card pairs its player with a
    /// collapsible analysis (and, in the read view, a practice recorder).
    fn media_section(&self) -> Element<'_, Message> {
        let editing = self.edit_view();
        let section = section_of(&self.vm, &self.shown, editing);
        let vm: &TechniqueDetailViewModel = &self.vm;
        let image = &self.voiceprint;
        // Editing builds the examples — analysis shown, no practice recorder.
        let builder: AnalysisBuilder<'_, Message> =
            Box::new(move || analysis::view(vm, image, !editing).map(Message::Analysis));
        section.view(&self.media, &self.players, Some(builder), Message::Media)
    }

    /// The read-only view: the same fields as the edit form, in the same
    /// order, display only. Every field is shown; empty ones read "Empty
    /// content".
    fn viewer<'a>(&'a self, t: &'a Technique) -> Element<'a, Message> {
        let chips =
            |labels: &'a [String], tint: bool| (!labels.is_empty()).then(|| chip_row(labels, tint));
        let page = column![
            view_field("Aliases", chips(&t.aliases, false), None, false),
            Space::new().height(20),
            view_field("Tags", chips(&t.tags, true), None, false),
            Space::new().height(20),
            self.media_section(),
            Space::new().height(24),
            view_field("Summary", None, Some(&t.summary), false),
            Space::new().height(20),
            view_field("Description", None, Some(&t.description), false),
            Space::new().height(20),
            view_field("How to produce it", None, Some(&t.cues), false),
            Space::new().height(20),
            view_field("Exercises", None, Some(&t.exercises), false),
            Space::new().height(20),
            view_field(
                "Acoustic signature in Vocara",
                None,
                Some(&t.acoustic_signature),
                true
            ),
        ]
        .width(Length::Fill)
        .padding(Padding {
            top: 22.0,
            right: 28.0,
            bottom: 28.0,
            left: 28.0,
        });
        scrollable(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::scrollbar)
            .into()
    }

    /// The edit form: the labeled text fields, then the examples block,
    /// then the prose. Frozen while the save commits: the commit snapshotted
    /// the form and the media draft when it started, so text or media edits
    /// made during it would be silently thrown away when the save lands and
    /// the screen drops out of edit mode.
    fn edit_form<'a>(&'a self, form: &'a EditForm) -> Element<'a, Message> {
        let enabled = !self.vm.saving();
        let line = |label: &str, value: &'a str, hint: &str, field: LineField| {
            Element::from(
                studio_field(label, value)
                    .hint(hint)
                    .on_input(move |v| Message::Field(field, v))
                    .id(field.id())
                    .enabled(enabled),
            )
        };
        let prose = |label: &str,
                     content: &'a text_editor::Content,
                     hint: &str,
                     lines: (u16, u16),
                     field: ProseField| {
            Element::from(
                studio_prose_field(label, content, lines.0, lines.1)
                    .hint(hint)
                    .on_action(move |a| Message::Prose(field, a))
                    .id(field.id())
                    .enabled(enabled),
            )
        };
        let page: Column<'a, Message> = column![
            line("Name", &form.name, "e.g. Twang", LineField::Name),
            Space::new().height(16),
            line(
                "Aliases (comma separated)",
                &form.aliases,
                "e.g. Edge, Brassy quality",
                LineField::Aliases,
            ),
            Space::new().height(16),
            line(
                "Tags (comma separated)",
                &form.tags,
                "e.g. EVT, Voice quality, Resonance",
                LineField::Tags,
            ),
            Space::new().height(22),
            rule(),
            Space::new().height(20),
            // Examples sit right after the identity fields so they're
            // visible without scrolling past the long prose fields.
            self.media_section(),
            Space::new().height(22),
            rule(),
            Space::new().height(20),
            prose(
                "Summary",
                &form.summary,
                "One line shown on the card",
                (1, 2),
                ProseField::Summary,
            ),
            Space::new().height(16),
            prose(
                "Description",
                &form.description,
                "What the technique is",
                (3, 8),
                ProseField::Description,
            ),
            Space::new().height(16),
            prose(
                "How to produce it",
                &form.cues,
                "Cues, sensations, steps",
                (3, 8),
                ProseField::Cues,
            ),
            Space::new().height(16),
            prose(
                "Exercises",
                &form.exercises,
                "Drills to practise it",
                (2, 6),
                ProseField::Exercises,
            ),
            Space::new().height(16),
            prose(
                "Acoustic signature in Vocara",
                &form.acoustic,
                "How it reads in RING, tilt, CPP, formants",
                (2, 6),
                ProseField::Acoustic,
            ),
        ];
        scrollable(page.width(Length::Fill).padding(Padding {
            top: 20.0,
            right: 28.0,
            bottom: 28.0,
            left: 28.0,
        }))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::scrollbar)
        .into()
    }
}

impl Screen for TechniqueDetailScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        self.handle(message).into_update()
    }

    fn view(&self) -> Element<'_, Message> {
        let page = column![
            self.header(),
            container(self.content())
                .width(Length::Fill)
                .height(Length::Fill)
        ];
        let mut layers = stack![container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)];
        if let Some(snack) = self.snack() {
            layers = layers.push(snack_bar(snack));
        }
        layers.into()
    }

    /// The recorder's ticks while it asks for them, the players' poll, and
    /// the remote-applied events (with no sync configured they never fire).
    fn subscription(&self, _on_top: bool) -> Subscription<Message> {
        let mut subscriptions = vec![
            self.remote_applied
                .subscription()
                .map(Message::RemoteApplied),
            media_section::subscription(&self.players).map(Message::Media),
        ];
        if self.vm.next_tick_due().is_some() {
            subscriptions.push(
                iced::time::every(MicRecordingController::TICK_PERIOD).map(|_| Message::Tick),
            );
        }
        if !self.snacks.is_empty() {
            subscriptions.push(Subscription::run_with(self.snack_seq, snack_timer));
        }
        Subscription::batch(subscriptions)
    }

    /// The page left the stack: the form's teardown (closing the view
    /// model's edit session), then the view model's; the clean-up they hand
    /// over runs on threads of its own (nothing waits for it). The players
    /// stop with the page, and the cached Spectrogram images go.
    fn dispose(&mut self, _services: &Services) {
        let mut next = self.dispose_form();
        next.extend(self.vm.dispose());
        for job in next.jobs() {
            job.detach();
        }
        self.players = ExamplePlayers::new();
        self.voiceprint.clear();
        release_spectrogram_images();
    }

    /// The snack bar showing, for the time it has left, then the ones
    /// waiting, for all of theirs; each queued behind the ones before it.
    fn take_snack_bars(&mut self) -> Vec<SnackBar> {
        let look = snack_bar::<Infallible>;
        let mut snacks = std::mem::take(&mut self.snacks).into_iter();
        let showing = snacks.next().and_then(|text| {
            SnackBar::showing(text, SNACK_DURATION, self.snack_since, false, look)
        });
        showing
            .into_iter()
            .chain(snacks.map(|text| SnackBar::waiting(text, SNACK_DURATION, false, look)))
            .collect()
    }
}

/// Snack bar timer `seq`.
fn snack_timer(seq: &u64) -> impl iced::futures::Stream<Item = Message> {
    crate::ui::one_shot(SNACK_DURATION, Message::SnackExpired(*seq))
}

/// The unsaved-changes dialog (`_confirmDiscard`); `then` takes whether the
/// user chose to discard.
fn discard_dialog(then: fn(bool) -> Message) -> Update<Message> {
    Update::dialog(
        ConfirmDialog::new(DISCARD_TITLE)
            .body("Your edits to this technique have not been saved yet. This cannot be undone.")
            .confirm_label("Discard")
            .cancel_label("Keep editing"),
        move |r| then(r.confirmed()),
    )
}

/// A full-width 1 px line in `Studio.border`.
fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(Studio::BORDER))
        .into()
}

/// `_ViewField`: a labeled read-only field. Shows `child` (e.g. chips) or
/// `text`; when both are absent/blank it shows a muted "Empty content"
/// placeholder so every field stays visible.
fn view_field<'a>(
    label: &str,
    child: Option<Element<'a, Message>>,
    value: Option<&'a str>,
    accent: bool,
) -> Element<'a, Message> {
    let body: Element<'a, Message> = match (child, value) {
        (Some(child), _) => child,
        (None, Some(value)) if !dart_string::trim(value).is_empty() => text(value)
            .size(14)
            .line_height(1.5)
            .color(Studio::TEXT_PRIMARY)
            .into(),
        _ => text(EMPTY_CONTENT)
            .size(13)
            .font(Font {
                style: iced::font::Style::Italic,
                ..font::REGULAR
            })
            .color(Studio::TEXT_FAINT)
            .into(),
    };
    column![
        text(label.to_uppercase())
            .size(10.5)
            .font(font::BOLD)
            .color(if accent {
                Studio::ACCENT_BRIGHT
            } else {
                Studio::TEXT_FAINT
            }),
        Space::new().height(7),
        body,
    ]
    .width(Length::Fill)
    .into()
}

/// `_ChipRow`: the aliases (plain) or tags (tinted) as wrapping chips.
fn chip_row<'a>(labels: &'a [String], tint: bool) -> Element<'a, Message> {
    let (fill, border, fg) = if tint {
        (
            alpha(Studio::ACCENT, 0.12),
            alpha(Studio::ACCENT, 0.3),
            Studio::ACCENT_BRIGHT,
        )
    } else {
        (
            Studio::SURFACE_RAISED,
            Studio::BORDER,
            Studio::TEXT_SECONDARY,
        )
    };
    let chips = labels.iter().map(|label| {
        // 8 / 3 inside the 1 px border.
        container(
            text(label.as_str())
                .size(11.5)
                .font(font::SEMIBOLD)
                .color(fg),
        )
        .padding([4, 9])
        .style(move |_| boxed(Some(fill), 6.0, Some(border)))
        .into()
    });
    Row::with_children(chips)
        .spacing(6)
        .align_y(Vertical::Center)
        .wrap()
        .vertical_spacing(6)
        .into()
}

/// Material 3's fixed snack bar along the page's bottom: the inverse
/// surface, 14 px text, 24 / 14 of padding, elevation 6. It takes its own
/// taps. (Of any message type: the app draws it too, once the page has
/// handed it over.)
fn snack_bar<M: 'static>(message: &str) -> Element<'_, M> {
    let bar = container(
        text(message)
            .size(14)
            .line_height(20.0 / 14.0)
            .color(theme::SNACK_BAR_DEFAULT_TEXT),
    )
    .width(Length::Fill)
    .padding([14, 24])
    .style(|_| container::Style {
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.3),
            offset: Vector::new(0.0, 3.0),
            blur_radius: 6.0,
        },
        ..boxed(Some(theme::SNACK_BAR_DEFAULT_BACKGROUND), 0.0, None)
    });
    container(opaque(bar))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Vertical::Bottom)
        .into()
}
