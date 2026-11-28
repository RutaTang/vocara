//! The technique page through the whole app — the router, the page stack
//! and the dialog stack, over an in-memory library: the library → technique
//! → back journey, a deep link to `/techniques/<id>`,
//! `deep_link_back_test.dart`'s technique cases, the technique part of
//! `technique_library_journey_test.dart` (a created technique opens, then
//! waits in the grid), and `technique_detail_journey_test.dart` (edit, save
//! and cancel round-trips through the repository, and a dropped example).
//! And a snack bar up as Back leaves, which stays up on the library (Dart's
//! `ScaffoldMessenger` sat above the pages), the library's own waiting
//! behind it.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use iced::{window, Event as IcedEvent, Size};
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use indexmap::IndexMap;
use vocara_model::config::environment::AppEnvironment;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_services::audio_dsp::write_wav_mono;
use vocara_store::repositories::TechniqueRepository;
use vocara_store::sync::sync_hashes::Loaded;

use super::*;
use crate::app::library::{Library, LibraryOpen};
use crate::app::{self, App, Boot, LibraryEvent, Page};
use crate::router::PageId;
use crate::services::ServicesFactory;
use crate::ui::core::widgets::confirm_dialog;
use crate::ui::home::testing::run_task;
use crate::ui::technique_library::technique_library_screen::CREATE_FAILED as LIBRARY_CREATE_FAILED;
use crate::ui::technique_library::widgets::media::technique_media_section::{
    ADD_ANOTHER, ADD_FIRST,
};

const SIZE: Size = Size::new(1440.0, 2400.0);

fn settings() -> iced::Settings {
    let mut settings = crate::app::settings();
    settings.fonts = font::FACES
        .iter()
        .copied()
        .chain([crate::ui::core::icons::FONT_BYTES])
        .map(Cow::Borrowed)
        .collect();
    settings
}

/// The `n`-th text reading `label` (0 first), in tree order: the page, then
/// the dialogs over it.
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

fn chevron() -> String {
    "Back".to_string()
}

/// A library holding "Twang" (and its id).
fn seeded() -> (Arc<Library>, String) {
    let library = Arc::new(Library::in_memory());
    let id = new_id();
    let now = DateTime::local_ymd(2026, 6, 20);
    library
        .technique_repo
        .save_technique(&Technique::new(id.clone(), "Twang", now, now))
        .expect("seeded");
    (library, id)
}

/// The app at `start`, over `library`, every task run.
fn boot(library: Arc<Library>, start: &str) -> App {
    boot_over(library, start, Services::detached)
}

/// [`boot`], over the services `services` makes.
fn boot_over(library: Arc<Library>, start: &str, services: ServicesFactory) -> App {
    let mut boot = Boot::new(AppEnvironment::Dev);
    boot.start = Some(start.to_string());
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

fn ui(app: &App) -> Simulator<'_, app::Message> {
    Simulator::with_size(settings(), SIZE, app.view())
}

fn feed(app: &mut App, messages: Vec<app::Message>) {
    for message in messages {
        let task = app.update(message);
        settle(app, task);
    }
}

fn click_nth(app: &mut App, label: &str, n: usize) {
    let messages: Vec<app::Message> = {
        let mut ui = ui(app);
        ui.click(nth(label, n))
            .unwrap_or_else(|e| panic!("{label} #{n}: {e:?}"));
        ui.into_messages().collect()
    };
    feed(app, messages);
}

fn click(app: &mut App, label: &str) {
    click_nth(app, label, 0);
}

fn shows(app: &App, label: &str) -> bool {
    ui(app).find(nth(label, 0)).is_ok()
}

/// The technique page on top.
fn technique(app: &App) -> (PageId, &TechniqueDetailScreen) {
    match app.top() {
        Some((id, Page::Technique(screen))) => (id, screen),
        other => panic!("a technique on top, not {other:?}"),
    }
}

/// Sends the technique page on top `message` (a field's text, as typing
/// leaves it).
fn send(app: &mut App, message: Message) {
    let (id, _) = technique(app);
    let task = app.update(app::Message::Technique(id, message));
    settle(app, task);
}

