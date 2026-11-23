//! The technique page on screen, headless: the screen cases the view
//! model's port left (each listed in its test module's doc) —
//! `technique_detail_layout_test`, `technique_example_identity_test`,
//! `technique_example_player_test`, `technique_media_pool_guard_test`,
//! `technique_pending_start_test` and `technique_practice_take_test`.
//!
//! The page runs over the view model tests' fakes (a repository, a media
//! service, an analysis, a recorder; `tests::support`) and fake media
//! players. Its jobs run here, on the test thread, in order, when a test
//! says so; a kind can be held back where the Dart test parked its fake on
//! a completer. The header's settle timer is fired by hand.

use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use iced::{mouse, window, Event as IcedEvent, Point, Rectangle, Size};
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use vocara_model::config::environment::AppEnvironment;
use vocara_model::util::id::new_id;
use vocara_services::audio_exporter::AudioExporter;
use vocara_services::audio_file_service::AudioFileService;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::media_player::{FakeMediaPlayers, FakePlayerHandle};
use vocara_services::mic_recording_controller::MicRecordingStart;

use super::*;
use crate::app::library::Library;
use crate::ui::core::widgets::confirm_dialog::{Dialog, DialogResult};
use crate::ui::technique_library::view_model::technique_detail_view_model::tests::support::{
    example, remote, twang, write_file, FakeMedia, Outcome, Setup, StartMode, ID,
};
use crate::ui::technique_library::view_model::technique_detail_view_model::JobKind;
use crate::ui::technique_library::widgets::media::technique_audio_player::UNAVAILABLE_CLIP_MESSAGE;
use crate::ui::technique_library::widgets::media::technique_media_section::{ADD_FIRST, STARTING};
use crate::ui::DialogRequest;

/// Tall enough for the whole edit form, the examples block included.
const TALL: Size = Size::new(1200.0, 2600.0);

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

/// A technique with a body (the Dart tests' `_technique()`): aliases, tags,
/// prose, and `media`.
fn full(media: Vec<TechniqueMedia>) -> Technique {
    Technique {
        aliases: vec!["Edge".into()],
        tags: vec!["EVT".into(), "Voice quality".into()],
        summary: "Bright carrying quality.".into(),
        description: "A long description. ".repeat(20),
        cues: "Quack like a duck.".into(),
        ..twang(ID, media)
    }
}

/// The page, its fakes, and what it asked for.
struct Harness {
    screen: TechniqueDetailScreen,
    services: Services,
    setup: Setup,
    players: Arc<FakeMediaPlayers>,
    size: Size,
    /// Jobs started and not yet run, in order.
    jobs: VecDeque<Job>,
    /// Spectrogram renders asked for, not yet run.
    renders: VecDeque<RenderRequest>,
    held: HashSet<JobKind>,
    navigations: Vec<Navigation>,
    dialog: Option<DialogRequest<Message>>,
}

impl Harness {
    /// The page over `setup`'s fakes (its first load queued, not run).
    fn new(setup: Setup) -> Self {
        let deps = setup.deps();
        Self::with_deps(setup, deps)
    }

    fn with_deps(setup: Setup, deps: TechniqueDetailDeps) -> Self {
        let services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
        let players = Arc::new(FakeMediaPlayers::new());
        let (screen, step) = TechniqueDetailScreen::open(deps, &services, players.clone());
        let mut h = Self {
            screen,
            services,
            setup,
            players,
            size: TALL,
            jobs: VecDeque::new(),
            renders: VecDeque::new(),
            held: HashSet::new(),
            navigations: Vec::new(),
            dialog: None,
        };
        h.absorb(step);
        h
    }

    /// The page over `setup`'s fakes, loaded.
    fn loaded(setup: Setup) -> Self {
        let mut h = Self::new(setup);
        h.settle();
        assert!(h.vm().technique().is_some(), "loaded");
        h
    }

    fn vm(&self) -> &TechniqueDetailViewModel {
        self.screen.view_model()
    }

    fn send(&mut self, message: Message) {
        let step = self.screen.handle(message);
        self.absorb(step);
    }

    fn absorb(&mut self, step: Step) {
        self.jobs.extend(step.jobs);
        self.renders.extend(step.renders);
        if let Some(navigation) = step.update.navigation {
            self.navigations.push(navigation);
        }
        if let Some(dialog) = step.update.dialog {
            self.dialog = Some(dialog);
        }
    }

    /// Runs and lands every job not held (and the renders), until none is
    /// left.
    fn settle(&mut self) {
        loop {
            if let Some(i) = self
                .jobs
                .iter()
                .position(|j| !self.held.contains(&j.kind()))
            {
                let job = self.jobs.remove(i).expect("queued");
                let done = job.run();
                self.send(Message::Landed(done));
                continue;
            }
            let Some(render) = self.renders.pop_front() else {
                return;
            };
            self.send(rendered(render.run()));
        }
    }

