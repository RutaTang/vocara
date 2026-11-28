//! Port of `ui/technique_library/widgets/studio_field.dart`: a labelled
//! text field used across the technique editor and its dialogs — a
//! sentence-case caption over the field — and [`parse_comma_list`], how the
//! aliases and tags fields read.
//!
//! Drawn as `DESIGN.md`'s text fields: the caption 11 px `TEXT_SECONDARY`,
//! 6 px over the field; the field its own well (`Studio::WELL`, radius 6,
//! an inset `BORDER_STRONG` outline, the accent at 60 % while focused). A
//! single-line field is 28 px with 12 px text, and a prose field writes at
//! the same 12 px on 16 px lines (`DESIGN.md`'s text fields), one line of
//! it as tall as a single-line field; the read view sets the text at
//! 13 px.
//!
//! Flutter's one `TextField` was single-line by default and multi-line with
//! `minLines`/`maxLines`; the app's text fields (`widgets::text_field`) are
//! a single-line one and a multi-line one, so the field is
//! [`studio_field`] over a `&str` (single line, Return submits) or
//! [`studio_prose_field`] over a `text_field::Content` (prose: it grows
//! from `min_lines` to `max_lines`, then scrolls). Both draw Flutter's
//! macOS caret in the coral `cursorColor` and open its right-click menu.
//! `autofocus` is the owner's `iced::widget::operation::focus` task on the
//! field's [`id`](StudioField::id) when it appears.

use iced::widget::{column, Space};
use iced::{widget, Background, Border, Element, Length, Padding, Pixels, Theme};
use vocara_model::util::dart_string;

use crate::ui::core::theme::{self, font, radius, type_scale, Studio};
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::text_field::editor::{self as text_editor, text_editor};
use crate::ui::core::widgets::text_field::text_input;

/// The read view's prose: its text size and line height.
pub const PROSE_SIZE: f32 = type_scale::TITLE;
pub const PROSE_LINE: f32 = 20.0;

/// A prose field's text size and line height: a text field's 12 px on
/// 16 px lines (`DESIGN.md`), as the single-line fields beside it.
pub const PROSE_FIELD_SIZE: f32 = theme::FIELD_TEXT_SIZE;
pub const PROSE_FIELD_LINE: f32 = type_scale::LINE;

/// A prose field's padding inside its well: a text field's, 6 above and
/// below a line (one line makes a 28 px field, as a single-line one), 10
/// at the sides.
pub const PROSE_PADDING: Padding = theme::FIELD_PADDING;

/// The space between the caption and the field.
pub const CAPTION_GAP: f32 = 6.0;

/// A line of a prose field: SkParagraph's, a whole pixel tall, as
/// `RenderEditable` held a field to `minLines` and `maxLines` of its
/// `preferredLineHeight`.
fn field_line() -> f32 {
    crate::ui::core::text::line_box(
        font::REGULAR,
        PROSE_FIELD_SIZE,
        Pixels(PROSE_FIELD_LINE).into(),
    )
    .height
}

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
}

/// The multi-line field drawing its own well: [`theme::field`]'s look for a
/// `text_editor` (its outline the accent at 60 % while focused).
fn studio_editor(_: &Theme, status: text_editor::Status) -> text_editor::Style {
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
            radius: radius::WELL.into(),
        },
        placeholder: Studio::TEXT_FAINT,
        value: if matches!(status, text_editor::Status::Disabled) {
            Studio::TEXT_FAINT
        } else {
            Studio::TEXT_PRIMARY
        },
        selection: theme::SELECTION,
    }
}

