//! The song practice page through the whole app — the router, the page
//! stack and the dialog stack, over an in-memory library with the app's own
//! services: Home → a practice → back (`home_reopen_test.dart`: the page
//! loaded, popped back to the same Home), a practice that cannot be loaded
//! still having a way back, a deep link to `/song/<id>` (its back chevron
//! goes Home, where nothing lies beneath), and the page cases of
//! `detail_arrange_journey_test.dart` that need no audio device: the deep
//! link booting into the seeded takes, the dock, the interpretation
//! switcher, New take and a note added from a take's header, each
//! round-tripping through the store. (`ui_journey_test.dart` runs through
//! this page from Home's own journeys.) And the toast a take's finalize
//! shows as Back leaves, which stays up on Home (Dart's `ScaffoldMessenger`
//! sat above the pages) until its time is up or Home toasts over it.

use std::borrow::Cow;
use std::collections::VecDeque;

use iced::Size;
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use vocara_model::config::environment::AppEnvironment;
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_services::audio_dsp::write_wav_mono;

use super::*;
use crate::app::library::{Library, LibraryOpen};
use crate::app::{self, App, Boot, LibraryEvent, Page};
use crate::router::{Navigation, PageId, Route};
use crate::services::ServicesFactory;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::font;
use crate::ui::core::widgets::confirm_dialog;
use crate::ui::home::testing::{run_task, spawn_task, FakePractices};

const SIZE: Size = Size::new(1440.0, 1000.0);

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

/// The first text reading `label`, in tree order.
fn text_of(label: &str) -> impl FnMut(Candidate<'_>) -> Option<Target> + Send {
    let label = label.to_string();
    move |candidate: Candidate<'_>| match &candidate {
        Candidate::Text { content, .. } if *content == label => Some(Target::from(candidate)),
        _ => None,
    }
}

/// A library holding a practice with `takes` vocal takes over real WAVs
/// (Dart's `seedPracticeWithTakes`), and its id; the files' folder lives as
/// long as the test.
fn seeded(takes: usize) -> (Arc<Library>, String, tempfile::TempDir) {
    let library = Arc::new(Library::in_memory());
    let dir = tempfile::tempdir().expect("a temp dir");
    let created = library
        .song_repo
        .create_practice("Seeded song")
        .expect("created");
    let samples: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    let now = DateTime::local_ymd(2026, 6, 20);
    let tracks = (1..=takes)
        .map(|n| {
            let path = dir.path().join(format!("take{n}.wav"));
            std::fs::write(&path, write_wav_mono(&samples, 44100)).expect("a wav");
            Arc::new(AudioTrack {
                version_id: created.active_version_id.clone(),
                ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, format!("Take {n}"), now)
                    .with_audio(path.to_string_lossy(), 0, 1000, Vec::new())
            })
        })
        .collect();
    library
        .song_repo
        .save_practice(&SongPractice {
            vocal_tracks: tracks,
            ..created.clone()
        })
        .expect("seeded");
    (library, created.id, dir)
}

/// The app at `start` (Home when `None`), over `library`, every task run.
fn boot(library: Arc<Library>, start: Option<&str>) -> App {
    boot_over(library, start, Services::detached)
}

/// [`boot`], over the services `services` makes.
fn boot_over(library: Arc<Library>, start: Option<&str>, services: ServicesFactory) -> App {
    let mut boot = Boot::new(AppEnvironment::Dev);
    boot.start = start.map(str::to_string);
    boot.services = services;
    let (mut app, _open) = App::new(boot);
    let task = app.update(app::Message::Library(LibraryEvent::Finished(
        LibraryOpen::Opened(library),
    )));
    settle(&mut app, task);
    app
}

/// Runs `task` and everything it leads to.
fn settle(app: &mut App, task: Task<app::Message>) {
    let mut queue: VecDeque<app::Message> = run_task(task).into();
    while let Some(message) = queue.pop_front() {
        let task = app.update(message);
        queue.extend(run_task(task));
    }
}

/// Runs `task`, handing the app each message as it comes (and settling
/// what that leads to): for a task that waits on the app meanwhile.
fn pump(app: &mut App, task: Task<app::Message>) {
    for message in spawn_task(task) {
        let task = app.update(message);
        settle(app, task);
    }
}

fn ui(app: &App) -> Simulator<'_, app::Message> {
    Simulator::with_size(settings(), SIZE, app.view())
}

fn click(app: &mut App, label: &str) {
    let messages: Vec<app::Message> = {
        let mut ui = ui(app);
        ui.click(text_of(label))
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        ui.into_messages().collect()
    };
    for message in messages {
        let task = app.update(message);
        settle(app, task);
    }
}

fn shows(app: &App, label: &str) -> bool {
    ui(app).find(text_of(label)).is_ok()
}

/// The song page on top.
fn song(app: &App) -> (PageId, &SongPracticeDetailScreen) {
    match app.top() {
        Some((id, Page::Song(screen))) => (id, &**screen),
        other => panic!("a song page on top, not {other:?}"),
    }
}

