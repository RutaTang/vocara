//! Home through the whole app — the router, the page stack and the dialog
//! stack: the integration journeys (`home_settings_journey_test.dart`,
//! `ui_journey_test.dart`) and `home_reopen_test.dart`. The practice page's
//! way back is its transport bar's chevron.

use std::collections::VecDeque;
use std::sync::Arc;

use vocara_model::config::environment::AppEnvironment;
use vocara_services::update_check_service::UpdateCheckService;

use super::*;
use crate::app::library::{Library, LibraryOpen};
use crate::app::{self, App, Boot, LibraryEvent, Page};
use crate::router::PageId;
use crate::services::ServicesFactory;
use crate::ui::core::widgets::confirm_dialog;
use crate::ui::home::testing::run_task;

fn boot(library: Arc<Library>, services: ServicesFactory) -> App {
    let mut boot = Boot::new(AppEnvironment::Dev);
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

/// What clicking `label` reports.
fn clicked(app: &App, label: &str) -> Vec<app::Message> {
    let mut ui = iced_test::simulator(app.view());
    ui.click(label).unwrap_or_else(|e| panic!("{label}: {e:?}"));
    ui.into_messages().collect()
}

fn click(app: &mut App, label: &str) {
    for message in clicked(app, label) {
        let task = app.update(message);
        settle(app, task);
    }
}

/// The practice page's back chevron (its transport bar's).
fn back(app: &mut App) {
    click(app, &char::from(Icon::ChevronLeft).to_string());
}

fn shows(app: &App, label: &str) -> bool {
    let mut ui = iced_test::simulator(app.view());
    ui.find(label).is_ok()
}

fn home(app: &App) -> (PageId, &HomeScreen) {
    match app.top() {
        Some((id, Page::Home(home))) => (id, &**home),
        other => panic!("Home on top, not {other:?}"),
    }
}

/// Fires Home's running timer that `pick` selects.
fn fire(app: &mut App, pick: fn(&Timer) -> bool) {
    let (id, timer) = {
        let (id, home) = home(app);
        let timer = home.timers().into_iter().find(pick).expect("a timer");
        (id, timer)
    };
    let task = app.update(app::Message::Home(id, timer.message()));
    settle(app, task);
}

fn opening(timer: &Timer) -> bool {
    matches!(timer, Timer::Opening(_))
}

fn song_route(app: &App) -> String {
    match app.routes().last() {
        Some(Some(Route::Song { id })) => id.clone(),
        other => panic!("a practice on top, not {other:?}"),
    }
}

// home_reopen_test, and the journey's "seeded practices render as cards;
// tapping one opens detail then the back chevron returns Home".
#[test]
fn a_card_opens_its_practice_and_back_returns_home_ready_for_the_next() {
    let library = Arc::new(Library::in_memory());
    let repo = library.song_repo.clone();
    let first = repo.create_practice("First").expect("create");
    let second = repo.create_practice("Second").expect("create");
    let mut app = boot(library, Services::detached);
    assert!(shows(&app, "First") && shows(&app, "Second"));

    click(&mut app, "First");
    // The opening dialog holds for ~1.5 s before the push.
    assert!(shows(&app, "Loading practice"));
    assert_eq!(app.routes(), [Some(Route::Home)]);
    fire(&mut app, opening);
    assert_eq!(song_route(&app), first.id);
    assert!(!shows(&app, "Second"), "Home is covered");

    // Renamed while the practice is open; the way back reloads the list.
    let mut renamed = second.clone();
    renamed.title = "Second, renamed".into();
    repo.save_practice(&renamed).expect("save");
    back(&mut app);
    assert_eq!(app.routes(), [Some(Route::Home)]);
    assert!(shows(&app, "Second, renamed"));

    // The same or another practice opens again.
    click(&mut app, "Second, renamed");
    fire(&mut app, opening);
    assert_eq!(song_route(&app), second.id);
    back(&mut app);
    click(&mut app, "First");
    fire(&mut app, opening);
    assert_eq!(song_route(&app), first.id);
}

// ui_journey_test: New practice → the prompt → Create → the new practice's
// page → back to its card on Home.
#[test]
fn a_new_practice_opens_and_its_card_waits_at_home() {
    let library = Arc::new(Library::in_memory());
    let mut app = boot(library.clone(), Services::detached);
    assert!(shows(&app, "No practices yet"));

    click(&mut app, "New practice");
    assert_eq!(app.dialogs(), 1, "the prompt covers the window");
    assert!(shows(&app, "New song practice"));
    assert!(shows(&app, "Create"));
    let task = app.update(app::Message::Dialog(confirm_dialog::Message::Input(
        "Bohemian Rhapsody".into(),
    )));
    settle(&mut app, task);
    click(&mut app, "Create");
    assert_eq!(app.dialogs(), 0);
    assert!(shows(&app, "Loading practice"));
    fire(&mut app, opening);
    let id = song_route(&app);
    let created = library.song_repo.get_practice(&id).expect("created");
    assert_eq!(created.title, "Bohemian Rhapsody");

    back(&mut app);
    assert_eq!(app.routes(), [Some(Route::Home)]);
    assert!(shows(&app, "Bohemian Rhapsody"));
    assert!(!shows(&app, "No practices yet"));
}

// The journey's "hovering a card reveals its delete affordance; confirming
// removes it and the empty state returns".
#[test]
fn delete_confirms_over_the_window_and_the_empty_state_returns() {
    let library = Arc::new(Library::in_memory());
    library
        .song_repo
        .create_practice("Doomed practice")
        .expect("create");
    let mut app = boot(library, Services::detached);
    click(&mut app, &char::from(Icon::Trash2).to_string());
    assert_eq!(app.dialogs(), 1);
    assert!(shows(&app, "Delete \"Doomed practice\"?"));
    click(&mut app, "Delete");
    assert_eq!(app.dialogs(), 0);
    assert!(!shows(&app, "Doomed practice"));
    assert!(shows(&app, "No practices yet"));
}

fn newer_release(library: Arc<Library>, environment: AppEnvironment) -> Services {
    let mut services = Services::detached(library, environment);
    services.update_check = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(|_| {
            Ok(r#"{"latest": "9.9.9", "url": "https://example.com/download", "notes": "New voiceprint views."}"#.to_string())
        }),
        "1.0.0",
    ));
    services
}