    /// Jobs of `kind` wait.
    fn hold(&mut self, kind: JobKind) {
        self.held.insert(kind);
    }

    /// Lets `kind` run again, and settles.
    fn release(&mut self, kind: JobKind) {
        self.held.remove(&kind);
        self.settle();
    }

    /// The header's settle time is up.
    fn settle_header(&mut self) {
        let seq = self.screen.settle_seq;
        self.send(Message::HeaderSettled(seq));
    }

    /// The open dialog.
    fn dialog(&self) -> &Dialog {
        &self.dialog.as_ref().expect("a dialog is open").dialog
    }

    /// Answers the open dialog.
    fn answer(&mut self, result: DialogResult) {
        let dialog = self.dialog.take().expect("a dialog is open");
        self.send((dialog.on_result)(result));
    }

    fn ui(&self) -> Simulator<'_, Message> {
        Simulator::with_size(settings(), self.size, self.screen.view())
    }

    /// What clicking the `n`-th `label` reports, not fed back.
    fn press_nth(&self, label: &str, n: usize) -> Vec<Message> {
        let mut ui = self.ui();
        ui.click(nth(label, n))
            .unwrap_or_else(|e| panic!("{label} #{n}: {e:?}"));
        ui.into_messages().collect()
    }

    /// What clicking `label` reports, not fed back, the pointer's
    /// entering a card (its hover) left out: whether the click tapped
    /// anything.
    fn press(&self, label: &str) -> Vec<Message> {
        let mut messages = self.press_nth(label, 0);
        messages.retain(|m| !matches!(m, Message::Media(media_section::Message::Hover(..))));
        messages
    }

    fn feed(&mut self, messages: Vec<Message>) {
        for m in messages {
            self.send(m);
        }
    }

    fn click_nth(&mut self, label: &str, n: usize) {
        let messages = self.press_nth(label, n);
        assert!(!messages.is_empty(), "{label} #{n} took the click");
        self.feed(messages);
    }

    fn click(&mut self, label: &str) {
        self.click_nth(label, 0);
    }

    fn find(&self, label: &str) -> Option<Rectangle> {
        self.ui().find(nth(label, 0)).ok().map(|t| t.bounds())
    }

    fn shows(&self, label: &str) -> bool {
        self.find(label).is_some()
    }

    fn count(&self, label: &str) -> usize {
        let mut ui = self.ui();
        (0..)
            .take_while(|&n| ui.find(nth(label, n)).is_ok())
            .count()
    }

    /// Opens the editor, as a tap on Edit does, and lets the header settle.
    fn edit(&mut self) {
        self.click("Edit");
        self.settle();
        self.settle_header();
        assert!(self.screen.editing(), "the editor opened");
    }

    /// What typing `text` into the field `id` reports (nothing when the
    /// field is frozen).
    fn typing(&self, id: widget::Id, text: &str) -> Vec<Message> {
        let mut ui = self.ui();
        ui.click(id).expect("the field");
        let _ = ui.typewrite(text);
        ui.into_messages().collect()
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
            events.push(IcedEvent::Window(window::Event::RedrawRequested(
                iced::time::Instant::now(),
            )));
            let _ = ui.simulate(events);
            ui.into_messages().collect()
        };
        self.feed(messages);
    }

    fn built(&self) -> Vec<FakePlayerHandle> {
        self.players.built()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // The page leaves: its clean-up runs.
        if !std::thread::panicking() {
            self.screen.dispose(&self.services);
        }
    }
}

// ------------------------------------------------------------ the layout

// technique_detail_layout_test: 'viewer with body + media lays out without
// overflow', and 'view mode with clips: Examples shown but play-only'.
#[test]
fn the_viewer_shows_the_fields_and_play_only_examples() {
    let media = vec![
        TechniqueMedia {
            kind: vocara_model::models::technique_media::TechniqueMediaKind::Audio,
            ..example("m1", "/tmp/a.m4a")
        },
        TechniqueMedia {
            kind: vocara_model::models::technique_media::TechniqueMediaKind::Video,
            ..example("m2", "/tmp/b.mp4")
        },
    ];
    let mut h = Harness::loaded(Setup::new(full(media)));
    h.size = Size::new(1200.0, 900.0);
    assert!(h.shows("Twang"), "the header names it");
    assert!(h.shows("EXAMPLES"));
    assert!(h.shows("a.m4a") && h.shows("b.mp4"));
    assert!(h.shows("Edge") && h.shows("EVT") && h.shows("Voice quality"));
    assert!(h.shows("Bright carrying quality."));
    assert!(h.shows("ACOUSTIC SIGNATURE IN VOCARA"));
    // The block shows, but with no add/drop zone (read-only).
    assert!(!h.shows(ADD_FIRST));
    assert_eq!(h.count(&glyph(Icon::Pencil)), 1, "Edit's, no rename");
    // The audio example's player opens now; the video's waits for its play.
    assert_eq!(h.built().len(), 1);
}