/// Sends the song page on top `message`.
fn send(app: &mut App, message: Message) {
    let (id, _) = song(app);
    let task = app.update(app::Message::Song(id, message));
    settle(app, task);
}

fn back(app: &mut App) {
    click(app, "Back");
}

// home_reopen_test: 'the transport back chevron returns Home ready to open
// the next practice' — the page between: it loads, and its chevron pops back
// to the very Home that pushed it (whose open guard the pop releases).
#[test]
fn home_opens_a_practice_whose_page_loads_and_its_chevron_pops_back() {
    let (library, id, _dir) = seeded(1);
    let mut app = boot(library, None);
    let (home, _) = app.top().expect("running");
    // Pushed as Home pushes a card's practice.
    let task = app.update(app::Message::Navigate(Navigation::Push(Route::Song {
        id: id.clone(),
    })));
    settle(&mut app, task);
    assert_eq!(
        app.routes(),
        [Some(Route::Home), Some(Route::Song { id: id.clone() })]
    );
    assert!(song(&app).1.view_model().practice().is_some(), "loaded");
    assert!(shows(&app, "Seeded song"), "the transport's title");
    assert!(shows(&app, "VOCAL TAKES"));
    assert!(shows(&app, "TAKE 1"));

    back(&mut app);
    assert_eq!(app.routes(), [Some(Route::Home)]);
    assert_eq!(
        app.top().map(|(id, _)| id),
        Some(home),
        "popped, not replaced"
    );
    assert_eq!(app.lingering(), 0, "nothing left landing");
}

// home_reopen_test: 'a practice that cannot be loaded still has a way back'.
// Loading and load failure showed no transport bar, so no back control: the
// user was stuck, and the opener's awaited push never completed.
#[test]
fn a_practice_that_cannot_be_loaded_still_has_a_way_back() {
    let mut app = boot(Arc::new(Library::in_memory()), None);
    let (home, _) = app.top().expect("running");
    let task = app.update(app::Message::Navigate(Navigation::Push(Route::Song {
        id: "missing".into(),
    })));
    settle(&mut app, task);
    assert!(shows(&app, NOT_LOADED));
    back(&mut app);
    assert_eq!(app.routes(), [Some(Route::Home)]);
    assert_eq!(
        app.top().map(|(id, _)| id),
        Some(home),
        "the push completed"
    );
}

// detail_arrange_journey: 'deep-link boots into the seeded practice with
// both takes'; and a deep-linked page is the ROOT of the stack, so its back
// chevron goes Home rather than popping into nothing (deep_link_back_test).
#[test]
fn a_deep_linked_practice_boots_with_its_takes_and_its_chevron_goes_home() {
    let (library, id, _dir) = seeded(2);
    let mut app = boot(library, Some(&crate::router::song_practice(&id)));
    assert_eq!(app.routes(), [Some(Route::Song { id: id.clone() })]);
    assert!(shows(&app, "TAKE 1") && shows(&app, "TAKE 2"));
    assert!(
        shows(&app, "Play"),
        "transport play is available once the practice has tracks"
    );

    back(&mut app);
    assert_eq!(app.routes(), [Some(Route::Home)]);
    assert!(shows(&app, "Seeded song"), "Home lists it");
}

// detail_arrange_journey: 'the bottom analysis dock opens'.
#[test]
fn the_bottom_analysis_dock_opens() {
    let (library, id, _dir) = seeded(2);
    let mut app = boot(library, Some(&crate::router::song_practice(&id)));
    assert!(shows(&app, &glyph(Icon::PanelBottomOpen)), "starts closed");
    click(&mut app, &glyph(Icon::PanelBottomOpen));
    assert!(shows(&app, &glyph(Icon::PanelBottomClose)));
}

// detail_arrange_journey: 'the interpretation switcher opens its menu'.
#[test]
fn the_interpretation_switcher_opens_its_menu() {
    let (library, id, _dir) = seeded(2);
    let mut app = boot(library, Some(&crate::router::song_practice(&id)));
    click(&mut app, &glyph(Icon::Drama));
    assert!(shows(&app, "New interpretation"));
    let active = song(&app)
        .1
        .view_model()
        .practice()
        .and_then(|p| p.active_version().map(|v| v.name.clone()))
        .expect("an interpretation");
    assert!(shows(&app, &active), "the seeded interpretation is listed");
}

// detail_arrange_journey: 'the "New take" button adds an empty vocal lane'.
#[test]
fn the_new_take_button_adds_an_empty_vocal_lane() {
    let (library, id, _dir) = seeded(2);
    let mut app = boot(library.clone(), Some(&crate::router::song_practice(&id)));
    click(&mut app, "New take");
    assert!(shows(&app, "TAKE 3"));
    let stored = library.song_repo.get_practice(&id).expect("stored");
    assert_eq!(stored.vocal_tracks.len(), 3, "saved");
}

