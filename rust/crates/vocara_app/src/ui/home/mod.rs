//! Home — the collection of song practices (`ui/home`).
//!
//! PLACEHOLDER until the home screen is ported: its title, and the two ways
//! off it the Dart header offered (the technique library and settings), so
//! the app runs and routing works end to end.

pub mod widgets;

use iced::{Element, Task};

use super::core::widgets::studio_button::studio_button;
use super::{placeholder, Screen, Update};
use crate::router::Route;
use crate::services::Services;

/// The home page.
#[derive(Debug, Default)]
pub struct HomeScreen {
    /// How many pushed pages have come back to it (Dart reloaded its list
    /// and released its open guard each time).
    pub returns: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    OpenTechniques,
    OpenSettings,
}

impl HomeScreen {
    pub fn new(_services: &Services) -> (Self, Task<Message>) {
        (Self::default(), Task::none())
    }
}

impl Screen for HomeScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        match message {
            Message::OpenTechniques => Update::push(Route::Techniques),
            Message::OpenSettings => Update::push(Route::Settings),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        placeholder(
            "Vocara",
            Some("Your song practices.".into()),
            None,
            vec![
                studio_button("Technique library", Message::OpenTechniques).into(),
                studio_button("Settings", Message::OpenSettings).into(),
            ],
        )
    }

    fn returned(&mut self, _services: &Services) -> Update<Message> {
        self.returns += 1;
        Update::none()
    }
}
