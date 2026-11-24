//! The single-line field: iced_widget 0.14.2's `text_input` (MIT), forked
//! for Flutter's macOS caret and right-click menu (see [the module
//! docs](super)). Its keyboard, mouse, clipboard and input-method handling,
//! its layout and its scrolling are iced's, cut down to what the app uses
//! (no icon, no secure mode, no paste hook, left-aligned).
//!
//! Besides the caret and the menu, it differs from iced's field where
//! Flutter's did: a press of any button outside it, not the primary one
//! alone, takes its focus away (`TextFieldTapRegion` unfocused the field on
//! every pointer down outside it), and the window losing focus closes its
//! menu as it hides its caret.
//!
//! The text scrolls as iced's does, not as Flutter's did: when it is wider
//! than the field, iced keeps the caret 5 px in from the right edge as it
//! moves, where Flutter's `EditableText` scrolled only as far as revealed
//! the caret, over 100 ms (`_showCaretOnScreen`).

mod cursor;
mod editor;

use iced::advanced::clipboard::{self, Clipboard};
use iced::advanced::graphics::text::cosmic_text::Affinity;
use iced::advanced::input_method::{self, InputMethod};
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::{self, click};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::text::paragraph::{self, Paragraph as _};
use iced::advanced::text::{self, Renderer as _, Text};
use iced::advanced::widget::operation::{self, Operation};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::{self, Widget};
use iced::advanced::{overlay, Shell};
use iced::keyboard::{self, key};
use iced::time::{Duration, Instant};
use iced::touch;
use iced::widget::text_input::{default, StyleFn};
pub use iced::widget::text_input::{Status, Style, Value};
use iced::window;
use iced::{
    alignment, Color, Element, Event, Font, Length, Padding, Pixels, Point, Rectangle, Renderer,
    Size, Theme, Vector,
};
use unicode_segmentation::UnicodeSegmentation;

pub use cursor::Cursor;
use editor::Editor;

use super::toolbar::{self, Item};
use super::{caret, DEFAULT_CARET};

type Paragraph = <Renderer as text::Renderer>::Paragraph;

/// A single-line field showing `value`, with `placeholder` while empty.
pub fn text_input<'a, Message: Clone>(placeholder: &str, value: &str) -> TextInput<'a, Message> {
    TextInput::new(placeholder, value)
}

/// A field that displays some text and lets the user edit it, one line.
pub struct TextInput<'a, Message> {
    id: Option<widget::Id>,
    placeholder: String,
    value: Value,
    font: Option<Font>,
    width: Length,
    padding: Padding,
    size: Option<Pixels>,
    line_height: text::LineHeight,
    on_input: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_submit: Option<Message>,
    caret_color: Color,
    style: StyleFn<'a, Theme>,
    last_status: Option<Status>,
}

/// iced's default padding.
pub const DEFAULT_PADDING: Padding = Padding::new(5.0);

impl<'a, Message: Clone> TextInput<'a, Message> {
    /// A field showing `value`, with `placeholder` while empty.
    pub fn new(placeholder: &str, value: &str) -> Self {
        TextInput {
            id: None,
            placeholder: String::from(placeholder),
            value: Value::new(value),
            font: None,
            width: Length::Fill,
            padding: DEFAULT_PADDING,
            size: None,
            line_height: text::LineHeight::default(),
            on_input: None,
            on_submit: None,
            caret_color: DEFAULT_CARET,
            style: Box::new(default),
            last_status: None,
        }
    }

    /// Its id, to focus it and find it.
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Its edits, as the whole new text; without, it cannot be edited.
    pub fn on_input(mut self, on_input: impl Fn(String) -> Message + 'a) -> Self {
        self.on_input = Some(Box::new(on_input));
        self
    }

    /// The message Return sends.
    pub fn on_submit(mut self, message: Message) -> Self {
        self.on_submit = Some(message);
        self
    }

