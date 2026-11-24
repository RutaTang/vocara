//! The text fields' tests: Flutter's macOS caret (its place, size, color,
//! blink and layer), the right-click menu (the word it selects, the items it
//! offers, what they do, what closes it, how it looks and fades), the caret
//! margin a multi-line field wraps short by, and the read-only field.

use std::time::Duration;

use iced::advanced::clipboard::{self, Clipboard};
use iced::advanced::renderer::{self as core_renderer, Headless};
use iced::event;
use iced::keyboard::{self, key::Named};
use iced::time::Instant;
use iced::widget::{button, column, container, text_input::Value, Space};
use iced::{mouse, window, Color, Element, Event, Length, Padding, Point, Renderer, Size};
use iced_test::runtime::user_interface::{self, UserInterface};

use super::caret;
use super::editor::{self, Action, Edit, Position};
use super::input;
use super::toolbar::{self, Item};
use super::*;
use crate::ui::core::motion::Curve;
use crate::ui::core::theme::{self, app_theme, font, Studio, DEFAULT_TEXT_SIZE};
use crate::ui::core::widgets::animated::testing::{near, over, BACKGROUND};
use crate::ui::core::widgets::studio_text::testing::{laid, load_fonts, spec};

const MS: Duration = Duration::from_millis(1);

#[derive(Debug, Clone, PartialEq)]
enum Msg {
    Input(String),
    Edit(Action),
    Tap,
}

/// A clipboard of one text.
#[derive(Default)]
struct Board(Option<String>);

impl Clipboard for Board {
    fn read(&self, _kind: clipboard::Kind) -> Option<String> {
        self.0.clone()
    }

    fn write(&mut self, _kind: clipboard::Kind, contents: String) {
        self.0 = Some(contents);
    }
}

/// A window as the app runs it: the view rebuilt each frame into the last
/// frame's tree, events with the pointer where it is, a frame at the
/// test's time, the drawing read back; with a clipboard.
struct Window {
    renderer: Renderer,
    cache: user_interface::Cache,
    size: Size,
    cursor: Point,
    board: Board,
    pixels: Vec<u8>,
}

impl Window {
    fn new(size: Size) -> Self {
        load_fonts();
        let renderer = iced_test::futures::futures::executor::block_on(
            <Renderer as Headless>::new(font::REGULAR, DEFAULT_TEXT_SIZE.into(), None),
        )
        .expect("a headless renderer");
        Self {
            renderer,
            cache: user_interface::Cache::default(),
            size,
            cursor: Point::new(-100.0, -100.0),
            board: Board::default(),
            pixels: Vec::new(),
        }
    }

    /// Builds `view`, puts the pointer at `at`, feeds it `events` and a
    /// frame at `now`, and draws it: what it published, and whether the
    /// events were captured.
    fn run<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        at: Point,
        events: &[Event],
        now: Instant,
    ) -> (Vec<M>, Vec<event::Status>) {
        self.cursor = at;
        let cursor = mouse::Cursor::Available(at);
        let mut ui = UserInterface::build(
            view,
            self.size,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut messages = Vec::new();
        let mut all = vec![Event::Mouse(mouse::Event::CursorMoved { position: at })];
        all.extend_from_slice(events);
        let (_, statuses) = ui.update(
            &all,
            cursor,
            &mut self.renderer,
            &mut self.board,
            &mut messages,
        );
        let _ = ui.update(
            &[Event::Window(window::Event::RedrawRequested(now))],
            cursor,
            &mut self.renderer,
            &mut self.board,
            &mut messages,
        );
        ui.draw(
            &mut self.renderer,
            &app_theme(),
            &core_renderer::Style {
                text_color: Color::WHITE,
            },
            cursor,
        );
        self.pixels = self.renderer.screenshot(
            Size::new(self.size.width as u32, self.size.height as u32),
            1.0,
            BACKGROUND,
        );
        self.cache = ui.into_cache();
        (messages, statuses[1..].to_vec())
    }

    /// The color drawn at pixel `(x, y)`.
    fn pixel(&self, x: u32, y: u32) -> Color {
        let i = ((y * self.size.width as u32 + x) * 4) as usize;
        let p = &self.pixels[i..i + 4];
        Color::from_rgba8(p[0], p[1], p[2], f32::from(p[3]) / 255.0)
    }
}

fn press(button: mouse::Button) -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(button))
}

fn release(button: mouse::Button) -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(button))
}

/// A primary click.
fn click() -> [Event; 2] {
    [press(mouse::Button::Left), release(mouse::Button::Left)]
}

