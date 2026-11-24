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
//!
//! A dialog comes in and goes as `DialogRoute` did, over 150 ms each way
//! ([`DialogMotion`]): its barrier's color on `Curves.ease` (the route's
//! `barrierCurve`), its card fading on `Curves.easeOut`, no scale. Its host
//! (the app's dialog stack, or the page that draws it) keeps the motion,
//! draws the dialog at its [`Fade`] and keeps drawing a closed one, taking
//! no input, until it has gone. iced has no subtree opacity, so a card
//! takes every color it draws (its texts, fills, borders, shadows, buttons
//! and fields) at the fade's opacity.

use std::time::Duration;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    center, column, container, mouse_area, opaque, row, scrollable, stack, text, text_input, Space,
};
use iced::{keyboard, widget, Background, Color, Element, Event, Length, Padding};

use super::studio_button::{studio_button, StudioButtonVariant};
use super::studio_text::studio_text;
use super::wrap::Wrap;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::motion::{Controller, Curve, Status};
use crate::ui::core::theme::{self, faded, faded_container, faded_input, font, Studio};
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

    /// The dialog over the whole window, all in: the barrier and the
    /// centred card.
    pub fn view(&self) -> Element<'_, Message> {
        self.view_at(Fade::SHOWN)
    }

    /// The dialog at `fade` (see the module docs).
    pub fn view_at(&self, fade: Fade) -> Element<'_, Message> {
        let card = match &self.dialog {
            Dialog::Confirm(d) => confirm_card(d, fade),
            Dialog::Prompt(p) => prompt_card(p, &self.text, self.input.clone(), fade),
            // The custom modals draw their own barrier and card.
            Dialog::UpdateDownload(d) => return d.view_at(fade),
            Dialog::NewTechnique(d) => return d.view_at(fade),
            Dialog::RenameExample(d) => return d.view_at(fade),
        };
        modal(card, fade)
    }
}

/// The card's outer geometry: 400 wide, 22 padding, inside a 24 margin.
fn card<'a>(content: Element<'a, Message>, fade: Fade) -> Element<'a, Message> {
    container(
        container(content)
            .width(400)
            .padding(22)
            .style(fade.style(theme::dialog_card)),
    )
    .padding(24)
    .into()
}

fn buttons<'a>(
    cancel: &str,
    confirm: &str,
    confirm_variant: StudioButtonVariant,
    fade: Fade,
) -> Element<'a, Message> {
    row![
        Space::new().width(Length::Fill),
        studio_button(cancel, Message::Cancel).opacity(fade.card),
        studio_button(confirm, Message::Confirm)
            .variant(confirm_variant)
            .opacity(fade.card),
    ]
    .spacing(10)
    .into()
}

fn confirm_card(d: &ConfirmDialog, fade: Fade) -> Element<'_, Message> {
    let mut heading = row![].align_y(Vertical::Top);
    if let Some(icon) = d.icon {
        heading = heading.push(
            container(icons::icon(icon, 15.0, fade.color(Studio::ACCENT)))
                .center_x(30)
                .center_y(30)
                .style(move |_| {
                    theme::boxed(
                        Some(fade.color(theme::alpha(Studio::ACCENT, 0.14))),
                        8.0,
                        None,
                    )
                }),
        );
        heading = heading.push(Space::new().width(12));
    }
    heading = heading.push(
        // Two lines at most, the second ellipsized past them.
        studio_text(d.title.as_str())
            .size(15)
            .font(font::BOLD)
            .line_height(1.3)
            .color(fade.color(Studio::TEXT_PRIMARY))
            .width(Length::Fill)
            .ellipsis(2),
    );
    let mut content = column![heading];
    if let Some(body) = &d.body {
        content = content.push(Space::new().height(12)).push(
            text(body.as_str())
                .size(13)
                .line_height(1.5)
                .color(fade.color(Studio::TEXT_SECONDARY)),
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
        fade,
    ));
    card(content.into(), fade)
}

