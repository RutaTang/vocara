//! The app shell: boot, the fatal-open flow, routing end to end, the quit
//! guard, and dialogs over the window. (`widget_test.dart`'s boot check,
//! `deep_link_back_test.dart`'s routing, `main.dart`'s guards.)

use std::sync::atomic::{AtomicUsize, Ordering};

use iced::keyboard;

use super::*;
use crate::app::library::Library;
use crate::ui::core::motion::Curve;
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

/// Clicks the page's back button (the toolbar's, labelled "Back"; the
/// placeholders' Back buttons read the same).
fn back(app: &mut App) {
    let chevron = "Back";
    click(app, chevron);
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
    // The home header title, in its toolbar with the badge.
    assert!(shows(&app, "Vocara"));
    assert!(shows(&app, "DEV"));
    assert_eq!(app.title(), "Vocara — DEV");
}

#[test]
fn navigates_home_to_settings_and_back() {
    let mut app = boot(None);
    let (home, _) = app.top().expect("running");
    // The toolbar's settings button (an icon labelled by its tooltip).
    let settings = "Settings";
    click(&mut app, settings);
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
    assert!(shows(&app, "Page not found"));
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

/// An installed update quits the app through the quit guard — as Cmd+Q
/// does, so the recordings are finalized before the process goes — once the
/// self-update waits for it (the helper that swaps the bundle waits for this
/// process to exit); any other change of the self-update only redraws.
#[test]
fn an_installed_update_quits_through_the_quit_guard() {
    use crate::ui::home::widgets::update_notice::testing::{image, release, updates};
    let hooks = Arc::new(CountingHooks::default());
    let mut b = Boot::new(AppEnvironment::Dev);
    b.quit_hooks = hooks.clone();
    let (mut app, _) = App::new(b);
    let _ = app.update(Message::Library(LibraryEvent::Finished(
        LibraryOpen::Opened(Arc::new(Library::stub())),
    )));
    let fake = updates(true);
    let State::Running(running) = &mut app.state else {
        panic!("running")
    };
    running.services.self_update = fake.updater.clone();

    fake.updater.start(&release(Some(&image())));
    let _ = app.update(Message::UpdaterChanged);
    assert_eq!(
        hooks.kills.load(Ordering::SeqCst),
        0,
        "downloading: no quit"
    );
    assert!(!app.closing);
    fake.let_through(4);
    fake.settle();
    assert!(fake.updater.wants_quit());
    let _ = app.update(Message::UpdaterChanged);
    assert_eq!(hooks.kills.load(Ordering::SeqCst), 1, "the quit guard ran");
    assert!(app.closing);
    // Once.
    let _ = app.update(Message::UpdaterChanged);
    assert_eq!(hooks.kills.load(Ordering::SeqCst), 1);
}

/// The app hears every change of the self-update, for as long as it runs.
#[test]
fn the_app_hears_the_self_update_change() {
    use iced::futures::StreamExt;
    let updater = Arc::new(vocara_services::self_update::SelfUpdate::unavailable("x"));
    let mut changes = Box::pin(updater_changes(&UpdaterFeed(Arc::clone(&updater))));
    updater.prepare();
    let heard = iced::futures::executor::block_on(changes.next());
    assert_eq!(heard, Some(()));
    drop(changes);
    // Nobody left to hear it: the listener went with the stream.
    updater.dismiss_failure();
    updater.startup();
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

/// The app menu's Preferences… (⌘,) opens Settings, as Home's gear does:
/// not under a dialog, and once.
#[test]
fn preferences_opens_settings_unless_a_dialog_is_up() {
    let mut app = boot(None);
    open_dialog(&mut app);
    let _ = app.update(Message::PreferencesRequested);
    assert_eq!(app.routes(), vec![Some(Route::Home)], "not under a dialog");
    let _ = app.update(Message::Dialog(confirm_dialog::Message::Cancel));
    assert_eq!(app.dialogs(), 0);
    let _ = app.update(Message::PreferencesRequested);
    assert_eq!(app.routes(), vec![Some(Route::Home), Some(Route::Settings)]);
    let _ = app.update(Message::PreferencesRequested);
    assert_eq!(
        app.routes(),
        vec![Some(Route::Home), Some(Route::Settings)],
        "not a second Settings"
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
fn a_dialog_fades_in_and_out_over_150_ms_on_frames_while_it_does() {
    use crate::ui::core::widgets::confirm_dialog::testing::{assert_comes_in, assert_goes};
    use iced::advanced::subscription::into_recipes;
    fn running(app: &App) -> &Running {
        let State::Running(running) = &app.state else {
            panic!("running")
        };
        running
    }
    let frames =
        |app: &App, at: Instant| into_recipes(motion::frames(running(app).is_animating(at))).len();
    let t = confirm_dialog::TRANSITION;
    let mut app = boot(None);
    assert_eq!(
        frames(&app, Instant::now()),
        0,
        "an idle app asks for no frames"
    );

    let before = Instant::now();
    open_dialog(&mut app);
    let after = Instant::now();
    assert_comes_in(&running(&app).dialogs[0].motion, before, after);
    assert_eq!(frames(&app, after), 1, "frames while it comes in");
    assert_eq!(frames(&app, after + t), 0, "none once it is in");

    // Say it got all the way in.
    let State::Running(r) = &mut app.state else {
        panic!("running")
    };
    r.dialogs[0].motion = DialogMotion::shown();
    let before = Instant::now();
    let _ = app.update(Message::Dialog(confirm_dialog::Message::Cancel));
    let after = Instant::now();
    assert_eq!(app.dialogs(), 0, "closed at once");
    let going = &running(&app)
        .closing
        .first()
        .expect("drawn as it goes")
        .motion;
    assert_goes(going, before, after);
    assert_eq!(frames(&app, after), 1, "frames while it goes");
    // Drawn as it goes, it is out of reach.
    assert!(!shows(&app, "Delete “Take 3”?"), "out of reach");
    let _ = app.update(Message::Frame(after + t));
    assert!(running(&app).closing.is_empty(), "dropped once gone");
    assert_eq!(frames(&app, after + t), 0, "and the frames stop");
}

#[test]
fn a_page_that_leaves_takes_its_dialog_with_it() {
    let mut app = boot(Some("/settings"));
    open_dialog(&mut app);
    let _ = app.update(Message::Navigate(Navigation::Go(Route::Home)));
    assert_eq!(app.dialogs(), 0);
}

/// What a press in the window's title-bar band that pans, then a double
/// tap there, report on the app's view: whether it dragged the window and
/// zoomed it, and what else it did.
fn moves_from_the_band(app: &App) -> (bool, bool, Vec<Message>) {
    use iced::{mouse, Event, Point};
    let at = Point::new(420.0, 20.0);
    let press = || Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    let release = || Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
    let mut ui = iced_test::simulator(app.view());
    ui.point_at(at);
    let moved = Point::new(at.x + 10.0, at.y);
    let _ = ui.simulate([
        press(),
        Event::Mouse(mouse::Event::CursorMoved { position: moved }),
        release(),
    ]);
    std::thread::sleep(std::time::Duration::from_millis(60));
    ui.point_at(at);
    let _ = ui.simulate([press(), release()]);
    std::thread::sleep(std::time::Duration::from_millis(60));
    let _ = ui.simulate([press(), release()]);
    let mut dragged = false;
    let mut zoomed = false;
    let mut rest = Vec::new();
    for m in ui.into_messages() {
        match m {
            Message::DragWindow => dragged = true,
            Message::ToggleMaximize => zoomed = true,
            m => rest.push(m),
        }
    }
    (dragged, zoomed, rest)
}

// The content view no longer moves the window by itself (272bd6f), and
// only the pages' toolbars moved it: the waiting and fatal windows could
// not be moved at all, nor the window under a dialog.
#[test]
fn the_window_moves_and_zooms_from_its_top_band_on_every_screen() {
    let (dragged, zoomed, _) = moves_from_the_band(&boot(None));
    assert!(dragged && zoomed, "the home page's toolbar");

    let (mut app, _) = App::new(Boot::new(AppEnvironment::Dev));
    let _ = app.update(Message::Library(LibraryEvent::Waiting));
    let (dragged, zoomed, _) = moves_from_the_band(&app);
    assert!(dragged && zoomed, "the waiting window");

    let (mut app, _) = App::new(Boot::new(AppEnvironment::Prod));
    let _ = app.update(Message::Library(LibraryEvent::Finished(
        LibraryOpen::LibraryInUse("in use".into()),
    )));
    let (dragged, zoomed, _) = moves_from_the_band(&app);
    assert!(dragged && zoomed, "the fatal window");

    let mut app = boot(None);
    open_dialog(&mut app);
    let (dragged, zoomed, rest) = moves_from_the_band(&app);
    assert!(dragged && zoomed, "under a dialog");
    assert!(
        !rest.iter().any(|m| matches!(m, Message::Dialog(_))),
        "the band's taps dismiss nothing: {rest:?}"
    );
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

// ------------------------------------------------------- page transitions

/// The page transition running: its progress at `at` (the moving page's
/// offset, the covered page's, the shadow), or `None`.
fn route_at(app: &App, at: Instant) -> Option<(f32, f32, f32)> {
    let State::Running(running) = &app.state else {
        panic!("running")
    };
    running.route.as_ref().map(|r| r.progress(at))
}

fn frames_running(app: &App) -> bool {
    use iced::advanced::subscription::into_recipes;
    let State::Running(running) = &app.state else {
        panic!("running")
    };
    !into_recipes(motion::frames(running.is_animating(Instant::now()))).is_empty()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn a_pushed_page_slides_in_over_500_ms_and_the_one_beneath_a_third_out() {
    let mut app = boot(None);
    assert!(!frames_running(&app), "an idle app asks for no frames");
    let t0 = Instant::now();
    let _ = app.update(Message::Navigate(Navigation::Push(Route::Settings)));
    assert_eq!(
        app.routes(),
        vec![Some(Route::Home), Some(Route::Settings)],
        "at once"
    );
    assert!(frames_running(&app), "frames while it slides");
    // At its start: all the way right, Home where it was.
    let (primary, secondary, shadow) = route_at(&app, t0).expect("a transition");
    assert!(close(primary, 1.0) && close(secondary, 0.0) && close(shadow, 0.0));
    // Halfway in time (give or take the instant the push took).
    let half = t0 + ROUTE_TRANSITION / 2;
    let (primary, secondary, _) = route_at(&app, half).expect("a transition");
    let along = |curve: Curve| curve.transform(0.5);
    assert!((primary - (1.0 - along(Curve::FastEaseInToSlowEaseOut))).abs() < 0.01);
    assert!((secondary + along(Curve::LinearToEaseOut) / 3.0).abs() < 0.01);
    // At its end: in, and Home a third of the way out.
    let (primary, secondary, shadow) =
        route_at(&app, t0 + 2 * ROUTE_TRANSITION).expect("a transition");
    assert!(close(primary, 0.0) && close(secondary, -1.0 / 3.0) && close(shadow, 1.0));
    // A frame past its end lets it go, and the frames stop.
    let _ = app.update(Message::Frame(Instant::now() + ROUTE_TRANSITION));
    assert!(route_at(&app, Instant::now()).is_none());
    assert!(!frames_running(&app));
}

#[test]
fn a_popped_page_slides_back_out_and_the_one_beneath_back_in() {
    let mut app = boot(None);
    let _ = app.update(Message::Navigate(Navigation::Push(Route::Settings)));
    let _ = app.update(Message::Frame(Instant::now() + ROUTE_TRANSITION));
    let t0 = Instant::now();
    let _ = app.update(Message::Navigate(Navigation::Pop));
    assert_eq!(app.routes(), vec![Some(Route::Home)], "at once");
    assert!(
        !shows(&app, "Repair library"),
        "the page going takes no part"
    );
    assert!(shows(&app, "New practice"), "Home is on top again");
    let (primary, secondary, _) = route_at(&app, t0).expect("a transition");
    assert!(close(primary, 0.0) && close(secondary, -1.0 / 3.0), "start");
    // Back along the flipped curve, Home on `Curves.easeInToLinear`.
    let half = t0 + ROUTE_TRANSITION / 2;
    let (primary, secondary, _) = route_at(&app, half).expect("a transition");
    let flipped = Curve::FastEaseInToSlowEaseOutFlipped.transform(0.5);
    assert!((primary - (1.0 - flipped)).abs() < 0.01, "{primary}");
    let eased = Curve::EaseInToLinear.transform(0.5);
    assert!((secondary + eased / 3.0).abs() < 0.01, "{secondary}");
    let (primary, secondary, _) = route_at(&app, t0 + 2 * ROUTE_TRANSITION).expect("a transition");
    assert!(close(primary, 1.0) && close(secondary, 0.0), "end");
}

#[test]
fn a_page_popped_as_it_comes_in_goes_back_from_where_it_got() {
    let mut app = boot(None);
    let t0 = Instant::now();
    let _ = app.update(Message::Navigate(Navigation::Push(Route::Settings)));
    let _ = app.update(Message::Navigate(Navigation::Pop));
    let State::Running(running) = &app.state else {
        panic!("running")
    };
    let route = running.route.as_ref().expect("a transition");
    // It had barely started: it is back out within the time it took in.
    assert!(route.controller.ends_at(Instant::now()) < t0 + ROUTE_TRANSITION / 10);
}

#[test]
fn a_go_slides_the_new_page_over_the_ones_it_replaced() {
    let mut app = boot(Some("/settings"));
    let t0 = Instant::now();
    let _ = app.update(Message::Navigate(Navigation::Go(Route::Home)));
    assert_eq!(app.routes(), vec![Some(Route::Home)]);
    let (primary, secondary, _) = route_at(&app, t0).expect("a transition");
    assert!(close(primary, 1.0) && close(secondary, 0.0));
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

/// The exit watchdog is armed in the app `main` runs, never in the tests'
/// boot (a test that drives the quit would be cut short).
#[test]
fn only_the_real_boot_arms_the_exit_watchdog() {
    assert_eq!(Boot::new(AppEnvironment::Dev).exit_watchdog, None);
    let real = Boot::from_env(
        super::library::open_library_stub,
        Arc::new(super::quit::NoQuitHooks),
    );
    assert_eq!(real.exit_watchdog, Some(super::EXIT_WATCHDOG));
}