impl<'a, M: Clone + 'a> From<StudioField<'a, M>> for Element<'a, M> {
    fn from(f: StudioField<'a, M>) -> Self {
        let enabled = f.enabled;
        let field: Element<'a, M> = match f.input {
            Input::Line {
                value,
                on_input,
                on_submit,
            } => {
                let mut input = text_input(&f.hint, value)
                    .size(theme::FIELD_TEXT_SIZE)
                    .line_height(Pixels(type_scale::LINE))
                    .padding(theme::FIELD_PADDING)
                    .caret_color(Studio::ACCENT)
                    .style(theme::field);
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
                let line = field_line();
                // iced's editor holds its text's height (not its own) to
                // `min_height`..`max_height` but lays its text out in
                // `max_height` less its padding: the padding goes on the
                // maximum so the text may fill `max_lines`.
                // In Inter, whose lines `field_line` counts.
                let mut editor = text_editor(content)
                    .placeholder(f.hint)
                    .font(font::REGULAR)
                    .size(PROSE_FIELD_SIZE)
                    .line_height(Pixels(PROSE_FIELD_LINE))
                    .padding(PROSE_PADDING)
                    .min_height(line * f32::from(min_lines.max(1)))
                    .max_height(
                        line * f32::from(max_lines.max(min_lines).max(1)) + PROSE_PADDING.y(),
                    )
                    .caret_color(Studio::ACCENT)
                    .style(studio_editor);
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
        // The caption: sentence case, 11 px `TEXT_SECONDARY` (`DESIGN.md`'s
        // field labels), over the field, which draws its own well.
        column![
            studio_text(f.label)
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .color(Studio::TEXT_SECONDARY),
            Space::new().height(CAPTION_GAP),
            field,
        ]
        .width(Length::Fill)
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
    fn the_caption_reads_in_sentence_case_over_the_field() {
        let mut ui = iced_test::simulator(Element::from(
            studio_field("Aliases (comma separated)", "Edge")
                .hint("e.g. Edge")
                .on_input(Msg::Input),
        ));
        assert!(ui.find("Aliases (comma separated)").is_ok());
        assert!(ui.find("ALIASES (COMMA SEPARATED)").is_err());
        assert!(ui.find("Edge").is_ok(), "the field shows its value");
    }

    #[test]
    fn the_field_sits_under_its_caption_at_the_designs_height() {
        // The caption (11 px on a 16 px line) and 6 above the field, which
        // spans the width: 28 tall (a 16 px line in 6 above and below).
        let id = widget::Id::unique();
        let mut ui = iced_test::simulator(Element::from(
            iced::widget::container(
                studio_field("Name", "Twang")
                    .id(id.clone())
                    .on_input(Msg::Input),
            )
            .width(300),
        ));
        let field = ui.find(id).expect("the field").bounds();
        assert_eq!(field.x, 0.0, "{field:?}");
        assert_eq!(field.y, type_scale::LINE + CAPTION_GAP, "{field:?}");
        assert_eq!(field.width, 300.0, "{field:?}");
        assert!(
            (field.height - theme::FIELD_HEIGHT).abs() < 1e-3,
            "{field:?}"
        );

        // A prose field: 12 px text, as the single-line fields' (it was
        // 13 px), three 16 px lines and 12 at least, eight and 12 at most,
        // in Inter, whose lines the field counts.
        crate::ui::core::widgets::studio_text::testing::load_fonts();
        assert_eq!(PROSE_FIELD_SIZE, theme::FIELD_TEXT_SIZE);
        assert_eq!(PROSE_FIELD_SIZE, 12.0);
        let line = field_line();
        assert_eq!(line, type_scale::LINE);
        assert_eq!(line + PROSE_PADDING.y(), theme::FIELD_HEIGHT);
        let short = text_editor::Content::with_text("One line");
        let long = text_editor::Content::with_text(&"A line\n".repeat(20));
        for (content, lines) in [(&short, 3.0), (&long, 8.0)] {
            let id = widget::Id::unique();
            let mut ui = iced_test::simulator(Element::from(
                iced::widget::container(
                    studio_prose_field("Description", content, 3, 8)
                        .id(id.clone())
                        .on_action(edit_or_move),
                )
                .width(300),
            ));
            let field = ui.find(id).expect("the field").bounds();
            assert!(
                (field.height - (lines * line + PROSE_PADDING.y())).abs() < 1e-3,
                "{lines} lines: {field:?}"
            );
        }
    }

    // The edit form's prose captions were not drawn: in a scroll view a
    // prose field lays its text out for its most lines, past its box, and
    // the renderer drew the raw text after it through the clip it left.
    #[test]
    fn a_caption_under_a_prose_field_in_a_scroll_view_is_drawn() {
        use crate::ui::core::widgets::animated::testing::Frames;
        use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
        use iced::time::Instant;
        use iced::{Point, Size};

        let first = text_editor::Content::with_text("One line");
        let second = text_editor::Content::with_text("Another");
        let view = || -> Element<'_, Msg> {
            scrollable(
                iced::widget::column![
                    studio_prose_field("Summary", &first, 1, 2).on_action(edit_or_move),
                    studio_prose_field("Description", &second, 3, 8).on_action(edit_or_move),
                ]
                .width(300),
            )
            .height(400)
            .fading()
            .into()
        };
        let mut window = Frames::new(Size::new(300.0, 400.0));
        window.frame(view(), &[], Instant::now());
        // The second caption sits under the first field (16 + 6 + 28).
        let top = type_scale::LINE + CAPTION_GAP + theme::FIELD_HEIGHT;
        let ink = window.ink(
            Point::new(0.0, top),
            Point::new(80.0, top + type_scale::LINE),
        );
        assert!(ink > 0x60, "the caption is drawn: {ink:#x}");
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
        assert!(ui.find("Description").is_ok());
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
