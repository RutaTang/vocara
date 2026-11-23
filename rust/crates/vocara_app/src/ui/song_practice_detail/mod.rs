//! A song practice (`ui/song_practice_detail`).
//!
//! PLACEHOLDER until the song practice screen is ported: its title, the
//! practice it shows, and its back affordance — pop when Home pushed it (Home
//! awaits that push to reload its list and release its open guard), go Home
//! when it is the root of the stack (a deep link).

use iced::{Element, Task};

use super::{placeholder, Screen, Update};
use crate::router::Route;
use crate::services::Services;

#[derive(Debug)]
pub struct SongScreen {
    pub practice_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Back,
}

impl SongScreen {
    pub fn new(practice_id: String, _services: &Services) -> (Self, Task<Message>) {
        (Self { practice_id }, Task::none())
    }
}

impl Screen for SongScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        match message {
            Message::Back => Update::pop_or_go(Route::Home),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        placeholder(
            "Song practice",
            Some(format!("Practice {}", self.practice_id)),
            Some(Message::Back),
            Vec::new(),
        )
    }
}
