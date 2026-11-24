//! The multi-line field: iced_widget 0.14.2's `text_editor` (MIT), forked
//! for Flutter's macOS caret and right-click menu and for `SelectableText`
//! (see [the module docs](super)). Its keyboard, mouse, clipboard and
//! input-method handling, its key bindings, its layout and its scrolling
//! are iced's, for plain text (no highlighter, no custom key bindings).
//!
//! As iced's, it publishes what the user does as [`Action`]s, which the
//! owner performs on its [`Content`]. Two things it changes on the content
//! itself, as the cursor moves iced's own field does not report: the word a
//! right-click selects, and the focus.
//!
//! Besides the caret and the menu, it differs from iced's editor where
//! Flutter's `TextField` did:
//! - its text wraps [`caret::MARGIN`] short of its width (`RenderEditable`
//!   kept that room for the caret);
//! - a press of any button outside it takes its focus away
//!   (`TextFieldTapRegion`), not the primary button's only.
//!
//! [`TextEditor::read_only`] makes it `SelectableText`: the text can be
//! selected (click, drag, double and triple click, the keyboard) and copied,
//! not changed, and it shows no caret (`showCursor: false`). With no
//! [`on_action`](TextEditor::on_action) it moves its own cursor, as
//! `SelectableText` kept its own controller: it publishes nothing.

use std::cell::RefCell;
use std::fmt;
use std::ops;
use std::sync::Arc;

use iced::advanced::clipboard::{self, Clipboard};
use iced::advanced::graphics::text::cosmic_text::Affinity;
use iced::advanced::input_method::{self, InputMethod};
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::text::editor::Editor as _;
use iced::advanced::text::highlighter::{self, PlainText};
use iced::advanced::text::{self, LineHeight, Renderer as _, Text, Wrapping};
use iced::advanced::widget::{self, operation, Widget};
use iced::advanced::{overlay, Shell};
use iced::alignment;
use iced::keyboard;
use iced::time::{Duration, Instant};
use iced::widget::text_editor::{default, Binding, KeyPress, StyleFn};
pub use iced::widget::text_editor::{Status, Style};
use iced::window;
use iced::{
    Color, Element, Event, Font, Length, Padding, Pixels, Point, Rectangle, Renderer, Size, Theme,
    Vector,
};
use unicode_segmentation::UnicodeSegmentation;

pub use iced::widget::text_editor::{
    Action, Cursor, Edit, Line, LineEnding, Motion, Position, Selection,
};

use super::toolbar::{self, Item};
use super::{caret, DEFAULT_CARET};

/// `RendererEditor` of the app's renderer.
type RendererEditor = <Renderer as text::Renderer>::Editor;

/// A multi-line field over `content`.
pub fn text_editor<Message>(content: &Content) -> TextEditor<'_, Message> {
    TextEditor::new(content)
}

/// A multi-line text field (or, read-only, selectable text).
pub struct TextEditor<'a, Message> {
    id: Option<widget::Id>,
    content: &'a Content,
    placeholder: Option<text::Fragment<'a>>,
    font: Option<Font>,
    text_size: Option<Pixels>,
    line_height: LineHeight,
    width: Length,
    height: Length,
    min_height: f32,
    max_height: f32,
    padding: Padding,
    wrapping: Wrapping,
    style: StyleFn<'a, Theme>,
    on_edit: Option<Box<dyn Fn(Action) -> Message + 'a>>,
    read_only: bool,
    caret_color: Color,
    last_status: Option<Status>,
}

impl<'a, Message> TextEditor<'a, Message> {
    /// A field over `content`.
    pub fn new(content: &'a Content) -> Self {
        Self {
            id: None,
            content,
            placeholder: None,
            font: None,
            text_size: None,
            line_height: LineHeight::default(),
            width: Length::Fill,
            height: Length::Shrink,
            min_height: 0.0,
            max_height: f32::INFINITY,
            padding: Padding::new(5.0),
            wrapping: Wrapping::default(),
            style: Box::new(default),
            on_edit: None,
            read_only: false,
            caret_color: DEFAULT_CARET,
            last_status: None,
        }
    }