    /// Its font (the renderer's default otherwise).
    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Its width.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Its padding: where its text sits inside it.
    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    /// Its text size.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Its line height.
    pub fn line_height(mut self, line_height: impl Into<text::LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    /// Its caret's color (`TextField.cursorColor`).
    pub fn caret_color(mut self, color: Color) -> Self {
        self.caret_color = color;
        self
    }

    /// Its style.
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self {
        self.style = Box::new(style);
        self
    }

    fn input_method<'b>(&self, state: &'b State, layout: Layout<'_>) -> InputMethod<&'b str> {
        let Some(Focus {
            is_window_focused: true,
            ..
        }) = &state.is_focused
        else {
            return InputMethod::Disabled;
        };

        let text_bounds = layout.children().next().unwrap().bounds();

        let caret_index = match state.cursor.state(&self.value) {
            cursor::State::Index(position) => position,
            cursor::State::Selection { start, end } => start.min(end),
        };

        let text = state.value.raw();
        let (cursor_x, scroll_offset) =
            measure_cursor_and_scroll_offset(text, text_bounds, caret_index);

        let x = (text_bounds.x + cursor_x).floor() - scroll_offset;

        InputMethod::Enabled {
            cursor: Rectangle::new(
                Point::new(x, text_bounds.y),
                Size::new(1.0, text_bounds.height),
            ),
            purpose: input_method::Purpose::Normal,
            preedit: state.preedit.as_ref().map(input_method::Preedit::as_ref),
        }
    }

    fn draw_field(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        layout: Layout<'_>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let value = &self.value;
        let is_disabled = self.on_input.is_none();

        let bounds = layout.bounds();
        let text_bounds = layout.children().next().unwrap().bounds();

        let style = (self.style)(theme, self.last_status.unwrap_or(Status::Disabled));

        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: style.border,
                ..renderer::Quad::default()
            },
            style.background,
        );

        let text = value.to_string();

        // The caret to draw over the text, the scroll offset, and whether
        // a selection is drawn.
        let (caret, offset, selection) = if let Some(focus) = state
            .is_focused
            .as_ref()
            .filter(|focus| focus.is_window_focused)
        {
            match state.cursor.state(value) {
                cursor::State::Index(position) => {
                    let (x, offset) =
                        measure_cursor_and_scroll_offset(state.value.raw(), text_bounds, position);

                    let is_caret_visible = !is_disabled
                        && ((focus.now - focus.updated_at).as_millis()
                            / CURSOR_BLINK_INTERVAL_MILLIS)
                            .is_multiple_of(2);

                    let caret = is_caret_visible.then(|| {
                        let max_x = state
                            .value
                            .raw()
                            .min_width()
                            .max(text_bounds.width - caret::MARGIN);
                        caret::rect(x, 0.0, text_bounds.height, max_x)
                    });

                    (caret, offset, None)
                }
                cursor::State::Selection { start, end } => {
                    let left = start.min(end);
                    let right = end.max(start);

                    let (left_position, left_offset) =
                        measure_cursor_and_scroll_offset(state.value.raw(), text_bounds, left);

                    let (right_position, right_offset) =
                        measure_cursor_and_scroll_offset(state.value.raw(), text_bounds, right);

                    let width = right_position - left_position;

                    (
                        None,
                        if end == right {
                            right_offset
                        } else {
                            left_offset
                        },
                        Some(Rectangle {
                            x: text_bounds.x + left_position,
                            y: text_bounds.y,
                            width,
                            height: text_bounds.height,
                        }),
                    )
                }
            }
        } else {
            (None, 0.0, None)
        };

