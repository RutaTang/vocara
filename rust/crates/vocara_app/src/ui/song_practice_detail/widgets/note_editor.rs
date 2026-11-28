//! Port of `ui/song_practice_detail/widgets/note_editor.dart`: the add/edit
//! note modal — a dark studio card matching the app. Editing an existing
//! note offers a Delete action. The header says where the note is (or will
//! be) pinned: "Note at 0:12", or "Note across 0:12 → 0:20" for a range
//! the user drew.
//!
//! `showNoteEditor` returned a `Future<NoteEditorResult?>`; here the screen
//! holds a [`NoteEditor`] while it is open, draws its
//! [`view`](NoteEditor::view) over the window, runs
//! [`focus`](NoteEditor::focus) when it opens (`autofocus`), and feeds it
//! the messages until [`update`](NoteEditor::update) says it closed, with
//! Dart's result: `None` (cancelled), the new text, or deleted.
//!
//! Like `showDialog`'s route: Escape cancels, and nothing beneath sees a key
//! while it is open. A tap on the barrier cancels too, but not over text the
//! user changed: a stray click outside the card threw what they had typed
//! away without a word. The text field is multi-line (3 to 6 lines, then it
//! scrolls), so Return breaks the line, and Cmd+Return (Ctrl+Return off
//! macOS) saves. (Dart owned the field's controller in the dialog's state,
//! disposed only after the exit transition; the state here is plain data and
//! goes with the editor.)
//!
//! It comes in and goes as a dialog route did: the screen keeps its
//! `confirm_dialog::DialogMotion` and draws it with
//! [`view_at`](NoteEditor::view_at), the card as one layer at the fade's
//! opacity (the route's `FadeTransition`), and keeps a closed one drawn,
//! taking no input, until it has faded out.
//!
//! The field is the app's multi-line text field (`widgets::text_field`):
//! Flutter's macOS caret in the coral `cursorColor`, the right-click menu,
//! and the field's inset inside its well (`theme::FIELD_PADDING`).

use std::fmt;
use std::time::Duration;

use super::lyrics_panel::lyric_time;
use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, center, column, container, opaque, row, stack, Space};
use iced::{
    keyboard, widget, Background, Border, Color, Element, Event, Length, Padding, Pixels, Shadow,
    Task,
};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::util::dart_string;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, font, Studio};
use crate::ui::core::widgets::confirm_dialog::{self, barrier, Fade};
use crate::ui::core::widgets::controls::{dialog_button_colors, DialogButton};
use crate::ui::core::widgets::focus_traversal::focus_scope;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::text_field::editor::{self as text_editor, text_editor};
use crate::ui::core::widgets::wrap::Wrap;

#[cfg(test)]
mod tests;

/// The hint in the empty field.
pub const HINT: &str = "What happened here? (e.g. breath runs out, pitch flat)";

/// The barrier's color: the studio dialogs' (black at 50 %).
pub const BARRIER: Color = theme::DIALOG_BARRIER;

/// The card's widest (`BoxConstraints(maxWidth: 460)`), and its inset from
/// the window's edges (`insetPadding`).
const CARD_MAX_WIDTH: f32 = 460.0;
const INSET: f32 = 24.0;

/// The field's text size and line height (`DESIGN.md`'s 12 px field text,
/// an 18 px line).
const FIELD_SIZE: f32 = theme::FIELD_TEXT_SIZE;
const FIELD_LINE_HEIGHT: f32 = 1.5;

/// How the field grows: from 3 lines to 6, then it scrolls.
const MIN_LINES: f32 = 3.0;
const MAX_LINES: f32 = 6.0;

/// A line of the field: SkParagraph's, a whole pixel tall (20 for 14 px
/// at 1.4), as `RenderEditable` held a field to `minLines` and `maxLines`
/// of its `preferredLineHeight`.
fn field_line() -> f32 {
    text::line_box(font::REGULAR, FIELD_SIZE, FIELD_LINE_HEIGHT.into()).height
}

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
    /// Cancel, or Escape.
    Cancel,
    /// A tap on the barrier: cancels, unless the text was changed.
    Dismiss,
    /// Cmd+Return (Ctrl+Return off macOS): Add / Save, when it is live.
    Submit,
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