// technique_detail_layout_test: 'empty technique: view shows every field
// with placeholders'.
#[test]
fn an_empty_technique_shows_every_field_with_a_placeholder() {
    let h = Harness::loaded(Setup::new(twang(ID, vec![])));
    // Read view: Edit/Delete in the header, no Cancel/Save.
    assert!(h.shows("Edit"));
    assert!(h.shows("Delete"));
    assert!(!h.shows("Cancel"));
    assert!(!h.shows("Save"));
    // Every field stays visible; empty ones read "Empty content". The
    // Examples label shows too, but with no add/drop zone.
    assert!(h.shows("EXAMPLES"));
    assert!(!h.shows(ADD_FIRST));
    // Aliases, tags, examples, summary, description, cues, exercises, and
    // the acoustic signature.
    assert_eq!(h.count(EMPTY_CONTENT), 8);
}

// technique_detail_layout_test: 'a failed load shows the error state, not
// an endless spinner'. Regression: the screen watches only the view model,
// and the load's failure path notified nothing but the (unwatched) command —
// the spinner, keyed on load.running, was never rebuilt away.
#[test]
fn a_failed_load_shows_the_error_state_not_an_endless_spinner() {
    let setup = Setup::new(twang(ID, vec![]));
    setup.repo.st().unreadable = true;
    let mut h = Harness::new(setup);
    h.hold(JobKind::Load);
    h.settle();
    assert!(h.vm().load().running(), "the spinner shows");
    assert!(!h.shows(NOT_LOADED));
    assert!(!h.shows("Edit"));
    assert!(h.shows(TITLE));

    // The read fails only once the screen is up and showing the spinner.
    h.release(JobKind::Load);
    assert!(h.vm().load().error());
    assert!(h.shows(NOT_LOADED));
    assert!(!h.shows("Edit") && !h.shows("Delete"), "nothing to edit");
}

// technique_detail_layout_test: 'tapping Edit opens the form with
// Cancel/Save in header'.
#[test]
fn edit_opens_the_form_with_cancel_and_save_in_the_header() {
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])));
    assert!(h.shows(&glyph(Icon::ChevronLeft)));
    h.edit();
    // Edit mode shows the "Edit technique" title and drops the back chevron
    // — Cancel/Save are the exits.
    assert!(h.shows(EDIT_TITLE));
    assert!(!h.shows(&glyph(Icon::ChevronLeft)));
    assert!(h.shows("Cancel"));
    assert!(h.shows("Save"));
    assert!(!h.shows("Delete"));
    // Media is addable while editing too.
    assert!(h.shows("EXAMPLES"));
    assert!(h.shows(ADD_FIRST));
    assert!(h.shows("NAME"));
    assert_eq!(h.screen.form().map(EditForm::name), Some("Twang"));
}

// The header ignores taps for a double-tap's span after it switches: its
// buttons trade places there (view's Delete sits where Save was), so a
// double-click on Save opened the Delete dialog, and a double-click on Edit
// cancelled the edit it opened.
#[test]
fn the_header_ignores_a_double_click_across_a_mode_switch() {
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])));
    h.click("Edit");
    assert!(h.screen.editing());
    // The second click of the double-click lands on Cancel.
    h.click("Cancel");
    assert!(h.screen.editing(), "the edit it opened stays open");
    h.settle_header();
    h.click("Save");
    h.settle();
    assert!(!h.screen.editing(), "saved, back in the read view");
    // The second click of a double-click on Save lands on Delete.
    h.click("Delete");
    assert!(h.dialog.is_none(), "no Delete dialog");
    h.settle_header();
    h.click("Delete");
    assert!(matches!(h.dialog(), Dialog::Confirm(d) if d.title == "Delete \"Twang\"?"));
}

// technique_detail_layout_test: 'dropping only unsupported files reports
// the skip'. Regression guard: the drop zone used to filter unsupported
// files out BEFORE calling the view model, so a drop of only unsupported
// files did nothing at all — no import, and the notice the view model
// already had for exactly this case was unreachable.
#[test]
fn dropping_only_unsupported_files_reports_the_skip() {
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])));
    h.edit();
    // On the zone itself: a drop is taken only where it can be seen.
    let zone = h.find(ADD_FIRST).expect("the add zone").center();
    h.drop_at(zone, &["/tmp/notes.pdf"]);
    h.settle();
    assert!(h.vm().effective_media().is_empty());
    assert_eq!(
        h.screen.snack(),
        Some("No supported audio or video files in the drop."),
        "a drop that imports nothing must say so"
    );
}

