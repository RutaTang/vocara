//! The home screen, driven as the Dart widget tests drove it
//! (`home_update_check_test.dart`, `home_delete_queue_test.dart`,
//! `home_landing_refresh_test.dart`, the Home cases of
//! `home_reopen_test.dart`, `list_remote_refresh_test.dart` and the
//! integration journeys): the screen alone over the app's services, its
//! tasks run to completion and their messages fed back; its timers fired by
//! hand, as flutter_test's fake clock pumped them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use vocara_model::config::environment::AppEnvironment;
use vocara_services::update_check_service::{FetchError, UpdateCheckService, UpdateStatus};
use vocara_store::repositories::SongPracticeRepository;

use super::*;
use crate::app::library::Library;
use crate::router::Navigation;
use crate::ui::core::widgets::confirm_dialog::{Dialog, DialogResult};
use crate::ui::home::testing::{remote_applied, run_task, spawn_task, summary, FakePractices};
use crate::ui::DialogRequest;

fn services_over(repo: Arc<dyn SongPracticeRepository>) -> Services {
    let mut services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
    services.song_repo = repo;
    services
}

/// The screen and what it asked of the app.
struct Home {
    screen: HomeScreen,
    services: Services,
    /// Where it navigated, in order.
    navigations: Vec<Navigation>,
    /// The dialog it asked for, while open.
    dialog: Option<DialogRequest<Message>>,
}

impl Home {
    /// Mounts the screen and lets its first read land.
    fn mount(services: Services) -> Self {
        let (screen, first) = HomeScreen::new(&services);
        let mut home = Home {
            screen,
            services,
            navigations: Vec::new(),
            dialog: None,
        };
        home.drive(first);
        home
    }

    fn over(repo: Arc<dyn SongPracticeRepository>) -> Self {
        Self::mount(services_over(repo))
    }

    /// Runs `task` to completion, feeding back what it reports.
    fn drive(&mut self, task: Task<Message>) {
        for message in run_task(task) {
            self.send(message);
        }
    }

    /// Handles `message`; its work is run to completion.
    fn send(&mut self, message: Message) {
        let task = self.send_held(message);
        self.drive(task);
    }

    /// Handles `message`, handing back its work unrun.
    fn send_held(&mut self, message: Message) -> Task<Message> {
        let update = self.screen.update(message, &self.services);
        self.take(update)
    }

    fn take(&mut self, update: Update<Message>) -> Task<Message> {
        if let Some(navigation) = update.navigation {
            self.navigations.push(navigation);
        }
        if let Some(dialog) = update.dialog {
            assert!(self.dialog.is_none(), "one dialog at a time");
            self.dialog = Some(dialog);
        }
        update.task
    }

    /// Closes the open dialog with `result`.
    fn answer(&mut self, result: DialogResult) {
        let dialog = self.dialog.take().expect("a dialog is open");
        self.send((dialog.on_result)(result));
    }

    /// Fires `timer`, which must be running.
    fn fire(&mut self, timer: Timer) {
        assert!(
            self.screen.timers().contains(&timer),
            "{timer:?} runs: {:?}",
            self.screen.timers()
        );
        self.send(timer.message());
    }

    /// The pushed page was popped.
    fn pop_back(&mut self) {
        let update = self.screen.returned(&self.services);
        let task = self.take(update);
        self.drive(task);
    }

    /// Clicks `label` on the screen and handles what it reports.
    fn click(&mut self, label: &str) {
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(self.screen.view());
            ui.click(label).unwrap_or_else(|e| panic!("{label}: {e:?}"));
            ui.into_messages().collect()
        };
        for message in messages {
            self.send(message);
        }
    }

    fn shows(&self, label: &str) -> bool {
        let mut ui = iced_test::simulator(self.screen.view());
        ui.find(label).is_ok()
    }

    fn titles(&self) -> Vec<String> {
        self.screen
            .vm()
            .projects()
            .iter()
            .map(|p| p.title.clone())
            .collect()
    }

    /// The open timer running now.
    fn opening_timer(&self) -> Timer {
        self.screen
            .timers()
            .into_iter()
            .find(|t| matches!(t, Timer::Opening(_)))
            .expect("an open in flight")
    }

    fn snack_timer(&self) -> crate::ui::snack::SnackTimer {
        self.screen.snack_timer().expect("a toast up")
    }
}

