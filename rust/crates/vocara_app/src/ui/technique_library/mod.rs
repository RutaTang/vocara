//! The technique library — a flat, tag-organized pool of vocal techniques
//! (`ui/technique_library`) — and, in [`detail`], one technique.
//!
//! PLACEHOLDER until the technique screens are ported: their titles and
//! back affordances (the library goes Home, a technique goes to the
//! library, each popping when a page lies beneath).

pub mod detail;
pub mod widgets;

use iced::{Element, Task};

use super::{placeholder, Screen, Update};
use crate::router::Route;
use crate::services::Services;

#[derive(Debug, Default)]
pub struct TechniqueLibraryScreen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Back,
}

impl TechniqueLibraryScreen {
    pub fn new(_services: &Services) -> (Self, Task<Message>) {
        (Self, Task::none())
    }
}

impl Screen for TechniqueLibraryScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        match message {
            Message::Back => Update::pop_or_go(Route::Home),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        placeholder("Technique library", None, Some(Message::Back), Vec::new())
    }
}