// technique_media_pool_guard_test: 'a lone clip refused for its size says
// so' — the refusal's words reach the snack bar.
#[test]
fn a_lone_clip_refused_for_its_size_says_so() {
    let mut media = FakeMedia::sandbox();
    media.refuse = Some("This file is larger than 2 GB.".into());
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])).media(media));
    h.edit();
    let zone = h.find(ADD_FIRST).expect("the add zone").center();
    h.drop_at(zone, &["/tmp/huge.wav"]);
    assert!(h.vm().importing_media());
    h.settle();
    assert!(h.vm().effective_media().is_empty());
    assert_eq!(h.screen.snack(), Some("This file is larger than 2 GB."));
}

// technique_detail_layout_test: 'view mode: header buttons hug the right
// edge'.
#[test]
fn the_header_buttons_hug_the_right_edge() {
    let mut h = Harness::loaded(Setup::new(full(vec![])));
    h.size = Size::new(1400.0, 900.0);
    assert!(h.shows("Twang"));
    assert_eq!(h.count("Edit"), 1);
    assert_eq!(h.count("Delete"), 1);
    // The action buttons sit flush right, not floating in the middle.
    let delete = h.find("Delete").expect("Delete");
    assert!(
        delete.x + delete.width > 1400.0 - 60.0,
        "Delete should hug the right edge: {delete:?}"
    );
}

// technique_detail_layout_test: 'an example can be exported, from the read
// view too'. A recorded example was reachable only by digging through the
// app's container, its file named by a hash.
#[test]
fn an_example_can_be_exported_from_the_read_view_too() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_file(dir.path(), "take.wav", &[1, 2, 3, 4]);
    let dest = dir.path().join("out").join("exported.wav");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let suggested = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let picked = dest.to_string_lossy().into_owned();
    let asked = suggested.clone();
    let setup = Setup::new(twang(
        ID,
        vec![TechniqueMedia {
            label: "Recording 1".into(),
            ..example("m1", &source)
        }],
    ));
    let exporter = AudioExporter::new(
        Arc::new(AudioFileService::new(Arc::new(
            LocalStorageService::with_root(dir.path().join("app")),
        ))),
        move |name| {
            asked.lock().unwrap().push(name.to_string());
            Ok(Some(picked.clone()))
        },
    )
    .with_finalizer(Arc::clone(&setup.finalizer));
    let mut h = Harness::loaded(setup.exporter(exporter));
    assert_eq!(h.count(&glyph(Icon::Download)), 1);

    h.click(&glyph(Icon::Download));
    h.settle();
    assert_eq!(*suggested.lock().unwrap(), ["Twang - Recording 1.wav"]);
    assert_eq!(std::fs::read(&dest).unwrap(), [1, 2, 3, 4]);
    assert_eq!(
        h.screen.snack(),
        Some(format!("Exported to {}", dest.display()).as_str())
    );
}

// technique_detail_layout_test: 'saving with the name cleared says so and
// keeps editing'. Save kept the old name and closed the editor as if the
// cleared one had been saved.
#[test]
fn saving_with_the_name_cleared_says_so_and_keeps_editing() {
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])));
    h.edit();
    h.send(Message::Field(LineField::Name, "  ".into()));
    h.click("Save");
    h.settle();
    assert_eq!(h.screen.snack(), Some(NAME_REQUIRED));
    assert!(h.shows(EDIT_TITLE));
    assert_eq!(h.vm().technique().unwrap().name, "Twang");
    assert_eq!(h.setup.repo.st().base_saves, 0);
}

/// Whether the edit form takes typing (its Name and Summary fields).
fn fields_enabled(h: &Harness) -> bool {
    let name = !h.typing(LineField::Name.id(), "x").is_empty();
    let summary = !h.typing(ProseField::Summary.id(), "x").is_empty();
    assert_eq!(name, summary, "the form freezes as one");
    name
}

