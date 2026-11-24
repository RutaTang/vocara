//! Port of `ui/song_practice_detail/widgets/note_editor.dart`: the add/edit
//! note modal — a dark studio card matching the app. Editing an existing
//! note offers a Delete action. The header says where the note is (or will
//! be) pinned: "Note at 00:12", or "Note across 00:12 → 00:20" for a range
//! the user drew.
//!
//! `showNoteEditor` returned a `Future<NoteEditorResult?>`; here the screen
//! holds a [`NoteEditor`] while it is open, draws its
//! [`view`](NoteEditor::view) over the window, runs
//! [`focus`](NoteEditor::focus) when it opens (`autofocus`), and feeds it
//! the messages until [`update`](NoteEditor::update) says it closed, with
//! Dart's result: `None` (cancelled), the new text, or deleted.
//!
//! Like `showDialog`'s route: a tap on the barrier or Escape cancels, and
//! nothing beneath sees a key while it is open. The text field is
//! multi-line (3 to 6 lines, then it scrolls), so Return breaks the line
//! rather than saving. (Dart owned the field's controller in the dialog's
//! state, disposed only after the exit transition; the state here is plain
//! data and goes with the editor.)
//!
//! It comes in and goes as a dialog route did: the screen keeps its
//! `confirm_dialog::DialogMotion` and draws it with
//! [`view_at`](NoteEditor::view_at), every color of the card at the fade's
//! opacity (iced has no subtree opacity), and keeps a closed one drawn,
//! taking no input, until it has faded out.
//!
//! Where iced differs: the caret is drawn in the text color (Flutter's was
//! coral).

use std::fmt;
use std::time::Duration;

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{center, column, container, opaque, row, stack, text_editor, Space};
use iced::{keyboard, widget, Background, Border, Color, Element, Event, Length, Shadow, Task};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::util::dart_string;
use vocara_model::util::format::format_clock_ms;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::faded;
use crate::ui::core::theme::{self, alpha, font, Studio, Zinc};
use crate::ui::core::widgets::confirm_dialog::{barrier, Fade};
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::wrap::Wrap;

#[cfg(test)]
mod tests;

/// The hint in the empty field.
pub const HINT: &str = "What happened here? (e.g. breath runs out, pitch flat)";

/// The barrier's color: black at 55 %.
pub const BARRIER: Color = alpha(Color::BLACK, 0.55);

/// The card's widest (`BoxConstraints(maxWidth: 460)`), and its inset from
/// the window's edges (`insetPadding`).
const CARD_MAX_WIDTH: f32 = 460.0;
const INSET: f32 = 24.0;

/// The field's text size and line height (14 px at 1.4).
const FIELD_SIZE: f32 = 14.0;
const FIELD_LINE_HEIGHT: f32 = 1.4;

/// How the field grows: from 3 lines to 6, then it scrolls.
const MIN_LINES: f32 = 3.0;
const MAX_LINES: f32 = 6.0;

/// Outcome of the note editor: either saved `content`, or `deleted`
/// (`NoteEditorResult`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NoteEditorResult {
    pub content: Option<String>,
    pub deleted: bool,
}

/// What the user did in the editor.
#[derive(Debug, Clone)]
pub enum Message {
    /// An edit in the text field.
    Edit(text_editor::Action),
    /// Add / Save.
    Save,
    /// Cancel, a tap on the barrier, or Escape.
    Cancel,
    /// Delete (editing only).
    Delete,
}

/// What a [`Message`] did to the open editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// It stays open (with its text edited).
    Stay,
    /// It closed with Dart's result: `None` when cancelled (or saved with
    /// nothing to save), else the text to keep or the delete.
    Closed(Option<NoteEditorResult>),
}

/// The range a note is kept at, as the view model normalizes it
/// (`normalizeMarkerRange`: the earlier edge first, never before 0, and a
/// span shorter than the minimum marker range collapsed to a point). The
/// screen passes the detail view model's.
pub type NormalizeRange = fn(i64, Option<i64>) -> (i64, Option<i64>);