fn glyph(icon: Icon) -> String {
    char::from(icon).to_string()
}

fn song(id: &str) -> Navigation {
    Navigation::Push(Route::Song { id: id.to_string() })
}

// ----------------------------------------------------------- the page

#[test]
fn an_empty_library_shows_the_empty_state_and_the_header() {
    let home = Home::over(FakePractices::with(Vec::new()));
    assert!(home.shows("Vocara"));
    assert!(home.shows("No practices yet"));
    assert!(home.shows("Create your first song practice to start recording and taking notes."));
    // The three header affordances are always present on Home.
    assert!(home.shows("New practice"));
    assert!(home.shows("Techniques"));
    assert!(home.shows("Settings"));
}

#[test]
fn the_header_pushes_the_technique_library_and_settings() {
    let mut home = Home::over(FakePractices::with(Vec::new()));
    home.click("Techniques");
    home.click("Settings");
    assert_eq!(
        home.navigations,
        [
            Navigation::Push(Route::Techniques),
            Navigation::Push(Route::Settings)
        ]
    );
    // Neither push is awaited: their return leaves the list alone.
    let reads = home.screen.vm().load().result().map(|r| r.is_ok());
    home.pop_back();
    assert_eq!(home.screen.vm().load().result().map(|r| r.is_ok()), reads);
    assert!(!home.screen.vm().load().running());
}

#[test]
fn the_list_shows_a_spinner_while_it_loads_then_the_cards() {
    let repo = FakePractices::with(vec![
        summary("a", "Bohemian Rhapsody"),
        summary("b", "Hai Kuo Tian Kong"),
    ]);
    let services = services_over(repo);
    let (screen, first) = HomeScreen::new(&services);
    assert!(screen.vm().load().running());
    {
        let mut ui = iced_test::simulator(screen.view());
        assert!(ui.find("No practices yet").is_err(), "loading, not empty");
        assert!(ui.find("Could not load your practices").is_err());
    }
    let mut home = Home {
        screen,
        services,
        navigations: Vec::new(),
        dialog: None,
    };
    home.drive(first);
    assert!(home.shows("Bohemian Rhapsody"));
    assert!(home.shows("Hai Kuo Tian Kong"));
    assert!(!home.shows("No practices yet"));
}

// A failed list read must read as an error (with Retry), never collapse into
// the empty state.
#[test]
fn a_failed_read_shows_the_error_with_a_retry_never_the_empty_state() {
    let repo = FakePractices::with(vec![summary("a", "Kept")]);
    repo.state().unreadable = true;
    let mut home = Home::over(repo.clone());
    assert!(home.shows("Could not load your practices"));
    assert!(home.shows("Something went wrong reading your library. Your data is safe."));
    assert!(!home.shows("No practices yet"));
    repo.state().unreadable = false;
    home.click("Retry");
    assert!(home.shows("Kept"));
    assert!(!home.shows("Could not load your practices"));
}

#[test]
fn the_grid_fits_cards_at_most_280_wide() {
    // Flutter's `(width / (280 + 16)).ceil()`, at least one.
    assert_eq!(grid_columns(0.0), 1);
    assert_eq!(grid_columns(280.0), 1);
    assert_eq!(grid_columns(296.0), 1);
    assert_eq!(grid_columns(297.0), 2);
    assert_eq!(grid_columns(592.0), 2);
    assert_eq!(grid_columns(976.0), 4);
}

// ------------------------------------------------------------- create

#[test]
fn a_created_practice_opens_after_the_loading_dialog() {
    let library = Library::in_memory();
    let mut home = Home::over(library.song_repo.clone());
    home.click("New practice");
    let Some(DialogRequest {
        dialog: Dialog::Prompt(prompt),
        ..
    }) = &home.dialog
    else {
        panic!("the new practice prompt")
    };
    assert_eq!(prompt.title, "New song practice");
    home.answer(DialogResult::Text(Some("Bohemian Rhapsody".into())));

    // The card is on the list at once, and the open begins.
    assert_eq!(home.titles(), ["Bohemian Rhapsody"]);
    let id = home.screen.vm().projects()[0].id.clone();
    assert_eq!(home.screen.opening(), Some(id.as_str()));
    assert!(home.shows("Loading practice"));
    assert!(home.navigations.is_empty(), "the dialog holds for a beat");
    let timer = home.opening_timer();
    assert_eq!(timer.delay(), Duration::from_millis(1500));
    home.fire(timer);
    assert!(!home.shows("Loading practice"));
    assert_eq!(home.navigations, [song(&id)]);
}