// technique_detail_layout_test: 'Save and Cancel are disabled while the
// save is in flight'.
#[test]
fn save_and_cancel_are_disabled_while_the_save_is_in_flight() {
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])));
    h.edit();
    assert!(!h.press("Save").is_empty(), "Save is live");
    assert!(!h.press("Cancel").is_empty(), "Cancel is live");
    assert!(fields_enabled(&h));

    // Start a save; the repository write is held so it stays in flight.
    h.hold(JobKind::Commit);
    h.click("Save");
    h.settle();
    assert!(h.vm().saving());
    assert!(h.press("Save").is_empty(), "no double commit");
    assert!(
        h.press("Cancel").is_empty(),
        "Cancel -> Discard would delete files the commit references"
    );
    // The FORM freezes too: text typed during the commit would be thrown
    // away when the save lands and the screen drops out of edit mode.
    assert!(
        !fields_enabled(&h),
        "every field is frozen while it commits"
    );
    // A late Cancel (a tap already in flight) opens no discard dialog,
    // which would tear down the edit session mid-save.
    h.send(Message::Cancel);
    assert!(h.dialog.is_none());

    // Release the save: it completes, the screen leaves edit mode.
    h.release(JobKind::Commit);
    assert_eq!(h.setup.repo.st().base_saves, 1);
    assert!(!h.shows("Cancel"), "back in the read view");
    assert!(h.shows("Edit"));
}

// technique_detail_layout_test: 'Save while an example take records keeps
// Cancel and the form frozen until the take and the edits are committed'.
// Regression: Save stopped the take and awaited its import with Cancel
// still live. Cancel → Discard in that window tore the form down, and the
// resuming Save then dereferenced the disposed controllers (or, with the
// dialog still open when the import landed, committed anyway).
#[test]
fn save_while_an_example_take_records_keeps_cancel_and_the_form_frozen() {
    let setup = Setup::new(twang(ID, vec![])).media(FakeMedia::sandbox());
    setup.recorder.st().mode = StartMode::At("/tmp/vocara_no_such_take_dir/take.wav".into());
    let mut h = Harness::loaded(setup);
    h.edit();
    h.send(Message::Field(LineField::Name, "Twang, revised".into()));
    h.click("Record an example");
    h.settle();
    assert!(h.vm().is_recording_example());

    // Save: the take stops and its import is held.
    h.hold(JobKind::Import);
    h.click("Save");
    h.settle();
    assert!(!h.vm().is_recording_example(), "Save released the mic");
    assert!(h.vm().saving_example(), "the take is still importing");
    assert!(
        h.press("Cancel").is_empty(),
        "a Discard now would tear the form down under the save"
    );
    assert!(h.press("Save").is_empty());
    assert!(
        !fields_enabled(&h),
        "the edits were already read for this save"
    );
    h.send(Message::Cancel);
    assert!(h.dialog.is_none());

    // The import lands, then the commit: the take AND the typed edit
    // persist, and the editor closes.
    h.release(JobKind::Import);
    let stored = h.setup.repo.technique();
    assert_eq!(stored.name, "Twang, revised");
    assert_eq!(
        stored
            .media
            .iter()
            .map(|m| m.path.as_str())
            .collect::<Vec<_>>(),
        ["/sandbox/take.wav"]
    );
    assert!(!h.shows("Cancel"), "back in the read view");
}

// The editor's Cancel: a clean form closes at once; a dirty one asks first,
// and "Keep editing" keeps it (technique_detail_journey_test's Cancel).
#[test]
fn cancel_asks_before_discarding_typed_edits() {
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])));
    h.edit();
    h.click("Cancel");
    assert!(h.dialog.is_none(), "nothing to lose: no prompt");
    assert!(!h.screen.editing());
    h.settle_header();

    h.edit();
    h.send(Message::Field(LineField::Name, "Should Not Persist".into()));
    h.click("Cancel");
    assert!(
        matches!(h.dialog(), Dialog::Confirm(d) if d.title == DISCARD_TITLE
        && d.confirm_label == "Discard" && d.cancel_label == "Keep editing")
    );
    h.answer(DialogResult::Confirmed(false));
    assert!(h.screen.editing(), "kept editing");
    h.click("Cancel");
    h.answer(DialogResult::Confirmed(true));
    h.settle();
    assert!(!h.screen.editing());
    assert_eq!(h.vm().technique().unwrap().name, "Twang");
    assert_eq!(h.setup.repo.st().base_saves, 0);
}

// The read view's Delete: confirmed, it deletes and goes back to the
// library (deep_link_back_test's detail part: a deep-linked page falls back
// to the library); a failed one says so and stays.
#[test]
fn delete_confirms_then_goes_back_to_the_library() {
    let mut h = Harness::loaded(Setup::new(twang(ID, vec![])));
    h.click("Delete");
    let Dialog::Confirm(d) = h.dialog() else {
        panic!("a confirm dialog")
    };
    assert_eq!(d.title, "Delete \"Twang\"?");
    assert_eq!(
        d.body.as_deref(),
        Some("This removes the technique and its example clips. This cannot be undone.")
    );
    h.answer(DialogResult::Confirmed(false));
    assert_eq!(h.setup.repo.st().deletes, 0);

    h.click("Delete");
    h.answer(DialogResult::Confirmed(true));
    h.settle();
    assert_eq!(h.setup.repo.st().deletes, 1);
    assert_eq!(
        h.navigations,
        [Navigation::PopOrGo(Route::Techniques)],
        "back to the library"
    );
}