// The journey through the app: library → a technique → back. The page
// pushed over the library pops back to that same library, whose open guard
// is released for the next open (deep_link_back_test: 'a pushed technique
// still pops back to the library it came from').
#[test]
fn the_library_opens_a_technique_and_back_returns_to_it() {
    let (library, id) = seeded();
    let mut app = boot(library, "/techniques");
    let (grid, _) = app.top().expect("running");
    assert!(shows(&app, "Twang"));

    click(&mut app, "Twang");
    assert_eq!(
        app.routes(),
        [
            Some(Route::Techniques),
            Some(Route::Technique { id: id.clone() })
        ]
    );
    // Its page loaded: the name in the header, the read view's actions.
    assert!(shows(&app, "Twang"));
    assert!(shows(&app, "Edit") && shows(&app, "Delete"));
    assert!(shows(&app, "Examples"));
    assert!(!shows(&app, "New technique"), "the library is covered");

    click(&mut app, &chevron());
    assert_eq!(app.routes(), [Some(Route::Techniques)]);
    // Popped, not replaced: the library underneath is the same page, so a
    // second open is not swallowed by its open guard.
    assert_eq!(app.top().map(|(id, _)| id), Some(grid));
    click(&mut app, "Twang");
    assert_eq!(technique(&app).1.technique_id, id);
}

// A deep link (`VOCARA_START=/techniques/<id>`) opens the technique as the
// root of the stack; its back chevron lands on the library
// (deep_link_back_test: 'a deep-linked technique: the back chevron lands on
// the library').
#[test]
fn a_deep_linked_technique_opens_and_its_back_chevron_lands_on_the_library() {
    let (library, id) = seeded();
    let mut app = boot(library, &crate::router::technique_detail(&id));
    assert_eq!(app.routes(), [Some(Route::Technique { id: id.clone() })]);
    assert!(shows(&app, "Twang"));
    assert!(shows(&app, "Edit"));

    click(&mut app, &chevron());
    assert_eq!(app.routes(), [Some(Route::Techniques)]);
    assert!(shows(&app, "Twang"), "the library lists it");
}

// deep_link_back_test: 'a deep-linked technique: Delete lands on the
// library'. Delete used to leave the user on the technique it had just
// deleted (go_router had nothing to pop).
#[test]
fn a_deep_linked_technique_deleted_lands_on_the_library() {
    let (library, id) = seeded();
    let repo = library.technique_repo.clone();
    let mut app = boot(library, &crate::router::technique_detail(&id));

    click(&mut app, "Delete");
    assert_eq!(app.dialogs(), 1);
    assert!(shows(&app, "Delete \"Twang\"?"));
    // The dialog's own Delete (the header's is the first).
    click_nth(&mut app, "Delete", 1);
    assert_eq!(app.dialogs(), 0);

    assert!(
        repo.list_techniques().expect("listed").is_empty(),
        "deleted"
    );
    assert_eq!(app.routes(), [Some(Route::Techniques)]);
    assert!(shows(&app, "Build your technique library"));
}

// technique_library_journey_test: 'creating a technique navigates to its
// detail, then it shows in the grid on return'.
#[test]
fn a_created_technique_opens_then_waits_in_the_grid() {
    let library = Arc::new(Library::in_memory());
    let repo = library.technique_repo.clone();
    let mut app = boot(library, "/techniques");
    assert!(shows(&app, "Build your technique library"));

    click(&mut app, "New technique");
    assert_eq!(app.dialogs(), 1);
    let task = app.update(app::Message::Dialog(confirm_dialog::Message::Input(
        "Twang".into(),
    )));
    settle(&mut app, task);
    click(&mut app, "Create");

    // Creating navigates straight into the new technique's page, which
    // loads with its name.
    let (_, page) = technique(&app);
    assert!(page.view_model().technique().is_some());
    assert!(shows(&app, "Twang"));

    // Back to the library — the new technique now shows as a card.
    click(&mut app, &chevron());
    assert_eq!(app.routes(), [Some(Route::Techniques)]);
    assert!(shows(&app, "Twang"));
    let all = repo.list_techniques().expect("listed");
    assert_eq!(
        all.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["Twang"]
    );
}

