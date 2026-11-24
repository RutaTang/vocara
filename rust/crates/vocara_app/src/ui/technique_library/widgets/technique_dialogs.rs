//! Port of `ui/technique_library/widgets/technique_dialogs.dart`: the
//! name-first create dialog for a new technique (`showNewTechniqueDialog`).
//! It returns the trimmed name, or nothing if cancelled; Create waits for a
//! name. Tags and the rest of the body are filled in the detail editor
//! afterward.
//!
//! The dialog is a modal like the studio dialogs (`confirm_dialog`): the same
//! barrier, Escape and barrier tap cancel, and it speaks their
//! [`Message`]s. The app's dialog stack owns it (`Dialog::NewTechnique`):
//! it draws [`view`](NewTechniqueDialog::view) over the window, runs
//! [`focus`](NewTechniqueDialog::focus) when it opens (`autofocus`), and
//! feeds it the messages until [`update`](NewTechniqueDialog::update) says it
//! closed. Unlike `showStudioPrompt`, a blank name keeps it open: closed, it
//! created nothing and said nothing. (Dart owned the field's controller in
//! the dialog's state so it was disposed only after the exit transition; the
//! state here is plain data and goes with the dialog.)

use iced::widget::{column, container, row, text, Space};
use iced::{widget, Element, Length, Task};
use vocara_model::util::dart_string;

use super::studio_field::studio_field;
use crate::ui::core::theme::{self, font, Studio};
use crate::ui::core::widgets::confirm_dialog::{modal, DialogResult, Fade};
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};

pub use crate::ui::core::widgets::confirm_dialog::Message;

/// An open new-technique dialog: the name as typed.
#[derive(Debug, Clone)]
pub struct NewTechniqueDialog {
    name: String,
    input: widget::Id,
}

impl Default for NewTechniqueDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl NewTechniqueDialog {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            input: widget::Id::unique(),
        }
    }

    /// The name as typed.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name field's id.
    pub fn input_id(&self) -> widget::Id {
        self.input.clone()
    }

    /// The task that puts the keyboard in the name field when the dialog
    /// opens (`autofocus: true`).
    pub fn focus<T: Send + 'static>(&self) -> Task<T> {
        iced::widget::operation::focus(self.input.clone())
    }

    /// The trimmed name, when there is one to create.
    fn trimmed(&self) -> Option<&str> {
        let t = dart_string::trim(&self.name);
        (!t.is_empty()).then_some(t)
    }

    /// Handles `message`; `Some` when the dialog closes:
    /// `DialogResult::Text(Some(name))` to create, `Text(None)` cancelled.
    pub fn update(&mut self, message: Message) -> Option<DialogResult> {
        match message {
            Message::Input(name) => {
                self.name = name;
                None
            }
            Message::Cancel => Some(DialogResult::Text(None)),
            // A blank name keeps it open: closed, it created nothing and said
            // nothing.
            Message::Confirm => self
                .trimmed()
                .map(|name| DialogResult::Text(Some(name.to_string()))),
        }
    }

    /// The dialog over the whole window, all in: the barrier and the
    /// centred card (440 wide, the studio dialog card).
    pub fn view(&self) -> Element<'_, Message> {
        self.view_at(Fade::SHOWN)
    }

    /// The dialog at `fade` (`confirm_dialog`'s module docs).
    pub fn view_at(&self, fade: Fade) -> Element<'_, Message> {
        let card = container(
            column![
                text("New technique")
                    .size(16)
                    .font(font::BOLD)
                    .color(fade.color(Studio::TEXT_PRIMARY)),
                Space::new().height(16),
                studio_field("Name", &self.name)
                    .hint("e.g. Twang, Mixed voice, Curbing")
                    .id(self.input.clone())
                    .on_input(Message::Input)
                    // Return creates, as in the app's other name dialogs.
                    .on_submit(Message::Confirm)
                    .opacity(fade.card),
                Space::new().height(20),
                row![
                    Space::new().width(Length::Fill),
                    studio_button("Cancel", Message::Cancel).opacity(fade.card),
                    Space::new().width(8),
                    studio_button("Create", Message::Confirm)
                        .variant(StudioButtonVariant::Accent)
                        .enabled(self.trimmed().is_some())
                        .opacity(fade.card),
                ],
            ]
            .width(Length::Fill),
        )
        .width(440)
        .padding(22)
        .style(fade.style(theme::dialog_card));
        modal(container(card).padding(24).into(), fade)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::{self, key::Named};

    /// Drives the dialog's view once and feeds what it reported back.
    fn drive(
        dialog: &mut NewTechniqueDialog,
        steps: impl FnOnce(&mut iced_test::Simulator<'_, Message>, widget::Id),
    ) -> Vec<Option<DialogResult>> {
        let id = dialog.input_id();
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(dialog.view());
            steps(&mut ui, id);
            ui.into_messages().collect()
        };
        messages.into_iter().map(|m| dialog.update(m)).collect()
    }

    #[test]
    fn return_in_the_name_field_creates_the_technique() {
        let mut dialog = NewTechniqueDialog::new();
        // `enterText`: the field's whole text at once.
        assert_eq!(dialog.update(Message::Input("  Twang ".into())), None);
        let results = drive(&mut dialog, |ui, id| {
            ui.click(id).expect("the name field");
            let _ = ui.tap_key(keyboard::Key::Named(Named::Enter));
        });
        // Return closes the dialog with the trimmed name.
        assert_eq!(
            results,
            vec![Some(DialogResult::Text(Some("Twang".into())))]
        );
    }

    #[test]
    fn typing_goes_through_the_name_field() {
        let mut dialog = NewTechniqueDialog::new();
        dialog.update(Message::Input("Twan".into()));
        drive(&mut dialog, |ui, id| {
            ui.click(id).expect("the name field");
            let _ = ui.tap_key(keyboard::Key::Named(Named::End));
            let _ = ui.typewrite("g");
        });
        assert_eq!(dialog.name(), "Twang");
    }

    #[test]
    fn a_blank_name_creates_nothing_and_keeps_the_dialog_open() {
        let mut dialog = NewTechniqueDialog::new();
        dialog.update(Message::Input("   ".into()));
        let results = drive(&mut dialog, |ui, id| {
            ui.click(id).expect("the name field");
            let _ = ui.tap_key(keyboard::Key::Named(Named::Enter));
            // Create is disabled: the tap does nothing.
            ui.click("Create").expect("the create button");
        });
        assert_eq!(results, vec![None], "Return reported, and stayed open");
        // Still open, still titled.
        let mut ui = iced_test::simulator(dialog.view());
        assert!(ui.find("New technique").is_ok());
        assert!(ui.find("NAME").is_ok());
    }

    #[test]
    fn cancel_escape_and_the_barrier_close_it_with_nothing() {
        let mut dialog = NewTechniqueDialog::new();
        dialog.update(Message::Input("Twang".into()));
        let results = drive(&mut dialog, |ui, _| {
            ui.click("Cancel").expect("the cancel button");
            let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
            ui.point_at(iced::Point::new(5.0, 5.0));
            let _ = ui.simulate(iced_test::simulator::click());
        });
        assert_eq!(results, vec![Some(DialogResult::Text(None)); 3]);
    }

    #[test]
    fn the_create_button_takes_a_named_technique() {
        let mut dialog = NewTechniqueDialog::new();
        dialog.update(Message::Input("Mixed voice".into()));
        let results = drive(&mut dialog, |ui, _| {
            ui.click("Create").expect("the create button");
        });
        assert_eq!(
            results,
            vec![Some(DialogResult::Text(Some("Mixed voice".into())))]
        );
    }
}