#[test]
fn the_loading_card_pads_its_content_inside_its_border() {
    // 20 by 16 inside the dialog card (its outline inset, taking no room):
    // the 40 disc, 12, then the label (13 px on an 18 px line), the card
    // centred in the 1024 × 768 window.
    let mut ui = iced_test::simulator(opening_dialog(Fade::SHOWN));
    let label = ui.find("Loading practice").expect("the label").bounds();
    assert_eq!(label.height, 18.0, "{label:?}");
    let height = 2.0 * 20.0 + 40.0 + 12.0 + 18.0;
    let card_top = (768.0 - height) / 2.0;
    assert!(
        (label.y - (card_top + 20.0 + 40.0 + 12.0)).abs() < 1e-3,
        "{label:?}"
    );
}

#[test]
fn the_loading_dialog_comes_in_and_goes_over_150_ms_with_frames_only_meanwhile() {
    use crate::ui::core::motion;
    use crate::ui::core::widgets::confirm_dialog::testing::{assert_comes_in, assert_goes};
    use crate::ui::core::widgets::confirm_dialog::{DialogMotion, TRANSITION};
    use iced::advanced::subscription::into_recipes;
    use std::time::Instant;

    let t = TRANSITION;
    let frames =
        |home: &Home, at: Instant| into_recipes(motion::frames(home.screen.is_animating(at))).len();
    let mut home = Home::over(FakePractices::with(vec![summary("a", "Song")]));
    assert_eq!(frames(&home, Instant::now()), 0, "idle");

    let before = Instant::now();
    home.send(Message::Open("a".into()));
    let after = Instant::now();
    assert_comes_in(home.screen.loading.as_ref().expect("drawn"), before, after);
    assert_eq!(frames(&home, after), 1, "frames while it comes in");
    assert_eq!(frames(&home, after + t), 0, "none once it is in");

    // Say it got all the way in. The hold over, it goes as the practice is
    // pushed, out of reach.
    home.screen.loading = Some(DialogMotion::shown());
    let timer = home.opening_timer();
    let before = Instant::now();
    home.fire(timer);
    let after = Instant::now();
    assert_eq!(home.navigations, [song("a")]);
    assert_goes(home.screen.loading.as_ref().expect("drawn"), before, after);
    assert!(!home.shows("Loading practice"), "out of reach");
    assert_eq!(frames(&home, after), 1, "frames while it goes");
    home.send(Message::DialogFrame(after + t));
    assert!(home.screen.loading_fade(after + t).is_none(), "gone");
    assert_eq!(frames(&home, after + t), 0, "and the frames stop");
}

#[test]
fn a_cancelled_prompt_creates_nothing() {
    let repo = FakePractices::with(Vec::new());
    let mut home = Home::over(repo.clone());
    home.click("New practice");
    home.answer(DialogResult::Text(None));
    assert_eq!(repo.state().creates, 0);
    assert!(home.screen.opening().is_none());
}

#[test]
fn a_failed_create_says_so_and_opens_nothing() {
    let repo = FakePractices::with(vec![summary("a", "Kept")]);
    repo.state().uncreatable = true;
    let mut home = Home::over(repo);
    home.click("New practice");
    home.answer(DialogResult::Text(Some("New".into())));
    assert_eq!(home.screen.snack(), Some(CREATE_FAILED));
    assert!(home.shows(CREATE_FAILED));
    assert!(home.screen.opening().is_none());
    assert_eq!(home.titles(), ["Kept"]);
    // The result is cleared once shown.
    assert!(home.screen.vm().create_project().result().is_none());
    // Up 2.8 s from the end of its entrance.
    let timer = home.snack_timer();
    let latest = std::time::Instant::now() + crate::ui::snack::TRANSITION;
    assert!(timer.deadline <= latest + Duration::from_millis(2800));
    assert!(timer.deadline > latest + Duration::from_millis(2500));
    home.send(Message::SnackExpired(timer.seq));
    assert!(home.screen.snack().is_none());
    assert!(home.shows(CREATE_FAILED), "leaving");
    home.send(Message::SnackFrame(
        std::time::Instant::now() + crate::ui::snack::TRANSITION,
    ));
    assert!(!home.shows(CREATE_FAILED), "gone");
}