/// A right-click.
fn right_click() -> [Event; 2] {
    [press(mouse::Button::Right), release(mouse::Button::Right)]
}

fn escape() -> Event {
    iced_test::simulator::press_key(keyboard::Key::Named(Named::Escape), None)
}

/// The width of `text` at 14 px in Inter, as the fields lay it out.
fn width(text: &str) -> f32 {
    laid(&spec(text, 14.0, font::REGULAR), f32::INFINITY)
        .min_bounds
        .width
}

/// Where the fields' text starts and its line is: a field at (20, 20) with
/// the studio inset (10 across, 6 down) and 14 px lines.
const TEXT_X: f32 = 30.0;
const LINE_TOP: f32 = 26.0;
const LINE: f32 = 14.0;
const MIDDLE: f32 = LINE_TOP + LINE / 2.0;

/// A single-line studio field holding `value`, at (20, 20), 260 wide.
fn field(value: &str) -> Element<'_, Msg> {
    container(
        text_input("", value)
            .on_input(Msg::Input)
            .size(14)
            .line_height(1.0)
            .padding(theme::FIELD_PADDING)
            .caret_color(Studio::ACCENT)
            .style(theme::studio_input),
    )
    .width(280)
    .padding(Padding {
        top: 20.0,
        left: 20.0,
        ..Padding::ZERO
    })
    .into()
}

fn window() -> Window {
    Window::new(Size::new(400.0, 200.0))
}

/// The point to click on an item of a menu of `count` items opened by a
/// right-click at `anchor` in `window`.
fn item_point(anchor: Point, count: usize, index: usize, window: Size) -> Point {
    let at = toolbar::place(anchor, toolbar::size(count), window);
    Point::new(
        at.x + toolbar::PADDING + 20.0,
        at.y + toolbar::PADDING + (index as f32 + 0.5) * toolbar::item_height(),
    )
}

// ------------------------------------------------------------------ caret

#[test]
fn the_caret_is_two_pixels_wide_a_pixel_taller_each_way_and_never_left_of_the_text() {
    caret::set_scale_factor(1.0);
    // At the start of the text: 2 device pixels left would be left of it.
    let r = caret::rect(0.0, 0.0, 14.0, 100.0);
    assert_eq!((r.x, r.y, r.width, r.height), (0.0, -1.0, 2.0, 16.0));
    // Elsewhere, 2 device pixels left of the glyph edge...
    assert_eq!(caret::rect(10.0, 0.0, 14.0, 100.0).x, 8.0);
    // ...which on a Retina screen is one point.
    caret::set_scale_factor(2.0);
    assert_eq!(caret::rect(10.0, 0.0, 14.0, 100.0).x, 9.0);
    // Never right of the text's box less the caret's room.
    assert_eq!(caret::rect(120.0, 0.0, 14.0, 100.0).x, 100.0);
    caret::set_scale_factor(1.0);
}

#[test]
fn a_focused_field_draws_flutters_caret_over_its_text_and_blinks_it() {
    let mut window = window();
    let start = Instant::now();
    // A click puts the caret at the start of an empty field.
    window.run(field(""), Point::new(150.0, MIDDLE), &click(), start);
    let t0 = Instant::now();
    window.run(field(""), Point::new(150.0, MIDDLE), &[], t0 + 100 * MS);
    let (x, top) = (TEXT_X as u32, LINE_TOP as u32);
    // Two pixels of the accent, from a pixel above the line to a pixel
    // below it, round at the ends.
    for y in top + 1..top + LINE as u32 - 1 {
        assert!(near(window.pixel(x, y), Studio::ACCENT), "({x}, {y})");
        assert!(
            near(window.pixel(x + 1, y), Studio::ACCENT),
            "({}, {y})",
            x + 1
        );
    }
    assert!(near(window.pixel(x + 2, top + 7), BACKGROUND));
    assert!(near(window.pixel(x - 1, top + 7), BACKGROUND));
    let cap = window.pixel(x, top - 1);
    assert!(
        cap.r > 0.05 && !near(cap, Studio::ACCENT),
        "a rounded cap above the line: {cap:?}"
    );
    assert!(near(window.pixel(x, top - 2), BACKGROUND));
    assert!(near(window.pixel(x, top + LINE as u32 + 1), BACKGROUND));
    // Hidden for the next 500 ms, shown again after them.
    window.run(field(""), Point::new(150.0, MIDDLE), &[], t0 + 600 * MS);
    assert!(near(window.pixel(x, top + 7), BACKGROUND));
    window.run(field(""), Point::new(150.0, MIDDLE), &[], t0 + 1100 * MS);
    assert!(near(window.pixel(x, top + 7), Studio::ACCENT));
}

