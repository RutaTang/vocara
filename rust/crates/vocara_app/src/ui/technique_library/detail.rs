//! One technique (`ui/technique_library/widgets/technique_detail_screen.dart`).
//!
//! PLACEHOLDER until the technique screen is ported: its title, the
//! technique it shows, and its back affordance (`popOrGo(context,
//! Routes.techniqueLibrary)`).

use iced::{Element, Task};

use crate::router::Route;
use crate::services::Services;
use crate::ui::{placeholder, Screen, Update};

#[derive(Debug)]
pub struct TechniqueDetailScreen {
    pub technique_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Back,
}

impl TechniqueDetailScreen {
    pub fn new(technique_id: String, _services: &Services) -> (Self, Task<Message>) {
        (Self { technique_id }, Task::none())
    }
}

impl Screen for TechniqueDetailScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        match message {
            Message::Back => Update::pop_or_go(Route::Techniques),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        placeholder(
            "Technique",
            Some(format!("Technique {}", self.technique_id)),
            Some(Message::Back),
            Vec::new(),
        )
    }
}
