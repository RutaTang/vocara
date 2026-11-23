//! Port of `ui/core/widgets/confirm_dialog.dart`: the studio-themed modals
//! (dark card, coral accent) that replaced the forui adaptive dialog —
//! [`ConfirmDialog`] (`showConfirmDialog`: confirm/cancel, returns whether
//! the user confirmed) and [`StudioPrompt`] (`showStudioPrompt`: a single
//! line of text, returns the trimmed text, or nothing on cancel / empty).
//!
//! A dialog is a modal over the whole window, drag bar included, as a
//! Flutter dialog route sat above every page: the screen asks the app to show
//! it (`ui::Update::dialog`), the app draws it over the page and routes the
//! [`DialogResult`] back to the screen as one of its messages. The pieces are
//! public too, for a screen that draws a modal of its own.
//!
//! Dismissal is Flutter `showDialog`'s: a tap on the barrier or Escape
//! cancels (the barrier is dismissible); Enter in the prompt's field
//! submits, as its `onSubmitted` did; Enter in the confirm dialog does
//! nothing (no button there takes focus). While a dialog is up it holds the
//! keyboard: nothing beneath sees a key.
//!
//! The app's custom modals go through the same stack, so they too cover the
//! drag bar as a Flutter dialog route did: the update download dialog
//! ([`Dialog::UpdateDownload`], Home's banner and Settings), the new
//! technique dialog ([`Dialog::NewTechnique`], the technique library) and
//! the example rename ([`Dialog::RenameExample`], a technique page). They
//! speak the same [`Message`]s; [`DialogState::step`] says what a message did
//! — the update dialog's "Open Download Page" keeps it open and asks the app
//! to open the browser ([`DialogStep::OpenUrl`]).

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    center, column, container, mouse_area, opaque, row, stack, text, text_input, Space,
};
use iced::{keyboard, widget, Background, Element, Event, Length, Padding};

use super::studio_button::{studio_button, StudioButtonVariant};
use super::wrap::Wrap;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, font, Studio};
use crate::ui::home::widgets::update_notice::{DialogOutcome, UpdateDownloadDialog};
use crate::ui::technique_library::widgets::media::technique_media_section::RenameExampleDialog;
use crate::ui::technique_library::widgets::technique_dialogs::NewTechniqueDialog;

/// `showConfirmDialog(title: ..., body: ..., ...)`. Defaults as in Dart:
/// "Delete" / "Cancel", destructive (red confirm button), no icon. The title
/// is held to two lines so a long song name can't blow up the dialog; an
/// optional icon renders in an accent-tinted badge before it.
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub body: Option<String>,
    pub confirm_label: String,
    pub cancel_label: String,
    pub icon: Option<Icon>,
    pub destructive: bool,
}

impl ConfirmDialog {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: None,
            confirm_label: "Delete".to_string(),
            cancel_label: "Cancel".to_string(),
            icon: None,
            destructive: true,
        }
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn confirm_label(mut self, label: impl Into<String>) -> Self {
        self.confirm_label = label.into();
        self
    }

    pub fn cancel_label(mut self, label: impl Into<String>) -> Self {
        self.cancel_label = label.into();
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }
}

/// `showStudioPrompt(title: ..., ...)`. Defaults as in Dart: no label, no
/// hint, empty field, "Create" / "Cancel".
#[derive(Debug, Clone, PartialEq)]
pub struct StudioPrompt {
    pub title: String,
    pub label: String,
    pub hint: String,
    pub initial: Option<String>,
    pub confirm_label: String,
    pub cancel_label: String,
}

impl StudioPrompt {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            label: String::new(),
            hint: String::new(),
            initial: None,
            confirm_label: "Create".to_string(),
            cancel_label: "Cancel".to_string(),
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn initial(mut self, initial: impl Into<String>) -> Self {
        self.initial = Some(initial.into());
        self
    }

    pub fn confirm_label(mut self, label: impl Into<String>) -> Self {
        self.confirm_label = label.into();
        self
    }

    pub fn cancel_label(mut self, label: impl Into<String>) -> Self {
        self.cancel_label = label.into();
        self
    }
}