#[test]
fn the_caret_sits_two_pixels_left_of_the_glyph_edge_over_the_glyphs() {
    caret::set_scale_factor(1.0);
    let mut window = window();
    // Between the two W's, whose diagonals reach the edge between them.
    let edge = TEXT_X + width("W");
    window.run(
        field("WW"),
        Point::new(edge, MIDDLE),
        &click(),
        Instant::now(),
    );
    window.run(field("WW"), Point::new(edge, MIDDLE), &[], Instant::now());
    let left = (edge - 2.0).round() as u32;
    for y in LINE_TOP as u32 + 1..(LINE_TOP + LINE) as u32 - 1 {
        // Painted over the text (`paintCursorAboveText`): the accent,
        // whatever the glyphs drew there.
        assert!(near(window.pixel(left, y), Studio::ACCENT), "({left}, {y})");
        assert!(
            near(window.pixel(left + 1, y), Studio::ACCENT),
            "({}, {y})",
            left + 1
        );
    }
}

#[test]
fn a_selection_shows_no_caret_and_an_unfocused_window_none_either() {
    let mut window = window();
    let at = Point::new(TEXT_X + 2.0, MIDDLE);
    window.run(field("alpha"), at, &click(), Instant::now());
    // Select all: no caret.
    window.run(
        field("alpha"),
        at,
        &[
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::COMMAND,
            )),
            iced_test::simulator::press_key(keyboard::Key::Character("a".into()), None),
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::default(),
            )),
        ],
        Instant::now(),
    );
    let x = TEXT_X as u32;
    assert!(!near(window.pixel(x, LINE_TOP as u32 + 7), Studio::ACCENT));
    // The caret back at the start (a click left of the text, far enough
    // from the last not to be a double click), then the window loses
    // focus: no caret.
    let at = Point::new(TEXT_X, MIDDLE + 7.0);
    window.run(field("alpha"), at, &click(), Instant::now());
    window.run(field("alpha"), at, &[], Instant::now());
    let caret_x = TEXT_X as u32;
    assert!(near(
        window.pixel(caret_x, LINE_TOP as u32 + 7),
        Studio::ACCENT
    ));
    window.run(
        field("alpha"),
        at,
        &[Event::Window(window::Event::Unfocused)],
        Instant::now(),
    );
    assert!(!near(
        window.pixel(caret_x, LINE_TOP as u32 + 7),
        Studio::ACCENT
    ));
}

// ------------------------------------------------------------------- menu

#[test]
fn the_menu_offers_what_macos_offered() {
    use Item::*;
    assert_eq!(toolbar::items(true, true, true), vec![Cut, Copy, Paste]);
    assert_eq!(toolbar::items(true, true, false), vec![Cut, Copy]);
    assert_eq!(toolbar::items(true, false, true), vec![Paste]);
    assert_eq!(toolbar::items(true, false, false), vec![]);
    // Read-only: Copy only, never Select All.
    assert_eq!(toolbar::items(false, true, true), vec![Copy]);
    assert_eq!(toolbar::items(false, false, true), vec![]);
    assert_eq!(
        [Cut, Copy, Paste].map(Item::label),
        ["Cut", "Copy", "Paste"]
    );
}

#[test]
fn the_menu_opens_at_the_click_inside_the_window_less_eight() {
    let window = Size::new(400.0, 200.0);
    let size = toolbar::size(3);
    assert_eq!(size.width, 222.0);
    assert!((size.height - (12.0 + 3.0 * (2.0 + 14.0 * 1.21 + 5.0))).abs() < 1e-3);
    // Where there is room: its top left at the click.
    assert_eq!(
        toolbar::place(Point::new(50.0, 40.0), size, window),
        Point::new(50.0, 40.0)
    );
    // Near the right and bottom edges: moved in to 8 from them.
    let at = toolbar::place(Point::new(350.0, 180.0), size, window);
    assert_eq!(at.x, 400.0 - 8.0 - 222.0);
    assert!((at.y - (200.0 - 8.0 - size.height)).abs() < 1e-3);
}