        let draw = |renderer: &mut Renderer, viewport| {
            let paragraph = if text.is_empty()
                && state
                    .preedit
                    .as_ref()
                    .map(|preedit| preedit.content.is_empty())
                    .unwrap_or(true)
            {
                state.placeholder.raw()
            } else {
                state.value.raw()
            };

            if let Some(selection) = selection {
                renderer.with_translation(Vector::new(-offset, 0.0), |renderer| {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: selection,
                            ..renderer::Quad::default()
                        },
                        style.selection,
                    );
                });
            }

            renderer.fill_paragraph(
                paragraph,
                text_bounds.anchor(
                    paragraph.min_bounds(),
                    alignment::Horizontal::Left,
                    alignment::Vertical::Center,
                ) + Vector::new(-offset, 0.0),
                if text.is_empty() {
                    style.placeholder
                } else {
                    style.value
                },
                viewport,
            );
        };

        if selection.is_some() {
            renderer.with_layer(text_bounds, |renderer| draw(renderer, *viewport));
        } else {
            draw(renderer, text_bounds);
        }

        // Over the text (`paintCursorAboveText`). `RenderEditable` clips
        // what it paints to its box only while the text overflows it
        // (`_hasVisualOverflow`): otherwise the caret's pixel above and
        // below the line shows.
        if let Some(rect) = caret {
            let overflows = state.value.raw().min_width() + caret::MARGIN > text_bounds.width;
            caret::draw(
                renderer,
                rect + Vector::new(text_bounds.x - offset, text_bounds.y),
                self.caret_color,
                if overflows { text_bounds } else { *viewport },
            );
        }
    }

    /// The text position under `x` (in the field's coordinates) and
    /// whether it lies after the glyph left of it (Flutter's upstream
    /// affinity): `getPositionForOffset`.
    fn position_at(&self, state: &State, text_bounds: Rectangle, x: f32) -> (usize, bool) {
        let offset = offset(text_bounds, &self.value, state);
        let content = self.value.to_string();
        let Some(hit) = state
            .value
            .raw()
            .buffer()
            .hit(x - text_bounds.x + offset, text_bounds.height / 2.0)
        else {
            return (0, false);
        };
        let byte = hit.index.min(content.len());
        let index = Value::new(&content[..byte]).len();
        (index, hit.affinity == Affinity::Before && index > 0)
    }

    /// A right-click came up at `position`, pressed there:
    /// `TextSelectionGestureDetectorBuilder.onSecondaryTap` on macOS.
    fn secondary_tap(
        &mut self,
        state: &mut State,
        layout: Layout<'_>,
        position: Point,
        clipboard: &mut dyn Clipboard,
    ) {
        let text_bounds = layout.children().next().unwrap().bounds();
        let (index, upstream) = self.position_at(state, text_bounds, position.x);
        let on_selection = match state.cursor.state(&self.value) {
            cursor::State::Index(caret) => caret == index,
            cursor::State::Selection { start, end } => {
                start.min(end) <= index && index <= start.max(end)
            }
        };
        if !on_selection || !state.is_focused() {
            let (start, end) = word_at(&self.value, index, upstream);
            state.cursor.select_range(start, end);
            focus(state);
        }
        state.toolbar = Some(toolbar::State::new(
            position,
            toolbar::clipboard_has_text(clipboard),
        ));
    }
}

/// `RenderEditable.getWordAtOffset` on macOS: past the end of the text, the
/// caret at the end; otherwise the word (UAX #29, as ICU's word boundaries)
/// holding the character at `index`, or before it when `upstream`.
pub(super) fn word_at(value: &Value, index: usize, upstream: bool) -> (usize, usize) {
    if index >= value.len() {
        return (value.len(), value.len());
    }
    let character = if upstream { index - 1 } else { index };
    let content = value.to_string();
    let byte = content
        .grapheme_indices(true)
        .nth(character)
        .map_or(content.len(), |(byte, _)| byte);
    let (start, word) = content
        .split_word_bound_indices()
        .take_while(|(start, _)| *start <= byte)
        .last()
        .unwrap_or((0, ""));
    let graphemes = |s: &str| s.graphemes(true).count();
    let first = graphemes(&content[..start]);
    (first, first + graphemes(word))
}

/// Focuses the field's `state` (a click, a right-click, a focus
/// operation), the caret shown afresh.
fn focus(state: &mut State) {
    let now = Instant::now();
    state.is_focused = Some(Focus {
        updated_at: now,
        now,
        is_window_focused: true,
    });
}

/// The field's menu, for the overlay: its text, cursor and edits.
struct Menu<'b, Message> {
    value: &'b mut Value,
    state: &'b mut State,
    on_input: Option<&'b dyn Fn(String) -> Message>,
}