// -------------------------------------------------------------- open

#[test]
fn a_rapid_second_tap_opens_once() {
    let mut home = Home::over(FakePractices::with(vec![
        summary("a", "First"),
        summary("b", "Second"),
    ]));
    home.click("First");
    home.click("Second");
    assert_eq!(home.screen.opening(), Some("a"));
    assert_eq!(
        home.screen
            .timers()
            .iter()
            .filter(|t| matches!(t, Timer::Opening(_)))
            .count(),
        1
    );
    let timer = home.opening_timer();
    home.fire(timer);
    // A stale timer message changes nothing.
    home.send(timer.message());
    assert_eq!(home.navigations, [song("a")]);
}

// home_reopen_test: Home → a practice → back → Home → the SAME or another
// practice must open again. The awaited push's completion is what reloads
// the list and releases the double-tap guard.
#[test]
fn the_return_from_a_practice_reloads_and_leaves_home_ready_to_open_the_next() {
    let library = Library::in_memory();
    let repo = library.song_repo.clone();
    repo.create_practice("First").expect("create");
    repo.create_practice("Second").expect("create");
    let mut home = Home::over(repo.clone());
    assert_eq!(home.titles(), ["Second", "First"]);

    let open_and_come_back = |home: &mut Home, title: &str, while_open: &dyn Fn()| {
        home.click(title);
        let timer = home.opening_timer();
        home.fire(timer);
        let Some(Navigation::Push(Route::Song { id })) = home.navigations.last().cloned() else {
            panic!("tapping {title} opens its detail")
        };
        let summary = home
            .screen
            .vm()
            .projects()
            .iter()
            .find(|p| p.id == id)
            .cloned();
        assert_eq!(summary.map(|p| p.title), Some(title.to_string()));
        while_open();
        home.pop_back();
        assert!(home.screen.opening().is_none(), "the guard is released");
    };

    open_and_come_back(&mut home, "First", &|| {});
    // The same awaited push is what refreshes the list on the way back.
    open_and_come_back(&mut home, "Second", &|| {
        let all = repo.list_practices().expect("list");
        let mut second = all
            .into_iter()
            .find(|p| p.title == "Second")
            .expect("second");
        second.title = "Second, renamed".into();
        repo.save_practice(&second).expect("save");
    });
    assert!(
        home.shows("Second, renamed"),
        "returning from a practice reloads the Home list"
    );
    open_and_come_back(&mut home, "First", &|| {});
    assert_eq!(home.navigations.len(), 3);
}

// home_landing_refresh_test: a save started on a practice's screen can
// outlive it: a stopped take lands after Back. Home reloaded its list as the
// screen popped and never again, so the card kept the practice as it was
// before the save.
#[test]
fn a_save_still_landing_after_back_reaches_the_card_once_it_lands() {
    let library = Library::in_memory();
    let repo = library.song_repo.clone();
    let practice = repo.create_practice("First").expect("create");
    let mut home = Home::over(repo.clone());
    home.click("First");
    let timer = home.opening_timer();
    home.fire(timer);
    assert_eq!(home.navigations, [song(&practice.id)]);

    // The practice screen starts a save that lands later, then goes Back.
    let landing = RecordingFinalizer::instance().begin_landing(&practice.id);
    let update = home.screen.returned(&home.services);
    let returned = spawn_task(home.take(update));
    let first = returned.recv().expect("the reload on return");
    assert!(matches!(first, Message::Landed(Landing::Loaded(_))));
    home.send(first);
    assert!(home.shows("First"));
    assert!(home.screen.opening().is_none(), "not held while it lands");

    // It lands.
    let mut landed = practice.clone();
    landed.title = "First, landed".into();
    repo.save_practice(&landed).expect("save");
    drop(landing);
    let settled = returned.recv().expect("the landing settles");
    assert!(matches!(&settled, Message::Settled(id) if *id == practice.id));
    home.send(settled);
    assert!(home.shows("First, landed"));
}

#[test]
fn the_opening_timer_holds_its_beat_then_reports_once() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime");
    rt.block_on(async {
        let started = std::time::Instant::now();
        let mut stream = std::pin::pin!(timer_stream(&Timer::Opening(7)));
        let message = stream.next().await;
        assert!(matches!(message, Some(Message::OpeningElapsed(7))));
        assert!(started.elapsed() >= OPENING_HOLD);
        // Then nothing more.
        let more = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
        assert!(more.is_err());
    });
}