#[test]
fn a_right_click_selects_the_word_under_it_as_icu_bounds_words() {
    let value = Value::new("go to https://example.com/x now");
    // In a word, on either side of a glyph's middle.
    assert_eq!(input::word_at(&value, 1, false), (0, 2));
    assert_eq!(input::word_at(&value, 2, true), (0, 2));
    // A dot between letters joins them (`MidNumLet`).
    assert_eq!(input::word_at(&value, 17, false), (14, 25));
    // "https" ends at the colon, which (before a slash) is a word of its
    // own, as is a space.
    assert_eq!(input::word_at(&value, 10, false), (6, 11));
    assert_eq!(input::word_at(&value, 11, false), (11, 12));
    assert_eq!(input::word_at(&value, 2, false), (2, 3));
    // Past the end of the text (even upstream of the last glyph): the
    // caret at the end.
    assert_eq!(input::word_at(&value, value.len(), true), (31, 31));

    let lines = ["first line".to_string(), String::new(), "last".to_string()];
    let at = |line, column| Position { line, column };
    assert_eq!(
        editor::word_at(&lines, 0, 2, false),
        Some((at(0, 0), at(0, 5)))
    );
    // Upstream at a line's end: its last word.
    assert_eq!(
        editor::word_at(&lines, 0, 10, true),
        Some((at(0, 6), at(0, 10)))
    );
    // An empty line: its line break.
    assert_eq!(
        editor::word_at(&lines, 1, 0, false),
        Some((at(1, 0), at(2, 0)))
    );
    // Past the end of the text.
    assert_eq!(editor::word_at(&lines, 2, 4, true), None);
}

#[test]
fn copy_puts_the_right_clicked_word_on_the_clipboard_and_closes_the_menu() {
    let mut window = window();
    let value = "alpha beta gamma";
    let anchor = Point::new(TEXT_X + width("alpha be"), MIDDLE);
    let (messages, _) = window.run(field(value), anchor, &right_click(), Instant::now());
    assert!(
        messages.is_empty(),
        "the word is selected in the field: {messages:?}"
    );
    // An empty clipboard: Cut and Copy.
    let menu = toolbar::place(anchor, toolbar::size(2), window.size);
    let below = Point::new(menu.x + 100.0, menu.y + toolbar::size(2).height - 3.0);
    window.run(field(value), Point::new(390.0, 190.0), &[], Instant::now());
    assert!(near(
        window.pixel(below.x as u32, below.y as u32),
        over(toolbar::BACKGROUND, BACKGROUND)
    ));
    let copy = item_point(anchor, 2, 1, window.size);
    let (messages, _) = window.run(field(value), copy, &click(), Instant::now());
    assert!(messages.is_empty());
    assert_eq!(window.board.0.as_deref(), Some("beta"));
    // Gone.
    window.run(field(value), Point::new(390.0, 190.0), &[], Instant::now());
    assert!(near(
        window.pixel(below.x as u32, below.y as u32),
        BACKGROUND
    ));
}

#[test]
fn cut_and_paste_change_the_text_as_flutters_did() {
    let value = "alpha beta gamma";
    let anchor = Point::new(TEXT_X + width("alpha be"), MIDDLE);

    let mut window = window();
    window.board = Board(Some("Z".into()));
    window.run(field(value), anchor, &right_click(), Instant::now());
    let cut = item_point(anchor, 3, 0, window.size);
    let (messages, _) = window.run(field(value), cut, &click(), Instant::now());
    assert_eq!(messages, vec![Msg::Input("alpha  gamma".into())]);
    assert_eq!(window.board.0.as_deref(), Some("beta"));

    let mut window = self::window();
    window.board = Board(Some("Z".into()));
    window.run(field(value), anchor, &right_click(), Instant::now());
    let paste = item_point(anchor, 3, 2, window.size);
    let (messages, _) = window.run(field(value), paste, &click(), Instant::now());
    assert_eq!(messages, vec![Msg::Input("alpha Z gamma".into())]);

    // Past the end of the text: the caret at the end, so Paste alone, which
    // appends.
    let mut window = self::window();
    window.board = Board(Some("!".into()));
    let past = Point::new(TEXT_X + width(value) + 20.0, MIDDLE);
    window.run(field(value), past, &right_click(), Instant::now());
    let paste = item_point(past, 1, 0, window.size);
    let (messages, _) = window.run(field(value), paste, &click(), Instant::now());
    assert_eq!(messages, vec![Msg::Input("alpha beta gamma!".into())]);
}

#[test]
fn a_right_click_on_the_selection_keeps_it() {
    let mut window = window();
    let value = "alpha beta gamma";
    let anchor = Point::new(TEXT_X + width("alpha be"), MIDDLE);
    window.run(field(value), anchor, &click(), Instant::now());
    window.run(
        field(value),
        anchor,
        &[
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::COMMAND,
            )),
            iced_test::simulator::press_key(keyboard::Key::Character("a".into()), None),
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::default(),
            )),
        ],
        Instant::now(),
    );
    window.run(field(value), anchor, &right_click(), Instant::now());
    let copy = item_point(anchor, 2, 1, window.size);
    window.run(field(value), copy, &click(), Instant::now());
    assert_eq!(window.board.0.as_deref(), Some(value));
}