fn prompt_card<'a>(
    p: &'a StudioPrompt,
    value: &str,
    input: widget::Id,
    fade: Fade,
) -> Element<'a, Message> {
    let mut content = column![
        text(p.title.as_str())
            .size(15)
            .font(font::BOLD)
            .color(fade.color(Studio::TEXT_PRIMARY)),
        Space::new().height(16),
    ];
    if !p.label.is_empty() {
        content = content
            .push(
                studio_text(p.label.to_uppercase())
                    .size(10.5)
                    .font(font::BOLD)
                    .letter_spacing(0.6)
                    .color(fade.color(Studio::TEXT_FAINT)),
            )
            .push(Space::new().height(6));
    }
    let field = text_input(&p.hint, value)
        .id(input)
        .on_input(Message::Input)
        .on_submit(Message::Confirm)
        .size(14)
        .padding(0)
        .style(fade.input(theme::studio_input));
    content = content
        .push(
            container(field)
                .padding(Padding::from([10, 12]))
                .style(fade.style(theme::inset(8.0))),
        )
        .push(Space::new().height(22))
        .push(buttons(
            &p.cancel_label,
            &p.confirm_label,
            StudioButtonVariant::Accent,
            fade,
        ));
    card(content.into(), fade)
}

/// How long a dialog takes to come in and to go (`DialogRoute`'s
/// `transitionDuration`).
pub const TRANSITION: Duration = Duration::from_millis(150);

/// How far a dialog is in: its barrier's strength and its card's opacity,
/// each 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    pub barrier: f32,
    pub card: f32,
}

impl Fade {
    /// All in.
    pub const SHOWN: Fade = Fade {
        barrier: 1.0,
        card: 1.0,
    };

    /// A color of the card, at its opacity.
    pub fn color(self, color: Color) -> Color {
        faded(color, self.card)
    }

    /// A container style of the card, at its opacity.
    pub fn style<'a>(
        self,
        style: impl Fn(&iced::Theme) -> container::Style + 'a,
    ) -> impl Fn(&iced::Theme) -> container::Style + 'a {
        move |theme| faded_container(style(theme), self.card)
    }

    /// A text field style of the card, at its opacity.
    pub fn input<'a>(
        self,
        style: impl Fn(&iced::Theme, text_input::Status) -> text_input::Style + 'a,
    ) -> impl Fn(&iced::Theme, text_input::Status) -> text_input::Style + 'a {
        move |theme, status| faded_input(style(theme, status), self.card)
    }

    /// A scrollable style of the card, at its opacity.
    pub fn scrollbar<'a>(
        self,
        style: impl Fn(&iced::Theme, scrollable::Status) -> scrollable::Style + 'a,
    ) -> impl Fn(&iced::Theme, scrollable::Status) -> scrollable::Style + 'a {
        move |theme, status| theme::faded_scrollable(style(theme, status), self.card)
    }
}

/// A dialog route's animation (`DialogRoute`): forward over [`TRANSITION`]
/// as it opens, back as it closes; the barrier on `Curves.ease`
/// (`barrierCurve`, a `CurveTween`), the card on `Curves.easeOut` both ways.
#[derive(Debug, Clone)]
pub struct DialogMotion(Controller);

impl DialogMotion {
    /// Coming in from `now`.
    pub fn opening(now: iced::time::Instant) -> Self {
        let mut route = Controller::new(TRANSITION, TRANSITION);
        route.forward(now);
        Self(route)
    }

    /// All in already.
    pub fn shown() -> Self {
        Self(Controller::completed(TRANSITION, TRANSITION))
    }

    /// Going from `now`, from wherever it got.
    pub fn close(&mut self, now: iced::time::Instant) {
        self.0.reverse(now);
    }

    /// How far in it is at `now`.
    pub fn fade(&self, now: iced::time::Instant) -> Fade {
        Fade {
            barrier: self.0.curved(Curve::Ease, None, now),
            card: self.0.curved(Curve::EaseOut, Some(Curve::EaseOut), now),
        }
    }

    /// Whether it comes in or goes at `now`.
    pub fn is_animating(&self, now: iced::time::Instant) -> bool {
        self.0.is_animating(now)
    }

    /// Whether it has gone at `now` (closed, and faded out).
    pub fn is_gone(&self, now: iced::time::Instant) -> bool {
        self.0.status(now) == Status::Dismissed
    }
}

