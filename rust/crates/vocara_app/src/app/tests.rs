//! The app shell: boot, the fatal-open flow, routing end to end, the quit
//! guard, and dialogs over the window. (`widget_test.dart`'s boot check,
//! `deep_link_back_test.dart`'s routing, `main.dart`'s guards.)

use std::sync::atomic::{AtomicUsize, Ordering};

use iced::keyboard;

use super::*;
use crate::app::library::Library;
use crate::ui::core::widgets::confirm_dialog::ConfirmDialog;

fn boot(start: Option<&str>) -> App {
    let mut b = Boot::new(AppEnvironment::Dev);
    b.start = start.map(str::to_string);
    let (mut app, _open) = App::new(b);
    let _ = app.update(Message::Library(LibraryEvent::Finished(
        LibraryOpen::Opened(Arc::new(Library::stub())),
    )));
    app
}

/// Clicks `label` on the app's current view and feeds the messages back.
fn click(app: &mut App, label: &str) {
    let messages: Vec<Message> = {
        let mut ui = iced_test::simulator(app.view());
        ui.click(label).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        ui.into_messages().collect()
    };
    for m in messages {
        let _ = app.update(m);
    }
}

fn shows(app: &App, label: &str) -> bool {
    let mut ui = iced_test::simulator(app.view());
    ui.find(label).is_ok()
}

#[test]
fn the_app_boots_to_the_home_screen() {
    let (app, _task) = App::new(Boot::new(AppEnvironment::Dev));
    assert!(app.routes().is_empty(), "nothing until the library is open");
    let app = boot(None);
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
    // The home header title, under the drag bar with its badge.
    assert!(shows(&app, "Vocara"));
    assert!(shows(&app, "DEV"));
    assert_eq!(app.title(), "Vocara — DEV");
}

#[test]
fn navigates_home_to_settings_and_back() {
    let mut app = boot(None);
    let (home, _) = app.top().expect("running");
    // The header's settings button (an icon with a "Settings" tooltip).
    let settings = char::from(crate::ui::core::icons::Icon::Settings).to_string();
    click(&mut app, &settings);
    assert_eq!(app.routes(), vec![Some(Route::Home), Some(Route::Settings)]);
    assert!(shows(&app, "Settings"));
    assert!(!shows(&app, "Vocara"), "Home is covered");

    click(&mut app, "Back");
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
    assert!(shows(&app, "Vocara"));
    // Popped, not replaced: the same Home page.
    assert_eq!(app.top().map(|(id, _)| id), Some(home));
}

#[test]
fn a_late_message_for_a_page_that_is_gone_is_dropped() {
    let mut app = boot(None);
    click(&mut app, "Techniques");
    let (gone, _) = app.top().unwrap();
    click(&mut app, "Back");
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
    // A result for the library arriving after it closed changes nothing.
    let _ = app.update(Message::Techniques(gone, technique_library::Message::Back));
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
}

// deep_link_back_test: a deep-linked page is the ROOT of the stack; its back
// affordance goes to its parent instead of popping into nothing.
#[test]
fn a_deep_linked_technique_goes_back_to_the_library() {
    let mut app = boot(Some("/techniques/T1000000000000000000000000"));
    assert_eq!(
        app.routes(),
        vec![Some(Route::Technique {
            id: "T1000000000000000000000000".into()
        })]
    );
    click(&mut app, "Back");
    assert_eq!(app.routes(), vec![Some(Route::Techniques)]);
}

#[test]
fn the_deep_linked_library_and_settings_go_back_home() {
    for start in ["/techniques", "/settings"] {
        let mut app = boot(Some(start));
        click(&mut app, "Back");
        assert_eq!(app.routes(), vec![Some(Route::Home)], "{start}");
    }
}

#[test]
fn a_deep_link_to_nowhere_shows_the_error_page_with_a_way_home() {
    let mut app = boot(Some("/nope"));
    assert_eq!(app.routes(), vec![None]);
    assert!(shows(&app, "Page Not Found"));
    assert!(shows(&app, "GoException: no routes for location: /nope"));
    click(&mut app, "Home");
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
}

