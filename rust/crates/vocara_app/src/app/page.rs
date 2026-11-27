//! The pages the router holds: one variant per screen (the routes of
//! `routing/router.dart`), plus go_router's error page for a location that
//! matches no route.

use crate::ui::core::text;
use iced::alignment::Horizontal;
use iced::widget::{center, column, container, Space};
use iced::{Element, Length, Pixels, Subscription, Task};

use super::Message;
use crate::router::{Navigation, PageId, Route, RouteError};
use crate::services::Services;
use crate::ui::core::theme::{self, font, type_scale, Studio};
use crate::ui::core::widgets::controls::secondary_button;
use crate::ui::core::widgets::toolbar;
use crate::ui::home::HomeScreen;
use crate::ui::settings::SettingsScreen;
use crate::ui::song_practice_detail::SongPracticeDetailScreen;
use crate::ui::technique_library::detail::TechniqueDetailScreen;
use crate::ui::technique_library::TechniqueLibraryScreen;
use crate::ui::{Screen, SnackBar, Update};

/// One open page.
#[derive(Debug)]
pub enum Page {
    /// Boxed: the home screen holds its list and its commands' outcomes.
    Home(Box<HomeScreen>),
    /// Boxed: the song page holds its view model and its panels' state.
    Song(Box<SongPracticeDetailScreen>),
    Settings(SettingsScreen),
    Techniques(TechniqueLibraryScreen),
    Technique(TechniqueDetailScreen),
    /// go_router's default error page (a `VOCARA_START` deep link to a
    /// location that matches no route): the error and a way home.
    NotFound(String),
}

/// A page's first task, once the router has given the page its id.
pub type Opening = Box<dyn FnOnce(PageId) -> Task<Message>>;

fn tagged<M: Send + 'static>(task: Task<M>, wrap: fn(PageId, M) -> Message) -> Opening {
    Box::new(move |id| task.map(move |m| wrap(id, m)))
}

/// Runs `$body` with the page's screen bound to `$screen` and its message
/// wrapper (`Message::Home`, ...) bound to `$wrap`; `$not_found` for the
/// error page.
macro_rules! with_screen {
    ($page:expr, $screen:ident, $wrap:ident => $body:expr, $not_found:expr) => {
        match $page {
            Page::Home($screen) => {
                let $wrap = Message::Home as fn(PageId, _) -> Message;
                $body
            }
            Page::Song($screen) => {
                let $wrap = Message::Song as fn(PageId, _) -> Message;
                $body
            }
            Page::Settings($screen) => {
                let $wrap = Message::Settings as fn(PageId, _) -> Message;
                $body
            }
            Page::Techniques($screen) => {
                let $wrap = Message::Techniques as fn(PageId, _) -> Message;
                $body
            }
            Page::Technique($screen) => {
                let $wrap = Message::Technique as fn(PageId, _) -> Message;
                $body
            }
            Page::NotFound(_) => $not_found,
        }
    };
}

pub(crate) use with_screen;

impl Page {
    /// The page for a location (go_router's route builders: each route
    /// creates its screen over the shared services).
    pub fn open(route: Result<Route, RouteError>, services: &Services) -> (Page, Opening) {
        match route {
            Ok(Route::Home) => {
                let (s, t) = HomeScreen::new(services);
                (Page::Home(Box::new(s)), tagged(t, Message::Home))
            }
            Ok(Route::Song { id }) => {
                let (s, t) = SongPracticeDetailScreen::new(id, services);
                (Page::Song(Box::new(s)), tagged(t, Message::Song))
            }
            Ok(Route::Settings) => {
                let (s, t) = SettingsScreen::new(services);
                (Page::Settings(s), tagged(t, Message::Settings))
            }
            Ok(Route::Techniques) => {
                let (s, t) = TechniqueLibraryScreen::new(services);
                (Page::Techniques(s), tagged(t, Message::Techniques))
            }
            Ok(Route::Technique { id }) => {
                let (s, t) = TechniqueDetailScreen::new(id, services);
                (Page::Technique(s), tagged(t, Message::Technique))
            }
            Err(e) => (Page::NotFound(e.to_string()), Box::new(|_| Task::none())),
        }
    }

