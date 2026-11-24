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

/// Clicks the page's back chevron (the technique pages' headers have no
/// "Back" label; the placeholders' Back buttons carry the same glyph).
fn back(app: &mut App) {
    let chevron = char::from(crate::ui::core::icons::Icon::ChevronLeft).to_string();
    click(app, &chevron);
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
    assert!(!shows(&app, "New practice"), "Home is covered");

    back(&mut app);
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
    assert!(shows(&app, "New practice"));
    // Popped, not replaced: the same Home page.
    assert_eq!(app.top().map(|(id, _)| id), Some(home));
}

#[test]
fn a_late_message_for_a_page_that_is_gone_is_dropped() {
    let mut app = boot(None);
    click(&mut app, "Techniques");
    let (gone, _) = app.top().unwrap();
    back(&mut app);
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
    back(&mut app);
    assert_eq!(app.routes(), vec![Some(Route::Techniques)]);
}

#[test]
fn the_deep_linked_library_and_settings_go_back_home() {
    for start in ["/techniques", "/settings"] {
        let mut app = boot(Some(start));
        back(&mut app);
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
fn a_closed_dialogs_barrier_fades_out_over_150_ms_on_frames_while_it_does() {
    use iced::advanced::subscription::into_recipes;
    use crate::ui::core::motion::Status;
    let mut app = boot(None);
    let frames = |app: &App| {
        let State::Running(running) = &app.state else {
            panic!("running")
        };
        into_recipes(motion::frames(running.is_animating(Instant::now()))).len()
    };
    assert_eq!(frames(&app), 0, "an idle app asks for no frames");
    open_dialog(&mut app);
    let closed = Instant::now();
    let _ = app.update(Message::Dialog(confirm_dialog::Message::Cancel));
    assert_eq!(app.dialogs(), 0, "closed at once");
    let barrier = |app: &App| {
        let State::Running(running) = &app.state else {
            panic!("running")
        };
        running.closing.first().cloned()
    };
    let fading = barrier(&app).expect("its barrier fades out");
    assert_eq!(frames(&app), 1, "frames while it does");
    let t = confirm_dialog::TRANSITION;
    assert_eq!(fading.status(closed), Status::Reverse);
    // At its start it is all there; halfway, on `Curves.ease`; then gone.
    assert!(fading.curved(Curve::Ease, None, closed) > 0.99);
    let half = closed + t / 2;
    let middle = fading.curved(Curve::Ease, None, half);
    assert!((middle - Curve::Ease.transform(fading.value(half))).abs() < 1e-6);
    assert!(middle > 0.4 && middle < 0.9, "{middle}");
    assert_eq!(fading.curved(Curve::Ease, None, closed + t * 2), 0.0, "end");
    let _ = app.update(Message::Frame(Instant::now() + t));
    assert!(barrier(&app).is_none(), "dropped once faded");
    assert_eq!(frames(&app), 0, "and the frames stop");
}

#[test]
fn a_page_that_leaves_takes_its_dialog_with_it() {
    let mut app = boot(Some("/settings"));
    open_dialog(&mut app);
    let _ = app.update(Message::Navigate(Navigation::Go(Route::Home)));
    assert_eq!(app.dialogs(), 0);
}

#[test]
fn a_quit_from_the_menu_or_the_dock_runs_the_same_guard_once() {
    let hooks = Arc::new(CountingHooks::default());
    let mut b = Boot::new(AppEnvironment::Dev);
    b.quit_hooks = hooks.clone();
    let (mut app, _) = App::new(b);
    let _ = app.update(Message::Library(LibraryEvent::Finished(
        LibraryOpen::Opened(Arc::new(Library::stub())),
    )));
    let _ = app.update(Message::QuitRequested);
    // The red close button meanwhile, and a second Cmd+Q.
    let _ = app.update(Message::CloseRequested(window::Id::unique()));
    let _ = app.update(Message::QuitRequested);
    assert_eq!(hooks.kills.load(Ordering::SeqCst), 1);
    assert!(app.closing);
}

// The app's half of `vocara_services::quit_request`: what the intercepted
// `terminate:` asks arrives as the app's quit requests.
#[test]
fn a_terminate_from_macos_reaches_the_app_as_a_quit_request() {
    use iced::futures::StreamExt;
    use vocara_services::quit_request::{QuitRequests, Verdict};

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let mut requests = Box::pin(quit_requests());
    assert_eq!(
        QuitRequests::global().terminate(std::time::Instant::now()),
        Verdict::Asked
    );
    let asked = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(5), requests.next()).await });
    assert_eq!(asked, Ok(Some(())));
}

/// What [`waiting_open`] waits for: another Vocara's boot to finish.
static OPEN_RELEASED: std::sync::Mutex<bool> = std::sync::Mutex::new(false);
static OPEN_RELEASE: std::sync::Condvar = std::sync::Condvar::new();

/// An open that waits on another Vocara's boot until
/// [`release_waiting_open`].
fn waiting_open(on_waiting: &mut dyn FnMut()) -> LibraryOpen {
    on_waiting();
    let mut released = OPEN_RELEASED.lock().unwrap();
    while !*released {
        released = OPEN_RELEASE.wait(released).unwrap();
    }
    LibraryOpen::LibraryInUse("released".into())
}

fn release_waiting_open() {
    *OPEN_RELEASED.lock().unwrap() = true;
    OPEN_RELEASE.notify_all();
}

// The waiting window's Quit: iced exits, and its runtime is torn down while
// the open still waits on the other Vocara. The teardown must not wait for
// the open (Dart's quit ended the process at once).
#[test]
fn a_library_open_still_waiting_does_not_hold_the_exit() {
    use iced::futures::StreamExt;

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (waiting, mut events) = iced::futures::channel::mpsc::channel(4);
    let open = runtime.spawn(open_on_own_thread(waiting_open, waiting));
    let first = runtime.block_on(events.next());
    assert!(matches!(first, Some(LibraryEvent::Waiting)), "{first:?}");

    let (dropped, teardown) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(open);
        drop(runtime);
        let _ = dropped.send(());
    });
    let done = teardown.recv_timeout(Duration::from_secs(5));
    // Let the open finish either way, so a failure here cannot hang the run.
    release_waiting_open();
    assert!(done.is_ok(), "the runtime's teardown waited on the open");
}

#[test]
fn a_library_open_that_panics_fails_the_boot() {
    fn panicking_open(_: &mut dyn FnMut()) -> LibraryOpen {
        panic!("the disk is on fire")
    }
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (waiting, _events) = iced::futures::channel::mpsc::channel(4);
    let outcome = runtime.block_on(open_on_own_thread(panicking_open, waiting));
    assert!(
        matches!(&outcome, LibraryOpen::Failed { detail, .. } if detail == "the disk is on fire"),
        "{outcome:?}"
    );
}