    /// Its id, to focus it and find it.
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The text shown while it is empty.
    pub fn placeholder(mut self, placeholder: impl text::IntoFragment<'a>) -> Self {
        self.placeholder = Some(placeholder.into_fragment());
        self
    }

    /// Its width.
    pub fn width(mut self, width: impl Into<Pixels>) -> Self {
        self.width = Length::from(width.into());
        self
    }

    /// Its height.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// The least height of its text, when it shrinks to it.
    pub fn min_height(mut self, min_height: impl Into<Pixels>) -> Self {
        self.min_height = min_height.into().0;
        self
    }

    /// The most height of its text, when it shrinks to it (iced lays the
    /// text out in this less the padding).
    pub fn max_height(mut self, max_height: impl Into<Pixels>) -> Self {
        self.max_height = max_height.into().0;
        self
    }

    /// What the user does in it, to perform on its content; without, it
    /// takes no input at all, unless it is read-only.
    pub fn on_action(mut self, on_edit: impl Fn(Action) -> Message + 'a) -> Self {
        self.on_edit = Some(Box::new(on_edit));
        self
    }

    /// Its font (the renderer's default otherwise).
    pub fn font(mut self, font: impl Into<Font>) -> Self {
        self.font = Some(font.into());
        self
    }

    /// Its text size.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.text_size = Some(size.into());
        self
    }

    /// Its line height.
    pub fn line_height(mut self, line_height: impl Into<LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    /// Its padding: where its text sits inside it.
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// How its text wraps.
    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.wrapping = wrapping;
        self
    }

    /// Selectable but not editable, with no caret (`SelectableText`): it
    /// publishes the actions that move the cursor or select (or, with no
    /// `on_action`, performs them on its content itself), and copies.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
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

    /// Whether it takes no input: not editable, not selectable.
    fn is_disabled(&self) -> bool {
        self.on_edit.is_none() && !self.read_only
    }

    fn line_height_px(&self, renderer: &Renderer) -> f32 {
        self.line_height
            .to_absolute(self.text_size.unwrap_or_else(|| renderer.default_size()))
            .into()
    }

    fn input_method<'b>(
        &self,
        state: &'b State,
        renderer: &Renderer,
        layout: Layout<'_>,
    ) -> InputMethod<&'b str> {
        let Some(Focus {
            is_window_focused: true,
            ..
        }) = &state.focus
        else {
            return InputMethod::Disabled;
        };
        if self.read_only {
            return InputMethod::Disabled;
        }

        let bounds = layout.bounds();
        let internal = self.content.0.borrow_mut();

        let text_bounds = bounds.shrink(self.padding);
        let translation = text_bounds.position() - Point::ORIGIN;

        let cursor = match internal.editor.selection() {
            Selection::Caret(position) => position,
            Selection::Range(ranges) => ranges.first().cloned().unwrap_or_default().position(),
        };

        let position = cursor + translation;

        InputMethod::Enabled {
            cursor: Rectangle::new(position, Size::new(1.0, self.line_height_px(renderer))),
            purpose: input_method::Purpose::Normal,
            preedit: state.preedit.as_ref().map(input_method::Preedit::as_ref),
        }
    }

    /// A right-click came up at `position`, pressed there:
    /// `TextSelectionGestureDetectorBuilder.onSecondaryTap` on macOS.
    fn secondary_tap(
        &self,
        state: &mut State,
        bounds: Rectangle,
        position: Point,
        clipboard: &mut dyn Clipboard,
    ) {
        let text_position = position
            - (bounds.position() - Point::ORIGIN)
            - Vector::new(self.padding.left, self.padding.top);
        let mut internal = self.content.0.borrow_mut();
        let hit = internal
            .editor
            .buffer()
            .hit(text_position.x, text_position.y);
        if let Some(hit) = hit {
            let cursor = internal.editor.cursor();
            let at = (hit.line, hit.index);
            let caret = (cursor.position.line, cursor.position.column);
            // `_lastSecondaryTapWasOnSelection`: the position within the
            // selection, its ends included. A word or line selection (a
            // double or triple click) reports where it was clicked, not its
            // ends, so the selection as drawn counts too.
            let on_selection = match cursor.selection {
                None => at == caret,
                Some(base) => {
                    let base = (base.line, base.column);
                    base.min(caret) <= at && at <= base.max(caret)
                }
            } || match internal.editor.selection() {
                Selection::Range(ranges) => {
                    ranges.iter().any(|range| range.contains(text_position))
                }
                Selection::Caret(_) => false,
            };
            if !on_selection || state.focus.is_none() {
                let lines: Vec<String> = (0..internal.editor.line_count())
                    .map(|i| {
                        internal
                            .editor
                            .line(i)
                            .map(|l| l.text.into_owned())
                            .unwrap_or_default()
                    })
                    .collect();
                match word_at(
                    &lines,
                    hit.line,
                    hit.index,
                    hit.affinity == Affinity::Before,
                ) {
                    // Past the end of the text: the caret at the end, no
                    // selection left.
                    None => internal.editor.perform(Action::Move(Motion::DocumentEnd)),
                    Some((start, end)) => internal.editor.move_to(Cursor {
                        position: end,
                        selection: Some(start),
                    }),
                }
            }
        }
        drop(internal);
        if state.focus.is_none() {
            state.focus = Some(Focus::now());
        }
        state.toolbar = Some(toolbar::State::new(
            position,
            !self.read_only && toolbar::clipboard_has_text(clipboard),
        ));
    }
}