#[test]
fn escape_closes_the_menu_and_goes_no_further() {
    use crate::ui::core::widgets::confirm_dialog::{self, DialogState, StudioPrompt};

    let state = DialogState::new(StudioPrompt::new("Rename").initial("Old song"));
    let mut window = Window::new(Size::new(1024.0, 768.0));
    // The prompt's field, in the 400-wide card centred in the window.
    let field_y = {
        let mut ui = iced_test::simulator(state.view());
        let field = ui.find("Old song").expect("the field").bounds();
        field.y + field.height / 2.0
    };
    let on_word = Point::new((1024.0 - 400.0) / 2.0 + 23.0 + 13.0 + 10.0 + 4.0, field_y);
    let (messages, _) = window.run(state.view(), on_word, &right_click(), Instant::now());
    assert!(messages.is_empty());
    // The first Escape closes the menu only; the second cancels the dialog.
    let (messages, statuses) = window.run(state.view(), on_word, &[escape()], Instant::now());
    assert_eq!(messages, Vec::<confirm_dialog::Message>::new());
    assert_eq!(statuses, vec![event::Status::Captured]);
    let (messages, _) = window.run(state.view(), on_word, &[escape()], Instant::now());
    assert_eq!(messages, vec![confirm_dialog::Message::Cancel]);
}

#[test]
fn a_press_elsewhere_closes_the_menu_and_goes_on() {
    let view = |value| -> Element<'_, Msg> {
        column![
            field(value),
            Space::new().height(80),
            button("Elsewhere")
                .on_press(Msg::Tap)
                .width(Length::Fixed(120.0)),
        ]
        .into()
    };
    let mut window = window();
    let value = "alpha beta";
    let anchor = Point::new(TEXT_X + 3.0, MIDDLE);
    window.run(view(value), anchor, &right_click(), Instant::now());
    // The button sits under the menu's end, left of its items: pressing it
    // closes the menu and presses the button.
    let button = Point::new(10.0, 20.0 + 26.0 + 80.0 + 10.0);
    let (messages, _) = window.run(view(value), button, &click(), Instant::now());
    assert_eq!(messages, vec![Msg::Tap]);
    let copy = item_point(anchor, 2, 1, window.size);
    window.run(view(value), copy, &click(), Instant::now());
    assert_eq!(window.board.0, None, "no menu left to copy with");
}

#[test]
fn an_item_under_the_pointer_fills_with_the_primary_color() {
    let mut window = window();
    let value = "alpha beta";
    let anchor = Point::new(TEXT_X + 3.0, MIDDLE);
    window.run(field(value), anchor, &right_click(), Instant::now());
    let copy = item_point(anchor, 2, 1, window.size);
    window.run(field(value), copy, &[], Instant::now());
    let menu = toolbar::place(anchor, toolbar::size(2), window.size);
    let row_y = menu.y + toolbar::PADDING + 1.5 * toolbar::item_height();
    // At the row's right end, past its label: the fill.
    let fill = window.pixel((menu.x + toolbar::WIDTH - 10.0) as u32, row_y as u32);
    assert!(near(fill, toolbar::HOVER), "{fill:?}");
    // The other row is not filled.
    let other = window.pixel(
        (menu.x + toolbar::WIDTH - 10.0) as u32,
        (row_y - toolbar::item_height()) as u32,
    );
    assert!(!near(other, toolbar::HOVER), "{other:?}");
}

#[test]
fn a_held_item_fades_to_seven_tenths_and_back_never_cutting_a_run_short() {
    let mut fade = toolbar::Fade::default();
    let t0 = Instant::now();
    assert_eq!(fade.opacity(t0), 1.0);
    fade.hold(true);
    // The run starts at the next frame and takes 120 ms.
    fade.advance(t0);
    let mid = fade.opacity(t0 + 60 * MS);
    assert!(mid < 1.0 && mid > 0.7, "{mid}");
    // Let go halfway: the run finishes first...
    fade.hold(false);
    assert!((fade.opacity(t0 + 120 * MS) - 0.7).abs() < 1e-4);
    fade.advance(t0 + 120 * MS);
    // ...then the way back starts at the next frame, over 180 ms.
    fade.advance(t0 + 136 * MS);
    let back = fade.opacity(t0 + 136 * MS + 90 * MS);
    assert!(back > 0.7 && back < 1.0, "{back}");
    assert!((fade.opacity(t0 + 136 * MS + 180 * MS) - 1.0).abs() < 1e-4);
}