// technique_detail_journey_test: 'Edit → change fields → Save persists
// through the repo'.
#[test]
fn edit_then_save_persists_through_the_repository() {
    let (library, id) = seeded();
    let repo = library.technique_repo.clone();
    let mut app = boot(library, &crate::router::technique_detail(&id));

    click(&mut app, "Edit");
    assert!(
        shows(&app, EDIT_TITLE),
        "tapping Edit opens the editor form"
    );
    assert!(shows(&app, "Name") && shows(&app, "Summary"));
    send(
        &mut app,
        Message::Field(LineField::Name, "Belted Twang".into()),
    );
    send(
        &mut app,
        Message::Prose(
            ProseField::Summary,
            text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(
                "bright, ringing edge".into(),
            ))),
        ),
    );

    click(&mut app, "Save");
    assert!(
        shows(&app, "Edit") && !shows(&app, EDIT_TITLE),
        "a successful save returns to view mode"
    );
    let reloaded = repo.get_technique(&id).expect("stored");
    assert_eq!(reloaded.name, "Belted Twang");
    assert_eq!(reloaded.summary, "bright, ringing edge");
    assert!(shows(&app, "Belted Twang"), "the header shows the new name");
}

// technique_detail_journey_test: 'Edit → change a field → Cancel discards
// via the confirm dialog'.
#[test]
fn cancel_discards_via_the_confirm_dialog() {
    let (library, id) = seeded();
    let repo = library.technique_repo.clone();
    let mut app = boot(library, &crate::router::technique_detail(&id));

    click(&mut app, "Edit");
    send(
        &mut app,
        Message::Field(LineField::Name, "Should Not Persist".into()),
    );
    click(&mut app, "Cancel");
    assert_eq!(app.dialogs(), 1, "cancelling a dirty editor asks first");
    assert!(shows(&app, DISCARD_TITLE));
    click(&mut app, "Discard");
    assert_eq!(app.dialogs(), 0);
    assert!(shows(&app, "Edit") && !shows(&app, EDIT_TITLE));
    assert_eq!(
        repo.get_technique(&id).expect("stored").name,
        "Twang",
        "a discarded edit never reaches the store"
    );
}

// technique_detail_journey_test: 'Edit → drop a WAV example → Save persists
// the media'.
#[test]
fn a_dropped_wav_example_is_saved_with_the_technique() {
    let (library, id) = seeded();
    let repo = library.technique_repo.clone();
    let mut app = boot(library, &crate::router::technique_detail(&id));
    click(&mut app, "Edit");
    // With no media yet, the add zone shows its full prompt.
    let zone = ui(&app)
        .find(nth(ADD_FIRST, 0))
        .expect("edit mode exposes the media add/drop zone")
        .bounds()
        .center();

    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("example.wav");
    let samples: Vec<f64> = (0..4410)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 330.0 * i as f64 / 44100.0).sin())
        .collect();
    std::fs::write(&wav, write_wav_mono(&samples, 44100)).unwrap();
    let dropped: Vec<app::Message> = {
        let mut ui = ui(&app);
        ui.point_at(zone);
        let _ = ui.simulate([
            IcedEvent::Window(window::Event::FileHovered(PathBuf::from(&wav))),
            IcedEvent::Window(window::Event::FileDropped(PathBuf::from(&wav))),
            IcedEvent::Window(window::Event::RedrawRequested(iced::time::Instant::now())),
        ]);
        ui.into_messages().collect()
    };
    feed(&mut app, dropped);
    // Once imported, the add zone goes dense — the clip is staged.
    assert!(shows(&app, ADD_ANOTHER), "the dropped clip is staged");
    assert!(shows(&app, "example"), "named after its file");

    click(&mut app, "Save");
    assert!(shows(&app, "Edit"), "save returns to view mode");
    let reloaded = repo.get_technique(&id).expect("stored");
    assert_eq!(reloaded.media.len(), 1, "the staged example persisted");
    assert!(
        std::path::Path::new(&reloaded.media[0].path).is_file(),
        "the pooled example file resolves on disk"
    );
}

