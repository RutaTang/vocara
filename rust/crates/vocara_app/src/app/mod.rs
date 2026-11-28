//! The iced application (port of `app.dart` and `main.dart`).
//!
//! [`App`] boots by opening the library off the UI thread
//! ([`library::OpenLibrary`]); while another Vocara is still starting up on
//! it, it shows the waiting window, and when the open is refused or fails it
//! shows the matching fatal window instead of the app (`FatalOpenApp`). Once
//! the library is open it builds the [`Services`] and the [`Router`] at the
//! start location (`/`, or the `VOCARA_START` deep link) and hosts the
//! screens: every page with its own unified toolbar at the top, which is
//! also the window's title bar (`widgets::toolbar`: the app watches its
//! empty area for the window's drag and zoom, and tells it the environment
//! badge), a studio dialog over the whole window when a screen asks for
//! one.
//!
//! A snack bar outlives the page that showed it, as under Dart's app-wide
//! `ScaffoldMessenger`: a page draws its own while it is on top, and the app
//! takes them over when it stops being on top and draws them over the page
//! on top until their time runs out (the `messenger` module).
//!
//! A navigation slides the pages as the `MaterialPage`s go_router built did
//! on macOS (`CupertinoPageTransition`, 500 ms; [`RouteTransition`]). The
//! stack changes at once — a popped page is disposed at once, as before —
//! and only the drawing follows: the page going is drawn as it goes (from
//! the app, or from its lingering), taking no input. The slide moves the
//! page's layout with its drawing, so a tap during it lands where the page
//! is drawn, as Flutter hit-tested the moving page. Each page's toolbar
//! slides with it.
//!
//! A studio dialog comes in and goes as Flutter's `DialogRoute` did (150
//! ms, `confirm_dialog::DialogMotion`): its barrier and card fade in, and a
//! closed one stays drawn where it was, taking no input, while it fades
//! out.
//!
//! The window is closed through the quit guard: a close request, or a quit
//! macOS sends through `terminate:` (Cmd+Q, the app-menu Quit, a Dock quit,
//! logout; [`vocara_services::quit_request`]), kills the separation
//! sidecars, finalizes the recordings ([`quit::QuitHooks`]), then exits —
//! once, however many requests arrive meanwhile. An installed update quits
//! the same way: once the self-update has staged it and started the helper
//! that swaps the bundle when this process is gone
//! ([`vocara_services::self_update`], `Phase::Relaunching`), the app runs
//! the same guard — the recordings saved, nothing cut off — and exits, and
//! the helper relaunches the new version. The app listens to the
//! self-update for as long as it runs, which also redraws the banner, the
//! dialog and Settings as the download moves.

pub mod library;
mod messenger;
mod page;
pub mod quit;

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::futures::SinkExt;
use iced::widget::{container, stack, Space};
use iced::{window, Element, Length, Size, Subscription, Task, Theme};
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_services::app_menu::PreferencesRequests;
use vocara_services::quit_request::QuitRequests;
use vocara_services::self_update::{ListenerId, SelfUpdate};

use self::library::{LibraryOpen, OpenLibrary};
use self::messenger::Messenger;
use self::page::{erase, with_screen, PageUpdate};
use self::quit::QuitHooks;
use crate::router::{self, Change, Navigation, PageId, Route, Router};
use crate::services::{Services, ServicesFactory};
use crate::ui::core::motion::{self, Controller};
use crate::ui::core::theme::{self, font};
use crate::ui::core::widgets::confirm_dialog::{
    self, Dialog, DialogMotion, DialogResult, DialogState, DialogStep,
};
use crate::ui::core::widgets::fatal_open_screen::{self, FatalOpenScreen};
use crate::ui::core::widgets::focus_traversal::focus_scope;
use crate::ui::core::widgets::slide::{cupertino, inert, slide};
use crate::ui::core::widgets::toolbar;
use crate::ui::home::widgets::update_notice::open_url_task;
use crate::ui::snack::SnackEvent;
use crate::ui::{home, settings, song_practice_detail, technique_library};
use crate::ui::{Screen, SnackBar};
pub use vocara_model::config::environment::AppEnvironment;

pub use self::page::Page;
pub use crate::ui::snack::SnackTimer;

/// The deep link the app opens at instead of Home (Dart's `START`
/// dart-define, `--dart-define=START=/song/<id>`).
pub const START_VAR: &str = "VOCARA_START";

/// Where to write a PNG of the app ~4 s after its first frame, then exit
/// (Dart's `SCREENSHOT` dart-define, a dev-only preview hook).
pub const SCREENSHOT_VAR: &str = "VOCARA_SCREENSHOT";

/// How long after the first frame the screenshot is taken.
const SCREENSHOT_DELAY: Duration = Duration::from_secs(4);

/// Settles where this environment's data lives, the first thing `main`
/// does, before anything resolves the application support directory: a
/// library a sandboxed release (0.3.0 and earlier) kept in its App Sandbox
/// container moves to `~/Library/Application Support/<bundle id>`, or, when
/// it cannot, every store (and the diagnostics log) is rooted at the
/// container for this launch, so the user never opens an empty library
/// beside theirs (`vocara_services::sandbox_migration`). What it did is
/// logged once [`attach_diagnostics`] binds the log.
pub fn settle_storage() {
    vocara_services::sandbox_migration::settle(AppEnvironment::current());
}

/// `DiagnosticsLog.attachGlobalHooks()`, right after [`settle_storage`]:
/// binds the diagnostics log to the application support directory (the
/// library folder, where the export and the release's launch check read
/// it), so every entry from here on — and the ones buffered before — is
/// appended to the rotating on-disk log, and logs every uncaught panic
/// before handing it to the default hook. Without the bind the log only
/// ever buffers in memory.
pub fn attach_diagnostics() {
    DiagnosticsLog::instance().attach_global_hooks(None);
}