/// Which studio dialog to show.
#[derive(Debug, Clone)]
pub enum Dialog {
    Confirm(ConfirmDialog),
    Prompt(StudioPrompt),
    /// `showUpdateDownloadDialog`: closes with [`DialogResult::Closed`].
    UpdateDownload(UpdateDownloadDialog),
    /// `showNewTechniqueDialog`: closes with [`DialogResult::Text`] (the
    /// trimmed name, or `None` cancelled).
    NewTechnique(NewTechniqueDialog),
    /// A technique example's rename (`_promptLabel`): closes with
    /// [`DialogResult::Text`] (the trimmed caption, empty to clear it, or
    /// `None` cancelled).
    RenameExample(RenameExampleDialog),
}

impl From<ConfirmDialog> for Dialog {
    fn from(d: ConfirmDialog) -> Self {
        Dialog::Confirm(d)
    }
}

impl From<StudioPrompt> for Dialog {
    fn from(d: StudioPrompt) -> Self {
        Dialog::Prompt(d)
    }
}

impl From<UpdateDownloadDialog> for Dialog {
    fn from(d: UpdateDownloadDialog) -> Self {
        Dialog::UpdateDownload(d)
    }
}

impl From<NewTechniqueDialog> for Dialog {
    fn from(d: NewTechniqueDialog) -> Self {
        Dialog::NewTechnique(d)
    }
}

impl From<RenameExampleDialog> for Dialog {
    fn from(d: RenameExampleDialog) -> Self {
        Dialog::RenameExample(d)
    }
}

/// How a dialog closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogResult {
    /// A [`ConfirmDialog`]: whether the user confirmed.
    Confirmed(bool),
    /// A [`StudioPrompt`]: the trimmed text, or `None` on cancel / empty.
    /// (A [`NewTechniqueDialog`] too: the trimmed name, or `None`
    /// cancelled.)
    Text(Option<String>),
    /// A dialog that answers nothing closed ([`UpdateDownloadDialog`]'s
    /// "Later", a barrier tap or Escape).
    Closed,
}

impl DialogResult {
    /// The confirm dialog's answer (`false` for a prompt).
    pub fn confirmed(&self) -> bool {
        matches!(self, DialogResult::Confirmed(true))
    }

    /// The prompt's text (`None` for a confirm dialog).
    pub fn text(self) -> Option<String> {
        match self {
            DialogResult::Text(t) => t,
            DialogResult::Confirmed(_) | DialogResult::Closed => None,
        }
    }
}

/// What the user did in a dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The cancel button, a barrier tap or Escape.
    Cancel,
    /// The confirm button (or Enter in the prompt's field).
    Confirm,
    /// The prompt's text changed.
    Input(String),
}

/// What a [`Message`] did to an open dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogStep {
    /// It stays open, as it is (or with its text edited).
    Stay,
    /// It stays open and asks for `url` to be opened in the browser (the
    /// update dialog's "Open Download Page": it stays open on purpose, so
    /// the URL it shows is the fallback if the launch fails).
    OpenUrl(String),
    /// It closed with this result.
    Closed(DialogResult),
}

/// An open dialog: what it shows, and the prompt's text as typed.
#[derive(Debug, Clone)]
pub struct DialogState {
    dialog: Dialog,
    text: String,
    input: widget::Id,
}

impl DialogState {
    pub fn new(dialog: impl Into<Dialog>) -> Self {
        let dialog = dialog.into();
        let text = match &dialog {
            Dialog::Prompt(p) => p.initial.clone().unwrap_or_default(),
            _ => String::new(),
        };
        Self {
            dialog,
            text,
            input: widget::Id::unique(),
        }
    }

    pub fn dialog(&self) -> &Dialog {
        &self.dialog
    }