// ------------------------------------------------- a snack bar as the page goes

/// The library's techniques, refusing every write.
struct Refusing(Arc<dyn TechniqueRepository>);

fn refused() -> vocara_store::Error {
    vocara_store::Error::State("refused".into())
}

impl TechniqueRepository for Refusing {
    fn list_techniques(&self) -> vocara_store::Result<Vec<Technique>> {
        self.0.list_techniques()
    }

    fn get_technique(&self, id: &str) -> vocara_store::Result<Technique> {
        self.0.get_technique(id)
    }

    fn load_technique(&self, id: &str) -> vocara_store::Result<Loaded<Technique>> {
        self.0.load_technique(id)
    }

    fn save_technique(&self, _technique: &Technique) -> vocara_store::Result<Technique> {
        Err(refused())
    }

    fn save_technique_with_base(
        &self,
        _technique: &Technique,
        _base_hashes: &IndexMap<String, String>,
    ) -> vocara_store::Result<Loaded<Technique>> {
        Err(refused())
    }

    fn delete_technique(&self, _id: &str) -> vocara_store::Result<()> {
        Err(refused())
    }
}

/// The app's services, its techniques refusing every write.
fn refusing_writes(library: Arc<Library>, environment: AppEnvironment) -> Services {
    let mut services = Services::detached(library, environment);
    services.technique_repo = Arc::new(Refusing(services.technique_repo.clone()));
    services
}