// --------------------------------------------------- the examples' identity

// technique_example_identity_test: 'two byte-identical examples open and
// select independently' — on the real repository, whose pool dedups the
// bytes: one file, two examples. Only the selected example's card shows
// the analysis.
#[test]
fn two_byte_identical_examples_open_and_select_independently() {
    let library = Library::in_memory();
    let repo = library.technique_repo.clone();
    let src = tempfile::tempdir().unwrap();
    let id = new_id();
    let ids = [new_id(), new_id()];
    let media = ids
        .iter()
        .enumerate()
        .map(|(i, mid)| TechniqueMedia {
            label: format!("Take {i}"),
            ..example(
                mid,
                &write_file(src.path(), &format!("take {i}.wav"), &[9, 9, 9]),
            )
        })
        .collect();
    repo.save_technique(&twang(&id, media)).unwrap();
    let setup = Setup::new(twang(&id, vec![]));
    let deps = TechniqueDetailDeps {
        repository: repo,
        ..setup.deps()
    };
    let mut h = Harness::with_deps(setup, deps);
    h.settle();
    let paths: HashSet<String> = h
        .vm()
        .technique()
        .unwrap()
        .media
        .iter()
        .map(|m| m.path.clone())
        .collect();
    assert_eq!(paths.len(), 1, "the pool dedups identical bytes");

    // Only the selected example shows its analysis (one dimension bar).
    assert_eq!(h.count("Spectrogram"), 1);
    assert_eq!(h.vm().reference_media().unwrap().id, ids[0]);

    // Expanding the second card selects IT, not the first sharing its file.
    let chevrons = h.count(&glyph(Icon::ChevronDown));
    h.click_nth(&glyph(Icon::ChevronDown), chevrons - 1);
    h.settle();
    assert_eq!(h.vm().reference_media().unwrap().id, ids[1]);
    assert_eq!(h.count("Spectrogram"), 1);
}

// ------------------------------------------------------ the example players

/// Dart's `_technique()` for the player tests: two audio examples, M0 and
/// M1, under a description long enough to scroll them far out of view.
fn two_examples() -> Technique {
    Technique {
        description: vec!["A long paragraph of cues."; 400].join("\n"),
        ..twang(
            ID,
            vec![
                TechniqueMedia {
                    label: "Example 0".into(),
                    ..example("M0", "/tmp/example-0.wav")
                },
                TechniqueMedia {
                    label: "Example 1".into(),
                    ..example("M1", "/tmp/example-1.wav")
                },
            ],
        )
    }
}

/// The placeholder `asset://` path the repository hydrates an example to
/// when its pooled bytes are missing on this device.
const MISSING: &str = "asset://01J00000000000000000000000";

// technique_example_player_test: 'scrolling the examples out of view keeps
// their players (playback does not stop or reset)'.
#[test]
fn scrolling_the_examples_out_of_view_keeps_their_players() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.size = Size::new(1200.0, 900.0);
    assert_eq!(h.built().len(), 2);
    {
        let mut ui = h.ui();
        ui.point_at(Point::new(600.0, 500.0));
        let wheel = |y| {
            IcedEvent::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y },
            })
        };
        let _ = ui.simulate([wheel(-6000.0), wheel(6000.0)]);
    }
    h.settle();
    assert_eq!(h.built().len(), 2, "no fresh player");
    assert!(!h.built().iter().any(FakePlayerHandle::disposed));
}

// technique_example_player_test: 'a clip whose file arrives in a reload
// gets its player'. The player read "not on this device" once, at mount:
// the same clip handed its real file by a reload kept saying so.
#[test]
fn a_clip_whose_file_arrives_in_a_reload_gets_its_player() {
    let technique = two_examples();
    let mut missing = technique.clone();
    missing.media[1].path = MISSING.into();
    let mut h = Harness::loaded(Setup::new(missing).analysis(Outcome::Nothing));
    h.size = Size::new(1200.0, 900.0);
    assert!(h.shows(UNAVAILABLE_CLIP_MESSAGE));
    assert_eq!(h.built().len(), 1);

    // Its bytes arrived.
    h.setup.repo.st().technique = Some(technique);
    h.send(Message::RemoteApplied(remote(ID)));
    h.settle();
    assert!(!h.shows(UNAVAILABLE_CLIP_MESSAGE));
    assert_eq!(h.built().len(), 2);
}

