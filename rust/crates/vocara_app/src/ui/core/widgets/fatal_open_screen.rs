//! Port of `ui/core/widgets/fatal_open_screen.dart`: the minimal window shown
//! when the library cannot open at launch.
//!
//! Deliberately tiny and dependency-light: no router, no services — when the
//! library cannot open, this screen must still be able to render. The app
//! shows it in place of everything else (Dart's `FatalOpenApp`), and its
//! window is visible from the start, so a fatal open never looks like the app
//! simply never launched.
//!
//! Drawn as the design system's dialogs are (`DESIGN.md`), on the page's
//! background: a centred column at most 460 wide, the title 15 px
//! semibold, the message 12 px `TEXT_SECONDARY` on 18 px lines, the error
//! detail in a well (11 px tabular, selectable through Copy details), and
//! the actions as dialog buttons: the secondary ones outlined, Quit Vocara
//! the one filled primary action.

use crate::ui::core::text;
use iced::alignment::Horizontal;
use iced::widget::{center, column, container, row, Space};
use iced::{Element, Length, Pixels};

use crate::ui::core::theme::{self, font, radius, type_scale, Studio};
use crate::ui::core::widgets::controls::{primary_button, secondary_button};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};

/// What the user can do on the fatal window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Copy the error detail to the clipboard (plain text here cannot be
    /// selected, and the details are what support needs).
    CopyDetails,
    /// Open the library folder in Finder (for backups or support).
    RevealLibrary,
    /// Quit Vocara.
    Quit,
}

/// What the app does for a [`Message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Put this text on the clipboard.
    Copy(String),
    RevealLibrary,
    Quit,
}

/// `FatalOpenScreen`: a title, a message, an optional raw error detail and
/// the actions that help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatalOpenScreen {
    pub title: String,
    pub message: String,
    /// Raw error detail, shown in an inset box when present.
    pub detail: Option<String>,
    /// Whether the Reveal Library Folder action is offered.
    pub reveal_library: bool,
}

impl FatalOpenScreen {
    /// The library on disk was written by a newer Vocara: refuse cleanly and
    /// tell the user the one action that helps. No detail and no folder
    /// button; the data is fine, only the app is too old.
    pub fn newer_library() -> Self {
        Self {
            title: "This library was created by a newer version of Vocara.".into(),
            message: "Please update Vocara to open it.".into(),
            detail: None,
            reveal_library: false,
        }
    }

    /// Another Vocara is running on the library, and this one would have to
    /// upgrade it to open it: upgrading under a running app could corrupt it.
    /// Reopening alone can't help while the other is running, so say what
    /// does.
    pub fn library_in_use() -> Self {
        Self {
            title: "Another Vocara is using this library.".into(),
            message: "Quit the other Vocara, then open this one again. It needs to \
                      update the library, which it can only do while no other copy \
                      of Vocara is running."
                .into(),
            detail: None,
            reveal_library: false,
        }
    }

    /// Another Vocara is still starting up on the library (recovering a
    /// crashed take can take a while), and this one waits for it before it
    /// opens: running alongside that boot, unguarded, its cleanup could trash
    /// what this one records. Not an error — the real window replaces this
    /// one as soon as the wait ends — but the same minimal chrome.
    pub fn waiting_for_library() -> Self {
        Self {
            title: "Waiting for another Vocara to finish starting up…".into(),
            message: "This window opens by itself as soon as it has. Quit if you \
                      would rather not wait."
                .into(),
            detail: None,
            reveal_library: false,
        }
    }

    /// Any other fatal open failure: show the error and offer the library
    /// folder (for backups or support) plus quit.
    pub fn open_failed(detail: impl Into<String>) -> Self {
        Self {
            title: "Vocara could not open its library.".into(),
            message: "Quit and reopen Vocara to try again. If this keeps happening, \
                      the details below can help."
                .into(),
            detail: Some(detail.into()),
            reveal_library: true,
        }
    }