/// How the app starts.
#[derive(Clone)]
pub struct Boot {
    pub environment: AppEnvironment,
    /// The start location; `None` (or empty) opens Home.
    pub start: Option<String>,
    /// The screenshot hook's output file.
    pub screenshot: Option<PathBuf>,
    /// Opens the library (`Persistence.open`).
    pub open_library: OpenLibrary,
    /// What runs before the app quits.
    pub quit_hooks: Arc<dyn QuitHooks>,
    /// Builds the services once the library is open.
    pub services: ServicesFactory,
    /// How long the exit may take after the quit guard before the process
    /// is ended ([`exit_watchdog`]); `None` (the tests' boot) arms nothing,
    /// so a test that drives the quit is not cut short.
    pub exit_watchdog: Option<Duration>,
}

impl Boot {
    /// A boot in `environment` at Home, with the stand-in library open, no
    /// quit hooks, and [`Services::detached`] (what the app's tests boot).
    pub fn new(environment: AppEnvironment) -> Self {
        Self {
            environment,
            start: None,
            screenshot: None,
            open_library: library::open_library_stub,
            quit_hooks: Arc::new(quit::NoQuitHooks),
            services: Services::detached,
            exit_watchdog: None,
        }
    }

    /// The boot `main` runs: the environment from `VOCARA_FLAVOR`, the deep
    /// link from `VOCARA_START`, the screenshot hook from
    /// `VOCARA_SCREENSHOT`, with the given library open and quit hooks.
    pub fn from_env(open_library: OpenLibrary, quit_hooks: Arc<dyn QuitHooks>) -> Self {
        let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
        Self {
            environment: AppEnvironment::current(),
            start: var(START_VAR),
            screenshot: var(SCREENSHOT_VAR).map(PathBuf::from),
            open_library,
            quit_hooks,
            services: Services::new,
            exit_watchdog: Some(EXIT_WATCHDOG),
        }
    }
}

/// The library open's progress.
#[derive(Debug, Clone)]
pub enum LibraryEvent {
    /// Another Vocara is still starting up on the library; this one waits.
    Waiting,
    /// The open finished.
    Finished(LibraryOpen),
}

/// Everything the app reacts to.
#[derive(Debug, Clone)]
pub enum Message {
    // Screens: each message is tagged with the page it belongs to.
    Home(PageId, home::Message),
    Song(PageId, song_practice_detail::Message),
    Settings(PageId, settings::Message),
    Techniques(PageId, technique_library::Message),
    Technique(PageId, technique_library::detail::Message),
    /// Navigation not owned by a screen (the error page's Home button).
    Navigate(Navigation),
    /// The open dialog's buttons, barrier and keys.
    Dialog(confirm_dialog::Message),
    /// The time of the snack bar the app shows (snack bar `n`) is up.
    SnackBarExpired(u64),
    /// A frame while one of the app's snack bars enters or leaves.
    SnackBarFrame(Instant),
    /// The app's snack bar `n` was swiped away.
    SnackBarSwiped(u64),
    /// A frame while a page slides or a dialog comes in or goes.
    Frame(Instant),
    /// Boot: the library open.
    Library(LibraryEvent),
    /// The fatal window's actions.
    Fatal(fatal_open_screen::Message),
    /// The window opened (its id).
    WindowOpened(window::Id),
    /// The window's scale factor, asked for when it opened.
    ScaleFactor(f32),
    /// The user asked to close the window.
    CloseRequested(window::Id),
    /// macOS asked the app to quit (Cmd+Q, the app-menu Quit, a Dock quit,
    /// logout: `NSApplication.terminate:`, see
    /// [`vocara_services::quit_request`]).
    QuitRequested,
    /// The app menu's Preferences… (⌘,): show Settings
    /// ([`vocara_services::app_menu::PreferencesRequests`]).
    PreferencesRequested,
    /// The quit guard finished its work: exit.
    QuitGuardDone,
    /// The self-update moved (its download's progress, its install, its
    /// failure): redraw, and quit once it waits for the app to go.
    UpdaterChanged,
    /// The toolbar's empty area: start a window drag / toggle maximize.
    DragWindow,
    ToggleMaximize,
    /// The screenshot hook: time to capture, and the capture.
    ScreenshotDue,
    Screenshot(window::Screenshot),
    /// Nothing to do (a finished side effect).
    Done,
}

/// A dialog a screen asked for, before the app opens it.
pub struct PendingDialog {
    pub page: PageId,
    pub dialog: Dialog,
    pub on_result: Box<dyn FnOnce(DialogResult) -> Message + Send>,
}

struct OpenDialog {
    page: PageId,
    state: DialogState,
    on_result: Box<dyn FnOnce(DialogResult) -> Message + Send>,
    /// Its route animation, coming in.
    motion: DialogMotion,
}

/// A dialog that closed, drawn as it goes.
struct ClosingDialog {
    /// How many dialogs were under it.
    depth: usize,
    state: DialogState,
    motion: DialogMotion,
}

struct Running {
    services: Services,
    router: Router<Page>,
    dialogs: Vec<OpenDialog>,
    /// Pages gone from the stack whose work is still landing (see
    /// [`Page::lingers`]): they get their messages, and nothing else, until
    /// it has.
    lingering: Vec<(PageId, Page)>,
    /// The snack bars pages handed over, shown over the page on top.
    messenger: Messenger,
    /// The dialogs that closed, fading out (each dialog's route animation
    /// running back), each over the dialogs that were under it.
    closing: Vec<ClosingDialog>,
    /// The page transition running, if any.
    route: Option<RouteTransition>,
    /// The last frame's time, what the drawing reads.
    now: Instant,
}