// detail_arrange_journey: 'adding a note via the take header persists it on
// the take'.
#[test]
fn adding_a_note_via_the_take_header_persists_it_on_the_take() {
    let (library, id, _dir) = seeded(2);
    let mut app = boot(library.clone(), Some(&crate::router::song_practice(&id)));
    click(&mut app, &glyph(Icon::StickyNote));
    assert!(song(&app).1.note_editor().is_some(), "the editor opened");
    assert!(shows(&app, "Add"));
    send(
        &mut app,
        Message::NoteEditor(note_editor::Message::Edit(
            iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Paste(
                Arc::new("breath runs out here".into()),
            )),
        )),
    );
    click(&mut app, "Add");
    assert!(song(&app).1.note_editor().is_none(), "confirming closes it");
    let stored = library.song_repo.get_practice(&id).expect("stored");
    let notes: Vec<String> = stored
        .vocal_tracks
        .iter()
        .flat_map(|t| t.notes.iter().map(|n| n.content.clone()))
        .collect();
    assert_eq!(notes, ["breath runs out here"]);
}

// ------------------------------------------------- a toast as the page goes

/// What a take whose recorder dropped a second of audio (48 000 frames)
/// says as it lands.
const DROPPED: &str = "Recording dropped about 1000 ms of audio while the disk was busy. \
                       The take has small gaps.";

/// Home's toast for a practice that could not be created.
const CREATE_FAILED: &str = "Could not create the practice. Please try again.";

/// The app at Home over `services`, with the take-flow page (a backing and
/// a take, over the page tests' fakes) pushed over it as Home pushes a
/// practice, recording a take its recorder will have dropped a second of.
fn recording_over_home(services: ServicesFactory) -> App {
    let mut app = boot_over(Arc::new(Library::in_memory()), None, services);
    let (screen, opening) = super::tests::take_flow_page(app.services(), 48_000);
    let task = app.push_page(Page::Song(Box::new(screen)), move |id| {
        opening.map(move |m| app::Message::Song(id, m))
    });
    settle(&mut app, task);
    assert!(song(&app).1.view_model().practice().is_some(), "loaded");
    // REC. The start hands its take setup to the page and waits for the
    // answer: its messages land as they come.
    let (id, _) = song(&app);
    let task = app.update(app::Message::Song(
        id,
        Message::Transport(detail_transport_bar::Message::StartRecording),
    ));
    pump(&mut app, task);
    assert!(song(&app).1.view_model().is_recording());
    app
}

/// Home's practices refuse to be created.
fn refusing_creates(library: Arc<Library>, environment: AppEnvironment) -> Services {
    let mut services = Services::detached(library, environment);
    let practices = FakePractices::with(Vec::new());
    practices.state().uncreatable = true;
    services.song_repo = practices;
    services
}

/// Takes down the snack bar the app shows, its time up, and lets it leave.
fn time_up(app: &mut App) {
    let timer = app.snack_timer().expect("a snack bar's timer runs");
    let task = app.update(app::Message::SnackBarExpired(timer.seq));
    settle(app, task);
    let left = std::time::Instant::now() + crate::ui::snack::TRANSITION;
    let task = app.update(app::Message::SnackBarFrame(left));
    settle(app, task);
}

// song_practice_detail_screen.dart: Back during a take awaits
// `stopBeforeLeaving`, whose finalize toasts just before the page pops; the
// app-wide `ScaffoldMessenger` kept that snack bar up on Home for the rest of
// its 2.8 s. The page's own snack bar went with the page: the message was
// never seen.
#[test]
fn a_takes_finalize_toast_at_back_stays_up_on_home_until_its_time_is_up() {
    let mut app = recording_over_home(Services::detached);
    back(&mut app);
    assert_eq!(app.routes(), [Some(Route::Home)], "the page left");
    assert_eq!(app.lingering(), 0, "its take landed");
    assert!(shows(&app, DROPPED), "Home shows the take's toast");
    assert!(shows(&app, "New practice"), "over Home");

    let bar = app.snack_bar().expect("the app shows it");
    assert_eq!(bar.text, DROPPED);
    assert!(bar.left <= SNACK_DURATION, "for the time it had left");
    assert!(app.snack_timer().is_some(), "its timer runs");

    time_up(&mut app);
    assert!(!shows(&app, DROPPED), "gone once its time is up");
    assert!(app.snack_timer().is_none(), "and no timer runs on");
}

// Home's toast cleared the snack bars up first (`clearSnackBars()`, then
// `showSnackBar`): a failed create's says so in place of the take's.
#[test]
fn a_home_toast_replaces_the_takes_toast() {
    let mut app = recording_over_home(refusing_creates);
    back(&mut app);
    assert!(shows(&app, DROPPED));

    click(&mut app, "New practice");
    let task = app.update(app::Message::Dialog(confirm_dialog::Message::Input(
        "Encore".into(),
    )));
    settle(&mut app, task);
    click(&mut app, "Create");
    // The take's leaves first.
    let left = std::time::Instant::now() + crate::ui::snack::TRANSITION;
    let task = app.update(app::Message::SnackBarFrame(left));
    settle(&mut app, task);
    assert!(shows(&app, CREATE_FAILED), "Home's toast shows");
    assert!(!shows(&app, DROPPED), "in place of the take's");

    time_up(&mut app);
    assert!(!shows(&app, CREATE_FAILED), "then goes in its own time");
    assert!(!shows(&app, DROPPED), "with nothing left behind it");
}