/// `RenderEditable.getWordAtOffset` on macOS over `lines`, for the text
/// position `index` (a byte) on `line`, `upstream` when it lies after the
/// glyph left of it: past the end of the text, `None` (the caret at the
/// end); otherwise the word (UAX #29, as ICU's word boundaries) holding the
/// character at the position (before it, when upstream), a line break
/// being one.
pub(super) fn word_at(
    lines: &[String],
    line: usize,
    index: usize,
    upstream: bool,
) -> Option<(Position, Position)> {
    let at = |line, column| Position { line, column };
    let last = lines.len().saturating_sub(1);
    let text = lines.get(line)?;
    let index = index.min(text.len());
    if line >= last && index >= text.len() {
        return None;
    }
    // The character the position stands for.
    let (line, character) = if upstream {
        match text[..index].grapheme_indices(true).next_back() {
            Some((byte, _)) => (line, Some(byte)),
            // Upstream of a line's start: the line break before it.
            None if line > 0 => (line - 1, None),
            None => (line, Some(0)),
        }
    } else if index >= text.len() {
        (line, None)
    } else {
        (line, Some(index))
    };
    let text = &lines[line];
    Some(match character {
        // A line break: one word of its own.
        None => (at(line, text.len()), at(line + 1, 0)),
        Some(byte) => {
            let (start, word) = text
                .split_word_bound_indices()
                .take_while(|(start, _)| *start <= byte)
                .last()
                .unwrap_or((0, ""));
            (at(line, start), at(line, start + word.len()))
        }
    })
}

/// The text of a [`TextEditor`]: iced's `text_editor::Content`, over the
/// app's renderer.
pub struct Content(RefCell<Internal>);

struct Internal {
    editor: RendererEditor,
}

impl Content {
    /// An empty content.
    pub fn new() -> Self {
        Self::with_text("")
    }

    /// A content holding `text`.
    pub fn with_text(text: &str) -> Self {
        Self(RefCell::new(Internal {
            editor: RendererEditor::with_text(text),
        }))
    }

    /// Performs `action` on it.
    pub fn perform(&mut self, action: Action) {
        let internal = self.0.get_mut();

        internal.editor.perform(action);
    }

    /// Moves its cursor (and selection) to `cursor`.
    pub fn move_to(&mut self, cursor: Cursor) {
        let internal = self.0.get_mut();

        internal.editor.move_to(cursor);
    }

    /// Its cursor.
    pub fn cursor(&self) -> Cursor {
        self.0.borrow().editor.cursor()
    }

    /// How many lines it has.
    pub fn line_count(&self) -> usize {
        self.0.borrow().editor.line_count()
    }

    /// Its line at `index`.
    pub fn line(&self, index: usize) -> Option<Line<'_>> {
        let internal = self.0.borrow();
        let line = internal.editor.line(index)?;