    /// What `message` asks the app to do.
    pub fn update(&self, message: Message) -> Option<Action> {
        match message {
            Message::CopyDetails => self.detail.clone().map(Action::Copy),
            Message::RevealLibrary => self.reveal_library.then_some(Action::RevealLibrary),
            Message::Quit => Some(Action::Quit),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let mut content = column![
            text(self.title.as_str())
                .size(type_scale::HEADING)
                .font(font::SEMIBOLD)
                .line_height(Pixels(20.0))
                .align_x(Horizontal::Center)
                .width(Length::Fill)
                .color(Studio::TEXT_PRIMARY),
            Space::new().height(8),
            text(self.message.as_str())
                .size(type_scale::BODY)
                .line_height(Pixels(18.0))
                .align_x(Horizontal::Center)
                .width(Length::Fill)
                .color(Studio::TEXT_SECONDARY),
        ];
        if let Some(detail) = &self.detail {
            content = content.push(Space::new().height(16)).push(
                container(
                    scrollable(
                        container(
                            text(detail.as_str())
                                .size(type_scale::LABEL)
                                .line_height(Pixels(type_scale::LINE))
                                .color(Studio::TEXT_SECONDARY),
                        )
                        .padding([8, 10])
                        .width(Length::Fill),
                    )
                    .fading(),
                )
                .max_height(140)
                .width(Length::Fill)
                .style(theme::well(radius::WELL)),
            );
        }
        // Wrapping, not a plain row: the window can be resized narrow, and
        // the actions must stay visible at any width.
        let mut actions = row![].spacing(8);
        if self.detail.is_some() {
            actions = actions.push(secondary_button(COPY_DETAILS, Some(Message::CopyDetails)));
        }
        if self.reveal_library {
            actions = actions.push(secondary_button(
                REVEAL_LIBRARY,
                Some(Message::RevealLibrary),
            ));
        }
        actions = actions.push(primary_button(QUIT, Some(Message::Quit)));
        content = content
            .push(Space::new().height(20))
            .push(container(actions.wrap().vertical_spacing(8)).center_x(Length::Fill));
        container(center(container(content).max_width(460).padding(32)))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)
            .into()
    }
}

/// The actions' labels (sentence case, `DESIGN.md`).
pub const COPY_DETAILS: &str = "Copy details";
pub const REVEAL_LIBRARY: &str = "Reveal library folder";
pub const QUIT: &str = "Quit Vocara";

#[cfg(test)]
mod tests {
    use super::*;

    fn clicks(screen: &FatalOpenScreen, labels: &[&str]) -> Vec<Message> {
        let mut ui = iced_test::simulator(screen.view());
        for label in labels {
            ui.click(*label).expect("action shown");
        }
        ui.into_messages().collect()
    }

    #[test]
    fn the_newer_library_window_shows_the_update_copy_and_quits() {
        let screen = FatalOpenScreen::newer_library();
        let mut ui = iced_test::simulator(screen.view());
        assert!(ui
            .find("This library was created by a newer version of Vocara.")
            .is_ok());
        assert!(ui.find("Please update Vocara to open it.").is_ok());
        // The data is fine and no folder action is offered, only quitting.
        assert!(ui.find(REVEAL_LIBRARY).is_err());
        assert_eq!(clicks(&screen, &[QUIT]), vec![Message::Quit]);
        assert_eq!(screen.update(Message::Quit), Some(Action::Quit));
    }

    #[test]
    fn the_library_in_use_window_says_to_quit_the_other_vocara() {
        // "Quit and reopen" (the generic copy) can't help while the other
        // copy runs: the window must name the one action that does.
        let screen = FatalOpenScreen::library_in_use();
        assert!(screen.message.contains("Quit the other Vocara"));
        let mut ui = iced_test::simulator(screen.view());
        assert!(ui.find("Another Vocara is using this library.").is_ok());
        assert!(ui.find(screen.message.as_str()).is_ok());
        assert!(ui.find(REVEAL_LIBRARY).is_err());
        assert_eq!(clicks(&screen, &[QUIT]), vec![Message::Quit]);
    }

    #[test]
    fn the_waiting_window_says_what_it_waits_for_and_can_quit() {
        let screen = FatalOpenScreen::waiting_for_library();
        let mut ui = iced_test::simulator(screen.view());
        assert!(ui
            .find("Waiting for another Vocara to finish starting up…")
            .is_ok());
        assert!(ui.find(REVEAL_LIBRARY).is_err());
        assert_eq!(clicks(&screen, &[QUIT]), vec![Message::Quit]);
    }

    #[test]
    fn the_open_failed_window_shows_the_error_detail_and_both_actions_work() {
        let detail = "SqliteException(11): database disk image is malformed";
        let screen = FatalOpenScreen::open_failed(detail);
        let mut ui = iced_test::simulator(screen.view());
        assert!(ui.find("Vocara could not open its library.").is_ok());
        assert!(ui.find(detail).is_ok());
        assert_eq!(
            clicks(&screen, &[REVEAL_LIBRARY, QUIT, COPY_DETAILS]),
            vec![Message::RevealLibrary, Message::Quit, Message::CopyDetails]
        );
        assert_eq!(
            screen.update(Message::RevealLibrary),
            Some(Action::RevealLibrary)
        );
        // The details are what support needs; as plain text they could not
        // be selected.
        assert_eq!(
            screen.update(Message::CopyDetails),
            Some(Action::Copy(detail.to_string()))
        );
    }
}
