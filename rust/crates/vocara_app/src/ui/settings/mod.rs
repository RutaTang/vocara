//! Global app settings (`ui/settings`).
//!
//! PLACEHOLDER until the settings screen is ported: its title and its back
//! affordance, which pops, or goes Home when it is the root of the stack
//! (`popOrGo(context, Routes.home)`).

pub mod licenses;

use iced::{Element, Task};

use super::{placeholder, Screen, Update};
use crate::router::Route;
use crate::services::Services;

#[derive(Debug, Default)]
pub struct SettingsScreen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Back,
}

impl SettingsScreen {
    pub fn new(_services: &Services) -> (Self, Task<Message>) {
        (Self, Task::none())
    }
}

impl Screen for SettingsScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        match message {
            Message::Back => Update::pop_or_go(Route::Home),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        placeholder("Settings", None, Some(Message::Back), Vec::new())
    }
}