impl Running {
    /// Whether something the app draws itself moves at `now` (a page
    /// transition, a dialog coming in or going).
    fn is_animating(&self, now: Instant) -> bool {
        self.closing.iter().any(|c| c.motion.is_animating(now))
            || self.dialogs.iter().any(|d| d.motion.is_animating(now))
            || self
                .route
                .as_ref()
                .is_some_and(|r| r.controller.is_animating(now))
    }

    /// The pages as drawn at the last frame (see [`RouteTransition`]), each
    /// in a slot of its own for as long as it is on the stack: slot `i + 1`
    /// for the page at place `i`, the one on top last; slot 0 holds the
    /// pages a go replaced, under the page coming in. iced matches a stack's
    /// layers by position, so a page moved to another slot is built anew:
    /// the page a push covered moved down a slot as the new one slid in,
    /// and Home's list jumped back to its top under the practice it opened.
    /// Only the page on top and the one a transition moves are drawn, the
    /// slots of the others left empty, and each is a focus scope
    /// (`focus_traversal`: Tab moves among its fields).
    fn pages_view<'a>(&'a self) -> Vec<Element<'a, Message>> {
        let (top, page) = self.router.top();
        let empty = || Element::from(Space::new().width(Length::Fill).height(Length::Fill));
        // The top page's slot is the last of the stack's.
        let depth = self.router.len();
        let mut slots: Vec<Element<'a, Message>> = (0..=depth).map(|_| empty()).collect();
        let Some(route) = &self.route else {
            slots[depth] = slide(focus_scope(page.view(top)), 0.0).into();
            return slots;
        };
        let (primary, secondary, shadow) = route.progress(self.now);
        let gone_view = |id: PageId, held: &'a Option<Box<Page>>| -> Option<Element<'a, Message>> {
            let page = held.as_deref().or_else(|| {
                self.lingering
                    .iter()
                    .find(|(p, _)| *p == id)
                    .map(|(_, page)| page)
            })?;
            Some(page.view(id))
        };
        match &route.kind {
            RouteKind::Push { below } => {
                // Where it was while it was on top, as it was built there.
                if let Some((place, (id, covered))) = self
                    .router
                    .pages()
                    .enumerate()
                    .find(|(_, (id, _))| id == below)
                {
                    slots[place + 1] = slide(focus_scope(covered.view(id)), secondary)
                        .inert()
                        .into();
                }
                slots[depth] = slide(focus_scope(page.view(top)), primary)
                    .shadow(shadow)
                    .into();
            }
            RouteKind::Pop { gone, held } => {
                slots[depth] = slide(focus_scope(page.view(top)), secondary).into();
                // Where it was: over the page it reveals.
                if let Some(view) = gone_view(*gone, held) {
                    slots.push(
                        slide(focus_scope(view), primary)
                            .shadow(shadow)
                            .inert()
                            .into(),
                    );
                }
            }
            RouteKind::Go { gone } => {
                slots[0] = gone
                    .iter()
                    .filter_map(|(id, held)| gone_view(*id, held))
                    .fold(stack![], |layers, view| {
                        layers.push(slide(view, 0.0).inert())
                    })
                    .into();
                slots[depth] = slide(focus_scope(page.view(top)), primary)
                    .shadow(shadow)
                    .into();
            }
        }
        slots
    }
}

/// How long a page takes to come in or go.
pub use crate::ui::core::widgets::slide::PAGE_TRANSITION as ROUTE_TRANSITION;

/// A page transition: Flutter's `CupertinoPageTransition`, which the
/// `MaterialPage`s go_router built used on macOS. The page coming in (a push
/// or a go) slides in from the right edge over 500 ms
/// (`Curves.fastEaseInToSlowEaseOut`), casting a faint shadow to its left,
/// while the page it covers slides a third of the way out to the left
/// (`Curves.linearToEaseOut`); a pop runs the same back (the flipped curve,
/// and `Curves.easeInToLinear` for the page revealed). A go's replaced pages
/// stay still under the page coming in, as routes being removed did.
struct RouteTransition {
    kind: RouteKind,
    /// The moving page's route animation: 1 when it is all in.
    controller: Controller,
}

enum RouteKind {
    /// The page on top was pushed over `below`.
    Push { below: PageId },
    /// Page `gone` was popped off the one on top now; the page itself is
    /// `held` here, or lingers.
    Pop {
        gone: PageId,
        held: Option<Box<Page>>,
    },
    /// The page on top replaced these (each held here, or lingering).
    Go {
        gone: Vec<(PageId, Option<Box<Page>>)>,
    },
}

impl RouteTransition {
    /// The moving page's offset (in its widths, rightward), the covered
    /// page's, and the edge shadow's strength, at `now`.
    fn progress(&self, now: Instant) -> (f32, f32, f32) {
        cupertino(&self.controller, now)
    }
}

enum State {
    /// Opening the library; nothing to show yet.
    Opening,
    /// Still opening, but waiting for another Vocara: the waiting window.
    Waiting(FatalOpenScreen),
    /// The library cannot open: the fatal window.
    Fatal(FatalOpenScreen),
    Running(Box<Running>),
}

/// The application.
pub struct App {
    boot: Boot,
    window: Option<window::Id>,
    /// The quit guard's re-entrancy latch: with close requests intercepted,
    /// every close click asks again, and a second run must not race a second
    /// exit or re-run the finalizers while the first save is in flight.
    closing: bool,
    state: State,
}