    /// The task that puts the keyboard in the dialog's field when it opens
    /// (`autofocus: true`: the prompt's, the new technique's name); nothing
    /// for a dialog without one.
    pub fn focus<T: Send + 'static>(&self) -> iced::Task<T> {
        match &self.dialog {
            Dialog::Prompt(_) => iced::widget::operation::focus(self.input.clone()),
            Dialog::NewTechnique(d) => d.focus(),
            Dialog::RenameExample(d) => d.focus(),
            Dialog::Confirm(_) | Dialog::UpdateDownload(_) => iced::Task::none(),
        }
    }

    /// Handles `message`; `Some` when the dialog closes, with its result.
    pub fn update(&mut self, message: Message) -> Option<DialogResult> {
        match self.step(message) {
            DialogStep::Closed(result) => Some(result),
            DialogStep::Stay | DialogStep::OpenUrl(_) => None,
        }
    }

    /// Handles `message`: whether the dialog stays, asks for the browser, or
    /// closes (with its result).
    pub fn step(&mut self, message: Message) -> DialogStep {
        let closed =
            |result: Option<DialogResult>| result.map_or(DialogStep::Stay, DialogStep::Closed);
        match (message, &mut self.dialog) {
            (message, Dialog::UpdateDownload(d)) => match d.update(message) {
                Some(DialogOutcome::Close) => DialogStep::Closed(DialogResult::Closed),
                Some(DialogOutcome::OpenUrl(url)) => DialogStep::OpenUrl(url),
                None => DialogStep::Stay,
            },
            (message, Dialog::NewTechnique(d)) => closed(d.update(message)),
            (message, Dialog::RenameExample(d)) => closed(d.update(message)),
            (Message::Input(text), _) => {
                self.text = text;
                DialogStep::Stay
            }
            (Message::Cancel, Dialog::Confirm(_)) => {
                DialogStep::Closed(DialogResult::Confirmed(false))
            }
            (Message::Confirm, Dialog::Confirm(_)) => {
                DialogStep::Closed(DialogResult::Confirmed(true))
            }
            (Message::Cancel, Dialog::Prompt(_)) => DialogStep::Closed(DialogResult::Text(None)),
            (Message::Confirm, Dialog::Prompt(_)) => {
                let t = self.text.trim();
                DialogStep::Closed(DialogResult::Text(if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }))
            }
        }
    }

    /// The dialog over the whole window: the barrier and the centred card.
    pub fn view(&self) -> Element<'_, Message> {
        let card = match &self.dialog {
            Dialog::Confirm(d) => confirm_card(d),
            Dialog::Prompt(p) => prompt_card(p, &self.text, self.input.clone()),
            // The custom modals draw their own barrier and card.
            Dialog::UpdateDownload(d) => return d.view(),
            Dialog::NewTechnique(d) => return d.view(),
            Dialog::RenameExample(d) => return d.view(),
        };
        modal(card)
    }
}

/// The card's outer geometry: 400 wide, 22 padding, inside a 24 margin.
fn card<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(
        container(content)
            .width(400)
            .padding(22)
            .style(theme::dialog_card),
    )
    .padding(24)
    .into()
}

fn buttons<'a>(
    cancel: &str,
    confirm: &str,
    confirm_variant: StudioButtonVariant,
) -> Element<'a, Message> {
    row![
        Space::new().width(Length::Fill),
        studio_button(cancel, Message::Cancel),
        studio_button(confirm, Message::Confirm).variant(confirm_variant),
    ]
    .spacing(10)
    .into()
}

fn confirm_card(d: &ConfirmDialog) -> Element<'_, Message> {
    let mut heading = row![].align_y(Vertical::Top);
    if let Some(icon) = d.icon {
        heading = heading.push(
            container(icons::icon(icon, 15.0, Studio::ACCENT))
                .center_x(30)
                .center_y(30)
                .style(|_| theme::boxed(Some(alpha(Studio::ACCENT, 0.14)), 8.0, None)),
        );
        heading = heading.push(Space::new().width(12));
    }
    heading = heading.push(
        // Two lines at most (Dart ellipsized past them; iced clips them).
        container(
            text(d.title.as_str())
                .size(15)
                .font(font::BOLD)
                .line_height(1.3)
                .color(Studio::TEXT_PRIMARY),
        )
        .width(Length::Fill)
        .max_height(15.0 * 1.3 * 2.0)
        .clip(true),
    );
    let mut content = column![heading];
    if let Some(body) = &d.body {
        content = content.push(Space::new().height(12)).push(
            text(body.as_str())
                .size(13)
                .line_height(1.5)
                .color(Studio::TEXT_SECONDARY),
        );
    }
    let variant = if d.destructive {
        StudioButtonVariant::Danger
    } else {
        StudioButtonVariant::Accent
    };
    content = content.push(Space::new().height(22)).push(buttons(
        &d.cancel_label,
        &d.confirm_label,
        variant,
    ));
    card(content.into())
}

