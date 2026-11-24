//! Settings through the whole app — the router, the page stack and the
//! dialog stack: the settings parts of the integration journeys
//! (`home_settings_journey_test.dart`'s Home → Settings → back,
//! `settings_journey_test.dart`'s layout and reverb drag), and the studio
//! dialogs Settings opens over the window. (`deep_link_back_test.dart`'s
//! deep-linked Settings going back Home is in `app/tests.rs`.)

use std::collections::VecDeque;
use std::sync::Arc;

use iced::{mouse, Event, Point, Size};
use iced_test::Simulator;
use vocara_model::config::environment::AppEnvironment;

use super::*;
use crate::app::library::{Library, LibraryOpen};
use crate::app::{self, App, Boot, LibraryEvent, Page};
use crate::router::PageId;
use crate::services::ServicesFactory;
use crate::ui::home::testing::run_task;

const TALL: Size = Size::new(1400.0, 2600.0);

fn boot(start: Option<&str>, services: ServicesFactory) -> App {
    let mut boot = Boot::new(AppEnvironment::Dev);
    boot.start = start.map(str::to_string);
    boot.services = services;
    let (mut app, _open) = App::new(boot);
    let task = app.update(app::Message::Library(LibraryEvent::Finished(
        LibraryOpen::Opened(Arc::new(Library::in_memory())),
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
    Simulator::with_size(iced::Settings::default(), TALL, app.view())
}

fn feed(app: &mut App, messages: Vec<app::Message>) {
    for message in messages {
        let task = app.update(message);
        settle(app, task);
    }
}

fn click(app: &mut App, label: &str) {
    let messages: Vec<app::Message> = {
        let mut ui = ui(app);
        ui.click(label).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        ui.into_messages().collect()
    };
    feed(app, messages);
}

fn shows(app: &App, label: &str) -> bool {
    ui(app).find(label).is_ok()
}

fn glyph(icon: Icon) -> String {
    char::from(icon).to_string()
}

fn settings_page(app: &App) -> (PageId, &SettingsScreen) {
    match app.top() {
        Some((id, Page::Settings(settings))) => (id, settings),
        other => panic!("Settings on top, not {other:?}"),
    }
}

/// The reverb readout (the only `N%` text on the page).
fn reverb_percent(app: &App) -> String {
    let (_, settings) = settings_page(app);
    let mut ui = Simulator::with_size(iced::Settings::default(), TALL, settings.view());
    let target = ui.find(
        |candidate: iced_test::selector::Candidate<'_>| match candidate {
            iced_test::selector::Candidate::Text { content, .. }
                if content.ends_with('%')
                    && content[..content.len() - 1].parse::<u32>().is_ok() =>
            {
                Some(content.to_string())
            }
            _ => None,
        },
    );
    target.expect("the reverb readout")
}

// home_settings_journey_test: "Home → Settings → back round-trips through
// the settings screen".
#[test]
fn home_to_settings_and_back_round_trips_through_the_settings_screen() {
    let mut app = boot(None, Services::detached);
    let (home, _) = app.top().expect("running");
    click(&mut app, &glyph(Icon::Settings));
    assert_eq!(app.routes(), [Some(Route::Home), Some(Route::Settings)]);

    // The settings surface lays out its device + monitoring sections.
    // Device lists depend on the machine, so only the always-present rows.
    assert!(shows(&app, "Settings"));
    assert!(shows(&app, "AUDIO OUTPUT"));
    assert!(shows(&app, "AUDIO INPUT"));
    assert!(shows(&app, "MONITORING"));
    assert!(shows(&app, "System Default"));
    assert!(ui(&app).find(REVERB_SLIDER).is_ok(), "the reverb slider");
    assert!(!shows(&app, "New practice"), "Home is covered");

    // The back chevron returns to the same Home.
    click(&mut app, &glyph(Icon::ChevronLeft));
    assert_eq!(app.routes(), [Some(Route::Home)]);
    assert!(shows(&app, "New practice"));
    assert_eq!(app.top().map(|(id, _)| id), Some(home));
}

// settings_journey_test: deep-linked into Settings, it lays out its device
// + monitoring sections, and dragging the reverb slider changes its
// percentage readout.
#[test]
fn a_reverb_drag_changes_its_percentage_readout() {
    let mut app = boot(Some("/settings"), Services::detached);
    assert_eq!(app.routes(), [Some(Route::Settings)]);
    assert!(shows(&app, "Settings"));
    assert!(shows(&app, "MONITORING"));

    let before = reverb_percent(&app);
    let messages: Vec<app::Message> = {
        let mut ui = ui(&app);
        let slider = ui.find(REVERB_SLIDER).expect("the slider").bounds();
        let y = slider.center_y();
        // Press, then drag right 80: the slider seeks to the press, then
        // follows.
        let from = Point::new(slider.x + 20.0, y);
        ui.point_at(from);
        ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))]);
        let to = Point::new(from.x + 80.0, y);
        ui.point_at(to);
        ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position: to })]);
        ui.simulate([Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        ))]);
        ui.into_messages().collect()
    };
    assert!(messages.len() >= 2, "the press and the drag: {messages:?}");
    feed(&mut app, messages);
    let after = reverb_percent(&app);
    assert_ne!(after, before);
    let (_, settings) = settings_page(&app);
    assert!(settings.pending_choices().is_empty());
}