/// The header for a note kept at `position_ms` (to `end_ms` for a range).
pub fn note_editor_title(position_ms: i64, end_ms: Option<i64>) -> String {
    match end_ms {
        Some(end) => format!(
            "Note across {} → {}",
            format_clock_ms(position_ms),
            format_clock_ms(end)
        ),
        None => format!("Note at {}", format_clock_ms(position_ms)),
    }
}

/// `Duration.inMilliseconds`.
fn in_millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// An open note editor: its header, whether it edits an existing note, and
/// the text as typed.
pub struct NoteEditor {
    title: String,
    editing: bool,
    content: text_editor::Content,
    input: widget::Id,
}

impl fmt::Debug for NoteEditor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoteEditor")
            .field("title", &self.title)
            .field("editing", &self.editing)
            .field("text", &self.content.text())
            .finish_non_exhaustive()
    }
}

impl NoteEditor {
    /// `showNoteEditor(existing: ..., at: ..., end: ...)`: `at` is the start
    /// the note is (or will be) pinned to; `end` (when given) marks a range
    /// note's end so the header reflects the span the user drew. An
    /// `existing` note's own range wins over both. The header names the
    /// range as it will be kept (`normalize`): a drag too short for a range
    /// is saved as a point, and was announced as "Note across 00:12 →
    /// 00:12".
    pub fn new(
        existing: Option<&PracticeNote>,
        at: Duration,
        end: Option<Duration>,
        normalize: NormalizeRange,
    ) -> Self {
        let (position_ms, end_ms) = normalize(
            existing.map_or(in_millis(at), |n| n.position_ms),
            existing.map_or(end.map(in_millis), |n| n.end_ms),
        );
        Self {
            title: note_editor_title(position_ms, end_ms),
            editing: existing.is_some(),
            content: text_editor::Content::with_text(existing.map_or("", |n| n.content.as_str())),
            input: widget::Id::unique(),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether it edits an existing note (Save and Delete) rather than adding
    /// one (Add).
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// The text as typed.
    pub fn text(&self) -> String {
        self.content.text()
    }

    /// The field's id.
    pub fn input_id(&self) -> widget::Id {
        self.input.clone()
    }

    /// The task that puts the keyboard in the field when the editor opens
    /// (`autofocus: true`).
    pub fn focus<T: Send + 'static>(&self) -> Task<T> {
        iced::widget::operation::focus(self.input.clone())
    }

    /// The trimmed text, when there is any to save.
    fn trimmed(&self) -> Option<String> {
        let text = self.content.text();
        let t = dart_string::trim(&text);
        (!t.is_empty()).then(|| t.to_string())
    }

    /// Whether Add / Save is live: not with nothing to save (it closed as if
    /// it had saved, the note keeping its old text).
    pub fn can_save(&self) -> bool {
        self.trimmed().is_some()
    }

    /// Handles `message`: whether the editor stays open or closes, and with
    /// what.
    pub fn update(&mut self, message: Message) -> Step {
        match message {
            Message::Edit(action) => {
                self.content.perform(action);
                Step::Stay
            }
            Message::Cancel => Step::Closed(None),
            Message::Delete => Step::Closed(Some(NoteEditorResult {
                content: None,
                deleted: true,
            })),
            // `_save`: the trimmed text, or nothing when it is blank.
            Message::Save => Step::Closed(self.trimmed().map(|content| NoteEditorResult {
                content: Some(content),
                deleted: false,
            })),
        }
    }

    /// The editor over the whole window, all in: the barrier and the card
    /// (at most 460 wide, inset 24 from the window's edges).
    pub fn view(&self) -> Element<'_, Message> {
        self.view_at(Fade::SHOWN)
    }

    /// The editor at `fade` (`confirm_dialog`'s module docs: its host keeps
    /// its `DialogMotion`).
    pub fn view_at(&self, fade: Fade) -> Element<'_, Message> {
        let heading = row![
            icons::icon(Icon::StickyNote, 16.0, fade.color(Studio::ACCENT)),
            Space::new().width(8),
            text(self.title.as_str())
                .size(15)
                .font(font::BOLD)
                .color(fade.color(Studio::TEXT_PRIMARY)),
        ]
        .align_y(Vertical::Center);