impl<Message> toolbar::Field<Message> for Menu<'_, Message> {
    fn menu(&self) -> Option<&toolbar::State> {
        self.state.toolbar.as_ref()
    }

    fn menu_mut(&mut self) -> Option<&mut toolbar::State> {
        self.state.toolbar.as_mut()
    }

    fn items(&self) -> Vec<Item> {
        let Some(menu) = &self.state.toolbar else {
            return Vec::new();
        };
        toolbar::items(
            self.on_input.is_some(),
            self.state.cursor.selection(self.value).is_some(),
            menu.can_paste(),
        )
    }

    fn perform(
        &mut self,
        item: Item,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let state = &mut *self.state;
        state.toolbar = None;
        match item {
            Item::Copy => {
                if let Some((start, end)) = state.cursor.selection(self.value) {
                    clipboard.write(
                        clipboard::Kind::Standard,
                        self.value.select(start, end).to_string(),
                    );
                }
            }
            Item::Cut => {
                let Some(on_input) = self.on_input else {
                    return;
                };
                if let Some((start, end)) = state.cursor.selection(self.value) {
                    clipboard.write(
                        clipboard::Kind::Standard,
                        self.value.select(start, end).to_string(),
                    );
                    let mut editor = Editor::new(self.value, &mut state.cursor);
                    editor.delete();
                    shell.publish(on_input(editor.contents()));
                }
            }
            Item::Paste => {
                let Some(on_input) = self.on_input else {
                    return;
                };
                let Some(content) = clipboard.read(clipboard::Kind::Standard) else {
                    return;
                };
                let content: String = content.chars().filter(|c| !c.is_control()).collect();
                let mut editor = Editor::new(self.value, &mut state.cursor);
                editor.paste(Value::new(&content));
                shell.publish(on_input(editor.contents()));
            }
        }
        if let Some(focus) = &mut state.is_focused {
            focus.updated_at = Instant::now();
        }
        shell.request_redraw();
    }

    fn close(&mut self) {
        self.state.toolbar = None;
    }
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, Renderer> for TextInput<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();

        // Stop pasting if input becomes disabled
        if self.on_input.is_none() {
            state.is_pasting = None;
            // A disabled field takes no pointer: its menu goes.
            state.toolbar = None;
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: Length::Shrink,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let value = &self.value;

        let font = self.font.unwrap_or_else(|| renderer.default_font());
        let text_size = self.size.unwrap_or_else(|| renderer.default_size());
        let padding = self.padding.fit(Size::ZERO, limits.max());
        let height = self.line_height.to_absolute(text_size);

        let limits = limits.width(self.width).shrink(padding);
        let text_bounds = limits.resolve(self.width, height, Size::ZERO);

        let placeholder_text = Text {
            font,
            line_height: self.line_height,
            content: self.placeholder.as_str(),
            bounds: Size::new(f32::INFINITY, text_bounds.height),
            size: text_size,
            align_x: text::Alignment::Default,
            align_y: alignment::Vertical::Center,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::default(),
        };

        let _ = state.placeholder.update(placeholder_text);

        let _ = state.value.update(Text {
            content: &value.to_string(),
            ..placeholder_text
        });

        let text = layout::Node::new(text_bounds).move_to(Point::new(padding.left, padding.top));

        layout::Node::with_children(text_bounds.expand(padding), vec![text])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<State>();

        operation.text_input(self.id.as_ref(), layout.bounds(), state);
        operation.focusable(self.id.as_ref(), layout.bounds(), state);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let update_cache = |state, value| {
            replace_paragraph(
                renderer,
                state,
                layout,
                value,
                self.font,
                self.size,
                self.line_height,
            );
        };

        match &event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                let state = state(tree);
                let cursor_before = state.cursor;

                let click_position = cursor.position_over(layout.bounds());

                // The press closes the menu (`onTapDown` on macOS), in the
                // field or out of it.
                state.toolbar = None;

                if click_position.is_some() {
                    focus(state);
                } else {
                    state.is_focused = None;
                }

                if let Some(cursor_position) = click_position {
                    let text_layout = layout.children().next().unwrap();

                    let target = cursor_position.x - text_layout.bounds().x;

                    let click =
                        mouse::Click::new(cursor_position, mouse::Button::Left, state.last_click);

                    match click.kind() {
                        click::Kind::Single => {
                            let position = if target > 0.0 {
                                find_cursor_position(
                                    text_layout.bounds(),
                                    &self.value,
                                    state,
                                    target,
                                )
                            } else {
                                None
                            }
                            .unwrap_or(0);

                            if state.keyboard_modifiers.shift() {
                                state
                                    .cursor
                                    .select_range(state.cursor.start(&self.value), position);
                            } else {
                                state.cursor.move_to(position);
                            }

                            state.is_dragging = Some(Drag::Select);
                        }
                        click::Kind::Double => {
                            let position = find_cursor_position(
                                text_layout.bounds(),
                                &self.value,
                                state,
                                target,
                            )
                            .unwrap_or(0);

                            state.cursor.select_range(
                                self.value.previous_start_of_word(position),
                                self.value.next_end_of_word(position),
                            );

                            state.is_dragging = Some(Drag::SelectWords { anchor: position });
                        }
                        click::Kind::Triple => {
                            state.cursor.select_all(&self.value);
                            state.is_dragging = None;
                        }
                    }

                    state.last_click = Some(click);

                    if cursor_before != state.cursor {
                        shell.request_redraw();
                    }

                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let state = state(tree);
                if cursor.is_over(layout.bounds()) {
                    if *button == mouse::Button::Right && self.on_input.is_some() {
                        // `onSecondaryTapDown`: the place the menu opens.
                        state.secondary_press = cursor.position();
                        shell.capture_event();
                    }
                } else if state.is_focused.is_some() {
                    // Any pointer down outside the field unfocuses it
                    // (`TextFieldTapRegion`'s tap outside).
                    state.is_focused = None;
                    state.toolbar = None;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right)) => {
                let state = state(tree);
                if let (Some(pressed), Some(released)) =
                    (state.secondary_press.take(), cursor.position())
                {
                    if pressed.distance(released) <= toolbar::TAP_SLOP {
                        self.secondary_tap(state, layout, pressed, clipboard);
                        shell.capture_event();
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
            | Event::Touch(touch::Event::FingerLost { .. }) => {
                state(tree).is_dragging = None;
            }
            Event::Mouse(mouse::Event::CursorMoved { position })
            | Event::Touch(touch::Event::FingerMoved { position, .. }) => {
                let state = state(tree);

                if let Some(is_dragging) = &state.is_dragging {
                    let text_layout = layout.children().next().unwrap();

                    let target = position.x - text_layout.bounds().x;

                    let value = self.value.clone();

                    let position =
                        find_cursor_position(text_layout.bounds(), &value, state, target)
                            .unwrap_or(0);

                    let selection_before = state.cursor.selection(&value);

                    match is_dragging {
                        Drag::Select => {
                            state
                                .cursor
                                .select_range(state.cursor.start(&value), position);
                        }
                        Drag::SelectWords { anchor } => {
                            if position < *anchor {
                                state.cursor.select_range(
                                    self.value.previous_start_of_word(position),
                                    self.value.next_end_of_word(*anchor),
                                );
                            } else {
                                state.cursor.select_range(
                                    self.value.previous_start_of_word(*anchor),
                                    self.value.next_end_of_word(position),
                                );
                            }
                        }
                    }

                    if let Some(focus) = &mut state.is_focused {
                        focus.updated_at = Instant::now();
                    }

                    if selection_before != state.cursor.selection(&value) {
                        shell.request_redraw();
                    }

                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                text,
                modified_key,
                physical_key,
                ..
            }) => {
                let state = state(tree);

                if let Some(focus) = &mut state.is_focused {
                    let modifiers = state.keyboard_modifiers;

                    match key.to_latin(*physical_key) {
                        Some('c') if state.keyboard_modifiers.command() => {
                            if let Some((start, end)) = state.cursor.selection(&self.value) {
                                clipboard.write(
                                    clipboard::Kind::Standard,
                                    self.value.select(start, end).to_string(),
                                );
                            }

                            shell.capture_event();
                            return;
                        }
                        Some('x') if state.keyboard_modifiers.command() => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if let Some((start, end)) = state.cursor.selection(&self.value) {
                                clipboard.write(
                                    clipboard::Kind::Standard,
                                    self.value.select(start, end).to_string(),
                                );
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            update_cache(state, &self.value);
                            return;
                        }
                        Some('v')
                            if state.keyboard_modifiers.command()
                                && !state.keyboard_modifiers.alt() =>
                        {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            let content = match state.is_pasting.take() {
                                Some(content) => content,
                                None => {
                                    let content: String = clipboard
                                        .read(clipboard::Kind::Standard)
                                        .unwrap_or_default()
                                        .chars()
                                        .filter(|c| !c.is_control())
                                        .collect();

                                    Value::new(&content)
                                }
                            };

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.paste(content.clone());

                            shell.publish((on_input)(editor.contents()));
                            shell.capture_event();

                            state.is_pasting = Some(content);
                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            update_cache(state, &self.value);
                            return;
                        }
                        Some('a') if state.keyboard_modifiers.command() => {
                            let cursor_before = state.cursor;

                            state.cursor.select_all(&self.value);

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                            return;
                        }
                        _ => {}
                    }

                    if let Some(text) = text {
                        let Some(on_input) = &self.on_input else {
                            return;
                        };

                        state.is_pasting = None;

                        if let Some(c) = text.chars().next().filter(|c| !c.is_control()) {
                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);

                            editor.insert(c);

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            update_cache(state, &self.value);
                            return;
                        }
                    }

                    #[cfg(target_os = "macos")]
                    let macos_shortcut = convert_macos_shortcut(key, modifiers);

                    #[cfg(target_os = "macos")]
                    let modified_key = macos_shortcut.as_ref().unwrap_or(modified_key);

                    match modified_key.as_ref() {
                        keyboard::Key::Named(key::Named::Enter) => {
                            if let Some(on_submit) = self.on_submit.clone() {
                                shell.publish(on_submit);
                                shell.capture_event();
                            }
                        }
                        keyboard::Key::Named(key::Named::Backspace) => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if state.cursor.selection(&self.value).is_none() {
                                if modifiers.macos_command() {
                                    state
                                        .cursor
                                        .select_range(state.cursor.start(&self.value), 0);
                                } else if modifiers.jump() {
                                    state.cursor.select_left_by_words(&self.value);
                                }
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.backspace();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            update_cache(state, &self.value);
                        }
                        keyboard::Key::Named(key::Named::Delete) => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if state.cursor.selection(&self.value).is_none() {
                                if modifiers.macos_command() {
                                    state.cursor.select_range(
                                        state.cursor.start(&self.value),
                                        self.value.len(),
                                    );
                                } else if modifiers.jump() {
                                    state.cursor.select_right_by_words(&self.value);
                                }
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            update_cache(state, &self.value);
                        }
                        keyboard::Key::Named(key::Named::Home) => {
                            let cursor_before = state.cursor;

                            if modifiers.shift() {
                                state
                                    .cursor
                                    .select_range(state.cursor.start(&self.value), 0);
                            } else {
                                state.cursor.move_to(0);
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::End) => {
                            let cursor_before = state.cursor;

                            if modifiers.shift() {
                                state.cursor.select_range(
                                    state.cursor.start(&self.value),
                                    self.value.len(),
                                );
                            } else {
                                state.cursor.move_to(self.value.len());
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::ArrowLeft) => {
                            let cursor_before = state.cursor;

                            if modifiers.macos_command() {
                                if modifiers.shift() {
                                    state
                                        .cursor
                                        .select_range(state.cursor.start(&self.value), 0);
                                } else {
                                    state.cursor.move_to(0);
                                }
                            } else if modifiers.jump() {
                                if modifiers.shift() {
                                    state.cursor.select_left_by_words(&self.value);
                                } else {
                                    state.cursor.move_left_by_words(&self.value);
                                }
                            } else if modifiers.shift() {
                                state.cursor.select_left(&self.value);
                            } else {
                                state.cursor.move_left(&self.value);
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::ArrowRight) => {
                            let cursor_before = state.cursor;

                            if modifiers.macos_command() {
                                if modifiers.shift() {
                                    state.cursor.select_range(
                                        state.cursor.start(&self.value),
                                        self.value.len(),
                                    );
                                } else {
                                    state.cursor.move_to(self.value.len());
                                }
                            } else if modifiers.jump() {
                                if modifiers.shift() {
                                    state.cursor.select_right_by_words(&self.value);
                                } else {
                                    state.cursor.move_right_by_words(&self.value);
                                }
                            } else if modifiers.shift() {
                                state.cursor.select_right(&self.value);
                            } else {
                                state.cursor.move_right(&self.value);
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::Escape) => {
                            state.is_focused = None;
                            state.is_dragging = None;
                            state.is_pasting = None;
                            state.toolbar = None;

                            state.keyboard_modifiers = keyboard::Modifiers::default();

                            shell.capture_event();
                        }
                        _ => {}
                    }
                }
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => {
                let state = state(tree);

                if state.is_focused.is_some()
                    && matches!(key.as_ref(), keyboard::Key::Character("v"))
                {
                    state.is_pasting = None;

                    shell.capture_event();
                }

                state.is_pasting = None;
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                let state = state(tree);

                state.keyboard_modifiers = *modifiers;
            }
            Event::InputMethod(event) => match event {
                input_method::Event::Opened | input_method::Event::Closed => {
                    let state = state(tree);

                    state.preedit = matches!(event, input_method::Event::Opened)
                        .then(input_method::Preedit::new);

                    shell.request_redraw();
                }
                input_method::Event::Preedit(content, selection) => {
                    let state = state(tree);

                    if state.is_focused.is_some() {
                        state.preedit = Some(input_method::Preedit {
                            content: content.to_owned(),
                            selection: selection.clone(),
                            text_size: self.size,
                        });

                        shell.request_redraw();
                    }
                }
                input_method::Event::Commit(text) => {
                    let state = state(tree);

                    if let Some(focus) = &mut state.is_focused {
                        let Some(on_input) = &self.on_input else {
                            return;
                        };

                        let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                        editor.paste(Value::new(text));

                        focus.updated_at = Instant::now();
                        state.is_pasting = None;
                        state.toolbar = None;

                        let message = (on_input)(editor.contents());
                        shell.publish(message);
                        shell.capture_event();

                        update_cache(state, &self.value);
                    }
                }
            },
            Event::Window(window::Event::Unfocused) => {
                let state = state(tree);

                if let Some(focus) = &mut state.is_focused {
                    focus.is_window_focused = false;
                }
                state.toolbar = None;
            }
            Event::Window(window::Event::Focused) => {
                let state = state(tree);

                if let Some(focus) = &mut state.is_focused {
                    focus.is_window_focused = true;
                    focus.updated_at = Instant::now();

                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::Rescaled(scale_factor)) => {
                caret::set_scale_factor(*scale_factor);
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                let state = state(tree);

                if let Some(focus) = state
                    .is_focused
                    .as_mut()
                    .filter(|focus| focus.is_window_focused)
                {
                    if matches!(state.cursor.state(&self.value), cursor::State::Index(_)) {
                        focus.now = *now;

                        let millis_until_redraw = CURSOR_BLINK_INTERVAL_MILLIS
                            - (*now - focus.updated_at).as_millis() % CURSOR_BLINK_INTERVAL_MILLIS;

                        shell.request_redraw_at(
                            *now + Duration::from_millis(millis_until_redraw as u64),
                        );
                    }

                    shell.request_input_method(&self.input_method(state, layout));
                }
            }
            _ => {}
        }

        let state = state(tree);
        let is_disabled = self.on_input.is_none();

        let status = if is_disabled {
            Status::Disabled
        } else if state.is_focused() {
            Status::Focused {
                is_hovered: cursor.is_over(layout.bounds()),
            }
        } else if cursor.is_over(layout.bounds()) {
            Status::Hovered
        } else {
            Status::Active
        };

        if let Event::Window(window::Event::RedrawRequested(_now)) = event {
            self.last_status = Some(status);
        } else if self
            .last_status
            .is_some_and(|last_status| status != last_status)
        {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.draw_field(tree, renderer, theme, layout, viewport);
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            if self.on_input.is_none() {
                mouse::Interaction::Idle
            } else {
                mouse::Interaction::Text
            }
        } else {
            mouse::Interaction::default()
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        _layout: Layout<'b>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let state = tree.state.downcast_mut::<State>();
        let position = state.toolbar.as_mut()?.position(translation);
        Some(toolbar::overlay(
            Box::new(Menu {
                value: &mut self.value,
                state,
                on_input: self.on_input.as_deref(),
            }),
            position,
        ))
    }
}

impl<'a, Message: Clone + 'a> From<TextInput<'a, Message>> for Element<'a, Message> {
    fn from(text_input: TextInput<'a, Message>) -> Element<'a, Message> {
        Element::new(text_input)
    }
}

/// The state of a [`TextInput`].
#[derive(Debug, Default, Clone)]
pub struct State {
    value: paragraph::Plain<Paragraph>,
    placeholder: paragraph::Plain<Paragraph>,
    is_focused: Option<Focus>,
    is_dragging: Option<Drag>,
    is_pasting: Option<Value>,
    preedit: Option<input_method::Preedit>,
    last_click: Option<mouse::Click>,
    cursor: Cursor,
    keyboard_modifiers: keyboard::Modifiers,
    /// A right-button press in the field, until it comes up.
    secondary_press: Option<Point>,
    /// The open context menu.
    toolbar: Option<toolbar::State>,
}

fn state(tree: &mut Tree) -> &mut State {
    tree.state.downcast_mut::<State>()
}

#[derive(Debug, Clone)]
struct Focus {
    updated_at: Instant,
    now: Instant,
    is_window_focused: bool,
}

#[derive(Debug, Clone)]
enum Drag {
    Select,
    SelectWords { anchor: usize },
}

impl State {
    /// Whether the field is focused.
    pub fn is_focused(&self) -> bool {
        self.is_focused.is_some()
    }

    /// The field's cursor.
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Whether its context menu is open.
    pub fn has_menu(&self) -> bool {
        self.toolbar.is_some()
    }

    fn focus(&mut self) {
        focus(self);
        self.move_cursor_to_end();
    }

    fn unfocus(&mut self) {
        self.is_focused = None;
        self.toolbar = None;
    }

    fn move_cursor_to_front(&mut self) {
        self.cursor.move_to(0);
    }

    fn move_cursor_to_end(&mut self) {
        self.cursor.move_to(usize::MAX);
    }

    fn move_cursor_to(&mut self, position: usize) {
        self.cursor.move_to(position);
    }

    fn select_all(&mut self) {
        self.cursor.select_range(0, usize::MAX);
    }

    fn select_range(&mut self, start: usize, end: usize) {
        self.cursor.select_range(start, end);
    }
}

impl operation::Focusable for State {
    fn is_focused(&self) -> bool {
        State::is_focused(self)
    }

    fn focus(&mut self) {
        State::focus(self);
    }

    fn unfocus(&mut self) {
        State::unfocus(self);
    }
}

impl operation::TextInput for State {
    fn text(&self) -> &str {
        if self.value.content().is_empty() {
            self.placeholder.content()
        } else {
            self.value.content()
        }
    }

    fn move_cursor_to_front(&mut self) {
        State::move_cursor_to_front(self);
    }

    fn move_cursor_to_end(&mut self) {
        State::move_cursor_to_end(self);
    }

    fn move_cursor_to(&mut self, position: usize) {
        State::move_cursor_to(self, position);
    }

    fn select_all(&mut self) {
        State::select_all(self);
    }

    fn select_range(&mut self, start: usize, end: usize) {
        State::select_range(self, start, end);
    }
}

fn offset(text_bounds: Rectangle, value: &Value, state: &State) -> f32 {
    if state.is_focused() {
        let cursor = state.cursor();

        let focus_position = match cursor.state(value) {
            cursor::State::Index(i) => i,
            cursor::State::Selection { end, .. } => end,
        };

        let (_, offset) =
            measure_cursor_and_scroll_offset(state.value.raw(), text_bounds, focus_position);

        offset
    } else {
        0.0
    }
}

fn measure_cursor_and_scroll_offset(
    paragraph: &Paragraph,
    text_bounds: Rectangle,
    cursor_index: usize,
) -> (f32, f32) {
    let grapheme_position = paragraph
        .grapheme_position(0, cursor_index)
        .unwrap_or(Point::ORIGIN);

    let offset = ((grapheme_position.x + 5.0) - text_bounds.width).max(0.0);

    (grapheme_position.x, offset)
}

fn find_cursor_position(
    text_bounds: Rectangle,
    value: &Value,
    state: &State,
    x: f32,
) -> Option<usize> {
    let offset = offset(text_bounds, value, state);
    let value = value.to_string();

    let char_offset = state
        .value
        .raw()
        .hit_test(Point::new(x + offset, text_bounds.height / 2.0))
        .map(text::Hit::cursor)?;

    Some(
        value[..char_offset.min(value.len())]
            .graphemes(true)
            .count(),
    )
}

fn replace_paragraph(
    renderer: &Renderer,
    state: &mut State,
    layout: Layout<'_>,
    value: &Value,
    font: Option<Font>,
    text_size: Option<Pixels>,
    line_height: text::LineHeight,
) {
    let font = font.unwrap_or_else(|| renderer.default_font());
    let text_size = text_size.unwrap_or_else(|| renderer.default_size());

    let text_bounds = layout.children().next().unwrap().bounds();

    state.value = paragraph::Plain::new(Text {
        font,
        line_height,
        content: value.to_string(),
        bounds: Size::new(f32::INFINITY, text_bounds.height),
        size: text_size,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Center,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::default(),
    });
}

/// `_kCursorBlinkHalfPeriod`, iced's `CURSOR_BLINK_INTERVAL_MILLIS`.
const CURSOR_BLINK_INTERVAL_MILLIS: u128 = 500;

/// iced's `text_editor::convert_macos_shortcut` (crate-private there):
/// macOS's Emacs keys.
#[cfg(target_os = "macos")]
fn convert_macos_shortcut(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
) -> Option<keyboard::Key> {
    if modifiers != keyboard::Modifiers::CTRL {
        return None;
    }

    let key = match key.as_ref() {
        keyboard::Key::Character("b") => key::Named::ArrowLeft,
        keyboard::Key::Character("f") => key::Named::ArrowRight,
        keyboard::Key::Character("a") => key::Named::Home,
        keyboard::Key::Character("e") => key::Named::End,
        keyboard::Key::Character("h") => key::Named::Backspace,
        keyboard::Key::Character("d") => key::Named::Delete,
        _ => return None,
    };

    Some(keyboard::Key::Named(key))
}