/// The library (the root, as a deep link opens it) with "Twang" open over
/// it, whose delete was refused: its page says so.
fn refused_delete() -> App {
    let (library, _) = seeded();
    let mut app = boot_over(library, "/techniques", refusing_writes);
    click(&mut app, "Twang");
    click(&mut app, "Delete");
    // The dialog's own Delete (the header's is the first).
    click_nth(&mut app, "Delete", 1);
    assert_eq!(technique(&app).1.snack(), Some(DELETE_FAILED));
    app
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

// technique_detail_screen.dart's snack bars went through the app-wide
// `ScaffoldMessenger`: one up as Back popped the page stayed up on the
// library for the rest of its 4 s. The page's own went with the page.
#[test]
fn a_snack_bar_up_at_back_stays_up_on_the_library_until_its_time_is_up() {
    let mut app = refused_delete();
    click(&mut app, &chevron());
    assert_eq!(app.routes(), [Some(Route::Techniques)], "the page left");
    assert!(shows(&app, DELETE_FAILED), "the library shows it");
    assert!(shows(&app, "New technique"), "over the library");

    let bar = app.snack_bar().expect("the app shows it");
    assert_eq!(bar.text, DELETE_FAILED);
    assert!(bar.left <= SNACK_DURATION, "for the time it had left");

    time_up(&mut app);
    assert!(!shows(&app, DELETE_FAILED), "gone once its time is up");
    assert!(app.snack_timer().is_none(), "and no timer runs on");
}

// The library's snack bars were plain `showSnackBar`s: one shown while the
// technique's is still up waits its turn behind it, then shows for all of
// its 4 s.
#[test]
fn a_library_snack_bar_waits_behind_it() {
    let mut app = refused_delete();
    click(&mut app, &chevron());
    assert!(shows(&app, DELETE_FAILED));

    click(&mut app, "New technique");
    let task = app.update(app::Message::Dialog(confirm_dialog::Message::Input(
        "Belt".into(),
    )));
    settle(&mut app, task);
    click(&mut app, "Create");
    assert_eq!(app.dialogs(), 0);
    assert!(shows(&app, DELETE_FAILED), "the technique's stays up");
    assert!(!shows(&app, LIBRARY_CREATE_FAILED), "the library's waits");

    time_up(&mut app);
    assert!(
        shows(&app, LIBRARY_CREATE_FAILED),
        "then the library's shows"
    );
    assert!(!shows(&app, DELETE_FAILED));
    let timer = app.snack_timer().expect("its timer runs");
    // It was handed over the moment it was shown: all of its time is
    // ahead, from the end of its entrance.
    assert!(
        timer.deadline >= std::time::Instant::now() + SNACK_DURATION,
        "for all of its time"
    );

    time_up(&mut app);
    assert!(!shows(&app, LIBRARY_CREATE_FAILED), "then goes");
    assert!(app.snack_timer().is_none());
}

// ------------------------------------------------ a video in fullscreen

/// What a drag on the page toolbar's empty area sends.
fn drag_the_bar(app: &App) -> Vec<app::Message> {
    use iced::mouse;
    let mut ui = ui(app);
    ui.point_at(iced::Point::new(600.0, 10.0));
    let _ = ui.simulate([
        IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        IcedEvent::Mouse(mouse::Event::CursorMoved {
            position: iced::Point::new(640.0, 12.0),
        }),
        IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ]);
    ui.into_messages().collect()
}

// media_kit's fullscreen route sat on the root navigator: over every page
// and the page toolbar, taking the clicks and keys it covered.
#[test]
fn a_fullscreen_video_covers_the_whole_window_and_takes_its_keys() {
    use crate::ui::technique_library::view_model::technique_detail_view_model::tests::support::{
        example, twang, Setup, ID,
    };
    use crate::ui::technique_library::widgets::media::example_players::{self, ExampleKey};
    use crate::ui::technique_library::widgets::media::technique_video_player as video;
    use crate::ui::technique_library::widgets::media::video_controls::Shortcut;
    use iced::keyboard::{key::Named, Key};
    use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
    use vocara_services::media_player::FakeMediaPlayers;

    let mut app = boot(Arc::new(Library::in_memory()), "/techniques");
    let clip = TechniqueMedia {
        kind: TechniqueMediaKind::Video,
        ..example("V", "/tmp/example.mp4")
    };
    let setup = Setup::new(twang(ID, vec![clip]));
    let players = Arc::new(FakeMediaPlayers::new());
    let (screen, step) = TechniqueDetailScreen::open(setup.deps(), app.services(), players);
    let opening = step.into_update().task;
    let task = app.push_page(Page::Technique(screen), move |id| {
        opening.map(move |m| app::Message::Technique(id, m))
    });
    settle(&mut app, task);
    let (page, _) = technique(&app);
    let to_video = |m: video::Message| {
        app::Message::Technique(
            page,
            Message::Media(media_section::Message::Player(
                example_players::Message::Video(
                    ExampleKey {
                        id: "V".to_string(),
                        unavailable: false,
                        video: true,
                    },
                    m,
                ),
            )),
        )
    };
    let drags = |messages: &[app::Message]| {
        messages
            .iter()
            .any(|m| matches!(m, app::Message::DragWindow))
    };
    assert!(drags(&drag_the_bar(&app)), "the bar, uncovered");

    feed(
        &mut app,
        vec![
            to_video(video::Message::Play),
            to_video(video::Message::Key(Shortcut::ToggleFullscreen)),
        ],
    );
    assert!(technique(&app).1.overlay().is_some());
    // Covered, the band is still the window's (it could not be moved while
    // a video was fullscreen), and the page beneath takes nothing.
    let covered = drag_the_bar(&app);
    assert!(drags(&covered), "{covered:?}");
    assert!(
        covered.iter().all(|m| matches!(
            m,
            app::Message::DragWindow | app::Message::Technique(_, Message::Media(_))
        )),
        "{covered:?}"
    );
    // Escape reaches the video, not the page under it.
    let sent: Vec<app::Message> = {
        let mut ui = ui(&app);
        let _ = ui.tap_key(Key::Named(Named::Escape));
        ui.into_messages().collect()
    };
    assert!(
        sent.iter().any(|m| matches!(
            m,
            app::Message::Technique(
                _,
                Message::Media(media_section::Message::Player(
                    example_players::Message::Video(
                        _,
                        video::Message::Key(Shortcut::ExitFullscreen)
                    )
                ))
            )
        )),
        "{sent:?}"
    );
    feed(&mut app, sent);
    assert!(technique(&app).1.overlay().is_none());
    assert!(drags(&drag_the_bar(&app)), "uncovered again");
}