        Some(Line {
            text: std::borrow::Cow::Owned(line.text.into_owned()),
            ending: line.ending,
        })
    }

    /// Its lines.
    pub fn lines(&self) -> impl Iterator<Item = Line<'_>> {
        (0..)
            .map(|i| self.line(i))
            .take_while(Option::is_some)
            .flatten()
    }

    /// Its whole text.
    pub fn text(&self) -> String {
        let mut contents = String::new();
        let mut lines = self.lines().peekable();

        while let Some(line) = lines.next() {
            contents.push_str(&line.text);

            if lines.peek().is_some() {
                contents.push_str(if line.ending == LineEnding::None {
                    LineEnding::default().as_str()
                } else {
                    line.ending.as_str()
                });
            }
        }

        contents
    }

    /// The selected text, if any.
    pub fn selection(&self) -> Option<String> {
        self.0.borrow().editor.copy()
    }

    /// Its first line's ending.
    pub fn line_ending(&self) -> Option<LineEnding> {
        Some(self.line(0)?.ending)
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.0.borrow().editor.is_empty()
    }
}

impl Clone for Content {
    fn clone(&self) -> Self {
        Self::with_text(&self.text())
    }
}

impl Default for Content {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let internal = self.0.borrow();

        f.debug_struct("Content")
            .field("editor", &internal.editor)
            .finish()
    }
}

/// The state of a [`TextEditor`].
#[derive(Debug, Default)]
pub struct State {
    focus: Option<Focus>,
    preedit: Option<input_method::Preedit>,
    last_click: Option<mouse::Click>,
    drag_click: Option<mouse::click::Kind>,
    partial_scroll: f32,
    /// A right-button press in the field, until it comes up.
    secondary_press: Option<Point>,
    /// The open context menu.
    toolbar: Option<toolbar::State>,
}

#[derive(Debug, Clone)]
struct Focus {
    updated_at: Instant,
    now: Instant,
    is_window_focused: bool,
}

impl Focus {
    const CURSOR_BLINK_INTERVAL_MILLIS: u128 = 500;

    fn now() -> Self {
        let now = Instant::now();

        Self {
            updated_at: now,
            now,
            is_window_focused: true,
        }
    }

    fn is_cursor_visible(&self) -> bool {
        self.is_window_focused
            && ((self.now - self.updated_at).as_millis() / Self::CURSOR_BLINK_INTERVAL_MILLIS)
                .is_multiple_of(2)
    }
}

impl State {
    /// Whether the field is focused.
    pub fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    /// Whether its context menu is open.
    pub fn has_menu(&self) -> bool {
        self.toolbar.is_some()
    }
}

impl operation::Focusable for State {
    fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    fn focus(&mut self) {
        self.focus = Some(Focus::now());
    }

    fn unfocus(&mut self) {
        self.focus = None;
        self.toolbar = None;
    }
}

/// Where what the user does goes: to the owner, or, for a read-only
/// field with none, straight onto its content.
enum Sink<'s, Message> {
    Publish(&'s dyn Fn(Action) -> Message),
    Perform(&'s Content),
}

impl<Message> Clone for Sink<'_, Message> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Message> Copy for Sink<'_, Message> {}

impl<Message> Sink<'_, Message> {
    fn act(self, action: Action, shell: &mut Shell<'_, Message>) {
        match self {
            Sink::Publish(on_edit) => shell.publish(on_edit(action)),
            Sink::Perform(content) => {
                content.0.borrow_mut().editor.perform(action);
                shell.request_redraw();
            }
        }
    }
}

/// The field's menu, for the overlay: its content, edits and focus.
struct Menu<'b, Message> {
    content: &'b Content,
    state: &'b mut State,
    sink: Sink<'b, Message>,
    read_only: bool,
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
        let has_selection = self.content.selection().is_some_and(|s| !s.is_empty());
        toolbar::items(!self.read_only, has_selection, menu.can_paste())
    }

    fn perform(
        &mut self,
        item: Item,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self.state.toolbar = None;
        match item {
            Item::Copy => {
                if let Some(selection) = self.content.selection() {
                    clipboard.write(clipboard::Kind::Standard, selection);
                }
            }
            Item::Cut if !self.read_only => {
                if let Some(selection) = self.content.selection() {
                    clipboard.write(clipboard::Kind::Standard, selection);
                    self.sink.act(Action::Edit(Edit::Delete), shell);
                }
            }
            Item::Paste if !self.read_only => {
                if let Some(contents) = clipboard.read(clipboard::Kind::Standard) {
                    self.sink
                        .act(Action::Edit(Edit::Paste(Arc::new(contents))), shell);
                }
            }
            Item::Cut | Item::Paste => {}
        }
        if let Some(focus) = &mut self.state.focus {
            focus.updated_at = Instant::now();
        }
        shell.request_redraw();
    }

    fn close(&mut self) {
        self.state.toolbar = None;
    }
}