impl App {
    /// Boots the app: starts opening the library.
    pub fn new(boot: Boot) -> (App, Task<Message>) {
        let open = boot.open_library;
        let task = Task::run(
            iced::stream::channel(
                4,
                async move |mut output: iced::futures::channel::mpsc::Sender<LibraryEvent>| {
                    let outcome = open_on_own_thread(open, output.clone()).await;
                    let _ = output.send(LibraryEvent::Finished(outcome)).await;
                },
            ),
            Message::Library,
        );
        // Every page's toolbar ends with the environment's badge.
        toolbar::set_environment_badge(&boot.environment.badge());
        let app = App {
            boot,
            window: None,
            closing: false,
            state: State::Opening,
        };
        (app, task)
    }

    /// The window title.
    pub fn title(&self) -> String {
        self.boot.environment.window_title()
    }

    pub fn theme(&self) -> Theme {
        theme::app_theme()
    }

    /// The route of every open page, bottom first (empty until running).
    pub fn routes(&self) -> Vec<Option<Route>> {
        match &self.state {
            State::Running(r) => r.router.pages().map(|(_, p)| p.route()).collect(),
            _ => Vec::new(),
        }
    }

    /// The page on top and its id, once running.
    pub fn top(&self) -> Option<(PageId, &Page)> {
        match &self.state {
            State::Running(r) => Some(r.router.top()),
            _ => None,
        }
    }

    /// The fatal (or waiting) window shown instead of the app, if any.
    pub fn fatal(&self) -> Option<&FatalOpenScreen> {
        match &self.state {
            State::Waiting(s) | State::Fatal(s) => Some(s),
            _ => None,
        }
    }

    /// How many dialogs are open.
    pub fn dialogs(&self) -> usize {
        match &self.state {
            State::Running(r) => r.dialogs.len(),
            _ => 0,
        }
    }

    /// The snack bar the app shows over the page on top (one a page handed
    /// over), if any.
    pub fn snack_bar(&self) -> Option<&SnackBar> {
        match &self.state {
            State::Running(r) => r.messenger.showing(),
            _ => None,
        }
    }

    /// The timer of the snack bar the app shows, while it shows one.
    pub fn snack_timer(&self) -> Option<SnackTimer> {
        match &self.state {
            State::Running(r) => r.messenger.timer(),
            _ => None,
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.handle(message);
        // Whatever the message changed, the snack bars of a page no longer
        // on top, or one toasting over the app's, are the app's now.
        if let State::Running(running) = &mut self.state {
            running.messenger.gather(&mut running.router);
        }
        task
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Library(event) => self.library(event),
            Message::Fatal(message) => self.fatal_action(message),
            Message::WindowOpened(id) => {
                // The Runner's MainMenu.xib, in place of the app menu winit
                // set when the app finished launching (before any window
                // opened). Once; on the main thread, where iced runs update
                // on macOS.
                vocara_services::app_menu::install();
                // The empty unified toolbar that centres the traffic lights
                // in the pages' 52 px toolbars (macOS; nothing elsewhere).
                vocara_services::window_toolbar::install();
                if self.window.is_some() {
                    return Task::none();
                }
                self.window = Some(id);
                // The text fields' caret sits by device pixels
                // (`devicePixelRatio`); they follow a change of it
                // themselves.
                let scale_factor = window::scale_factor(id).map(Message::ScaleFactor);
                // Dev-only preview hook: a PNG of the rendered app a while
                // after its first frame, then exit. Avoids needing OS
                // screen-recording access.
                if self.boot.screenshot.is_some() {
                    return Task::batch([
                        scale_factor,
                        Task::perform(tokio::time::sleep(SCREENSHOT_DELAY), |_| {
                            Message::ScreenshotDue
                        }),
                    ]);
                }
                scale_factor
            }
            Message::ScaleFactor(scale_factor) => {
                crate::ui::core::scale_factor::set_scale_factor(scale_factor);
                Task::none()
            }
            Message::CloseRequested(id) => {
                self.window.get_or_insert(id);
                self.quit()
            }
            Message::QuitRequested => self.quit(),
            Message::QuitGuardDone => {
                if let Some(after) = self.boot.exit_watchdog {
                    exit_watchdog(after);
                }
                iced::exit()
            }
            Message::UpdaterChanged => match &self.state {
                // The helper swaps the bundle once this process is gone:
                // quit through the guard, as Cmd+Q does, so the recordings
                // are saved first. (A quit already under way is not run
                // twice.)
                State::Running(running) if running.services.self_update.wants_quit() => {
                    DiagnosticsLog::instance().log("update", "quitting for the relaunch");
                    self.quit()
                }
                _ => Task::none(),
            },
            Message::DragWindow => self.window.map_or_else(Task::none, window::drag),
            Message::ToggleMaximize => self.window.map_or_else(Task::none, window::toggle_maximize),
            Message::ScreenshotDue => match self.window {
                Some(id) => window::screenshot(id).map(Message::Screenshot),
                None => Task::none(),
            },
            Message::Screenshot(shot) => {
                let Some(path) = self.boot.screenshot.clone() else {
                    return Task::none();
                };
                // Exits whether or not the write worked, as Dart did.
                Task::perform(
                    crate::util::blocking(move || write_png(&shot, &path)),
                    |_| Message::QuitGuardDone,
                )
            }
            Message::Done => Task::none(),
            Message::Navigate(navigation) => match &mut self.state {
                State::Running(running) => navigate(running, navigation),
                _ => Task::none(),
            },
            Message::PreferencesRequested => match &mut self.state {
                State::Running(running) if opens_settings(running) => {
                    navigate(running, Navigation::Push(Route::Settings))
                }
                _ => Task::none(),
            },
            Message::Dialog(message) => self.dialog(message),
            Message::SnackBarExpired(seq) => {
                if let State::Running(running) = &mut self.state {
                    running.messenger.event(SnackEvent::Expired(seq));
                }
                Task::none()
            }
            Message::SnackBarFrame(at) => {
                if let State::Running(running) = &mut self.state {
                    running.messenger.event(SnackEvent::Frame(at));
                }
                Task::none()
            }
            Message::SnackBarSwiped(seq) => {
                if let State::Running(running) = &mut self.state {
                    running.messenger.event(SnackEvent::Swiped(seq));
                }
                Task::none()
            }
            Message::Frame(at) => {
                if let State::Running(running) = &mut self.state {
                    running.now = running.now.max(at);
                    let now = running.now;
                    running.closing.retain(|c| c.motion.is_animating(now));
                    if running
                        .route
                        .as_ref()
                        .is_some_and(|r| !r.controller.is_animating(now))
                    {
                        running.route = None;
                    }
                }
                Task::none()
            }
            Message::Home(id, m) => self.page(id, |page, services| match page {
                Page::Home(s) => Some(erase(s.update(m, services), id, Message::Home)),
                _ => None,
            }),
            Message::Song(id, m) => self.page(id, |page, services| match page {
                Page::Song(s) => Some(erase(s.update(m, services), id, Message::Song)),
                _ => None,
            }),
            Message::Settings(id, m) => self.page(id, |page, services| match page {
                Page::Settings(s) => Some(erase(s.update(m, services), id, Message::Settings)),
                _ => None,
            }),
            Message::Techniques(id, m) => self.page(id, |page, services| match page {
                Page::Techniques(s) => Some(erase(s.update(m, services), id, Message::Techniques)),
                _ => None,
            }),
            Message::Technique(id, m) => self.page(id, |page, services| match page {
                Page::Technique(s) => Some(erase(s.update(m, services), id, Message::Technique)),
                _ => None,
            }),
        }
    }