// ------------------------------------------------------------ delete

#[test]
fn delete_asks_first_then_removes_the_card() {
    let repo = FakePractices::with(vec![summary("a", "Doomed practice")]);
    let mut home = Home::over(repo.clone());
    // The trash shows while the card is hovered; a click there hovers it.
    home.click(&glyph(Icon::Trash2));
    let Some(DialogRequest {
        dialog: Dialog::Confirm(confirm),
        ..
    }) = &home.dialog
    else {
        panic!("a delete confirmation")
    };
    assert_eq!(confirm.title, "Delete \"Doomed practice\"?");
    assert_eq!(
        confirm.body.as_deref(),
        Some("This removes the practice and all of its notes. This cannot be undone.")
    );
    assert_eq!(confirm.confirm_label, "Delete");
    assert!(confirm.destructive);
    home.answer(DialogResult::Confirmed(false));
    assert_eq!(repo.state().deletes, 0);
    assert!(home.shows("Doomed practice"));

    home.click(&glyph(Icon::Trash2));
    home.answer(DialogResult::Confirmed(true));
    assert_eq!(repo.state().deletes, 1);
    assert!(!home.shows("Doomed practice"));
    assert!(home.shows("No practices yet"));
}

#[test]
fn a_failed_delete_says_so_and_keeps_the_card() {
    let repo = FakePractices::with(vec![summary("a", "Kept")]);
    repo.state().undeletable = true;
    let mut home = Home::over(repo);
    home.send(Message::DeleteConfirmed {
        id: "a".into(),
        confirmed: true,
    });
    assert_eq!(home.screen.snack(), Some(DELETE_FAILED));
    assert!(home.shows("Kept"));
    assert!(home.screen.vm().delete_project().result().is_none());
}

// home_delete_queue_test: a practice delete confirmed while another one
// still runs: the command drops a call made while it runs, so the second
// card stayed, with no word said.
#[test]
fn a_delete_confirmed_while_another_runs_waits_its_turn_instead_of_being_dropped() {
    let repo = FakePractices::with(vec![summary("1", "First"), summary("2", "Second")]);
    let mut home = Home::over(repo.clone());
    // The first parks in the repository...
    let first = home.send_held(Message::DeleteConfirmed {
        id: "1".into(),
        confirmed: true,
    });
    assert!(home.screen.vm().delete_project().running());
    // ...and the second is confirmed meanwhile.
    let second = home.send_held(Message::DeleteConfirmed {
        id: "2".into(),
        confirmed: true,
    });
    assert!(run_task(second).is_empty(), "it waits its turn");
    home.drive(first);
    assert_eq!(repo.state().deletes, 2);
    assert!(repo.state().practices.is_empty());
    assert!(!home.shows("Second"));
    assert!(home.shows("No practices yet"));
}

// ------------------------------------------------------ remote changes

#[test]
fn a_remote_change_to_a_practice_reloads_the_list_and_one_to_a_technique_does_not() {
    let repo = FakePractices::with(vec![summary("a", "Old")]);
    let mut home = Home::over(repo.clone());
    repo.state().practices = vec![summary("a", "New")];
    home.send(Message::RemoteApplied(remote_applied(&[], &[], &["T1"])));
    assert!(home.shows("Old"));
    assert_eq!(repo.state().summary_reads, 1);
    home.send(Message::RemoteApplied(remote_applied(
        &[("tracks", "TR1")],
        &["a"],
        &[],
    )));
    assert!(home.shows("New"));
    assert_eq!(repo.state().summary_reads, 2);
}

#[test]
fn the_screen_listens_to_the_librarys_remote_applied_broadcast() {
    let broadcast = Arc::new(RemoteAppliedBroadcast::default());
    // Announced with nobody listening: dropped, as a broadcast stream does.
    broadcast.publish(&remote_applied(&[], &["gone"], &[]));
    let mut events = feed_stream(&Feed(Arc::clone(&broadcast)));
    assert_eq!(broadcast.listeners(), 1);
    let sink = broadcast.sink();
    sink(&remote_applied(&[], &["P1"], &[]));
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let event = rt.block_on(events.next()).expect("an event");
    assert_eq!(event.practice_ids.iter().collect::<Vec<_>>(), ["P1"]);
    drop(events);
    broadcast.publish(&remote_applied(&[], &["P2"], &[]));
    assert_eq!(broadcast.listeners(), 0, "a dropped listener is forgotten");
}