    /// The route the page shows (`None` for the error page).
    pub fn route(&self) -> Option<Route> {
        Some(match self {
            Page::Home(_) => Route::Home,
            Page::Song(s) => Route::Song {
                id: s.practice_id().to_string(),
            },
            Page::Settings(_) => Route::Settings,
            Page::Techniques(_) => Route::Techniques,
            Page::Technique(s) => Route::Technique {
                id: s.technique_id.clone(),
            },
            Page::NotFound(_) => return None,
        })
    }

    /// The page's content, its messages tagged with its id.
    pub fn view(&self, id: PageId) -> Element<'_, Message> {
        if let Page::NotFound(error) = self {
            return not_found(error);
        }
        with_screen!(
            self,
            screen,
            wrap => screen.view().map(move |m| wrap(id, m)),
            unreachable!("handled above")
        )
    }

    /// What the page shows over the whole window, its messages tagged with
    /// its id (see [`Screen::overlay`]).
    pub fn overlay(&self, id: PageId) -> Option<Element<'_, Message>> {
        with_screen!(
            self,
            screen,
            wrap => screen.overlay().map(move |e| e.map(move |m| wrap(id, m))),
            None
        )
    }

    /// The page's subscription, its messages tagged with its id.
    pub fn subscription(&self, id: PageId, on_top: bool) -> Subscription<Message> {
        with_screen!(
            self,
            screen,
            wrap => screen
                .subscription(on_top)
                .with((id, wrap))
                .map(|((id, wrap), m)| wrap(id, m)),
            Subscription::none()
        )
    }

    /// Tells the page it left the stack; the work its teardown started
    /// (its messages tagged with its id, for the page lingering on).
    pub fn dispose(&mut self, id: PageId, services: &Services) -> Task<Message> {
        with_screen!(self, screen, _wrap => screen.dispose(services), ());
        match self {
            Page::Song(s) => s.take_teardown().map(move |m| Message::Song(id, m)),
            _ => Task::none(),
        }
    }

    /// Whether the page, gone from the stack, is kept until the work it
    /// started lands (the song page's view model outlives it while a take,
    /// a save or a guarded edit is in flight, as Dart's futures did).
    pub fn lingers(&self) -> bool {
        matches!(self, Page::Song(s) if s.lingers())
    }

    /// The page's snack bars, handed over to the app (see
    /// [`Screen::take_snack_bars`]).
    pub fn take_snack_bars(&mut self) -> Vec<SnackBar> {
        with_screen!(self, screen, _wrap => screen.take_snack_bars(), Vec::new())
    }
}

/// The screen updates a page produced, type-erased to app messages.
pub struct PageUpdate {
    pub task: Task<Message>,
    pub navigation: Option<Navigation>,
    pub dialog: Option<super::PendingDialog>,
}

/// Tags a screen's [`Update`] with its page.
pub fn erase<M: Send + 'static>(
    update: Update<M>,
    id: PageId,
    wrap: fn(PageId, M) -> Message,
) -> PageUpdate {
    PageUpdate {
        task: update.task.map(move |m| wrap(id, m)),
        navigation: update.navigation,
        dialog: update.dialog.map(|d| super::PendingDialog {
            page: id,
            dialog: d.dialog,
            on_result: Box::new(move |r| wrap(id, (d.on_result)(r))),
        }),
    }
}

/// go_router's `MaterialErrorScreen`: "Page not found", the error, and a
/// Home button that goes to `/`; drawn as the app's other pages are
/// (`DESIGN.md`): under the unified toolbar, the 15 px heading, the error
/// in 12 px `TEXT_SECONDARY`, a secondary button.
fn not_found(error: &str) -> Element<'_, Message> {
    let content = column![
        text("Page not found")
            .size(type_scale::HEADING)
            .line_height(Pixels(20.0))
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_PRIMARY),
        Space::new().height(6),
        text(error)
            .size(type_scale::BODY)
            .line_height(Pixels(18.0))
            .color(Studio::TEXT_SECONDARY),
        Space::new().height(18),
        secondary_button("Home", Some(Message::Navigate(Navigation::Go(Route::Home)))),
    ]
    .align_x(Horizontal::Center);
    container(column![
        // The app's name where Home's sits, 12 past the toolbar's inset.
        toolbar::page_toolbar(container(toolbar::title("Vocara")).padding(iced::Padding {
            left: 12.0,
            ..iced::Padding::ZERO
        })),
        center(content),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::page)
    .into()
}