    /// Delivers a screen message to page `id` — dropped when the page has
    /// left the stack (a late result for a screen that is gone), unless the
    /// page lingers for work still landing: then it gets the message, and
    /// what it asks besides its tasks (a navigation, a dialog) is dropped.
    fn page(
        &mut self,
        id: PageId,
        update: impl FnOnce(&mut Page, &Services) -> Option<PageUpdate>,
    ) -> Task<Message> {
        let State::Running(running) = &mut self.state else {
            return Task::none();
        };
        let Running {
            services,
            router,
            lingering,
            ..
        } = running.as_mut();
        if let Some(page) = router.get_mut(id) {
            return match update(page, services) {
                Some(u) => apply(running, u),
                None => Task::none(),
            };
        }
        let Some((_, page)) = lingering.iter_mut().find(|(p, _)| *p == id) else {
            return Task::none();
        };
        let task = update(page, services).map_or_else(Task::none, |u| u.task);
        lingering.retain(|(_, p)| p.lingers());
        task
    }

    /// How many pages gone from the stack still linger.
    pub fn lingering(&self) -> usize {
        match &self.state {
            State::Running(r) => r.lingering.len(),
            _ => 0,
        }
    }

    fn library(&mut self, event: LibraryEvent) -> Task<Message> {
        let log = DiagnosticsLog::instance();
        match event {
            LibraryEvent::Waiting => {
                if matches!(self.state, State::Opening) {
                    self.state = State::Waiting(FatalOpenScreen::waiting_for_library());
                }
                Task::none()
            }
            LibraryEvent::Finished(outcome) => {
                if !matches!(self.state, State::Opening | State::Waiting(_)) {
                    return Task::none();
                }
                match outcome {
                    LibraryOpen::Opened(library) => {
                        let services = (self.boot.services)(library, self.boot.environment);
                        // Dev-only deep link: VOCARA_START=/song/<id>.
                        let start = self.boot.start.as_deref().unwrap_or(router::HOME);
                        let (page, opening) = Page::open(Route::parse(start), &services);
                        let router = Router::new(page);
                        let task = opening(router.top().0);
                        self.state = State::Running(Box::new(Running {
                            services,
                            router,
                            dialogs: Vec::new(),
                            lingering: Vec::new(),
                            messenger: Messenger::default(),
                            closing: Vec::new(),
                            route: None,
                            now: Instant::now(),
                        }));
                        task
                    }
                    LibraryOpen::NewerLibrary(e) => {
                        log.log("boot", &format!("open refused: {e}"));
                        self.state = State::Fatal(FatalOpenScreen::newer_library());
                        Task::none()
                    }
                    LibraryOpen::LibraryInUse(e) => {
                        log.log("boot", &format!("open refused: {e}"));
                        self.state = State::Fatal(FatalOpenScreen::library_in_use());
                        Task::none()
                    }
                    LibraryOpen::Failed { detail, log: full } => {
                        log.log("boot", &format!("open failed: {full}"));
                        self.state = State::Fatal(FatalOpenScreen::open_failed(detail));
                        Task::none()
                    }
                }
            }
        }
    }

    fn fatal_action(&mut self, message: fatal_open_screen::Message) -> Task<Message> {
        let (State::Fatal(screen) | State::Waiting(screen)) = &self.state else {
            return Task::none();
        };
        match screen.update(message) {
            Some(fatal_open_screen::Action::Copy(text)) => iced::clipboard::write(text),
            Some(fatal_open_screen::Action::RevealLibrary) => {
                let env = self.boot.environment;
                Task::perform(
                    crate::util::blocking(move || library::reveal_library_folder(env)),
                    |_| Message::Done,
                )
            }
            Some(fatal_open_screen::Action::Quit) => self.quit(),
            None => Task::none(),
        }
    }

    /// The quit guard: kill the separation sidecars (synchronously), then
    /// finalize the recordings off the UI thread, then exit — whatever
    /// happened (a panic in a hook still quits: a guard left set made every
    /// later close click do nothing). Once only.
    fn quit(&mut self) -> Task<Message> {
        if self.closing {
            return Task::none();
        }
        self.closing = true;
        let hooks = Arc::clone(&self.boot.quit_hooks);
        let _ = crate::util::run_caught::<_, ()>(|| {
            hooks.kill_separation_sidecars();
            Ok(())
        });
        Task::perform(
            crate::util::blocking(move || hooks.finalize_recordings()),
            |_| Message::QuitGuardDone,
        )
    }