/// A barrier of `color` over everything at `strength` (0 to 1); a press on
/// it reports `on_press`, when given. It takes every press either way.
pub fn barrier<'a, M: Clone + 'a>(
    color: Color,
    strength: f32,
    on_press: Option<M>,
) -> Element<'a, M> {
    let fill = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(faded(color, strength))),
            ..container::Style::default()
        });
    match on_press {
        Some(on_press) => opaque(mouse_area(fill).on_press(on_press)),
        None => opaque(fill),
    }
}

/// `content` centred over a dismissible barrier (`Color(0xB0000000)`)
/// covering everything, both at `fade`: a tap on the barrier cancels, the
/// card takes its own taps, Escape cancels and no key reaches what lies
/// beneath.
pub fn modal<'a>(content: Element<'a, Message>, fade: Fade) -> Element<'a, Message> {
    let barrier = barrier(theme::DIALOG_BARRIER, fade.barrier, Some(Message::Cancel));
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

/// Checks on a [`DialogMotion`] whose run began at some unknown time
/// between two instants (a test cannot stop the clock while a page opens
/// or closes its dialog).
#[cfg(test)]
pub(crate) mod testing {
    use iced::time::Instant;

    use super::{DialogMotion, Fade, TRANSITION};
    use crate::ui::core::motion::Curve;

    const GONE: Fade = Fade {
        barrier: 0.0,
        card: 0.0,
    };

    /// Halfway on its curves: the barrier's `Curves.ease`, the card's
    /// `Curves.easeOut`.
    fn halfway() -> Fade {
        Fade {
            barrier: Curve::Ease.transform(0.5),
            card: Curve::EaseOut.transform(0.5),
        }
    }

    fn at_most(a: Fade, b: Fade) -> bool {
        a.barrier <= b.barrier && a.card <= b.card
    }

    /// It came in from nothing, starting between `before` and `after`:
    /// nothing at the start, halfway on its curves half its time in, all in
    /// at its end.
    pub fn assert_comes_in(motion: &DialogMotion, before: Instant, after: Instant) {
        let t = TRANSITION;
        assert_eq!(motion.fade(before), GONE, "start");
        let (early, late) = (motion.fade(before + t / 2), motion.fade(after + t / 2));
        assert!(
            at_most(early, halfway()) && at_most(halfway(), late),
            "middle: {early:?} {late:?}"
        );
        assert_eq!(motion.fade(after + t), Fade::SHOWN, "end");
        assert!(motion.is_animating(after) && !motion.is_animating(after + t));
    }

    /// It went from all in, starting between `before` and `after`: all
    /// there at the start, halfway on its curves half its time on, gone at
    /// its end.
    pub fn assert_goes(motion: &DialogMotion, before: Instant, after: Instant) {
        let t = TRANSITION;
        assert_eq!(motion.fade(before), Fade::SHOWN, "start");
        let (early, late) = (motion.fade(before + t / 2), motion.fade(after + t / 2));
        assert!(
            at_most(late, halfway()) && at_most(halfway(), early),
            "middle: {early:?} {late:?}"
        );
        assert_eq!(motion.fade(after + t), GONE, "end");
        assert!(motion.is_gone(after + t) && !motion.is_animating(after + t));
    }
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
    fn a_dialog_comes_in_and_goes_over_150_ms_its_barrier_on_ease_its_card_on_ease_out() {
        use crate::ui::core::widgets::animated::testing::MS;
        use iced::time::Instant;

        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        let t0 = Instant::now();
        let coming = DialogMotion::opening(t0);
        assert_eq!(
            coming.fade(t0),
            Fade {
                barrier: 0.0,
                card: 0.0
            },
            "start"
        );
        let middle = coming.fade(t0 + 75 * MS);
        assert!(
            close(middle.barrier, Curve::Ease.transform(0.5)),
            "{middle:?}"
        );
        assert!(
            close(middle.card, Curve::EaseOut.transform(0.5)),
            "{middle:?}"
        );
        assert_eq!(coming.fade(t0 + 150 * MS), Fade::SHOWN, "end");
        assert!(coming.is_animating(t0 + 149 * MS));
        assert!(!coming.is_animating(t0 + 150 * MS));

        // Going runs the same curves back.
        let mut going = DialogMotion::shown();
        assert!(!going.is_animating(t0));
        going.close(t0);
        assert_eq!(going.fade(t0), Fade::SHOWN, "start");
        let middle = going.fade(t0 + 75 * MS);
        assert!(
            close(middle.barrier, Curve::Ease.transform(0.5)),
            "{middle:?}"
        );
        assert!(
            close(middle.card, Curve::EaseOut.transform(0.5)),
            "{middle:?}"
        );
        assert_eq!(
            going.fade(t0 + 150 * MS),
            Fade {
                barrier: 0.0,
                card: 0.0
            },
            "end"
        );
        assert!(!going.is_animating(t0 + 150 * MS));

        // Closed while still coming in, it goes back from where it got.
        let mut quick = DialogMotion::opening(t0);
        quick.close(t0 + 75 * MS);
        let back = quick.fade(t0 + 75 * MS);
        assert!(close(back.card, Curve::EaseOut.transform(0.5)), "{back:?}");
        assert_eq!(
            quick.fade(t0 + 150 * MS),
            Fade {
                barrier: 0.0,
                card: 0.0
            }
        );
    }