// technique_example_player_test: 'removing an earlier example keeps a later
// card's player'. Regression: the cards were unkeyed, so removing one
// handed every later card's slot to the next clip and rebuilt its player —
// the example playing below the removed one stopped.
#[test]
fn removing_an_earlier_example_keeps_a_later_cards_player() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.edit();
    let built = h.built();
    assert_eq!(built.len(), 2);

    // M0's trash (the header shows none while editing), then the prompt.
    h.click(&glyph(Icon::Trash2));
    let Dialog::Confirm(d) = h.dialog() else {
        panic!("a confirm dialog")
    };
    assert_eq!(d.title, "Remove this example?");
    assert_eq!(d.confirm_label, "Remove");
    assert_eq!(
        d.body.as_deref(),
        Some("It's removed from this technique when you save. Cancel the edit to keep it.")
    );
    h.answer(DialogResult::Confirmed(true));
    h.settle();

    assert_eq!(h.built().len(), 2, "M1 kept its player");
    assert!(built[0].disposed(), "only M0's went");
    assert!(!built[1].disposed());
    assert!(h.shows("Example 1") && !h.shows("Example 0"));
}

// technique_example_player_test: 'starting a practice take pauses every
// example player'. Regression: the examples play through the media player,
// not the engine, so the engine's echo canceller has no reference to
// subtract — a clip left playing on speakers was recorded into the take it
// was compared with.
#[test]
fn starting_a_practice_take_pauses_every_example_player() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.size = Size::new(1200.0, 1400.0);
    let built = h.built();
    assert_eq!(built.len(), 2);

    h.click(analysis::RECORD_PRACTICE);
    h.settle();
    assert!(h.vm().is_recording_practice());
    assert!(built.iter().all(|p| p.pauses() > 0));

    // Nor can one be started again until the take ends: paused only at the
    // arm, an example played on into the take.
    assert_eq!(h.count(&glyph(Icon::Play)), 2);
    let pressed = h.press(&glyph(Icon::Play));
    h.feed(pressed);
    h.settle();
    assert!(built.iter().all(|p| p.plays() == 0));
}

// ------------------------------------------------------ a pending start

// technique_pending_start_test: 'Save is disabled while an example
// recording start is pending'. Regression: Save looked live through the
// permission prompt, and a tap there toasted "Couldn't save" (the save
// refuses a pending start).
#[test]
fn save_is_disabled_while_an_example_recording_start_is_pending() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.edit();
    assert!(!h.press("Save").is_empty());

    h.hold(JobKind::StartRecording);
    h.click("Record an example");
    h.settle();
    assert!(h.press("Save").is_empty(), "Save waits for the start");

    h.setup.recorder.st().mode = StartMode::Resolve(MicRecordingStart::Invalidated);
    h.release(JobKind::StartRecording);
    assert!(!h.press("Save").is_empty());
}

// technique_pending_start_test: 'a pending example start reads as
// starting, not as saving'. Regression: the record row took its busy state
// from saving_example, which also covers a pending start, so through the
// permission prompt and the mic open it said "Saving example…" before
// anything was recorded.
#[test]
fn a_pending_example_start_reads_as_starting_not_as_saving() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.edit();
    h.hold(JobKind::StartRecording);
    h.click("Record an example");
    h.settle();
    assert!(!h.shows("Saving example…"));
    assert!(h.shows(STARTING));

    h.setup.recorder.st().mode = StartMode::Resolve(MicRecordingStart::Invalidated);
    h.release(JobKind::StartRecording);
    assert!(!h.shows(STARTING));
}

// The practice recorder reads "Starting the microphone…" (and takes no
// taps) while its start pends (technique_practice_failure_test's screen
// part).
#[test]
fn a_pending_practice_start_reads_as_starting() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.size = Size::new(1200.0, 1400.0);
    h.hold(JobKind::StartRecording);
    h.click(analysis::RECORD_PRACTICE);
    h.settle();
    assert!(h.shows(analysis::STARTING));
    assert!(!h.shows(analysis::RECORD_PRACTICE));
    assert!(h.press(analysis::STARTING).is_empty(), "no taps");
    h.release(JobKind::StartRecording);
    assert!(h.vm().is_recording_practice());
    assert!(h.shows(analysis::STOP));
}

// A failed start is said (`_record` / `_RecordRow._start` toasting the
// error the start returned).
#[test]
fn a_failed_start_says_why() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.size = Size::new(1200.0, 1400.0);
    h.setup.recorder.st().mode = StartMode::Resolve(MicRecordingStart::Failed(
        vocara_services::mic_recording_controller::MicRecordingFailure::FileCreateFailed,
    ));
    h.click(analysis::RECORD_PRACTICE);
    h.settle();
    assert_eq!(
        h.screen.snack(),
        Some("Couldn't create the recording file. Check your free disk space.")
    );
    assert!(h.shows(analysis::RECORD_PRACTICE), "ready for another try");
}

// ------------------------------------------------------ a practice take