// ----------------------------------------------------------------- editor

/// A multi-line studio field over `content` at (20, 20), with the studio
/// inset and 14 px lines.
fn prose(content: &Content, read_only: bool) -> Element<'_, Msg> {
    container(
        text_editor(content)
            .on_action(Msg::Edit)
            .read_only(read_only)
            .size(14)
            .line_height(1.0)
            .padding(theme::FIELD_PADDING)
            .caret_color(Studio::ACCENT),
    )
    .width(280)
    .padding(Padding {
        top: 20.0,
        left: 20.0,
        ..Padding::ZERO
    })
    .into()
}

#[test]
fn a_right_click_in_a_multi_line_field_selects_the_word_on_its_line() {
    let content = Content::with_text("first line\nsecond word here");
    let mut window = window();
    let anchor = Point::new(TEXT_X + width("second wo"), LINE_TOP + LINE + 7.0);
    let (messages, _) = window.run(
        prose(&content, false),
        anchor,
        &right_click(),
        Instant::now(),
    );
    assert!(messages.is_empty(), "{messages:?}");
    assert_eq!(content.selection().as_deref(), Some("word"));
    let cut = item_point(anchor, 2, 0, window.size);
    let (messages, _) = window.run(prose(&content, false), cut, &click(), Instant::now());
    assert_eq!(messages, vec![Msg::Edit(Action::Edit(Edit::Delete))]);
    assert_eq!(window.board.0.as_deref(), Some("word"));
}

#[test]
fn a_read_only_field_selects_and_copies_and_shows_no_caret() {
    let content = Content::with_text("https://example.com/download");
    let mut window = window();
    window.board = Board(Some("x".into()));
    // A click focuses it: no caret.
    let at = Point::new(TEXT_X + width("https://ex"), MIDDLE);
    window.run(prose(&content, true), at, &click(), Instant::now());
    window.run(prose(&content, true), at, &[], Instant::now());
    for x in TEXT_X as u32..(TEXT_X + width("https://example.com/download")) as u32 {
        assert!(
            !near(window.pixel(x, LINE_TOP as u32 + 7), Studio::ACCENT),
            "x {x}"
        );
    }
    // The menu offers Copy alone, which copies the word.
    window.run(prose(&content, true), at, &right_click(), Instant::now());
    let copy = item_point(at, 1, 0, window.size);
    window.run(prose(&content, true), copy, &click(), Instant::now());
    assert_eq!(window.board.0.as_deref(), Some("example.com"));
}

#[test]
fn a_multi_line_field_wraps_its_text_short_of_its_width_by_the_carets_room() {
    let text = "aaa bbb";
    let full = width(text);
    let height = |w: f32| {
        let content = Content::with_text(text);
        let id = iced::widget::Id::unique();
        let mut ui = iced_test::simulator(Element::from(
            container(
                text_editor::<Msg>(&content)
                    .id(id.clone())
                    .on_action(Msg::Edit)
                    .font(font::REGULAR)
                    .size(14)
                    .line_height(1.0)
                    .padding(0),
            )
            .width(w),
        ));
        ui.find(id).expect("the field").bounds().height
    };
    // Room for the line but not for the caret after it: two lines.
    assert_eq!(height(full + 2.0), 28.0);
    assert_eq!(height(full + caret::MARGIN + 0.5), 14.0);
}

#[test]
fn a_multi_line_field_draws_the_caret_on_its_line() {
    let content = Content::with_text("one\ntwo");
    let mut window = window();
    // A click after "two" puts the caret at the end of the second line.
    let at = Point::new(TEXT_X + 150.0, LINE_TOP + LINE + 7.0);
    let (messages, _) = window.run(prose(&content, false), at, &click(), Instant::now());
    let mut content = content;
    for message in messages {
        if let Msg::Edit(action) = message {
            content.perform(action);
        }
    }
    window.run(prose(&content, false), at, &[], Instant::now());
    let x = (TEXT_X + width("two") - 2.0).round() as u32;
    let top = (LINE_TOP + LINE) as u32;
    for y in top + 1..top + LINE as u32 - 1 {
        assert!(near(window.pixel(x, y), Studio::ACCENT), "({x}, {y})");
    }
    // Not on the first line.
    assert!(!near(window.pixel(x, LINE_TOP as u32 + 7), Studio::ACCENT));
}

// ------------------------------------------------------------ the scroll

/// Reads the first single-line field's scroll.
struct ReadScroll(Option<f32>);

