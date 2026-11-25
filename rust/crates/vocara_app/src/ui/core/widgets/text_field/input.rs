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
//! Its text scrolls as Flutter's did, not as iced's (which kept the caret
//! 5 px in from the right edge as it moved, and the text at its start while
//! unfocused): see [`scroll`].

mod cursor;
mod editor;
pub mod scroll;

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
use scroll::{Geometry, Reveal, Scroll};

use super::super::pan_zoom::{self, Scroll as Wheel, TOUCH_SLOP};
use super::super::scroll_fling::{self, MacOsVelocityTracker};
use super::show_on_screen::{self, Request};
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

        let cursor_x = glyph_x(state, caret_index);

        let x = (text_bounds.x + cursor_x).floor() - state.scroll.pixels();

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

        // The text's scroll, which stays where it was put while the field
        // is not focused (see `scroll`).
        let offset = state.scroll.pixels();

        // The caret to draw over the text, and whether a selection is
        // drawn.
        let (caret, selection) = if let Some(focus) = state
            .is_focused
            .as_ref()
            .filter(|focus| focus.is_window_focused)
        {
            match state.cursor.state(value) {
                cursor::State::Index(position) => {
                    let x = glyph_x(state, position);

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

                    (caret, None)
                }
                cursor::State::Selection { start, end } => {
                    let left_position = glyph_x(state, start.min(end));
                    let right_position = glyph_x(state, end.max(start));

                    (
                        None,
                        Some(Rectangle {
                            x: text_bounds.x + left_position,
                            y: text_bounds.y,
                            width: right_position - left_position,
                            height: text_bounds.height,
                        }),
                    )
                }
            }
        } else {
            (None, None)
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
        // what it paints to its box only while the text overflows it or is
        // scrolled (`_hasVisualOverflow`): otherwise the caret's pixel above
        // and below the line shows.
        if let Some(rect) = caret {
            let overflows =
                state.value.raw().min_width() + caret::MARGIN > text_bounds.width || offset != 0.0;
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
        let offset = state.scroll.pixels();
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
            let (before, was_focused) = (state.cursor, state.is_focused());
            state.cursor.select_range(start, end);
            focus(state);
            if before != state.cursor || !was_focused {
                state.scroll.schedule(Reveal::Animate);
            }
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
    /// The field's layout, for the scroll.
    geometry: Geometry,
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
                    // `copySelection(toolbar)`'s `bringIntoView`.
                    let x = extent_x(state, self.value);
                    bring_into_view(state, &self.geometry, x);
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
                    // The edit's reveal, then a `bringIntoView` after the
                    // frame, which stops it.
                    state.scroll.schedule(Reveal::Jump);
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
                state.scroll.schedule(Reveal::Jump);
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
        tree::State::new(State {
            disabled: self.on_input.is_none(),
            ..State::default()
        })
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        state.disabled = self.on_input.is_none();

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
        // Its own state ([`State`]) and its text box, for the keyboard's
        // traversal (`focus_traversal`).
        operation.custom(self.id.as_ref(), text_bounds(layout), state);
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

        if !matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            // A reveal whose frame has been drawn ran at that frame's end,
            // before this.
            let state = state(tree);
            // What the views around took is gone; what they did not, stale.
            state.requests.clear();
            flush_reveal(state, layout, &self.value);
        }

        match &event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                let state = state(tree);
                let cursor_before = state.cursor;
                let was_focused = state.is_focused();

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

                    // A click that moved the selection or brought the focus
                    // reveals the caret (`userUpdateTextEditingValue`,
                    // `_handleFocusChanged`).
                    if cursor_before != state.cursor || !was_focused {
                        state.scroll.schedule(Reveal::Animate);
                    }

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
                    let cursor_before = state.cursor;

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

                    // A drag shows the selection's moving end at once.
                    if cursor_before != state.cursor {
                        let geometry = geometry(state, layout);
                        let x = extent_x(state, &value);
                        bring_into_view(state, &geometry, x);
                        shell.request_redraw();
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

                            // Nothing to cut without a selection
                            // (`CopySelectionTextIntent.cut` does nothing
                            // with a collapsed one).
                            let Some((start, end)) = state.cursor.selection(&self.value) else {
                                shell.capture_event();
                                return;
                            };
                            clipboard.write(
                                clipboard::Kind::Standard,
                                self.value.select(start, end).to_string(),
                            );

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            state.scroll.schedule(Reveal::Animate);
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

                            let before = (self.value.to_string(), state.cursor);
                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.paste(content.clone());

                            shell.publish((on_input)(editor.contents()));
                            shell.capture_event();

                            state.is_pasting = Some(content);
                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            if before != (self.value.to_string(), state.cursor) {
                                state.scroll.schedule(Reveal::Animate);
                            }
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

                            // `_updateSelection`'s `bringIntoView`.
                            let geometry = geometry(state, layout);
                            let x = extent_x(state, &self.value);
                            bring_into_view(state, &geometry, x);

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
                            state.scroll.schedule(Reveal::Animate);
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

                            let before = (self.value.to_string(), state.cursor);
                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.backspace();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            if before != (self.value.to_string(), state.cursor) {
                                state.scroll.schedule(Reveal::Animate);
                            }
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

                            let before = (self.value.to_string(), state.cursor);
                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            state.toolbar = None;
                            if before != (self.value.to_string(), state.cursor) {
                                state.scroll.schedule(Reveal::Animate);
                            }
                            update_cache(state, &self.value);
                        }
                        keyboard::Key::Named(key::Named::Home)
                            if SCROLL_KEYS_ONLY_SCROLL && !modifiers.shift() =>
                        {
                            // `scrollToBeginningOfDocument:`: the text
                            // scrolls to its start; the caret stays.
                            let geometry = geometry(state, layout);
                            bring_into_view(state, &geometry, glyph_x(state, 0));
                            shell.request_redraw();
                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::End)
                            if SCROLL_KEYS_ONLY_SCROLL && !modifiers.shift() =>
                        {
                            // `scrollToEndOfDocument:`.
                            let geometry = geometry(state, layout);
                            let end = glyph_x(state, self.value.len());
                            bring_into_view(state, &geometry, end);
                            shell.request_redraw();
                            shell.capture_event();
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

                            let geometry = geometry(state, layout);
                            let x = extent_x(state, &self.value);
                            bring_into_view(state, &geometry, x);

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

                            let geometry = geometry(state, layout);
                            let x = extent_x(state, &self.value);
                            bring_into_view(state, &geometry, x);

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

                            let geometry = geometry(state, layout);
                            let x = extent_x(state, &self.value);
                            bring_into_view(state, &geometry, x);

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

                            let geometry = geometry(state, layout);
                            let x = extent_x(state, &self.value);
                            bring_into_view(state, &geometry, x);

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
                        state.scroll.schedule(Reveal::Animate);

                        let message = (on_input)(editor.contents());
                        shell.publish(message);
                        shell.capture_event();

                        update_cache(state, &self.value);
                    }
                }
            },
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let over = cursor.is_over(layout.bounds());
                let wheel = pan_zoom::current();
                let state = state(tree);
                let geometry = geometry(state, layout);
                match wheel {
                    Wheel::Signal => {
                        let dx =
                            horizontal_scroll(*delta, state.keyboard_modifiers).filter(|_| over);
                        if dx.is_some_and(|dx| state.scroll.scroll_by(&geometry, dx)) {
                            shell.capture_event();
                            shell.request_redraw();
                        }
                    }
                    // Its `Scrollable`'s horizontal drag, there while the
                    // text can scroll (`setCanDrag`): it holds the offset,
                    // and enters the gesture's arena (see `input::scroll`).
                    Wheel::Start { gesture, time }
                        if over && (geometry.max() > 0.0 || state.scroll.pixels() != 0.0) =>
                    {
                        state.scroll.hold();
                        let mut tracker = MacOsVelocityTracker::default();
                        tracker.add(time, 0.0, pan_zoom::event_time());
                        state.pan = Some(Pan {
                            gesture,
                            pan: Vector::ZERO,
                            entry: pan_zoom::enter(gesture, time),
                            won: None,
                            tracker,
                        });
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    Wheel::Update {
                        gesture,
                        delta,
                        time,
                    } => {
                        if let Some(pan) = state.pan.as_mut().filter(|p| p.gesture == gesture) {
                            pan.pan += delta;
                            pan.tracker.add(time, pan.pan.x, pan_zoom::event_time());
                            // Alone in the arena it won at the start, and
                            // this move is its drag's; with others, neither
                            // drag moves until one is past its slop (the
                            // field's handed the move first), and the move
                            // that takes it past starts the drag.
                            let mut moves = true;
                            if pan.won.is_none() {
                                pan.won = pan.entry.standing();
                                if pan.won.is_none() && pan.pan.x.abs() > TOUCH_SLOP as f32 {
                                    pan.won = Some(pan.entry.accept());
                                    moves = false;
                                }
                            }
                            match pan.won {
                                None => shell.capture_event(),
                                Some(true) if moves => {
                                    // `applyUserOffset(dx)`.
                                    state.scroll.scroll_by(&geometry, -delta.x);
                                    shell.capture_event();
                                    shell.request_redraw();
                                }
                                Some(true) => shell.capture_event(),
                                Some(false) => {}
                            }
                        }
                    }
                    Wheel::End { gesture } => {
                        if let Some(pan) = state.pan.as_mut().filter(|p| p.gesture == gesture) {
                            if pan.won.is_none() {
                                // Alone, it won at the start though nothing
                                // moved since.
                                pan.won = pan.entry.standing();
                            }
                            match pan.won {
                                Some(true) => {
                                    let estimate = pan.tracker.estimate(pan_zoom::event_time());
                                    let velocity = scroll_fling::drag_end(estimate);
                                    state.scroll.fling(&geometry, velocity);
                                    shell.capture_event();
                                    shell.request_redraw();
                                }
                                // Neither won: nothing moved.
                                None => shell.capture_event(),
                                Some(false) => {}
                            }
                        }
                    }
                    // A finger on the trackpad during macOS's momentum, the
                    // pointer over the field: its `Scrollable`'s
                    // `pointerScroll(0)` (see `input::scroll`). A view
                    // around it stops too.
                    Wheel::InertiaCancel => {
                        if over && state.scroll.stop(&geometry) {
                            shell.request_redraw();
                        }
                    }
                    // The momentum after a swipe no other drag won reached
                    // nothing (in a scroll view, the view keeps it from
                    // the field).
                    Wheel::Nothing { gesture } => {
                        if state
                            .pan
                            .as_ref()
                            .is_some_and(|p| p.gesture == gesture && p.won != Some(false))
                        {
                            shell.capture_event();
                        }
                    }
                    Wheel::Start { .. } => {}
                }
            }
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
                    state.scroll.schedule(Reveal::Animate);

                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::Rescaled(scale_factor)) => {
                caret::set_scale_factor(*scale_factor);
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                let state = state(tree);

                state.requests.clear();
                flush_reveal(state, layout, &self.value);
                let geometry = geometry(state, layout);
                let x = extent_x(state, &self.value);
                if state.scroll.frame(*now, &geometry, x) {
                    shell.request_redraw();
                }

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
        layout: Layout<'b>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let state = tree.state.downcast_mut::<State>();
        let position = state.toolbar.as_mut()?.position(translation);
        let geometry = geometry(state, layout);
        Some(toolbar::overlay(
            Box::new(Menu {
                value: &mut self.value,
                state,
                on_input: self.on_input.as_deref(),
                geometry,
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
    /// The text's horizontal scroll.
    scroll: Scroll,
    /// What it asks the scroll view around it to show
    /// (`show_on_screen`).
    pub(super) requests: Vec<Request>,
    /// It cannot be edited (`enabled: false`): it takes no focus from the
    /// keyboard's traversal.
    disabled: bool,
    /// The trackpad gesture that started on it last.
    pan: Option<Pan>,
}

fn state(tree: &mut Tree) -> &mut State {
    tree.state.downcast_mut::<State>()
}

/// A trackpad gesture that started on the field: its pan so far, its
/// horizontal drag's place in the gesture's arena and whether it won
/// there (`None` while undecided), and the pan's velocity tracker.
#[derive(Debug, Clone)]
struct Pan {
    gesture: u64,
    pan: Vector,
    entry: pan_zoom::Entry,
    won: Option<bool>,
    tracker: MacOsVelocityTracker,
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

    /// How far its text is scrolled left, as last drawn.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll.pixels()
    }

    /// Focused by the app (`autofocus`, a focus request), which
    /// `EditableText` took as its own (`_flagInternalFocus`): the caret at
    /// the end, as a field with no selection yet got it, and revealed.
    fn focus(&mut self) {
        focus(self);
        self.move_cursor_to_end();
        self.scroll.schedule(Reveal::Animate);
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

impl crate::ui::core::widgets::focus_traversal::Traversable for State {
    fn can_focus(&self) -> bool {
        !self.disabled
    }

    fn is_focused(&self) -> bool {
        State::is_focused(self)
    }

    /// Focused from elsewhere: a single-line field selects all its text on
    /// macOS (`selectAllOnFocus`), and its end is revealed.
    fn focus_by_traversal(&mut self) {
        focus(self);
        self.select_all();
        self.scroll.schedule(Reveal::Animate);
    }

    fn unfocus_by_traversal(&mut self) {
        State::unfocus(self);
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

/// The glyph edge of the text position `index`, in the line's
/// coordinates.
fn glyph_x(state: &State, index: usize) -> f32 {
    state
        .value
        .raw()
        .grapheme_position(0, index)
        .map_or(0.0, |position| position.x)
}

/// The text's box.
fn text_bounds(layout: Layout<'_>) -> Rectangle {
    layout
        .children()
        .next()
        .map_or_else(|| layout.bounds(), |text| text.bounds())
}

/// What the field's scroll needs of its layout.
fn geometry(state: &State, layout: Layout<'_>) -> Geometry {
    Geometry {
        bounds: text_bounds(layout),
        text_width: state.value.raw().min_width(),
    }
}

/// `bringIntoView`: the text scrolls at once to show the caret at the
/// glyph edge `x`, and asks the view around it to show the bare caret at
/// once (`renderEditable.showOnScreen(rect: targetOffset.rect)`).
fn bring_into_view(state: &mut State, geometry: &Geometry, x: f32) {
    state.scroll.bring_into_view(geometry, x);
    let rect = caret_box(geometry.bounds, true);
    ask(state, rect, false);
}

/// A reveal whose frame has been drawn runs (`_scheduleShowCaretOnScreen`
/// after its frame): the text scrolls to the caret, and the view around it
/// is asked to show the caret (or the selection's line) with the field's
/// scroll padding over the animation; after a post-frame `bringIntoView`
/// (the menu's Cut and Paste), the bare caret at once as well.
fn flush_reveal(state: &mut State, layout: Layout<'_>, value: &Value) {
    let geometry = geometry(state, layout);
    let x = extent_x(state, value);
    let Some(reveal) = state.scroll.flush(&geometry, x) else {
        return;
    };
    let collapsed = state.cursor.selection(value).is_none();
    let line = caret_box(geometry.bounds, collapsed);
    ask(state, line.expand(show_on_screen::CARET_PADDING), true);
    if reveal == Reveal::Jump {
        ask(state, caret_box(geometry.bounds, true), false);
    }
}

/// The caret's rectangle down the text box `bounds` (a pixel above and
/// below the line), or the selection's line box; across, the box.
fn caret_box(bounds: Rectangle, caret: bool) -> Rectangle {
    if caret {
        bounds.expand(Padding {
            top: caret::OVERHANG,
            bottom: caret::OVERHANG,
            ..Padding::ZERO
        })
    } else {
        bounds
    }
}

/// Leaves `rect` for the scroll view around the field to show.
fn ask(state: &mut State, rect: Rectangle, animate: bool) {
    state.requests.push(Request { rect, animate });
    show_on_screen::ask();
}

/// The glyph edge of the selection's moving end (`selection.extent`).
fn extent_x(state: &State, value: &Value) -> f32 {
    glyph_x(state, state.cursor.end(value))
}

fn find_cursor_position(
    text_bounds: Rectangle,
    value: &Value,
    state: &State,
    x: f32,
) -> Option<usize> {
    let offset = state.scroll.pixels();
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

/// Whether Home and End only scroll the text, the caret left where it is:
/// macOS bound them to `scrollToBeginningOfDocument:` and
/// `scrollToEndOfDocument:`, which `EditableText` took as
/// `ScrollToDocumentBoundaryIntent` (`bringIntoView` of the text's start or
/// end). Elsewhere Flutter moved the caret, as iced's field does.
const SCROLL_KEYS_ONLY_SCROLL: bool = cfg!(target_os = "macos");

/// What a scroll signal's `delta` scrolls the text by, if it is the
/// field's (`_pointerSignalEventDelta`): Flutter's scroll delta
/// (`pan_zoom::scroll_delta`: a line 40 px, a precise device's point a
/// pixel, macOS's swap under Shift undone) across, or down with Shift
/// (`pointerAxisModifiers`, a signal being a mouse's).
fn horizontal_scroll(delta: mouse::ScrollDelta, modifiers: keyboard::Modifiers) -> Option<f32> {
    let shift = modifiers.shift();
    let delta = pan_zoom::scroll_delta(delta, shift);
    let across = if shift { delta.y } else { delta.x };
    (across != 0.0).then_some(across)
}

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