// technique_practice_take_test: 'Edit is refused while a practice take is
// recording or starting'. Regression: Edit stayed live during a practice
// take. The edit view has no practice Stop control, its Save refuses while
// the mic is hot (every attempt toasted "Couldn't save"), and Cancel's
// draft discard silently aborted the take — the user's only way out threw
// the take away.
#[test]
fn edit_is_refused_while_a_practice_take_is_recording_or_starting() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.size = Size::new(1200.0, 1400.0);
    assert!(!h.press("Edit").is_empty(), "Edit is live");

    h.click(analysis::RECORD_PRACTICE);
    h.settle();
    assert!(h.vm().is_recording_practice());
    assert!(
        h.press("Edit").is_empty(),
        "stop the take first: the editor has no practice controls"
    );
    h.send(Message::Edit);
    assert!(!h.shows(EDIT_TITLE));
    assert!(h.vm().is_recording_practice(), "the take is untouched");

    h.click(analysis::STOP);
    h.settle();
    assert!(!h.press("Edit").is_empty());

    // A start still pending behind its waits: the tap itself must still be
    // refused, or the take arms inside the editor with no Stop.
    h.setup.recorder.st().starting = true;
    h.send(Message::Edit);
    assert!(!h.shows(EDIT_TITLE));

    h.setup.recorder.st().starting = false;
    h.click("Edit");
    assert!(h.shows(EDIT_TITLE));
    assert_eq!(h.setup.repo.st().base_saves, 0);
}

// The analysis card in the read view: the subject switch, the recorder, and
// the gap table once a practice take has its analysis.
#[test]
fn a_practice_take_is_compared_against_the_reference() {
    let setup = Setup::new(two_examples()).analysis(Outcome::Fresh);
    let mut h = Harness::loaded(setup);
    h.size = Size::new(1200.0, 1600.0);
    assert!(h.vm().reference_analysis().is_some());
    assert!(h.press(analysis::YOUR_PRACTICE).is_empty(), "no take yet");

    h.click(analysis::RECORD_PRACTICE);
    h.settle();
    h.click(analysis::STOP);
    h.settle();
    assert!(h.vm().practice_analysis().is_some());
    assert!(h.shows(analysis::RE_RECORD));
    assert!(h.shows("REFERENCE") && h.shows("YOU") && h.shows("GAP"));
    assert!(h.shows("RING") && h.shows("SINGER'S FORMANT"));

    h.click(analysis::YOUR_PRACTICE);
    assert_eq!(
        h.vm().analysis_subject(),
        crate::ui::technique_library::view_model::technique_detail_view_model::TechniqueAnalysisSubject::Practice
    );
    h.click(analysis::CLEAR);
    assert!(h.vm().practice_analysis().is_none());
    assert!(h.shows(analysis::RECORD_PRACTICE));
    assert!(!h.shows("GAP"));
}

// The page's dimension bar picks the view the box shows, and keeps it on
// the view model (it survives the panel moving between cards).
#[test]
fn the_dimension_bar_picks_the_view() {
    let setup = Setup::new(two_examples()).analysis(Outcome::Fresh);
    let mut h = Harness::loaded(setup);
    h.size = Size::new(1200.0, 1600.0);
    assert_eq!(h.vm().analysis_dim(), 0);
    h.click("LTAS");
    assert_eq!(h.vm().analysis_dim(), 5);
    h.click("Range");
    assert!(h.shows("The Range view is not available yet."));
}

// A recording notice (an input lost, a degraded device) is toasted once.
#[test]
fn a_recording_notice_is_toasted_once() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.size = Size::new(1200.0, 1400.0);
    h.click(analysis::RECORD_PRACTICE);
    h.settle();
    h.setup
        .recorder
        .st()
        .events
        .push(vocara_services::mic_recording_controller::MicEvent::InputDeviceLost);
    h.send(Message::Tick);
    h.settle();
    assert_eq!(
        h.screen.snack(),
        Some("Recording stopped: the input device was disconnected.")
    );
    assert!(h.vm().recording_notice().is_none(), "cleared once shown");
    let seq = h.screen.snack_seq;
    h.send(Message::SnackExpired(seq));
    assert_eq!(h.screen.snack(), None);
}

// A rename goes through the app's dialog stack and stages the caption.
#[test]
fn a_rename_stages_the_new_caption() {
    let mut h = Harness::loaded(Setup::new(two_examples()));
    h.edit();
    h.click(&glyph(Icon::Pencil));
    assert!(matches!(h.dialog(), Dialog::RenameExample(d) if d.text() == "Example 0"));
    h.answer(DialogResult::Text(Some("Belted".into())));
    assert!(h.shows("Belted"));
    assert!(h.vm().has_media_draft());
}
