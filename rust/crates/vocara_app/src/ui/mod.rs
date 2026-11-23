//! The user interface: the shared layer (`core`), the screens, and the
//! contract every screen implements so the app can host it.
//!
//! # How a screen plugs in
//!
//! A screen lives in `ui::<screen>` and is a type implementing [`Screen`]:
//!
//! - its own `Message` enum (what its widgets and tasks report);
//! - a constructor `new(<route params>, &Services) -> (Self, Task<Message>)`
//!   the app calls when its route is pushed or gone to (the Dart route
//!   builder that created the screen's view model);
//! - [`Screen::update`], which handles a message and returns an [`Update`]:
//!   the tasks to run (service calls on a blocking thread, see
//!   `util::blocking`), where to navigate, and a dialog to show;
//! - [`Screen::view`], [`Screen::subscription`] (keyboard, ticks: skip
//!   events a widget captured), [`Screen::returned`] (a page this one pushed
//!   was popped: Dart's `await context.push(...)` completing) and
//!   [`Screen::dispose`] (the page left the stack).
//!
//! The view model (`view_model.rs`) holds the Dart view model's state and
//! logic with no iced types; its async work comes back as `Effect` values the
//! screen's `update` turns into tasks, and their results come back as the
//! screen's messages. The app tags every message with the page it belongs
//! to, so a late result for a page that is gone never arrives, and one for a
//! page under another still does — Dart's `_disposed` / generation guards
//! keep their meaning.

pub mod core;
pub mod home;
pub mod settings;
pub mod song_practice_detail;
pub mod technique_library;

use iced::alignment::Vertical;
use iced::widget::{column, container, row, text, Space};
use iced::{Element, Length, Subscription, Task};

use crate::router::{Navigation, Route};
use crate::services::Services;
use crate::ui::core::icons::Icon;
use crate::ui::core::theme::{font, Studio};
use crate::ui::core::widgets::confirm_dialog::{Dialog, DialogResult};
use crate::ui::core::widgets::studio_button::studio_button;

/// A screen the app can host (see the module docs).
pub trait Screen {
    type Message: std::fmt::Debug + Clone + Send + 'static;

    /// Handles one of the screen's messages.
    fn update(&mut self, message: Self::Message, services: &Services) -> Update<Self::Message>;

    /// The page's content (the app puts the window drag bar above it).
    fn view(&self) -> Element<'_, Self::Message>;

    /// What the screen listens to; `on_top` is false while another page
    /// covers it (its timers may run on; its keys should not).
    fn subscription(&self, _on_top: bool) -> Subscription<Self::Message> {
        Subscription::none()
    }

    /// A page this one pushed was popped (go_router completed the push).
    fn returned(&mut self, _services: &Services) -> Update<Self::Message> {
        Update::none()
    }

    /// The page left the stack (popped, or dropped by a go).
    fn dispose(&mut self, _services: &Services) {}
}

/// A dialog a screen asks the app to show, and how its result comes back.
pub struct DialogRequest<M> {
    pub dialog: Dialog,
    pub on_result: Box<dyn FnOnce(DialogResult) -> M + Send>,
}

/// What a screen's [`Screen::update`] hands back to the app.
pub struct Update<M> {
    /// Work to run; its messages come back to the same page.
    pub task: Task<M>,
    /// Where to go, if anywhere.
    pub navigation: Option<Navigation>,
    /// A studio dialog to show over the window (`showConfirmDialog` /
    /// `showStudioPrompt`); its result comes back as `on_result(result)`.
    pub dialog: Option<DialogRequest<M>>,
}

impl<M> Update<M> {
    /// Nothing to do.
    pub fn none() -> Self {
        Self {
            task: Task::none(),
            navigation: None,
            dialog: None,
        }
    }

    /// Run `task`.
    pub fn task(task: Task<M>) -> Self {
        Self {
            task,
            ..Self::none()
        }
    }

    /// Navigate.
    pub fn navigate(navigation: Navigation) -> Self {
        Self {
            navigation: Some(navigation),
            ..Self::none()
        }
    }

    /// `context.push(route)`.
    pub fn push(route: Route) -> Self {
        Self::navigate(Navigation::Push(route))
    }

    /// `popOrGo(context, fallback)`.
    pub fn pop_or_go(fallback: Route) -> Self {
        Self::navigate(Navigation::PopOrGo(fallback))
    }

    /// Show `dialog`; `on_result` turns how it closed into a message.
    pub fn dialog(
        dialog: impl Into<Dialog>,
        on_result: impl FnOnce(DialogResult) -> M + Send + 'static,
    ) -> Self {
        Self {
            dialog: Some(DialogRequest {
                dialog: dialog.into(),
                on_result: Box::new(on_result),
            }),
            ..Self::none()
        }
    }

    /// Also run `task`.
    pub fn and_task(mut self, task: Task<M>) -> Self
    where
        M: Send + 'static,
    {
        self.task = Task::batch([self.task, task]);
        self
    }

    /// Also navigate.
    pub fn and_navigate(mut self, navigation: Navigation) -> Self {
        self.navigation = Some(navigation);
        self
    }
}

impl<M> From<Task<M>> for Update<M> {
    fn from(task: Task<M>) -> Self {
        Update::task(task)
    }
}

/// The page every not-yet-ported screen shows: its title, what it would
/// show, a back button when it has one, and its actions. PLACEHOLDER for the
/// screen ports.
pub(crate) fn placeholder<'a, M: Clone + 'a>(
    title: &str,
    detail: Option<String>,
    back: Option<M>,
    actions: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let mut header = row![].spacing(12).align_y(Vertical::Center);
    if let Some(back) = back {
        header = header.push(studio_button("Back", back).icon(Icon::ChevronLeft));
    }
    header = header.push(
        text(title.to_string())
            .size(22)
            .font(font::BOLD)
            .color(Studio::TEXT_PRIMARY),
    );
    let mut content = column![header].spacing(16);
    if let Some(detail) = detail {
        content = content.push(text(detail).size(13).color(Studio::TEXT_SECONDARY));
    }
    if !actions.is_empty() {
        content = content.push(row(actions).spacing(10));
    }
    content = content.push(Space::new().height(Length::Fill));
    container(content)
        .padding(24)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