    #[test]
    fn a_dialog_draws_its_barrier_and_every_color_of_its_card_at_its_fade() {
        use crate::ui::core::motion::Lerp;
        use crate::ui::core::widgets::animated::testing::{near, Frames};
        use iced::time::Instant;
        use iced::{Point, Size, Theme};

        // Over white, so the barrier's darkening shows.
        let view = |fade: Fade| -> Element<'static, Message> {
            stack![
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::fill(Color::WHITE)),
                modal(card(Space::new().height(100).into(), fade), fade),
            ]
            .into()
        };
        let mut window = Frames::new(Size::new(600.0, 400.0));
        let corner = Point::new(5.0, 5.0);
        for (barrier, card) in [(0.0, 0.0), (0.5, 0.75), (1.0, 1.0)] {
            let fade = Fade { barrier, card };
            window.frame(view(fade), &[], Instant::now());
            let shade = Color::WHITE.lerp(Color::BLACK, theme::DIALOG_BARRIER.a * barrier);
            assert!(near(window.pixel(corner), shade), "barrier at {barrier}");
            let full = theme::dialog_card(&Theme::Dark);
            let style = fade.style(theme::dialog_card)(&Theme::Dark);
            assert_eq!(
                style.background,
                full.background.map(|b| b.scale_alpha(card)),
                "fill at {card}"
            );
            assert_eq!(style.border.color, faded(full.border.color, card));
            assert_eq!(style.shadow.color, faded(full.shadow.color, card));
            assert_eq!(
                fade.color(Studio::TEXT_PRIMARY).a,
                Studio::TEXT_PRIMARY.a * card
            );
        }
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
    fn a_long_title_wraps_to_two_lines_the_second_ellipsized() {
        use iced_test::selector::Candidate;

        crate::ui::core::widgets::studio_text::testing::load_fonts();
        let title = "Delete the take “Verse two, third pass, with the harmony \
                     line and the ad-libs” and every clip recorded into it?";
        let state = DialogState::new(ConfirmDialog::new(title).body("This cannot be undone."));
        let mut ui = iced_test::simulator(state.view());
        // Found by its full text, as a Flutter finder reads `Text.data`...
        let full = ui.find(title).expect("the title").bounds();
        // ...it shows two lines (15 px at 1.3), the second ending in "…".
        assert!((full.height - 2.0 * 15.0 * 1.3).abs() < 0.01, "{full:?}");
        let shown = ui
            .find(|candidate: Candidate<'_>| match candidate {
                Candidate::Text { content, .. } if content.ends_with('\u{2026}') => {
                    Some(content.to_string())
                }
                _ => None,
            })
            .expect("the cut title");
        let lines: Vec<&str> = shown.lines().collect();
        assert_eq!(lines.len(), 2, "{shown:?}");
        assert!(title.starts_with(lines[0]));
        // The short title of the other tests shows whole, on one line.
        let state = DialogState::new(ConfirmDialog::new("Delete?"));
        let mut ui = iced_test::simulator(state.view());
        let short = ui.find("Delete?").expect("the title").bounds();
        assert!((short.height - 15.0 * 1.3).abs() < 0.01, "{short:?}");
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