    fn dialog(&mut self, message: confirm_dialog::Message) -> Task<Message> {
        let State::Running(running) = &mut self.state else {
            return Task::none();
        };
        let Some(open) = running.dialogs.last_mut() else {
            return Task::none();
        };
        match open.state.step(message) {
            DialogStep::Closed(result) => {
                let OpenDialog {
                    state,
                    on_result,
                    mut motion,
                    ..
                } = running.dialogs.pop().expect("checked above");
                // It goes from wherever it got.
                motion.close(Instant::now());
                running.closing.push(ClosingDialog {
                    depth: running.dialogs.len(),
                    state,
                    motion,
                });
                let message = on_result(result);
                self.update(message)
            }
            // The update dialog stays open: its URL is the fallback.
            DialogStep::OpenUrl(url) => open_url_task(url),
            DialogStep::Stay => Task::none(),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        match &self.state {
            State::Opening => container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::page)
                .into(),
            // Movable and zoomable by its top band, as a page is by its
            // toolbar ("Another Vocara is starting…" could not be moved).
            State::Waiting(screen) | State::Fatal(screen) => toolbar::drag_region(
                screen.view().map(Message::Fatal),
                Message::DragWindow,
                Message::ToggleMaximize,
            ),
            State::Running(running) => {
                // The snack bar the app shows sits over the page, where the
                // page draws its own. The page keeps its place in the stack
                // whether one shows or not, or whether it slides, so its
                // widgets keep their state; and the snack bar keeps its own
                // however many slots the pages take.
                let pages = stack(running.pages_view())
                    .width(Length::Fill)
                    .height(Length::Fill);
                let mut content = stack![pages].width(Length::Fill).height(Length::Fill);
                if let Some(snack_bar) = running.messenger.view() {
                    content = content.push(snack_bar);
                }
                // Each page draws its toolbar at its top; the app turns a
                // drag or a double click on the toolbar's empty area into
                // the window's. Dialogs render above, their barrier covering
                // the whole window.
                let chrome = container(content).width(Length::Fill).height(Length::Fill);
                let mut layers = stack![container(chrome)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::page)];
                // What the page on top shows over the whole window (a route
                // on the root navigator: a video's fullscreen), over the
                // toolbar and the snack bars, under the dialogs. Its slot
                // stays when empty, so the dialogs keep theirs. It takes
                // every press over it; one in the title-bar band that
                // nothing in it took is the window's (a video's picture
                // takes its own).
                let (top, page) = running.router.top();
                layers = layers.push(page.overlay(top).map_or_else(
                    || Space::new().width(Length::Fill).height(Length::Fill).into(),
                    toolbar::window_backdrop,
                ));
                // A closed dialog fades out where it was (over the dialogs
                // under it, under any opened since), taking no input (as its
                // route went, Flutter's barrier ignored the pointer).
                let now = running.now;
                let fading = |depth: usize| {
                    running
                        .closing
                        .iter()
                        .filter(move |c| c.depth == depth)
                        .map(move |c| {
                            inert(c.state.view_at(c.motion.fade(now)).map(Message::Dialog))
                        })
                };
                for (depth, open) in running.dialogs.iter().enumerate() {
                    layers = layers.extend(fading(depth));
                    let fade = open.motion.fade(now);
                    layers = layers.push(open.state.view_at(fade).map(Message::Dialog));
                }
                layers = layers.extend(fading(running.dialogs.len()));
                // Around the dialogs too: a press on the band that a
                // dialog's barrier or a full-window view covers is the
                // window's as well (`toolbar::window_backdrop`).
                toolbar::drag_region(layers, Message::DragWindow, Message::ToggleMaximize)
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            window::close_requests().map(Message::CloseRequested),
            Subscription::run(quit_requests).map(|()| Message::QuitRequested),
            Subscription::run(preferences_requests).map(|()| Message::PreferencesRequested),
            window::open_events().map(Message::WindowOpened),
        ];
        if let State::Running(running) = &self.state {
            let top = running.router.top().0;
            // A dialog holds the keyboard: the page beneath it is not on top.
            let covered = !running.dialogs.is_empty();
            for (id, page) in running.router.pages() {
                subscriptions.push(page.subscription(id, id == top && !covered));
            }
            subscriptions.push(running.messenger.subscription());
            // Until a frame at or past the end is handled (see
            // `SnackQueue::subscription`): a closing page is let go, a
            // dialog's fade ends, only at such a frame.
            let animating =
                running.is_animating(Instant::now()) || running.is_animating(running.now);
            subscriptions.push(motion::frames(animating).map(Message::Frame));
            subscriptions.push(
                Subscription::run_with(
                    UpdaterFeed(Arc::clone(&running.services.self_update)),
                    updater_changes,
                )
                .map(|()| Message::UpdaterChanged),
            );
        }
        Subscription::batch(subscriptions)
    }
}

/// The self-update as a subscription's identity (the service itself, by
/// address).
struct UpdaterFeed(Arc<SelfUpdate>);

impl Hash for UpdaterFeed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

/// Removes its listener when the stream holding it is dropped.
struct UpdaterListening {
    updater: Arc<SelfUpdate>,
    id: ListenerId,
}

impl Drop for UpdaterListening {
    fn drop(&mut self) {
        self.updater.remove_listener(self.id);
    }
}