// ------------------------------------------- home_update_check_test

/// A service whose manifest advertises 9.9.9 over a running 1.0.0, counting
/// its fetches.
fn newer_available(fetches: &Arc<AtomicUsize>) -> Arc<UpdateCheckService> {
    let fetches = Arc::clone(fetches);
    Arc::new(UpdateCheckService::with_fetcher(
        Box::new(move |_| {
            fetches.fetch_add(1, Ordering::SeqCst);
            Ok(r#"{"latest": "9.9.9", "url": "https://example.com/download", "notes": "New voiceprint views."}"#.to_string())
        }),
        "1.0.0",
    ))
}

fn home_with_updates(updates: Arc<UpdateCheckService>) -> Home {
    let mut services = services_over(FakePractices::with(Vec::new()));
    services.update_check = updates;
    Home::mount(services)
}

#[test]
fn the_auto_check_fires_once_5s_after_mount_and_raises_the_banner() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let mut home = home_with_updates(newer_available(&fetches));

    // Nothing before the deferred check fires: on mount it only waits.
    assert_eq!(Timer::UpdateCheck.delay(), Duration::from_secs(5));
    assert!(home.screen.timers().contains(&Timer::UpdateCheck));
    assert_eq!(fetches.load(Ordering::SeqCst), 0);
    assert!(!home.shows("Vocara 9.9.9 is available."));

    // At ~5 s the check runs and the banner appears.
    home.fire(Timer::UpdateCheck);
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert!(home.shows("Vocara 9.9.9 is available."));
    assert!(home.shows("Download"));
    assert!(!home.screen.timers().contains(&Timer::UpdateCheck));
    assert!(home.screen.snack().is_none());

    // Never twice: not from this page, nor from Home mounted again.
    home.send(Message::UpdateCheckDue);
    let services = home.services.clone();
    let mut again = Home::mount(services);
    again.fire(Timer::UpdateCheck);
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert!(again.shows("Vocara 9.9.9 is available."));
}

#[test]
fn an_up_to_date_auto_check_shows_no_banner_and_no_error() {
    let updates = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(|_| Ok(r#"{"latest": "1.0.0", "url": "https://example.com"}"#.to_string())),
        "1.0.0",
    ));
    let mut home = home_with_updates(updates.clone());
    home.fire(Timer::UpdateCheck);
    assert_eq!(updates.status(), UpdateStatus::UpToDate);
    assert!(!home.shows("Download"));
    // The silent policy: nothing error-shaped ever shows on an auto-check.
    assert!(home.screen.snack().is_none());
}

#[test]
fn a_failed_auto_check_stays_silent() {
    let updates = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(|_| Err(FetchError("offline".into()))),
        "1.0.0",
    ));
    let mut home = home_with_updates(updates.clone());
    home.fire(Timer::UpdateCheck);
    assert_eq!(updates.status(), UpdateStatus::Unknown);
    assert!(!home.shows("Download"));
    assert!(home.screen.snack().is_none());
}

#[test]
fn download_opens_the_update_dialog_over_the_window() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let mut home = home_with_updates(newer_available(&fetches));
    home.fire(Timer::UpdateCheck);
    home.click("Download");
    let Some(DialogRequest {
        dialog: Dialog::UpdateDownload(dialog),
        ..
    }) = &home.dialog
    else {
        panic!("the update dialog, through the app's dialog stack")
    };
    assert_eq!(dialog.info().version, "9.9.9");
    assert_eq!(dialog.info().notes, "New voiceprint views.");
    assert_eq!(dialog.info().url, "https://example.com/download");
    home.answer(DialogResult::Closed);
    assert!(home.navigations.is_empty());
}

#[test]
fn dismissing_the_banner_hides_it_for_the_rest_of_the_run() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let updates = newer_available(&fetches);
    let mut home = home_with_updates(updates.clone());
    home.fire(Timer::UpdateCheck);
    assert!(home.shows("Vocara 9.9.9 is available."));
    home.click(&glyph(Icon::X));
    assert!(!home.shows("Vocara 9.9.9 is available."));
    assert!(updates.notice_dismissed());
}