fn prompt_card<'a>(p: &'a StudioPrompt, value: &str, input: widget::Id) -> Element<'a, Message> {
    let mut content = column![
        text(p.title.as_str())
            .size(15)
            .font(font::BOLD)
            .color(Studio::TEXT_PRIMARY),
        Space::new().height(16),
    ];
    if !p.label.is_empty() {
        content = content
            .push(
                text(p.label.to_uppercase())
                    .size(10.5)
                    .font(font::BOLD)
                    .color(Studio::TEXT_FAINT),
            )
            .push(Space::new().height(6));
    }
    let field = text_input(&p.hint, value)
        .id(input)
        .on_input(Message::Input)
        .on_submit(Message::Confirm)
        .size(14)
        .padding(0)
        .style(theme::studio_input);
    content = content
        .push(
            container(field)
                .padding(Padding::from([10, 12]))
                .style(theme::inset(8.0)),
        )
        .push(Space::new().height(22))
        .push(buttons(
            &p.cancel_label,
            &p.confirm_label,
            StudioButtonVariant::Accent,
        ));
    card(content.into())
}

/// `content` centred over a dismissible barrier (`Color(0xB0000000)`)
/// covering everything: a tap on the barrier cancels, the card takes its own
/// taps, Escape cancels and no key reaches what lies beneath.
pub fn modal<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    let barrier = opaque(
        mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| container::Style {
                    background: Some(Background::Color(theme::DIALOG_BARRIER)),
                    ..container::Style::default()
                }),
        )
        .on_press(Message::Cancel),
    );
    let layers = stack![barrier, center(opaque(content)).align_x(Horizontal::Center)];
    Wrap::<Message, ()>::new(
        layers,
        Box::new(|_, event, _, _, shell| {
            if let Event::Keyboard(event) = event {
                if let keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                } = event
                {
                    shell.publish(Message::Cancel);
                }
                // The dialog holds the keyboard (a text field inside it has
                // already had its turn).
                shell.capture_event();
            }
        }),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::Named;
    use iced::Point;

    fn run(
        state: &mut DialogState,
        drive: impl FnOnce(&mut iced_test::Simulator<'_, Message>),
    ) -> Vec<Message> {
        let mut ui = iced_test::simulator(state.view());
        drive(&mut ui);
        ui.into_messages().collect()
    }

    #[test]
    fn confirm_returns_true_cancel_false() {
        let mut state =
            DialogState::new(ConfirmDialog::new("Delete “Song”?").body("This cannot be undone."));
        let messages = run(&mut state, |ui| {
            assert!(ui.find("This cannot be undone.").is_ok());
            ui.click("Delete").expect("confirm button");
        });
        assert_eq!(messages, vec![Message::Confirm]);
        assert_eq!(
            state.update(Message::Confirm),
            Some(DialogResult::Confirmed(true))
        );

        let mut state = DialogState::new(ConfirmDialog::new("Delete?"));
        let messages = run(&mut state, |ui| {
            ui.click("Cancel").expect("cancel button");
        });
        assert_eq!(messages, vec![Message::Cancel]);
        assert_eq!(
            state.update(Message::Cancel),
            Some(DialogResult::Confirmed(false))
        );
    }

    #[test]
    fn escape_and_a_barrier_tap_cancel_and_enter_does_nothing_in_a_confirm() {
        let mut state = DialogState::new(ConfirmDialog::new("Delete?"));
        let messages = run(&mut state, |ui| {
            let _ = ui.tap_key(keyboard::Key::Named(Named::Enter));
            let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
            ui.point_at(Point::new(5.0, 5.0));
            let _ = ui.simulate(iced_test::simulator::click());
        });
        assert_eq!(messages, vec![Message::Cancel, Message::Cancel]);
    }

    #[test]
    fn a_tap_on_the_card_is_not_a_barrier_tap() {
        let mut state = DialogState::new(ConfirmDialog::new("Delete?").body("Body"));
        let messages = run(&mut state, |ui| {
            ui.click("Body").expect("body shown");
        });
        assert!(messages.is_empty());
    }

    #[test]
    fn the_prompt_returns_the_trimmed_text_or_nothing() {
        let mut state = DialogState::new(
            StudioPrompt::new("New practice")
                .label("Name")
                .initial("  Song  "),
        );
        assert!(state.update(Message::Input("  My song ".into())).is_none());
        assert_eq!(
            state.update(Message::Confirm),
            Some(DialogResult::Text(Some("My song".into())))
        );
        let mut state = DialogState::new(StudioPrompt::new("New practice"));
        state.update(Message::Input("   ".into()));
        assert_eq!(
            state.update(Message::Confirm),
            Some(DialogResult::Text(None))
        );
        let mut state = DialogState::new(StudioPrompt::new("New practice").initial("x"));
        assert_eq!(
            state.update(Message::Cancel),
            Some(DialogResult::Text(None))
        );
    }

    #[test]
    fn the_prompt_shows_its_label_upper_cased_and_its_buttons() {
        let mut state = DialogState::new(
            StudioPrompt::new("New practice")
                .label("Name")
                .hint("Song title"),
        );
        let messages = run(&mut state, |ui| {
            assert!(ui.find("NAME").is_ok());
            assert!(ui.find("New practice").is_ok());
            ui.click("Create").expect("confirm button");
        });
        assert_eq!(messages, vec![Message::Confirm]);
    }

    #[test]
    fn typing_and_enter_in_the_prompt_submit_it() {
        let mut state = DialogState::new(StudioPrompt::new("Rename").initial("Old"));
        let messages = run(&mut state, |ui| {
            ui.click("Old").expect("field shows the initial text");
            let _ = ui.tap_key(keyboard::Key::Named(Named::End));
            let _ = ui.typewrite("!");
            let _ = ui.tap_key(keyboard::Key::Named(Named::Enter));
            let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
        });
        assert_eq!(
            messages,
            vec![
                Message::Input("Old!".into()),
                Message::Confirm,
                Message::Cancel
            ]
        );
    }

    #[test]
    fn the_update_dialog_opens_the_browser_and_stays_and_closes_with_nothing() {
        use vocara_services::update_check_service::UpdateInfo;
        let mut state = DialogState::new(UpdateDownloadDialog::new(UpdateInfo {
            version: "9.9.9".into(),
            url: "https://example.com/download".into(),
            notes: "Notes.".into(),
        }));
        let messages = run(&mut state, |ui| {
            assert!(ui.find("Vocara 9.9.9 is available").is_ok());
            ui.click("Open Download Page").expect("the open button");
            ui.click("Later").expect("the later button");
        });
        assert_eq!(messages, vec![Message::Confirm, Message::Cancel]);
        assert_eq!(
            state.step(Message::Confirm),
            DialogStep::OpenUrl("https://example.com/download".into())
        );
        // The URL is read-only.
        assert_eq!(state.step(Message::Input("x".into())), DialogStep::Stay);
        assert_eq!(
            state.step(Message::Cancel),
            DialogStep::Closed(DialogResult::Closed)
        );
        assert!(!DialogResult::Closed.confirmed());
        assert_eq!(DialogResult::Closed.text(), None);
    }

    #[test]
    fn the_new_technique_dialog_waits_for_a_name() {
        let mut state = DialogState::new(NewTechniqueDialog::new());
        let messages = run(&mut state, |ui| {
            assert!(ui.find("New technique").is_ok());
            ui.click("Cancel").expect("the cancel button");
        });
        assert_eq!(messages, vec![Message::Cancel]);
        assert_eq!(state.step(Message::Input("   ".into())), DialogStep::Stay);
        assert_eq!(state.step(Message::Confirm), DialogStep::Stay, "blank");
        state.step(Message::Input(" Twang ".into()));
        assert_eq!(
            state.update(Message::Confirm),
            Some(DialogResult::Text(Some("Twang".into())))
        );
        assert_eq!(
            DialogState::new(NewTechniqueDialog::new()).update(Message::Cancel),
            Some(DialogResult::Text(None))
        );
    }
}