/// One item per change of the self-update, for as long as the app runs.
fn updater_changes(feed: &UpdaterFeed) -> impl iced::futures::Stream<Item = ()> {
    use iced::futures::StreamExt;
    let (sender, receiver) = iced::futures::channel::mpsc::unbounded();
    let id = feed.0.add_listener(Box::new(move || {
        let _ = sender.unbounded_send(());
    }));
    let listening = UpdaterListening {
        updater: Arc::clone(&feed.0),
        id,
    };
    receiver.map(move |()| {
        let _ = &listening;
    })
}

/// The quits macOS routes through `terminate:` (Cmd+Q, the app-menu Quit, a
/// Dock quit, logout), which `main` hands to [`QuitRequests::global`] instead
/// of letting AppKit end the process before the quit guard ran.
fn quit_requests() -> impl iced::futures::Stream<Item = ()> {
    iced::futures::stream::unfold(QuitRequests::global().subscribe(), |mut requests| async {
        requests.recv().await.map(|()| ((), requests))
    })
}

/// The app menu's Preferences… (⌘,), which the menu's app target hands to
/// [`PreferencesRequests::global`], as a quit comes.
fn preferences_requests() -> impl iced::futures::Stream<Item = ()> {
    iced::futures::stream::unfold(
        PreferencesRequests::global().subscribe(),
        |mut requests| async { requests.recv().await.map(|()| ((), requests)) },
    )
}

/// Whether Preferences… opens Settings now, as Home's gear does: not while
/// a dialog is up (nothing beneath one takes a click), nor over a page that
/// holds the window (a video's fullscreen, the song page while a take is
/// recorded or saved), nor a second time over Settings.
fn opens_settings(running: &Running) -> bool {
    if !running.dialogs.is_empty() {
        return false;
    }
    let (id, top) = running.router.top();
    !matches!(top, Page::Settings(_)) && !top.holds_window(id)
}

/// Applies what a page's update asked for.
fn apply(running: &mut Running, update: PageUpdate) -> Task<Message> {
    let mut tasks = vec![update.task];
    if let Some(pending) = update.dialog {
        let state = DialogState::new(pending.dialog);
        tasks.push(state.focus());
        running.dialogs.push(OpenDialog {
            page: pending.page,
            state,
            on_result: pending.on_result,
            motion: DialogMotion::opening(Instant::now()),
        });
    }
    if let Some(navigation) = update.navigation {
        tasks.push(navigate(running, navigation));
    }
    Task::batch(tasks)
}

/// Carries out a navigation on the page stack.
fn navigate(running: &mut Running, navigation: Navigation) -> Task<Message> {
    let services = running.services.clone();
    let mut opening = None;
    let open = |route: Route, opening: &mut Option<page::Opening>| {
        let (page, task) = Page::open(Ok(route), &services);
        *opening = Some(task);
        page
    };
    let now = Instant::now();
    let change = match navigation {
        Navigation::Push(route) => running.router.push(open(route, &mut opening)),
        Navigation::Go(route) => running.router.go(open(route, &mut opening)),
        Navigation::Pop => running.router.pop(),
        Navigation::PopOrGo(fallback) => {
            router::pop_or_go(&mut running.router, || open(fallback, &mut opening))
        }
    };
    match change {
        Change::Pushed(id) => {
            running.route = covered(&running.router, id).map(|below| RouteTransition {
                kind: RouteKind::Push { below },
                controller: forward(now),
            });
            opening.map_or_else(Task::none, |o| o(id))
        }
        Change::Replaced { page, dropped } => {
            let mut tasks = Vec::new();
            let mut gone = Vec::new();
            for (id, p) in dropped {
                let (task, held) = retire(running, id, p);
                tasks.push(task);
                gone.push((id, held));
            }
            running.route = Some(RouteTransition {
                kind: RouteKind::Go { gone },
                controller: forward(now),
            });
            tasks.push(opening.map_or_else(Task::none, |o| o(page)));
            Task::batch(tasks)
        }
        Change::Popped {
            popped: (gone, p),
            revealed,
        } => {
            let (teardown, held) = retire(running, gone, p);
            // A page popped while it was still coming in goes back from
            // where it got (its route animation reversed).
            let mut controller = match running.route.take() {
                Some(RouteTransition {
                    kind: RouteKind::Push { .. },
                    controller,
                }) if controller.is_animating(now) => controller,
                _ => Controller::completed(ROUTE_TRANSITION, ROUTE_TRANSITION),
            };
            controller.reverse(now);
            running.route = Some(RouteTransition {
                kind: RouteKind::Pop { gone, held },
                controller,
            });
            // The push the revealed page made has completed.
            let Running {
                services, router, ..
            } = running;
            let update = router.get_mut(revealed).and_then(|page| {
                with_screen!(
                    page,
                    screen,
                    wrap => Some(erase(screen.returned(services), revealed, wrap)),
                    None
                )
            });
            let returned = update.map_or_else(Task::none, |u| apply(running, u));
            Task::batch([teardown, returned])
        }
        Change::Nothing => {
            // go_router threw "There is nothing to pop".
            DiagnosticsLog::instance().log("router", "pop with nothing beneath ignored");
            Task::none()
        }
    }
}

/// A page left the stack: its dialogs go, the snack bars it shows stay up
/// (the app's now), it is disposed, and it lingers while the work it started
/// is still landing. The work its teardown started, to run, and the page
/// unless it lingers (its transition draws it as it goes).
fn retire(
    running: &mut Running,
    gone: PageId,
    mut page: Page,
) -> (Task<Message>, Option<Box<Page>>) {
    running.dialogs.retain(|d| d.page != gone);
    running
        .messenger
        .adopt(page.take_snack_bars(), Instant::now());
    let teardown = page.dispose(gone, &running.services);
    if page.lingers() {
        running.lingering.push((gone, page));
        (teardown, None)
    } else {
        (teardown, Some(Box::new(page)))
    }
}