#[test]
fn a_refused_or_failed_open_shows_its_fatal_window() {
    let cases = [
        (
            LibraryOpen::NewerLibrary("newer".into()),
            FatalOpenScreen::newer_library(),
        ),
        (
            LibraryOpen::LibraryInUse("in use".into()),
            FatalOpenScreen::library_in_use(),
        ),
        (
            LibraryOpen::Failed {
                detail: "SqliteException(11)".into(),
                log: "SqliteException(11)\n<stack>".into(),
            },
            FatalOpenScreen::open_failed("SqliteException(11)"),
        ),
    ];
    for (outcome, screen) in cases {
        let (mut app, _) = App::new(Boot::new(AppEnvironment::Prod));
        let _ = app.update(Message::Library(LibraryEvent::Finished(outcome)));
        assert_eq!(app.fatal(), Some(&screen));
        assert!(app.routes().is_empty());
        assert!(shows(&app, &screen.title));
    }
}

#[test]
fn the_waiting_window_gives_way_to_the_app_once_the_library_opens() {
    let (mut app, _) = App::new(Boot::new(AppEnvironment::Dev));
    let _ = app.update(Message::Library(LibraryEvent::Waiting));
    assert_eq!(app.fatal(), Some(&FatalOpenScreen::waiting_for_library()));
    let _ = app.update(Message::Library(LibraryEvent::Finished(
        LibraryOpen::Opened(Arc::new(Library::stub())),
    )));
    assert_eq!(app.fatal(), None);
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
}

#[derive(Default)]
struct CountingHooks {
    kills: AtomicUsize,
}

impl QuitHooks for CountingHooks {
    fn kill_separation_sidecars(&self) {
        self.kills.fetch_add(1, Ordering::SeqCst);
    }
    fn finalize_recordings(&self) {}
}

#[test]
fn the_quit_guard_runs_once_however_many_close_requests_arrive() {
    let hooks = Arc::new(CountingHooks::default());
    let mut b = Boot::new(AppEnvironment::Dev);
    b.quit_hooks = hooks.clone();
    let (mut app, _) = App::new(b);
    let id = window::Id::unique();
    let _ = app.update(Message::CloseRequested(id));
    let _ = app.update(Message::CloseRequested(id));
    // The fatal window's Quit goes through the same guard.
    let _ = app.update(Message::Fatal(fatal_open_screen::Message::Quit));
    assert_eq!(hooks.kills.load(Ordering::SeqCst), 1);
    assert!(app.closing);
}

/// Opens a confirm dialog for the page on top, as a screen's `Update::dialog`
/// does; a confirmed result navigates to Settings.
fn open_dialog(app: &mut App) {
    let State::Running(running) = &mut app.state else {
        panic!("running")
    };
    let page = running.router.top().0;
    let _ = apply(
        running,
        PageUpdate {
            task: Task::none(),
            navigation: None,
            dialog: Some(PendingDialog {
                page,
                dialog: ConfirmDialog::new("Delete “Take 3”?").into(),
                on_result: Box::new(move |r| {
                    if r.confirmed() {
                        Message::Navigate(Navigation::Push(Route::Settings))
                    } else {
                        Message::Done
                    }
                }),
            }),
        },
    );
}

#[test]
fn a_dialog_covers_the_window_and_its_result_reaches_its_page() {
    let mut app = boot(None);
    open_dialog(&mut app);
    assert_eq!(app.dialogs(), 1);
    assert!(shows(&app, "Delete “Take 3”?"));

    // Escape cancels; nothing beneath sees the key.
    let messages: Vec<Message> = {
        let mut ui = iced_test::simulator(app.view());
        let _ = ui.tap_key(keyboard::Key::Named(keyboard::key::Named::Escape));
        ui.into_messages().collect()
    };
    assert!(matches!(
        messages.as_slice(),
        [Message::Dialog(confirm_dialog::Message::Cancel)]
    ));
    for m in messages {
        let _ = app.update(m);
    }
    assert_eq!(app.dialogs(), 0);
    assert_eq!(app.routes(), vec![Some(Route::Home)]);

    // Confirming hands the result to the page's continuation.
    open_dialog(&mut app);
    click(&mut app, "Delete");
    assert_eq!(app.dialogs(), 0);
    assert_eq!(app.routes(), vec![Some(Route::Home), Some(Route::Settings)]);
}

#[test]
fn a_page_that_leaves_takes_its_dialog_with_it() {
    let mut app = boot(Some("/settings"));
    open_dialog(&mut app);
    let _ = app.update(Message::Navigate(Navigation::Go(Route::Home)));
    assert_eq!(app.dialogs(), 0);
}