// The update dialog goes through the app's dialog stack: it covers the drag
// bar, holds the keyboard, and Open Download Page keeps it up.
#[test]
fn the_update_dialog_covers_the_window() {
    let mut app = boot(Arc::new(Library::in_memory()), newer_release);
    fire(&mut app, |t| *t == Timer::UpdateCheck);
    assert!(shows(&app, "Vocara 9.9.9 is available."));

    click(&mut app, "Download");
    assert_eq!(app.dialogs(), 1);
    assert!(shows(&app, "Vocara 9.9.9 is available"));
    assert!(shows(&app, "New voiceprint views."));
    click(&mut app, "Later");
    assert_eq!(app.dialogs(), 0);

    click(&mut app, "Download");
    // Its task would open the browser: not run here.
    for message in clicked(&app, "Open Download Page") {
        let _ = app.update(message);
    }
    assert_eq!(app.dialogs(), 1, "it stays open: its URL is the fallback");
    let escape: Vec<app::Message> = {
        let mut ui = iced_test::simulator(app.view());
        let _ = ui.tap_key(iced::keyboard::Key::Named(
            iced::keyboard::key::Named::Escape,
        ));
        ui.into_messages().collect()
    };
    for message in escape {
        let task = app.update(message);
        settle(&mut app, task);
    }
    assert_eq!(app.dialogs(), 0);
    assert_eq!(app.routes(), [Some(Route::Home)]);
}