impl iced::advanced::widget::Operation for ReadScroll {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation)) {
        operate(self);
    }

    fn custom(
        &mut self,
        _id: Option<&iced::widget::Id>,
        _bounds: iced::Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if let Some(state) = state.downcast_ref::<input::State>() {
            self.0.get_or_insert(state.scroll_offset());
        }
    }
}

impl Window {
    /// How far the single-line field in `view` has its text scrolled, as
    /// last drawn.
    fn scroll<'a, M>(&mut self, view: impl Into<Element<'a, M>>) -> f32 {
        let mut ui = UserInterface::build(
            view,
            self.size,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut read = ReadScroll(None);
        ui.operate(&self.renderer, &mut read);
        self.cache = ui.into_cache();
        read.0.expect("a single-line field")
    }
}

/// The field's text box: 240 wide (the 260 field less its inset).
const BOX: f32 = 240.0;

fn typed(text: &str) -> Event {
    iced_test::simulator::press_key(keyboard::Key::Character(text.into()), Some(text.into()))
}

fn key(named: Named) -> Event {
    iced_test::simulator::press_key(keyboard::Key::Named(named), None)
}

fn command() -> Event {
    Event::Keyboard(keyboard::Event::ModifiersChanged(
        keyboard::Modifiers::COMMAND,
    ))
}

/// The text the field published last among `messages`, if any.
fn input_of(messages: Vec<Msg>) -> Option<String> {
    messages.into_iter().rev().find_map(|m| match m {
        Msg::Input(text) => Some(text),
        _ => None,
    })
}

/// The longest run of a's the box holds with the caret's room after it.
fn filler() -> String {
    let mut text = String::from("a");
    while width(&format!("{text}a")) + caret::MARGIN < BOX {
        text.push('a');
    }
    text
}

/// Where the caret at the end of `text` is revealed: its right edge (2 px
/// wide, 2 device pixels left of the glyph edge, on a pixel) on the box's.
fn revealed_end(text: &str) -> f32 {
    let left = (TEXT_X + width(text) - 2.0).round() - TEXT_X;
    left + caret::WIDTH - BOX
}

/// The field holding the filler, clicked at its end, then `more` typed
/// there at `t0` (one frame): the text, after the frame that shows it.
fn type_past_the_end(window: &mut Window, more: &str, t0: Instant) -> String {
    let text = filler();
    let at = Point::new(TEXT_X + width(&text) + 1.0, MIDDLE);
    window.run(field(&text), at, &click(), t0 - 300 * MS);
    window.run(field(&text), at, &[], t0 - 200 * MS);
    let keys: Vec<Event> = more.chars().map(|c| typed(&c.to_string())).collect();
    let (messages, _) = window.run(field(&text), at, &keys, t0);
    input_of(messages).expect("typed")
}

#[test]
fn typing_past_the_edge_scrolls_only_as_far_as_shows_the_caret_over_100_ms() {
    caret::set_scale_factor(1.0);
    let mut window = window();
    let at = Point::new(150.0, MIDDLE);
    let t0 = Instant::now() + 1000 * MS;
    let text = type_past_the_end(&mut window, "WW", t0);
    // The frame that showed it, still unscrolled; then the run's first
    // frame...
    assert_eq!(window.scroll(field(&text)), 0.0);
    let t1 = t0 + 16 * MS;
    window.run(field(&text), at, &[], t1);
    assert_eq!(window.scroll(field(&text)), 0.0);
    // ...half way on fastOutSlowIn...
    let to = revealed_end(&text);
    assert!(
        to > 0.0 && to < width("WW"),
        "only as far as the caret: {to}"
    );
    window.run(field(&text), at, &[], t1 + 50 * MS);
    let half = to * Curve::FastOutSlowIn.transform(0.5);
    assert!((window.scroll(field(&text)) - half).abs() < 0.01);
    // ...and there after 100 ms.
    window.run(field(&text), at, &[], t1 + 100 * MS);
    assert!((window.scroll(field(&text)) - to).abs() < 0.01);
    // The caret on the box's last two pixels, over the text.
    let x = (TEXT_X + BOX) as u32;
    assert!(near(
        window.pixel(x - 1, LINE_TOP as u32 + 7),
        Studio::ACCENT
    ));
    assert!(near(
        window.pixel(x - 2, LINE_TOP as u32 + 7),
        Studio::ACCENT
    ));

    // The focus going elsewhere leaves the text where it was (iced's
    // field went back to its start).
    let away = Point::new(10.0, 150.0);
    window.run(field(&text), away, &click(), t1 + 300 * MS);
    window.run(field(&text), away, &[], t1 + 400 * MS);
    assert!((window.scroll(field(&text)) - to).abs() < 0.01);
}

