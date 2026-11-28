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
//!   (`TextFieldTapRegion`), not the primary button's only;
//! - a wheel event is its own only while its text overflows, and a
//!   trackpad's swipe drags the text with the fingers (in lines), first
//!   winning the gesture's arena from a scroll view around it; the
//!   momentum after reaches nothing (`pan_zoom`, see `TextEditor::wheel`).
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
use iced::advanced::graphics::text::cosmic_text::{self, Affinity, Buffer, LayoutLine, LineBox};
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

use super::super::pan_zoom::{self, Scroll, TOUCH_SLOP};
use super::show_on_screen::{self, Request};
use super::toolbar::{self, Item};
use super::undo::{self, History, Kind, Snapshot};
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

    /// The caret's rectangle down the text (a pixel above and below its
    /// line), or the selection's last line box; across, the text box: in
    /// layout coordinates, what the view around is asked to show.
    fn caret_line(&self, layout: Layout<'_>, renderer: &Renderer) -> Rectangle {
        let text_bounds = layout.bounds().shrink(self.padding);
        let line = self.line_height_px(renderer);
        let internal = self.content.0.borrow();
        let (top, height) = match selection(&internal.editor) {
            Selection::Caret(position) => {
                (position.y - caret::OVERHANG, line + 2.0 * caret::OVERHANG)
            }
            Selection::Range(ranges) => ranges
                .last()
                .map_or((0.0, line), |range| (range.y, range.height)),
        };
        Rectangle {
            y: text_bounds.y + top,
            height,
            ..text_bounds
        }
    }

    /// A line's height: SkParagraph's, a whole pixel
    /// (`crate::ui::core::text::line_box`; `preferredLineHeight`, which
    /// `RenderEditable` sized its lines and caret by).
    fn line_height_px(&self, renderer: &Renderer) -> f32 {
        crate::ui::core::text::line_box(
            self.font.unwrap_or_else(|| renderer.default_font()),
            self.text_size.unwrap_or_else(|| renderer.default_size()).0,
            self.line_height,
        )
        .height
    }

    /// A wheel event as Flutter's multi-line field took it (`pan_zoom`),
    /// its `Scrollable` a vertical one that could drag, and take a
    /// scroll signal, only while its text overflowed (`setCanDrag`,
    /// `shouldAcceptUserOffset`); whether it is done with the event (iced's
    /// handling below takes the rest: a signal it scrolls by its lines).
    ///
    /// - A swipe that starts on it enters the gesture's arena, as a
    ///   single-line field's does (`input::scroll`); won, each move scrolls
    ///   the text by the pan, in the whole lines iced's editor scrolls by
    ///   (the rest carried to the next). It does not fling when the
    ///   fingers lift, where Flutter's did: iced's editor scrolls by lines.
    /// - The momentum after reaches nothing.
    /// - An inertia cancel (a finger touching the trackpad during macOS's
    ///   momentum) has nothing to stop here, the text never moving on its
    ///   own; a view around it stops.
    /// - With nothing to scroll it takes no wheel event: the view around
    ///   it, if any, has them.
    fn wheel(
        &self,
        state: &mut State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        sink: Sink<'_, Message>,
        shell: &mut Shell<'_, Message>,
    ) -> bool {
        let overflows = {
            let internal = self.content.0.borrow();
            let laid = internal.editor.bounds().height;
            laid < i32::MAX as f32 && internal.editor.min_bounds().height > laid + 0.5
        };
        match pan_zoom::current() {
            Scroll::Signal => !overflows,
            Scroll::Start { gesture, time } => {
                if overflows && cursor.is_over(bounds) {
                    state.pan = Some(Pan {
                        gesture,
                        pan: 0.0,
                        entry: pan_zoom::enter(gesture, time),
                        won: None,
                    });
                    shell.capture_event();
                }
                true
            }
            Scroll::Update { gesture, delta, .. } => {
                let Some(pan) = state.pan.as_mut().filter(|pan| pan.gesture == gesture) else {
                    return true;
                };
                pan.pan += delta.y;
                let mut moves = true;
                if pan.won.is_none() {
                    pan.won = pan.entry.standing();
                    if pan.won.is_none() && pan.pan.abs() > TOUCH_SLOP as f32 {
                        pan.won = Some(pan.entry.accept());
                        moves = false;
                    }
                }
                match pan.won {
                    Some(false) => {}
                    Some(true) if moves => {
                        // `applyUserOffset(dy)`, in lines.
                        let lines = -delta.y / self.line_height_px(renderer) + state.partial_scroll;
                        state.partial_scroll = lines.fract();
                        if lines.trunc() != 0.0 {
                            sink.act(
                                Action::Scroll {
                                    lines: lines as i32,
                                },
                                shell,
                            );
                        }
                        shell.capture_event();
                    }
                    _ => shell.capture_event(),
                }
                true
            }
            Scroll::End { gesture } => {
                if state
                    .pan
                    .take_if(|pan| pan.gesture == gesture)
                    .is_some_and(|pan| pan.won != Some(false))
                {
                    shell.capture_event();
                }
                true
            }
            Scroll::Nothing { .. } => true,
            // An inertia cancel stopped what moved its `Scrollable`: here
            // nothing does (no fling, no animated scroll). A view around it
            // stops its own.
            Scroll::InertiaCancel => true,
        }
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

        let cursor = match selection(&internal.editor) {
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
            } || match selection(&internal.editor) {
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

/// The box of the visual line `line` of `buffer`: SkParagraph's, as the
/// buffer laid it out and draws it (`crate::ui::core::text`).
fn line_box(buffer: &Buffer, line: &LayoutLine) -> LineBox {
    line.line_box(buffer.metrics().line_height, buffer.is_empty_paragraph())
}

/// `editor`'s selection (or caret) with its lines where the buffer lays
/// them out. iced puts the `k`th visual line from the scroll `k` line
/// heights down, unrounded, and makes it a line height tall; the buffer
/// puts it below the whole-pixel heights of the lines above it, as tall as
/// its own (SkParagraph's lines: `crate::ui::core::text`).
fn selection(editor: &RendererEditor) -> Selection {
    let buffer = editor.buffer();
    let pitch = buffer.metrics().line_height;
    let selection = editor.selection();
    if pitch <= 0.0 {
        return selection;
    }
    let scroll = buffer.scroll();
    // The heights of the visual lines laid out from the scroll's line on,
    // and of those before it (a line not laid out has none, as iced counts
    // it).
    let heights = |lines: &[cosmic_text::BufferLine]| -> Vec<f32> {
        lines
            .iter()
            .flat_map(|text_line| {
                text_line
                    .layout_opt()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            })
            .map(|visual| line_box(buffer, visual).height)
            .collect()
    };
    let split = scroll.line.min(buffer.lines.len());
    let (above, below) = (
        heights(&buffer.lines[..split]),
        heights(&buffer.lines[split..]),
    );
    // The top (from the scroll) and height of the visual line iced put
    // at `y`.
    let visual = |y: f32| -> Option<(f32, f32)> {
        let k = ((y + scroll.vertical) / pitch).round() as i64;
        let (top, height) = if k >= 0 {
            let k = usize::try_from(k).ok()?;
            (
                below[..k.min(below.len())].iter().sum::<f32>(),
                *below.get(k)?,
            )
        } else {
            let index = above.len().checked_sub(usize::try_from(-k).ok()?)?;
            (-above[index..].iter().sum::<f32>(), above[index])
        };
        Some((top - scroll.vertical, height))
    };
    match selection {
        Selection::Caret(position) => match visual(position.y) {
            Some((top, _)) => Selection::Caret(Point::new(position.x, top)),
            None => Selection::Caret(position),
        },
        Selection::Range(ranges) => Selection::Range(
            ranges
                .into_iter()
                .map(|range| match visual(range.y) {
                    Some((top, height)) => Rectangle {
                        y: top,
                        height,
                        ..range
                    },
                    None => range,
                })
                .collect(),
        ),
    }
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

    /// `motion` from the caret as a macOS text view takes it: Up on the
    /// first visual line goes to the start of the text and Down on the last
    /// to its end (cosmic-text's leave the caret on that line). Shift
    /// extends the selection the same way. Any other motion is itself.
    fn past_the_edge(&self, motion: Motion) -> Motion {
        let (up, to) = match motion {
            Motion::Up => (true, Motion::DocumentStart),
            Motion::Down => (false, Motion::DocumentEnd),
            other => return other,
        };
        let internal = self.0.borrow();
        let position = internal.editor.cursor().position;
        let buffer = internal.editor.buffer();
        let Some(layout) = buffer
            .lines
            .get(position.line)
            .and_then(cosmic_text::BufferLine::layout_opt)
        else {
            return motion;
        };
        // The visual line the caret is on: the last that starts at or
        // before it (at a wrap, the line the caret starts).
        let visual = layout
            .iter()
            .rposition(|line| line.glyphs.first().map_or(0, |g| g.start) <= position.column)
            .unwrap_or(0);
        let at_edge = if up {
            position.line == 0 && visual == 0
        } else {
            position.line + 1 >= buffer.lines.len() && visual + 1 >= layout.len()
        };
        if at_edge {
            to
        } else {
            motion
        }
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
    /// It can be neither edited nor selected from: it takes no focus from
    /// the keyboard's traversal.
    disabled: bool,
    /// The keyboard's traversal focused it: after the frame that shows it,
    /// the view around it is asked to show its caret (`true` once that
    /// frame has been drawn).
    reveal: Option<bool>,
    /// What it asks the scroll view around it to show
    /// (`show_on_screen`).
    pub(super) requests: Vec<Request>,
    /// The trackpad gesture its `Scrollable`'s drag took last.
    pan: Option<Pan>,
    /// Its edits, for ⌘Z and ⇧⌘Z (`undo`). The owner performs each edit
    /// after the update that publishes it, so what one left is taken in
    /// once the text has changed ([`State::sync`]).
    history: History<Cursor>,
    /// The text before the edits published since the field last saw its
    /// text change, while it waits for its owner to perform them.
    pending: Option<String>,
    /// An undo's or a redo's caret, put back once the owner has its text in.
    restore: Option<Snapshot<Cursor>>,
}

impl State {
    /// Takes in what became of the edits it published: once the text has
    /// changed from before them, what they left (and an undo's caret put
    /// back); with none waiting, a text changed from outside the field
    /// forgets the history.
    fn sync(&mut self, content: &Content) {
        let text = content.text();
        if let Some(restore) = self.restore.take() {
            if restore.text == text {
                content.0.borrow_mut().editor.move_to(restore.cursor);
            } else if self.pending.is_some() {
                self.restore = Some(restore);
            }
        }
        match self.pending.take() {
            Some(before) if before == text => self.pending = Some(before),
            Some(_) => self.history.left(Snapshot {
                text,
                cursor: content.cursor(),
            }),
            None => self.history.follow(&text),
        }
    }

    /// Takes in that an edit of `kind` is published, the text and caret
    /// `content`'s now.
    fn record(&mut self, content: &Content, kind: Kind) {
        let before = Snapshot {
            text: content.text(),
            cursor: content.cursor(),
        };
        self.pending.get_or_insert_with(|| before.text.clone());
        self.history.record(before, kind, Instant::now());
    }
}

/// A trackpad gesture that started on the field while its text could
/// scroll: its pan down so far, its vertical drag's place in the
/// gesture's arena and whether it won there (`None` while undecided).
#[derive(Debug, Clone, Copy)]
struct Pan {
    gesture: u64,
    pan: f32,
    entry: pan_zoom::Entry,
    won: Option<bool>,
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

impl crate::ui::core::widgets::focus_traversal::Traversable for State {
    fn can_focus(&self) -> bool {
        !self.disabled
    }

    fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    /// Focused from elsewhere: a multi-line field keeps its selection,
    /// and the view around it shows its caret after the frame
    /// (`_handleFocusChanged`'s `_scheduleShowCaretOnScreen`).
    fn focus_by_traversal(&mut self) {
        self.focus = Some(Focus::now());
        self.reveal = Some(false);
    }

    fn unfocus_by_traversal(&mut self) {
        self.focus = None;
        self.toolbar = None;
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
                    self.state.record(self.content, Kind::Other);
                    self.sink.act(Action::Edit(Edit::Delete), shell);
                }
            }
            Item::Paste if !self.read_only => {
                if let Some(contents) = clipboard.read(clipboard::Kind::Standard) {
                    self.state.record(self.content, Kind::Other);
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
        widget::tree::State::new(State {
            disabled: self.is_disabled(),
            ..State::default()
        })
    }

    fn diff(&self, tree: &mut widget::Tree) {
        tree.state.downcast_mut::<State>().disabled = self.is_disabled();
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
        if state.focus.is_some()
            && !self.read_only
            && (is_redraw || matches!(event, Event::Keyboard(_) | Event::InputMethod(_)))
        {
            state.sync(self.content);
        }
        // What the views around took is gone; what they did not, stale.
        state.requests.clear();
        if is_redraw {
            match state.reveal {
                Some(false) => {
                    state.reveal = Some(true);
                    shell.request_redraw();
                }
                Some(true) => {
                    state.reveal = None;
                    let rect = self.caret_line(layout, renderer);
                    state.requests.push(Request {
                        rect: rect.expand(show_on_screen::CARET_PADDING),
                        animate: true,
                    });
                    show_on_screen::ask();
                }
                None => {}
            }
        }

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
            Event::Mouse(mouse::Event::WheelScrolled { .. })
                if self.wheel(state, layout.bounds(), cursor, renderer, sink, shell) =>
            {
                return;
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
                Update::History(redo) => {
                    if !self.read_only {
                        let current = Snapshot {
                            text: self.content.text(),
                            cursor: self.content.cursor(),
                        };
                        let step = if redo {
                            state.history.redo(current)
                        } else {
                            state.history.undo(current)
                        };
                        if let Some(step) = step {
                            // The whole text put back, through the owner as
                            // every edit is; its caret once it is in.
                            state.toolbar = None;
                            state.pending.get_or_insert_with(|| self.content.text());
                            sink.act(Action::SelectAll, shell);
                            sink.act(
                                Action::Edit(Edit::Paste(Arc::new(step.text.clone()))),
                                shell,
                            );
                            state.restore = Some(step);
                            if let Some(focus) = &mut state.focus {
                                focus.updated_at = Instant::now();
                            }
                        }
                    }
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
                            state.record(self.content, Kind::Typing);
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
                        let edit =
                            |state: &mut State, shell: &mut Shell<'_, Message>, edit, kind| {
                                if !read_only {
                                    // A change to the text closes the menu.
                                    state.toolbar = None;
                                    state.record(content, kind);
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
                                    edit(state, shell, Edit::Delete, Kind::Other);
                                }
                            }
                            Binding::Paste => {
                                if let Some(contents) = clipboard.read(clipboard::Kind::Standard) {
                                    edit(
                                        state,
                                        shell,
                                        Edit::Paste(Arc::new(contents)),
                                        Kind::Other,
                                    );
                                }
                            }
                            Binding::Move(motion) => {
                                sink.act(Action::Move(content.past_the_edge(motion)), shell);
                            }
                            Binding::Select(motion) => {
                                sink.act(Action::Select(content.past_the_edge(motion)), shell);
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
                                edit(state, shell, Edit::Insert(c), Kind::Typing);
                            }
                            Binding::Enter => {
                                edit(state, shell, Edit::Enter, Kind::Typing);
                            }
                            Binding::Backspace => {
                                edit(state, shell, Edit::Backspace, Kind::Deleting);
                            }
                            Binding::Delete => {
                                edit(state, shell, Edit::Delete, Kind::Deleting);
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
            match selection(&internal.editor) {
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
            let line = self.line_height_px(renderer);
            let laid_out: f32 = buffer
                .lines
                .iter()
                .map(|text_line| {
                    text_line.layout_opt().map_or(line, |layout| {
                        layout
                            .iter()
                            .map(|visual| line_box(buffer, visual).height)
                            .sum()
                    })
                })
                .sum();
            let overflows =
                scroll.line > 0 || scroll.vertical != 0.0 || laid_out > text_bounds.height + 0.01;
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
        // Its own state and its text box, for the keyboard's traversal
        // (`focus_traversal`).
        operation.custom(
            self.id.as_ref(),
            layout.bounds().shrink(self.padding),
            state,
        );
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
    /// ⌘Z (`false`) or ⇧⌘Z (`true`), while it is focused.
    History(bool),
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

                if state.focus.is_some() {
                    if let Some(redo) = undo::undo_key(key.to_latin(*physical_key), *modifiers) {
                        return Some(Self::History(redo));
                    }
                }

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
