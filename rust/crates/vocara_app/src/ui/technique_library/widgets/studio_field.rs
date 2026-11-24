//! Port of `ui/technique_library/widgets/studio_field.dart`: a labeled,
//! studio-styled text field used across the technique editor and its
//! dialogs — a small caption over a dark inset box wrapping a borderless
//! field — and [`parse_comma_list`], how the aliases and tags fields read.
//!
//! Flutter's one `TextField` was single-line by default and multi-line with
//! `minLines`/`maxLines`; iced has a single-line `text_input` and a
//! multi-line `text_editor`, so the field is [`studio_field`] over a `&str`
//! (single line, Return submits) or [`studio_prose_field`] over a
//! `text_editor::Content` (prose: it grows from `min_lines` to `max_lines`,
//! then scrolls). `autofocus` is the owner's `iced::widget::operation::focus`
//! task on the field's [`id`](StudioField::id) when it appears. iced draws
//! the caret in the text colour (the coral `cursorColor` has no
//! counterpart).

use iced::widget::{column, container, text_editor, text_input, Space};
use iced::{widget, Background, Border, Color, Element, Length, Padding, Theme};
use vocara_model::util::dart_string;

use crate::ui::core::theme::{self, alpha, font, Studio, Zinc};
use crate::ui::core::widgets::studio_text::studio_text;

/// The field's text size and line height (`fontSize: 14, height: 1.45`).
const TEXT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 1.45;

enum Input<'a, M> {
    Line {
        value: &'a str,
        on_input: Option<Box<dyn Fn(String) -> M + 'a>>,
        on_submit: Option<M>,
    },
    Prose {
        content: &'a text_editor::Content,
        on_action: Option<Box<dyn Fn(text_editor::Action) -> M + 'a>>,
        min_lines: u16,
        max_lines: u16,
    },
}

/// `StudioField(label: ..., ...)`; configure with the builder methods, then
/// convert into an [`Element`]. With no `on_input` / `on_action` the field
/// cannot be edited.
pub struct StudioField<'a, M> {
    label: String,
    hint: String,
    input: Input<'a, M>,
    id: Option<widget::Id>,
    enabled: bool,
    /// Every color at this opacity (inside a fading dialog).
    opacity: f32,
}

/// A single-line field showing `value` (Flutter's default `minLines: 1,
/// maxLines: 1`).
pub fn studio_field<'a, M>(label: impl Into<String>, value: &'a str) -> StudioField<'a, M> {
    StudioField {
        label: label.into(),
        hint: String::new(),
        input: Input::Line {
            value,
            on_input: None,
            on_submit: None,
        },
        id: None,
        enabled: true,
        opacity: 1.0,
    }
}

/// A prose field over `content`, `min_lines` tall at least and `max_lines`
/// at most (it scrolls past that).
pub fn studio_prose_field<'a, M>(
    label: impl Into<String>,
    content: &'a text_editor::Content,
    min_lines: u16,
    max_lines: u16,
) -> StudioField<'a, M> {
    StudioField {
        label: label.into(),
        hint: String::new(),
        input: Input::Prose {
            content,
            on_action: None,
            min_lines,
            max_lines,
        },
        id: None,
        enabled: true,
        opacity: 1.0,
    }
}

impl<'a, M> StudioField<'a, M> {
    /// The faint placeholder shown while the field is empty.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// A single-line field's edits (the whole new text).
    pub fn on_input(mut self, on_input: impl Fn(String) -> M + 'a) -> Self {
        if let Input::Line { on_input: slot, .. } = &mut self.input {
            *slot = Some(Box::new(on_input));
        }
        self
    }

    /// Return in a single-line field (a dialog's name, say).
    pub fn on_submit(mut self, on_submit: M) -> Self {
        if let Input::Line {
            on_submit: slot, ..
        } = &mut self.input
        {
            *slot = Some(on_submit);
        }
        self
    }

    /// A prose field's edits, to `perform` on its content.
    pub fn on_action(mut self, on_action: impl Fn(text_editor::Action) -> M + 'a) -> Self {
        if let Input::Prose {
            on_action: slot, ..
        } = &mut self.input
        {
            *slot = Some(Box::new(on_action));
        }
        self
    }

    /// The field's id, to focus it (`autofocus`).
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// False freezes the field (e.g. while the technique save is committing —
    /// edits typed during the await would be thrown away when the save lands
    /// and the screen leaves edit mode). Rendered like a disabled
    /// `StudioButton`: still visible, text dimmed to the faint tone, no edit
    /// cursor.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Draws every color at `opacity` (inside a fading dialog: iced has no
    /// subtree opacity).
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }
}

/// The borderless multi-line field: [`theme::studio_input`]'s look for a
/// `text_editor`.
fn studio_editor(_: &Theme, status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: Studio::TEXT_FAINT,
        value: if matches!(status, text_editor::Status::Disabled) {
            Studio::TEXT_FAINT
        } else {
            Studio::TEXT_PRIMARY
        },
        selection: alpha(Zinc::PRIMARY, 0.40),
    }
}

/// An editor's look at `opacity`.
fn faded_editor(style: text_editor::Style, opacity: f32) -> text_editor::Style {
    text_editor::Style {
        background: match style.background {
            Background::Color(c) => Background::Color(theme::faded(c, opacity)),
            other => other,
        },
        border: Border {
            color: theme::faded(style.border.color, opacity),
            ..style.border
        },
        placeholder: theme::faded(style.placeholder, opacity),
        value: theme::faded(style.value, opacity),
        selection: theme::faded(style.selection, opacity),
    }
}