#[test]
fn deleting_at_the_end_slides_the_text_back_into_range() {
    caret::set_scale_factor(1.0);
    let mut window = window();
    let at = Point::new(150.0, MIDDLE);
    let t0 = Instant::now() + 1000 * MS;
    let text = type_past_the_end(&mut window, "WWW", t0);
    window.run(field(&text), at, &[], t0 + 16 * MS);
    window.run(field(&text), at, &[], t0 + 200 * MS);
    let before = window.scroll(field(&text));
    assert!((before - revealed_end(&text)).abs() < 0.01);

    let (messages, _) = window.run(field(&text), at, &[key(Named::Backspace)], t0 + 300 * MS);
    let text = input_of(messages).expect("deleted");
    let max = width(&text) + caret::MARGIN - BOX;
    assert!(before > max, "out of range");
    // Out of range for the frame that shows the edit, then back over
    // 100 ms.
    assert_eq!(window.scroll(field(&text)), before);
    let t1 = t0 + 316 * MS;
    window.run(field(&text), at, &[], t1);
    assert_eq!(window.scroll(field(&text)), before);
    window.run(field(&text), at, &[], t1 + 50 * MS);
    let half = before + (max - before) * Curve::FastOutSlowIn.transform(0.5);
    assert!((window.scroll(field(&text)) - half).abs() < 0.01);
    window.run(field(&text), at, &[], t1 + 100 * MS);
    assert!((window.scroll(field(&text)) - max).abs() < 0.01);
}

#[test]
fn a_keyboard_move_reveals_the_caret_at_once() {
    caret::set_scale_factor(1.0);
    let mut window = window();
    let at = Point::new(150.0, MIDDLE);
    let t0 = Instant::now() + 1000 * MS;
    let text = type_past_the_end(&mut window, "WWWW", t0);
    window.run(field(&text), at, &[], t0 + 16 * MS);
    window.run(field(&text), at, &[], t0 + 200 * MS);
    let end = window.scroll(field(&text));
    assert!(end > 0.0);
    // Home (on macOS it scrolls alone; elsewhere it takes the caret
    // along): at once, in the frame of the key.
    window.run(field(&text), at, &[key(Named::Home)], t0 + 300 * MS);
    assert_eq!(window.scroll(field(&text)), 0.0);
    // End: back to the end, at once.
    window.run(field(&text), at, &[key(Named::End)], t0 + 400 * MS);
    let back = window.scroll(field(&text));
    assert!((back - end).abs() < 0.01, "{back} {end}");
    // Select All shows the selection's end, at once.
    window.run(field(&text), at, &[key(Named::Home)], t0 + 500 * MS);
    window.run(field(&text), at, &[command(), typed("a")], t0 + 600 * MS);
    let back = window.scroll(field(&text));
    assert!((back - end).abs() < 0.01, "{back} {end}");
}

#[test]
fn a_trackpad_swipe_across_the_field_scrolls_it_within_its_range() {
    caret::set_scale_factor(1.0);
    let mut window = window();
    let text = format!("{}WWWW", filler());
    let at = Point::new(150.0, MIDDLE);
    let swipe = |x: f32, y: f32| {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x, y },
        })
    };
    let t0 = Instant::now();
    let (_, statuses) = window.run(field(&text), at, &[swipe(-30.0, 2.0)], t0);
    assert_eq!(statuses, vec![event::Status::Captured]);
    assert_eq!(window.scroll(field(&text)), 30.0);
    // Mostly down: the page's, not the field's.
    let (_, statuses) = window.run(field(&text), at, &[swipe(-3.0, -20.0)], t0);
    assert_eq!(statuses, vec![event::Status::Ignored]);
    assert_eq!(window.scroll(field(&text)), 30.0);
    // No further than the line and the caret's room.
    window.run(field(&text), at, &[swipe(-500.0, 0.0)], t0);
    let max = width(&text) + caret::MARGIN - BOX;
    assert!((window.scroll(field(&text)) - max).abs() < 0.01);
}

#[test]
fn cut_without_a_selection_cuts_nothing() {
    let mut window = window();
    let at = Point::new(TEXT_X + 1.0, MIDDLE);
    let t0 = Instant::now();
    window.run(field("abc"), at, &click(), t0);
    let (messages, _) = window.run(field("abc"), at, &[command(), typed("x")], t0 + 16 * MS);
    assert_eq!(messages, vec![]);
}
