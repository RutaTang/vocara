//! Port of `ui/home/widgets/new_practice_dialog.dart`: the modal to name a
//! new song practice. It returns the trimmed title, or nothing if the user
//! cancelled; Create waits for a title. Dark studio styling (matches the rest of the
//! app's dialogs): it is a studio prompt, which the screen shows with
//! `Update::dialog(new_practice_dialog(), ...)` and reads back with
//! `DialogResult::text`.

use crate::ui::core::widgets::confirm_dialog::StudioPrompt;

/// `showNewPracticeDialog(context)`.
pub fn new_practice_dialog() -> StudioPrompt {
    StudioPrompt::new("New song practice")
        .label("Title")
        .hint("e.g. 海阔天空")
        .confirm_label("Create")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::core::widgets::confirm_dialog::{
        DialogResult, DialogState, DialogStep, Message,
    };

    #[test]
    fn the_new_practice_prompt_asks_for_a_title() {
        let mut state = DialogState::new(new_practice_dialog());
        state.update(Message::Input("Song".into()));
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(state.view());
            assert!(ui.find("New song practice").is_ok());
            assert!(ui.find("Title").is_ok());
            assert!(ui.find("Cancel").is_ok());
            ui.click("Create").expect("the create button");
            ui.into_messages().collect()
        };
        assert_eq!(messages, vec![Message::Confirm]);
        assert_eq!(new_practice_dialog().hint, "e.g. 海阔天空");
    }

    #[test]
    fn it_returns_the_trimmed_title_and_waits_for_one() {
        let mut state = DialogState::new(new_practice_dialog());
        state.update(Message::Input("  Hai Kuo Tian Kong ".into()));
        assert_eq!(
            state.update(Message::Confirm),
            Some(DialogResult::Text(Some("Hai Kuo Tian Kong".into())))
        );
        // Blank: Create is disabled and Return keeps it open.
        let mut state = DialogState::new(new_practice_dialog());
        state.update(Message::Input("   ".into()));
        assert_eq!(state.step(Message::Confirm), DialogStep::Stay);
        let mut state = DialogState::new(new_practice_dialog());
        assert_eq!(
            state.update(Message::Cancel),
            Some(DialogResult::Text(None))
        );
    }
}