impl<'a, M: Clone + 'a> From<StudioField<'a, M>> for Element<'a, M> {
    fn from(f: StudioField<'a, M>) -> Self {
        let enabled = f.enabled;
        let opacity = f.opacity;
        let field: Element<'a, M> = match f.input {
            Input::Line {
                value,
                on_input,
                on_submit,
            } => {
                let mut input = text_input(&f.hint, value)
                    .size(TEXT_SIZE)
                    .line_height(LINE_HEIGHT)
                    .padding(0)
                    .style(move |t, s| theme::faded_input(theme::studio_input(t, s), opacity));
                if let Some(id) = f.id {
                    input = input.id(id);
                }
                if enabled {
                    if let Some(on_input) = on_input {
                        input = input.on_input(on_input);
                    }
                    if let Some(on_submit) = on_submit {
                        input = input.on_submit(on_submit);
                    }
                }
                input.into()
            }
            Input::Prose {
                content,
                on_action,
                min_lines,
                max_lines,
            } => {
                let line = TEXT_SIZE * LINE_HEIGHT;
                let mut editor = text_editor(content)
                    .placeholder(f.hint)
                    .size(TEXT_SIZE)
                    .line_height(LINE_HEIGHT)
                    .padding(0)
                    .min_height(line * f32::from(min_lines.max(1)))
                    .max_height(line * f32::from(max_lines.max(min_lines).max(1)))
                    .style(move |t, s| faded_editor(studio_editor(t, s), opacity));
                if let Some(id) = f.id {
                    editor = editor.id(id);
                }
                if enabled {
                    if let Some(on_action) = on_action {
                        editor = editor.on_action(on_action);
                    }
                }
                editor.into()
            }
        };
        column![
            studio_text(f.label.to_uppercase())
                .size(10.5)
                .font(font::BOLD)
                .letter_spacing(0.6)
                .color(theme::faded(Studio::TEXT_FAINT, opacity)),
            Space::new().height(6),
            container(field)
                .width(Length::Fill)
                .padding(Padding::from([10, 12]))
                .style(move |t| theme::faded_container(theme::inset(8.0)(t), opacity)),
        ]
        .into()
    }
}

/// Parses a comma-separated input ("Twang, Edge") into a trimmed,
/// de-duplicated list, dropping blanks. Used for the aliases and tags fields.
pub fn parse_comma_list(raw: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for part in raw.split(',') {
        let t = dart_string::trim(part);
        if t.is_empty() || !seen.insert(t.to_lowercase()) {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Input(String),
        Submit,
        Edit,
        Move,
    }

    fn edit_or_move(action: text_editor::Action) -> Msg {
        if action.is_edit() {
            Msg::Edit
        } else {
            Msg::Move
        }
    }

    #[test]
    fn a_comma_list_is_trimmed_deduplicated_and_without_blanks() {
        assert_eq!(parse_comma_list("Twang, Edge"), vec!["Twang", "Edge"]);
        assert_eq!(
            parse_comma_list(" EVT ,, evt, Voice quality ,  ,Resonance,RESONANCE"),
            vec!["EVT", "Voice quality", "Resonance"]
        );
        assert!(parse_comma_list("").is_empty());
        assert!(parse_comma_list(" , ,").is_empty());
    }

    #[test]
    fn the_caption_is_upper_cased_over_the_field() {
        let mut ui = iced_test::simulator(Element::from(
            studio_field("Aliases (comma separated)", "Edge")
                .hint("e.g. Edge")
                .on_input(Msg::Input),
        ));
        assert!(ui.find("ALIASES (COMMA SEPARATED)").is_ok());
        assert!(ui.find("Edge").is_ok(), "the field shows its value");
    }

    #[test]
    fn typing_edits_and_return_submits_a_single_line_field() {
        let id = widget::Id::unique();
        let mut ui = iced_test::simulator(Element::from(
            studio_field("Name", "Twan")
                .id(id.clone())
                .on_input(Msg::Input)
                .on_submit(Msg::Submit),
        ));
        ui.click(id).expect("the field");
        let _ = ui.tap_key(keyboard::Key::Named(keyboard::key::Named::End));
        let _ = ui.typewrite("g");
        let _ = ui.tap_key(keyboard::Key::Named(keyboard::key::Named::Enter));
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Input("Twang".into()), Msg::Submit]
        );
    }

    #[test]
    fn a_disabled_field_takes_no_edits() {
        let id = widget::Id::unique();
        let mut ui = iced_test::simulator(Element::from(
            studio_field("Name", "Twang")
                .id(id.clone())
                .on_input(Msg::Input)
                .on_submit(Msg::Submit)
                .enabled(false),
        ));
        let _ = ui.click(id);
        let _ = ui.typewrite("x");
        let _ = ui.tap_key(keyboard::Key::Named(keyboard::key::Named::Enter));
        assert!(ui.into_messages().next().is_none());
    }

    #[test]
    fn a_prose_field_edits_its_content() {
        let content = text_editor::Content::with_text("What the technique is");
        let id = widget::Id::unique();
        let mut ui = iced_test::simulator(Element::from(
            studio_prose_field("Description", &content, 3, 8)
                .id(id.clone())
                .on_action(edit_or_move),
        ));
        assert!(ui.find("DESCRIPTION").is_ok());
        ui.click(id).expect("the editor");
        let _ = ui.typewrite("x");
        assert!(ui.into_messages().any(|m| m == Msg::Edit));

        // Frozen, it takes none.
        let id = widget::Id::unique();
        let mut ui = iced_test::simulator(Element::from(
            studio_prose_field("Description", &content, 3, 8)
                .id(id.clone())
                .on_action(edit_or_move)
                .enabled(false),
        ));
        let _ = ui.click(id);
        let _ = ui.typewrite("x");
        assert!(ui.into_messages().next().is_none());
    }
}