/// The header for a note kept at `position_ms` (to `end_ms` for a range),
/// its times `m:ss` as the notes panel and the lyrics read them (it wrote
/// "00:12" where the panel beside it read "0:12").
pub fn note_editor_title(position_ms: i64, end_ms: Option<i64>) -> String {
    match end_ms {
        Some(end) => format!(
            "Note across {} → {}",
            lyric_time(position_ms),
            lyric_time(end)
        ),
        None => format!("Note at {}", lyric_time(position_ms)),
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
    /// The text it opened on.
    initial: String,
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
    /// is saved as a point, and was announced as "Note across 0:12 →
    /// 0:12".
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
        let initial = existing.map_or("", |n| n.content.as_str());
        Self {
            title: note_editor_title(position_ms, end_ms),
            editing: existing.is_some(),
            initial: initial.to_string(),
            content: text_editor::Content::with_text(initial),
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

    /// Whether the text differs from what the editor opened on.
    pub fn changed(&self) -> bool {
        dart_string::trim(&self.content.text()) != dart_string::trim(&self.initial)
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
            Message::Dismiss if self.changed() => Step::Stay,
            Message::Dismiss => Step::Closed(None),
            Message::Submit if !self.can_save() => Step::Stay,
            Message::Submit => self.update(Message::Save),
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
            .min_height(field_line() * MIN_LINES)
            .max_height(field_line() * MAX_LINES + theme::FIELD_PADDING.y())
            .caret_color(Studio::ACCENT)
            .style(field_style);

        let mut actions = row![].spacing(8).align_y(Vertical::Center);
        if self.editing {
            actions = actions.push(delete_button());
        }
        actions = actions.push(confirm_dialog::buttons(
            "Cancel",
            Message::Cancel,
            if self.editing { "Save" } else { "Add" },
            self.can_save().then_some(Message::Save),
            DialogButton::Primary,
        ));

        let card = container(
            column![
                confirm_dialog::title(self.title.as_str()),
                Space::new().height(14),
                field,
                Space::new().height(20),
                actions,
            ]
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .max_width(CARD_MAX_WIDTH)
        .padding(confirm_dialog::CARD_PADDING)
        .style(theme::dialog_card);
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

/// The editor's Delete (editing only), apart on the left: a secondary
/// dialog button whose label reads in `RECORD_TEXT` after a trash glyph.
fn delete_button<'a>() -> Element<'a, Message> {
    let label = row![
        icons::icon(Icon::Trash2, 13.0, Studio::RECORD_TEXT),
        studio_text("Delete")
            .size(theme::type_scale::BODY)
            .line_height(Pixels(theme::type_scale::LINE))
            .font(font::MEDIUM)
            .color(Studio::RECORD_TEXT),
    ]
    .spacing(6)
    .align_y(Vertical::Center);
    container(
        button(container(label).center_y(Length::Fill))
            .padding(Padding::from([0, 12]))
            .height(theme::height::TOOLBAR_BUTTON)
            .on_press(Message::Delete)
            .style(|_, status| {
                let (fill, text, outline) = dialog_button_colors(DialogButton::Secondary, status);
                button::Style {
                    background: fill.map(Background::Color),
                    text_color: text,
                    border: Border {
                        color: outline.unwrap_or(Color::TRANSPARENT),
                        width: 1.0,
                        radius: theme::radius::WELL.into(),
                    },
                    shadow: Shadow::default(),
                    snap: true,
                }
            }),
    )
    .width(Length::Shrink)
    .into()
}

/// The field (`DESIGN.md`, Text fields): its own `WELL` fill, radius 6, an
/// inset `BORDER_STRONG` outline (`ACCENT` at 60 % while focused),
/// `TEXT_PRIMARY` text, a `TEXT_FAINT` hint, the selection colour.
fn field_style(_: &iced::Theme, status: text_editor::Status) -> text_editor::Style {
    let focused = matches!(status, text_editor::Status::Focused { .. });
    text_editor::Style {
        background: Background::Color(Studio::WELL),
        border: Border {
            color: if focused {
                theme::FIELD_FOCUS
            } else {
                Studio::BORDER_STRONG
            },
            width: 1.0,
            radius: theme::radius::WELL.into(),
        },
        placeholder: Studio::TEXT_FAINT,
        value: Studio::TEXT_PRIMARY,
        selection: crate::ui::core::theme::SELECTION,
    }
}

/// `content` centred over a dismissible [`BARRIER`] covering everything,
/// both at `fade` (the card as one layer): a tap on the barrier dismisses
/// ([`Message::Dismiss`]), the card takes its own taps, Escape cancels,
/// Cmd+Return submits (before the field, which would break the line) and
/// no key reaches what lies beneath.
fn modal(content: Element<'_, Message>, fade: Fade) -> Element<'_, Message> {
    let barrier = barrier(BARRIER, fade.barrier, Some(Message::Dismiss));
    let layers = stack![
        barrier,
        center(opaque(fade.card(content))).align_x(Horizontal::Center)
    ];
    let submits = Wrap::<Message, ()>::new(
        layers,
        Box::new(|_, event, _, _, shell| {
            if let Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Enter),
                modifiers,
                ..
            }) = event
            {
                if modifiers.command() && !shell.is_event_captured() {
                    shell.publish(Message::Submit);
                    shell.capture_event();
                }
            }
        }),
    )
    .first();
    Wrap::<Message, ()>::new(
        // A dialog route's scope: Tab goes round its fields.
        focus_scope(submits),
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