/// The page right under page `id` on the stack, if any.
fn covered(router: &Router<Page>, id: PageId) -> Option<PageId> {
    router
        .pages()
        .map(|(p, _)| p)
        .take_while(|p| *p != id)
        .last()
}

/// A route animation running forward from `now`, as a page comes in.
fn forward(now: Instant) -> Controller {
    let mut controller = Controller::new(ROUTE_TRANSITION, ROUTE_TRANSITION);
    controller.forward(now);
    controller
}

/// How long the exit may take once the quit guard has saved everything
/// (see [`exit_watchdog`]).
const EXIT_WATCHDOG: Duration = Duration::from_secs(2);

/// Ends the process [after] the quit guard finished, if it has not ended by
/// then. `iced::exit` closes the window and drops the runtime, and the
/// runtime's teardown waits for every blocking task still running: a queue
/// of analysis jobs waiting on the analysis gate kept a quit app in the Dock
/// for tens of seconds, and kept a self-update's helper (which waits for
/// this process to end) from swapping and relaunching. The quit guard has
/// saved the recordings by then (Dart's quit ended the process at this
/// point too); the log is flushed before going.
fn exit_watchdog(after: Duration) {
    let spawned = std::thread::Builder::new()
        .name("exit-watchdog".into())
        .spawn(move || {
            std::thread::sleep(after);
            DiagnosticsLog::instance().log("quit", "the exit took too long; ending the process");
            DiagnosticsLog::instance().flush();
            std::process::exit(0);
        });
    if let Err(e) = spawned {
        DiagnosticsLog::instance().log("quit", &format!("no exit watchdog: {e}"));
    }
}

/// Runs the library open (`Persistence.open`) on a thread of its own and
/// waits for its outcome; `waiting` gets [`LibraryEvent::Waiting`] when the
/// open starts waiting on another Vocara's boot. A panic in the open comes
/// back as a failure.
///
/// Not on the runtime's blocking pool: that wait lasts as long as the other
/// boot does, and the runtime's teardown at exit waits for every blocking
/// task, so the waiting window's Quit would hang until the other Vocara
/// finished starting up. Dart's quit ended the process with the open still
/// pending; a plain thread ends with the process the same way.
async fn open_on_own_thread(
    open: OpenLibrary,
    mut waiting: iced::futures::channel::mpsc::Sender<LibraryEvent>,
) -> LibraryOpen {
    let (done, outcome) = iced::futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("library-open".into())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                open(&mut || {
                    let _ = waiting.try_send(LibraryEvent::Waiting);
                })
            }));
            let _ = done.send(outcome.map_err(|p| crate::util::panic_message(p.as_ref())));
        });
    if let Err(e) = spawned {
        return LibraryOpen::Failed {
            detail: e.to_string(),
            log: format!("{e:?}"),
        };
    }
    match outcome.await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(panic)) => LibraryOpen::Failed {
            detail: panic.clone(),
            log: panic,
        },
        // The thread always answers; this is the channel dropping unsent.
        Err(gone) => LibraryOpen::Failed {
            detail: gone.to_string(),
            log: format!("{gone:?}"),
        },
    }
}

/// Writes a window screenshot as a PNG.
fn write_png(shot: &window::Screenshot, path: &std::path::Path) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        shot.size.width,
        shot.size.height,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&shot.rgba)
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

/// The window: 1440 × 900 (at least 960 × 600), centred, closed through the
/// quit guard; on macOS the title bar is hidden and transparent over a
/// full-size content view, and once the window is open it takes an empty
/// unified toolbar (`vocara_services::window_toolbar`), which centres the
/// traffic lights in the 52 px toolbar every page draws.
pub fn window_settings() -> window::Settings {
    window::Settings {
        size: Size::new(1440.0, 900.0),
        min_size: Some(Size::new(960.0, 600.0)),
        position: window::Position::Centered,
        // Intercept the OS close so an in-flight recording is finalized
        // (saved) before the process is torn down.
        exit_on_close_request: false,
        #[cfg(target_os = "macos")]
        platform_specific: window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        ..window::Settings::default()
    }
}

/// The app-wide iced settings: Inter as the default face, Material's 14 px
/// body text as the default size, antialiased geometry. The bundled Inter
/// is installed first, over any copy the machine has ([`font::install`]).
pub fn settings() -> iced::Settings {
    font::install();
    iced::Settings {
        default_font: font::REGULAR,
        default_text_size: theme::DEFAULT_TEXT_SIZE.into(),
        antialiasing: true,
        ..iced::Settings::default()
    }
}

/// Runs the app.
pub fn run(boot: Boot) -> iced::Result {
    let mut application = iced::application(move || App::new(boot.clone()), App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window_settings())
        .settings(settings());
    for face in font::FACES {
        application = application.font(face);
    }
    application.font(crate::ui::core::icons::FONT_BYTES).run()
}

#[cfg(test)]
impl App {
    /// The services the pages open over, once running.
    pub(crate) fn services(&self) -> &Services {
        match &self.state {
            State::Running(r) => &r.services,
            _ => panic!("the app is not running"),
        }
    }

    /// Pushes `page` as a push navigation would (a test's page, over its
    /// fakes); `opening` makes its first task once it has its id.
    pub(crate) fn push_page(
        &mut self,
        page: Page,
        opening: impl FnOnce(PageId) -> Task<Message>,
    ) -> Task<Message> {
        let State::Running(running) = &mut self.state else {
            panic!("the app is not running");
        };
        let Change::Pushed(id) = running.router.push(page) else {
            unreachable!("a push pushes");
        };
        running.messenger.gather(&mut running.router);
        opening(id)
    }
}

#[cfg(test)]
mod tests;
