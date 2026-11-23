//! The pages the router holds: one variant per screen (the routes of
//! `routing/router.dart`), plus go_router's error page for a location that
//! matches no route.

use iced::alignment::Horizontal;
use iced::widget::{center, column, text};
use iced::{Element, Subscription, Task};

use super::Message;
use crate::router::{Navigation, PageId, Route, RouteError};
use crate::services::Services;
use crate::ui::core::theme::{font, Studio};
use crate::ui::core::widgets::studio_button::studio_button;
use crate::ui::home::HomeScreen;
use crate::ui::settings::SettingsScreen;
use crate::ui::song_practice_detail::SongScreen;
use crate::ui::technique_library::detail::TechniqueDetailScreen;
use crate::ui::technique_library::TechniqueLibraryScreen;
use crate::ui::{Screen, Update};

/// One open page.
#[derive(Debug)]
pub enum Page {
    Home(HomeScreen),
    Song(SongScreen),
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
                (Page::Home(s), tagged(t, Message::Home))
            }
            Ok(Route::Song { id }) => {
                let (s, t) = SongScreen::new(id, services);
                (Page::Song(s), tagged(t, Message::Song))
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
                id: s.practice_id.clone(),
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

    /// Tells the page it left the stack.
    pub fn dispose(&mut self, services: &Services) {
        with_screen!(self, screen, _wrap => screen.dispose(services), ())
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

/// go_router's `MaterialErrorScreen`: "Page Not Found", the error, and a
/// Home button that goes to `/`.
fn not_found(error: &str) -> Element<'_, Message> {
    center(
        column![
            text("Page Not Found")
                .size(22)
                .font(font::BOLD)
                .color(Studio::TEXT_PRIMARY),
            text(error).size(14).color(Studio::TEXT_SECONDARY),
            studio_button("Home", Message::Navigate(Navigation::Go(Route::Home))),
        ]
        .spacing(16)
        .align_x(Horizontal::Center),
    )
    .into()
}