// The repair confirmation is a studio dialog over the whole window; its
// answer reaches the page, and the report comes back as its toast.
#[test]
fn the_repair_confirmation_covers_the_window_and_the_report_toasts() {
    let mut app = boot(Some("/settings"), Services::detached);
    click(&mut app, "Repair library");
    assert_eq!(app.dialogs(), 1);
    assert!(shows(&app, "Repair library?"));
    click(&mut app, "Repair");
    assert_eq!(app.dialogs(), 0);
    assert!(shows(&app, "Repair finished. Everything looks consistent."));
    let (_, settings) = settings_page(&app);
    assert!(!settings.repairing());
}

// Settings' toasts went through the app-wide `ScaffoldMessenger`: one up as
// the page left stayed up on Home for the rest of its 3.5 s.
#[test]
fn a_toast_up_as_settings_leaves_stays_up_on_home_until_its_time_is_up() {
    const REPORT: &str = "Repair finished. Everything looks consistent.";
    let mut app = boot(Some("/settings"), Services::detached);
    click(&mut app, "Repair library");
    click(&mut app, "Repair");
    assert_eq!(settings_page(&app).1.toast(), Some(REPORT));

    // The root of the stack (a deep link): Back goes Home.
    click(&mut app, &glyph(Icon::ChevronLeft));
    assert_eq!(app.routes(), [Some(crate::router::Route::Home)]);
    assert!(shows(&app, REPORT), "Home shows it");
    let timer = app.snack_timer().expect("its timer runs");
    let latest = Instant::now() + crate::ui::snack::TRANSITION + TOAST_DURATION;
    assert!(timer.deadline <= latest, "for the time it had left");

    let task = app.update(app::Message::SnackBarExpired(timer.seq));
    settle(&mut app, task);
    let left = Instant::now() + crate::ui::snack::TRANSITION;
    let task = app.update(app::Message::SnackBarFrame(left));
    settle(&mut app, task);
    assert!(
        !shows(&app, REPORT),
        "gone once its time is up and it has left"
    );
}

fn newer_release(library: Arc<Library>, environment: AppEnvironment) -> Services {
    let mut services = Services::detached(library, environment);
    services.update_check = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(|_| {
            Ok(r#"{"latest": "9.9.9", "url": "https://example.com/dl", "notes": "Better vibrato analysis."}"#.to_string())
        }),
        "1.0.0",
    ));
    services
}

// The manual check's download dialog goes through the app's dialog stack,
// as Home's banner's does; behind it the row shows the result.
#[test]
fn a_newer_release_found_from_settings_opens_its_dialog_over_the_window() {
    let mut app = boot(Some("/settings"), newer_release);
    assert!(shows(
        &app,
        "Compares this build against the latest release."
    ));
    click(&mut app, "Check for updates");
    assert_eq!(app.dialogs(), 1);
    assert!(shows(&app, "Vocara 9.9.9 is available"));
    assert!(shows(&app, "Better vibrato analysis."));
    click(&mut app, "Later");
    assert_eq!(app.dialogs(), 0);
    assert!(shows(
        &app,
        "Vocara 9.9.9 is available. Click for the download."
    ));
    assert_eq!(app.routes(), [Some(Route::Settings)]);
}