        let field = text_editor(&self.content)
            .id(self.input.clone())
            .placeholder(HINT)
            .on_action(Message::Edit)
            .size(FIELD_SIZE)
            .line_height(FIELD_LINE_HEIGHT)
            .padding(theme::FIELD_PADDING)
            .height(Length::Shrink)
            // iced's editor holds its text's height (not its own) to these
            // but lays its text out in `max_height` less its padding: the
            // padding goes on the maximum so the text may fill six lines.
            .min_height(FIELD_SIZE * FIELD_LINE_HEIGHT * MIN_LINES)
            .max_height(FIELD_SIZE * FIELD_LINE_HEIGHT * MAX_LINES + theme::FIELD_PADDING.y())
            .style(move |t, s| faded_field(field_style(t, s), fade.card));

        let mut actions = row![].align_y(Vertical::Center);
        if self.editing {
            actions = actions.push(
                studio_button("Delete", Message::Delete)
                    .icon(Icon::Trash2)
                    .variant(StudioButtonVariant::Danger)
                    .opacity(fade.card),
            );
        }
        actions = actions
            .push(Space::new().width(Length::Fill))
            .push(studio_button("Cancel", Message::Cancel).opacity(fade.card))
            .push(Space::new().width(8))
            .push(
                studio_button(if self.editing { "Save" } else { "Add" }, Message::Save)
                    .variant(StudioButtonVariant::Accent)
                    .enabled(self.can_save())
                    .opacity(fade.card),
            );

        let card = container(
            column![
                heading,
                Space::new().height(14),
                // 10 / 12, and the well's 1 px border: a decoration's border
                // pads its child too (as on the card).
                container(field)
                    .width(Length::Fill)
                    .padding([11, 13])
                    .style(fade.style(theme::inset(8.0))),
                Space::new().height(18),
                actions,
            ]
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .max_width(CARD_MAX_WIDTH)
        // 20, and the card's 1 px border.
        .padding(21)
        .style(fade.style(card_style));
        modal(
            container(card)
                .width(Length::Fill)
                .max_width(CARD_MAX_WIDTH + 2.0 * INSET)
                .padding(INSET)
                .into(),
            fade,
        )
    }
}

/// The card: `Studio.surface`, radius 12, `Studio.border`, shadow
/// `0x99000000` blurred 28 at (0, 12).
fn card_style(_: &iced::Theme) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0x99 as f32 / 255.0),
            offset: iced::Vector::new(0.0, 12.0),
            blur_radius: 28.0,
        },
        ..theme::boxed(Some(Studio::SURFACE), 12.0, Some(Studio::BORDER))
    }
}

/// The field: no fill or border of its own (the inset well draws them),
/// `Studio.textPrimary` text, a `Studio.textFaint` hint, Material 3's
/// selection color.
fn field_style(_: &iced::Theme, _: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: Studio::TEXT_FAINT,
        value: Studio::TEXT_PRIMARY,
        selection: alpha(Zinc::PRIMARY, 0.40),
    }
}

/// The field's look at `opacity`.
fn faded_field(style: text_editor::Style, opacity: f32) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: faded(style.placeholder, opacity),
        value: faded(style.value, opacity),
        selection: faded(style.selection, opacity),
    }
}

/// `content` centred over a dismissible [`BARRIER`] covering everything,
/// both at `fade`: a tap on the barrier cancels, the card takes its own
/// taps, Escape cancels and no key reaches what lies beneath.
fn modal(content: Element<'_, Message>, fade: Fade) -> Element<'_, Message> {
    let barrier = barrier(BARRIER, fade.barrier, Some(Message::Cancel));
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
                // The editor holds the keyboard (its field has already had
                // its turn).
                shell.capture_event();
            }
        }),
    )
    .into()
}