impl<Message> Widget<Message, Theme, Renderer> for TextEditor<'_, Message> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }

    fn diff(&self, tree: &mut widget::Tree) {
        if self.is_disabled() {
            // A disabled field takes no pointer: its menu goes.
            tree.state.downcast_mut::<State>().toolbar = None;
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(
        &mut self,
        _tree: &mut widget::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let mut internal = self.content.0.borrow_mut();

        let limits = limits
            .width(self.width)
            .height(self.height)
            .min_height(self.min_height)
            .max_height(self.max_height);

        // The text is laid out short of the width by the caret's room
        // (`RenderEditable._adjustConstraints`).
        let text_box = limits.shrink(self.padding).max();
        internal.editor.update(
            Size::new((text_box.width - caret::MARGIN).max(0.0), text_box.height),
            self.font.unwrap_or_else(|| renderer.default_font()),
            self.text_size.unwrap_or_else(|| renderer.default_size()),
            self.line_height,
            self.wrapping,
            &mut PlainText,
        );

        match self.height {
            Length::Fill | Length::FillPortion(_) | Length::Fixed(_) => {
                layout::Node::new(limits.max())
            }
            Length::Shrink => {
                let min_bounds = internal.editor.min_bounds();

                layout::Node::new(
                    limits
                        .height(min_bounds.height)
                        .max()
                        .expand(Size::new(0.0, self.padding.y())),
                )
            }
        }
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let sink = match self.on_edit.as_deref() {
            Some(on_edit) => Sink::Publish(on_edit),
            None if self.read_only => Sink::Perform(self.content),
            None => return,
        };

        let state = tree.state.downcast_mut::<State>();
        let is_redraw = matches!(event, Event::Window(window::Event::RedrawRequested(_now)),);

        match event {
            Event::Window(window::Event::Unfocused) => {
                if let Some(focus) = &mut state.focus {
                    focus.is_window_focused = false;
                }
                state.toolbar = None;
            }
            Event::Window(window::Event::Focused) => {
                if let Some(focus) = &mut state.focus {
                    focus.is_window_focused = true;
                    focus.updated_at = Instant::now();

                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::Rescaled(scale_factor)) => {
                caret::set_scale_factor(*scale_factor);
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                if let Some(focus) = state
                    .focus
                    .as_mut()
                    .filter(|focus| focus.is_window_focused && !self.read_only)
                {
                    focus.now = *now;

                    let millis_until_redraw = Focus::CURSOR_BLINK_INTERVAL_MILLIS
                        - (focus.now - focus.updated_at).as_millis()
                            % Focus::CURSOR_BLINK_INTERVAL_MILLIS;

                    shell.request_redraw_at(
                        focus.now + Duration::from_millis(millis_until_redraw as u64),
                    );
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) if *button != mouse::Button::Left => {
                if cursor.is_over(layout.bounds()) {
                    if *button == mouse::Button::Right {
                        // `onSecondaryTapDown`: the place the menu opens.
                        state.secondary_press = cursor.position();
                        shell.capture_event();
                    }
                } else if state.focus.is_some() {
                    // Any pointer down outside the field unfocuses it
                    // (`TextFieldTapRegion`'s tap outside).
                    state.focus = None;
                    state.drag_click = None;
                    state.toolbar = None;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right)) => {
                if let (Some(pressed), Some(released)) =
                    (state.secondary_press.take(), cursor.position())
                {
                    if pressed.distance(released) <= toolbar::TAP_SLOP {
                        self.secondary_tap(state, layout.bounds(), pressed, clipboard);
                        shell.capture_event();
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // The press closes the menu (`onTapDown` on macOS), in the
                // field or out of it.
                state.toolbar = None;
            }
            _ => {}
        }

        if let Some(update) =
            Update::from_event(event, state, layout.bounds(), self.padding, cursor)
        {
            match update {
                Update::Click(click) => {
                    let action = match click.kind() {
                        mouse::click::Kind::Single => Action::Click(click.position()),
                        mouse::click::Kind::Double => Action::SelectWord,
                        mouse::click::Kind::Triple => Action::SelectLine,
                    };

                    state.focus = Some(Focus::now());
                    state.last_click = Some(click);
                    state.drag_click = Some(click.kind());

                    sink.act(action, shell);
                    shell.capture_event();
                }
                Update::Drag(position) => {
                    sink.act(Action::Drag(position), shell);
                }
                Update::Release => {
                    state.drag_click = None;
                }
                Update::Scroll(lines) => {
                    let bounds = self.content.0.borrow().editor.bounds();

                    if bounds.height >= i32::MAX as f32 {
                        return;
                    }

                    let lines = lines + state.partial_scroll;
                    state.partial_scroll = lines.fract();

                    sink.act(
                        Action::Scroll {
                            lines: lines as i32,
                        },
                        shell,
                    );
                    shell.capture_event();
                }
                Update::InputMethod(update) => match update {
                    Ime::Toggle(is_open) => {
                        state.preedit = is_open.then(input_method::Preedit::new);

                        shell.request_redraw();
                    }
                    Ime::Preedit { content, selection } => {
                        state.preedit = Some(input_method::Preedit {
                            content,
                            selection,
                            text_size: self.text_size,
                        });

                        shell.request_redraw();
                    }
                    Ime::Commit(text) => {
                        if !self.read_only {
                            state.toolbar = None;
                            sink.act(Action::Edit(Edit::Paste(Arc::new(text))), shell);
                        }
                    }
                },
                Update::Binding(binding) => {
                    fn apply_binding<Message>(
                        binding: Binding<Message>,
                        content: &Content,
                        state: &mut State,
                        read_only: bool,
                        sink: Sink<'_, Message>,
                        clipboard: &mut dyn Clipboard,
                        shell: &mut Shell<'_, Message>,
                    ) {
                        let edit = |state: &mut State, shell: &mut Shell<'_, Message>, edit| {
                            if !read_only {
                                // A change to the text closes the menu.
                                state.toolbar = None;
                                sink.act(Action::Edit(edit), shell);
                            }
                        };

                        match binding {
                            Binding::Unfocus => {
                                state.focus = None;
                                state.drag_click = None;
                                state.toolbar = None;
                            }
                            Binding::Copy => {
                                if let Some(selection) = content.selection() {
                                    clipboard.write(clipboard::Kind::Standard, selection);
                                }
                            }
                            Binding::Cut => {
                                if let Some(selection) = content.selection().filter(|_| !read_only)
                                {
                                    clipboard.write(clipboard::Kind::Standard, selection);
                                    edit(state, shell, Edit::Delete);
                                }
                            }
                            Binding::Paste => {
                                if let Some(contents) = clipboard.read(clipboard::Kind::Standard) {
                                    edit(state, shell, Edit::Paste(Arc::new(contents)));
                                }
                            }
                            Binding::Move(motion) => {
                                sink.act(Action::Move(motion), shell);
                            }
                            Binding::Select(motion) => {
                                sink.act(Action::Select(motion), shell);
                            }
                            Binding::SelectWord => {
                                sink.act(Action::SelectWord, shell);
                            }
                            Binding::SelectLine => {
                                sink.act(Action::SelectLine, shell);
                            }
                            Binding::SelectAll => {
                                sink.act(Action::SelectAll, shell);
                            }
                            Binding::Insert(c) => {
                                edit(state, shell, Edit::Insert(c));
                            }
                            Binding::Enter => {
                                edit(state, shell, Edit::Enter);
                            }
                            Binding::Backspace => {
                                edit(state, shell, Edit::Backspace);
                            }
                            Binding::Delete => {
                                edit(state, shell, Edit::Delete);
                            }
                            Binding::Sequence(sequence) => {
                                for binding in sequence {
                                    apply_binding(
                                        binding, content, state, read_only, sink, clipboard, shell,
                                    );
                                }
                            }
                            Binding::Custom(message) => {
                                shell.publish(message);
                            }
                        }
                    }

                    if !matches!(binding, Binding::Unfocus) {
                        shell.capture_event();
                    }

                    apply_binding(
                        binding,
                        self.content,
                        state,
                        self.read_only,
                        sink,
                        clipboard,
                        shell,
                    );

                    if let Some(focus) = &mut state.focus {
                        focus.updated_at = Instant::now();
                    }
                }
            }
        }

        let status = {
            let is_disabled = self.is_disabled();
            let is_hovered = cursor.is_over(layout.bounds());

            if is_disabled {
                Status::Disabled
            } else if state.focus.is_some() {
                Status::Focused { is_hovered }
            } else if is_hovered {
                Status::Hovered
            } else {
                Status::Active
            }
        };

        if is_redraw {
            self.last_status = Some(status);

            shell.request_input_method(&self.input_method(state, renderer, layout));
        } else if self
            .last_status
            .is_some_and(|last_status| status != last_status)
        {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        let mut internal = self.content.0.borrow_mut();
        let state = tree.state.downcast_ref::<State>();

        let font = self.font.unwrap_or_else(|| renderer.default_font());

        internal.editor.highlight(font, &mut PlainText, |_: &()| {
            highlighter::Format::default()
        });

        let style = (self.style)(theme, self.last_status.unwrap_or(Status::Active));

        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: style.border,
                ..renderer::Quad::default()
            },
            style.background,
        );

        let text_bounds = bounds.shrink(self.padding);

        if internal.editor.is_empty() {
            if let Some(placeholder) = self.placeholder.clone() {
                renderer.fill_text(
                    Text {
                        content: placeholder.into_owned(),
                        bounds: text_bounds.size(),
                        size: self.text_size.unwrap_or_else(|| renderer.default_size()),
                        line_height: self.line_height,
                        font,
                        align_x: text::Alignment::Default,
                        align_y: alignment::Vertical::Top,
                        shaping: text::Shaping::Advanced,
                        wrapping: self.wrapping,
                    },
                    text_bounds.position(),
                    style.placeholder,
                    text_bounds,
                );
            }
        }

        let translation = text_bounds.position() - Point::ORIGIN;
        let mut caret = None;

        if let Some(focus) = state.focus.as_ref() {
            match internal.editor.selection() {
                Selection::Caret(position) if focus.is_cursor_visible() && !self.read_only => {
                    caret = Some(caret::rect(
                        position.x,
                        position.y,
                        self.line_height_px(renderer),
                        text_bounds.width - caret::MARGIN,
                    ));
                }
                Selection::Range(ranges) => {
                    for range in ranges
                        .into_iter()
                        .filter_map(|range| text_bounds.intersection(&(range + translation)))
                    {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: range,
                                ..renderer::Quad::default()
                            },
                            style.selection,
                        );
                    }
                }
                Selection::Caret(_) => {}
            }
        }

        // The selection is drawn first here, after the text in iced's:
        // within a layer, quads go under text either way.
        if !internal.editor.is_empty() {
            renderer.fill_editor(
                &internal.editor,
                text_bounds.position(),
                style.value,
                text_bounds,
            );
        }

        // Over the text (`paintCursorAboveText`). `RenderEditable` clips
        // what it paints to its box only while the text overflows it
        // (`_hasVisualOverflow`): otherwise the caret's pixel above and
        // below the line shows.
        if let Some(rect) = caret {
            let buffer = internal.editor.buffer();
            let scroll = buffer.scroll();
            let line_height = buffer.metrics().line_height;
            let laid_out: usize = buffer
                .lines
                .iter()
                .map(|line| line.layout_opt().map_or(1, Vec::len))
                .sum();
            let overflows = scroll.line > 0
                || scroll.vertical != 0.0
                || laid_out as f32 * line_height > text_bounds.height + 0.01;
            caret::draw(
                renderer,
                rect + translation,
                self.caret_color,
                if overflows { text_bounds } else { *viewport },
            );
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let is_disabled = self.is_disabled();

        if cursor.is_over(layout.bounds()) {
            if is_disabled {
                mouse::Interaction::NotAllowed
            } else {
                mouse::Interaction::Text
            }
        } else {
            mouse::Interaction::default()
        }
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        let state = tree.state.downcast_mut::<State>();

        operation.focusable(self.id.as_ref(), layout.bounds(), state);
        // `SelectableText` is text: a finder reads it as Flutter's
        // `find.text` did.
        if self.read_only {
            operation.text(self.id.as_ref(), layout.bounds(), &self.content.text());
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut widget::Tree,
        _layout: Layout<'b>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let state = tree.state.downcast_mut::<State>();
        let position = state.toolbar.as_mut()?.position(translation);
        Some(toolbar::overlay(
            Box::new(Menu {
                content: self.content,
                state,
                sink: match self.on_edit.as_deref() {
                    Some(on_edit) => Sink::Publish(on_edit),
                    None => Sink::Perform(self.content),
                },
                read_only: self.read_only,
            }),
            position,
        ))
    }
}

impl<'a, Message: 'a> From<TextEditor<'a, Message>> for Element<'a, Message> {
    fn from(text_editor: TextEditor<'a, Message>) -> Self {
        Self::new(text_editor)
    }
}

enum Update<Message> {
    Click(mouse::Click),
    Drag(Point),
    Release,
    Scroll(f32),
    InputMethod(Ime),
    Binding(Binding<Message>),
}

enum Ime {
    Toggle(bool),
    Preedit {
        content: String,
        selection: Option<ops::Range<usize>>,
    },
    Commit(String),
}

impl<Message> Update<Message> {
    fn from_event(
        event: &Event,
        state: &State,
        bounds: Rectangle,
        padding: Padding,
        cursor: mouse::Cursor,
    ) -> Option<Self> {
        let binding = |binding| Some(Update::Binding(binding));

        match event {
            Event::Mouse(event) => match event {
                mouse::Event::ButtonPressed(mouse::Button::Left) => {
                    if let Some(cursor_position) = cursor.position_in(bounds) {
                        let cursor_position =
                            cursor_position - Vector::new(padding.left, padding.top);

                        let click = mouse::Click::new(
                            cursor_position,
                            mouse::Button::Left,
                            state.last_click,
                        );

                        Some(Update::Click(click))
                    } else if state.focus.is_some() {
                        binding(Binding::Unfocus)
                    } else {
                        None
                    }
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) => Some(Update::Release),
                mouse::Event::CursorMoved { .. } => match state.drag_click {
                    Some(mouse::click::Kind::Single) => {
                        let cursor_position =
                            cursor.position_in(bounds)? - Vector::new(padding.left, padding.top);

                        Some(Update::Drag(cursor_position))
                    }
                    _ => None,
                },
                mouse::Event::WheelScrolled { delta } if cursor.is_over(bounds) => {
                    Some(Update::Scroll(match delta {
                        mouse::ScrollDelta::Lines { y, .. } => {
                            if y.abs() > 0.0 {
                                y.signum() * -(y.abs() * 4.0).max(1.0)
                            } else {
                                0.0
                            }
                        }
                        mouse::ScrollDelta::Pixels { y, .. } => -y / 4.0,
                    }))
                }
                _ => None,
            },
            Event::InputMethod(event) => match event {
                input_method::Event::Opened | input_method::Event::Closed => Some(
                    Update::InputMethod(Ime::Toggle(matches!(event, input_method::Event::Opened))),
                ),
                input_method::Event::Preedit(content, selection) if state.focus.is_some() => {
                    Some(Update::InputMethod(Ime::Preedit {
                        content: content.clone(),
                        selection: selection.clone(),
                    }))
                }
                input_method::Event::Commit(content) if state.focus.is_some() => {
                    Some(Update::InputMethod(Ime::Commit(content.clone())))
                }
                _ => None,
            },
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modified_key,
                physical_key,
                modifiers,
                text,
                ..
            }) => {
                let status = if state.focus.is_some() {
                    Status::Focused {
                        is_hovered: cursor.is_over(bounds),
                    }
                } else {
                    Status::Active
                };

                let key_press = KeyPress {
                    key: key.clone(),
                    modified_key: modified_key.clone(),
                    physical_key: *physical_key,
                    modifiers: *modifiers,
                    text: text.clone(),
                    status,
                };

                Binding::from_key_press(key_press).map(Self::Binding)
            }
            _ => None,
        }
    }
}
