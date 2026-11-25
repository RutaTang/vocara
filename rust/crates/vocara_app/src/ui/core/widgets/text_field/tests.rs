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
use iced::{
    mouse, window, Color, Element, Event, Length, Padding, Point, Rectangle, Renderer, Size,
};
use iced_test::runtime::user_interface::{self, UserInterface};

use super::caret;
use super::editor::{self, Action, Edit, Position};
use super::input;
use super::toolbar::{self, Item};
use super::*;
use crate::ui::core::motion::Curve;
use crate::ui::core::theme::{self, app_theme, font, Studio, DEFAULT_TEXT_SIZE};
use crate::ui::core::widgets::animated::testing::{near, over, BACKGROUND};
use crate::ui::core::widgets::pan_zoom::testing::{set_event_time, Sent, Trackpad};
use crate::ui::core::widgets::studio_text::testing::{laid, load_fonts, spec};
use iced_winit::gesture::Phase;

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
        self.run_with(view, at, events, &[], now)
    }

    /// [`Window::run`] for wheel events with the gestures the run loop
    /// hands along (`pan_zoom::testing`), handled at `now`.
    fn run_sent<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        at: Point,
        sent: &[Sent],
        now: Instant,
    ) -> (Vec<M>, Vec<event::Status>) {
        let events: Vec<Event> = sent.iter().map(|(event, _)| event.clone()).collect();
        let wheels: Vec<_> = sent.iter().map(|(_, wheel)| *wheel).collect();
        set_event_time(Some(now));
        let ran = self.run_with(view, at, &events, &wheels, now);
        set_event_time(None);
        ran
    }

    fn run_with<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        at: Point,
        events: &[Event],
        wheels: &[Option<iced_winit::gesture::Wheel>],
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
        let (renderer, board) = (&mut self.renderer, &mut self.board);
        let (_, statuses) = iced_winit::gesture::update_each(&all, wheels, |events| {
            ui.update(events, cursor, renderer, board, &mut messages)
        });
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
fn a_scroll_signal_across_the_field_scrolls_it_and_one_down_is_the_pages() {
    caret::set_scale_factor(1.0);
    let mut window = window();
    let text = format!("{}WWWW", filler());
    let at = Point::new(150.0, MIDDLE);
    let t0 = Instant::now();
    let mut pad = Trackpad::new();
    // A precise device's points without phases: a signal, by its delta.
    let (_, statuses) = window.run_sent(field(&text), at, &[pad.pixels(-30.0, 2.0, t0)], t0);
    assert_eq!(statuses, vec![event::Status::Captured]);
    assert_eq!(window.scroll(field(&text)), 30.0);
    // Down: no delta across, the page's.
    let (_, statuses) = window.run_sent(field(&text), at, &[pad.pixels(0.0, -20.0, t0)], t0);
    assert_eq!(statuses, vec![event::Status::Ignored]);
    // No further than the line and the caret's room.
    window.run_sent(field(&text), at, &[pad.pixels(-500.0, 0.0, t0)], t0);
    let max = width(&text) + caret::MARGIN - BOX;
    assert!((window.scroll(field(&text)) - max).abs() < 0.01);
}

#[test]
fn a_two_finger_swipe_alone_on_the_field_drags_it_and_flings_it() {
    caret::set_scale_factor(1.0);
    let mut window = window();
    let text = format!("{}WWWWWWWWWWWWWWWWWWWW", filler());
    let max = width(&text) + caret::MARGIN - BOX;
    let at = Point::new(150.0, MIDDLE);
    let t0 = Instant::now();
    let mut pad = Trackpad::new();
    // Alone in its arena: it drags from the first move, by the pan across
    // only.
    let (_, statuses) = window.run_sent(field(&text), at, &[pad.began(t0)], t0);
    assert_eq!(statuses, vec![event::Status::Captured]);
    let mut t = t0;
    for _ in 0..4 {
        t += 10 * MS;
        let (_, statuses) = window.run_sent(field(&text), at, &[pad.changed(-10.0, -6.0, t)], t);
        assert_eq!(statuses, vec![event::Status::Captured]);
    }
    assert_eq!(window.scroll(field(&text)), 40.0);
    // The fingers lift at 1000 px/s: it flies on, slowing, past where they
    // left it, and the momentum macOS sends moves nothing.
    let up = t + 2 * MS;
    window.run_sent(field(&text), at, &[pad.ended(up)], up);
    window.run(field(&text), at, &[], up + 16 * MS);
    let (_, statuses) = window.run_sent(
        field(&text),
        at,
        &[pad.momentum(Phase::Began, -400.0, 0.0, up)],
        up + 32 * MS,
    );
    assert_eq!(statuses, vec![event::Status::Captured]);
    let early = window.scroll(field(&text));
    assert!(early > 40.0, "{early}");
    let mut now = up + 48 * MS;
    for _ in 0..40 {
        window.run(field(&text), at, &[], now);
        now += 16 * MS;
    }
    let done = window.scroll(field(&text));
    assert!(done > early && done <= max, "{early} {done} {max}");
    window.run(field(&text), at, &[], now + 16 * MS);
    assert_eq!(window.scroll(field(&text)), done, "stopped");
}

#[test]
fn in_a_scroll_view_a_swipe_is_the_fields_past_18_px_across_or_the_views_past_18_down() {
    use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};

    caret::set_scale_factor(1.0);
    let mut window = window();
    let text = format!("{}WWWW", filler());
    let at = Point::new(150.0, MIDDLE);
    fn page(text: &str) -> Element<'_, Msg> {
        scrollable(column![field(text), Space::new().height(400)])
            .width(Length::Fill)
            .height(200)
            .fading()
            .into()
    }
    let t0 = Instant::now();
    let mut pad = Trackpad::new();
    // Across: neither moves until it is past 18 px; then the rest of it is
    // the field's.
    let mut sent = vec![pad.began(t0)];
    for _ in 0..3 {
        sent.push(pad.changed(-7.0, -1.0, t0));
    }
    let (_, statuses) = window.run_sent(page(&text), at, &sent, t0);
    assert!(statuses.iter().all(|s| *s == event::Status::Captured));
    assert_eq!(window.scroll(page(&text)), 0.0, "the pan before it won");
    window.run_sent(page(&text), at, &[pad.changed(-5.0, -3.0, t0)], t0);
    assert_eq!(window.scroll(page(&text)), 5.0);
    window.run_sent(page(&text), at, &[pad.ended(t0 + 200 * MS)], t0 + 200 * MS);
    // Down: the view's past 18 px; the field moves not, nor takes the rest.
    let t1 = t0 + 300 * MS;
    let sent = [
        pad.began(t1),
        pad.changed(-2.0, -10.0, t1),
        pad.changed(-2.0, -10.0, t1),
    ];
    window.run_sent(page(&text), at, &sent, t1);
    let (_, statuses) = window.run_sent(page(&text), at, &[pad.changed(-30.0, -5.0, t1)], t1);
    assert_eq!(statuses, vec![event::Status::Captured], "the view scrolled");
    assert_eq!(window.scroll(page(&text)), 5.0);
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

// --------------------------------------------------- the toolbar's look

#[test]
fn the_toolbar_fills_its_superellipse_seventy_percent_over_what_lies_behind() {
    let mut window = window();
    let value = "alpha beta";
    let anchor = Point::new(TEXT_X + 3.0, MIDDLE);
    // The menu away from the pointer: nothing hovered.
    window.run(field(value), anchor, &right_click(), Instant::now());
    window.run(field(value), Point::new(390.0, 190.0), &[], Instant::now());
    let menu = toolbar::place(anchor, toolbar::size(2), window.size);
    // Past the labels, on the box's middle and between its items: the
    // 70 % fill over the window's black (and the shadow's black under it).
    let x = (menu.x + toolbar::WIDTH - 20.0) as u32;
    let y = (menu.y + toolbar::PADDING + toolbar::item_height()) as u32;
    assert!(near(
        window.pixel(x, y),
        over(toolbar::BACKGROUND, BACKGROUND)
    ));
    // The corner itself is outside the shape: the window's.
    let corner = window.pixel(menu.x as u32, menu.y as u32);
    assert!(!near(corner, toolbar::BORDER), "{corner:?}");
}

/// The toolbar's outline at `bounds` (`RoundedSuperellipseBorder`), its
/// cubics cut fine into a polygon.
fn outline_polygon(bounds: Rectangle) -> Vec<Point> {
    let (start, cubics) = crate::ui::core::widgets::rsuperellipse::outline(bounds, toolbar::RADIUS);
    let mut points = vec![start];
    let mut from = start;
    for [a, b, to] in cubics {
        for step in 1..=64 {
            let t = step as f32 / 64.0;
            let u = 1.0 - t;
            let at = |p0: f32, p1: f32, p2: f32, p3: f32| {
                u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
            };
            points.push(Point::new(
                at(from.x, a.x, b.x, to.x),
                at(from.y, a.y, b.y, to.y),
            ));
        }
        from = to;
    }
    points
}

/// How far `point` lies inside `polygon`'s edge (negative: outside).
fn depth_in(polygon: &[Point], point: Point) -> f32 {
    let mut inside = false;
    let mut distance = f32::INFINITY;
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        if (a.y > point.y) != (b.y > point.y)
            && point.x < a.x + (point.y - a.y) / (b.y - a.y) * (b.x - a.x)
        {
            inside = !inside;
        }
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let length = dx * dx + dy * dy;
        let t = if length > 0.0 {
            (((point.x - a.x) * dx + (point.y - a.y) * dy) / length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (ex, ey) = (a.x + t * dx - point.x, a.y + t * dy - point.y);
        distance = distance.min((ex * ex + ey * ey).sqrt());
    }
    if inside {
        distance
    } else {
        -distance
    }
}

#[test]
fn the_toolbar_cuts_its_fill_and_border_at_its_outline_with_a_hard_edge() {
    // `Container(clipBehavior: Clip.hardEdge)` clipped the box's fill and
    // border (antialiased, as drawn) to its outline without antialiasing:
    // a pixel whose center lies outside shows none of them, however much
    // of it they cover, and one inside shows them as drawn.
    let mut window = window();
    let value = "alpha beta";
    let anchor = Point::new(TEXT_X + 3.0, MIDDLE);
    window.run(field(value), anchor, &right_click(), Instant::now());
    window.run(field(value), Point::new(390.0, 190.0), &[], Instant::now());
    let size = toolbar::size(2);
    let bounds = Rectangle::new(toolbar::place(anchor, size, window.size), size);
    let outline = outline_polygon(bounds);
    let mut cut = 0;
    // The bottom corners, below the field, over the window's black (and
    // the shadow's, black too).
    let bottom = (bounds.y + bounds.height) as u32;
    for left in [bounds.x as u32 - 2, (bounds.x + bounds.width) as u32 - 8] {
        for y in bottom - 8..bottom + 2 {
            for x in left..left + 10 {
                let depth = depth_in(&outline, Point::new(x as f32 + 0.5, y as f32 + 0.5));
                if depth.abs() < 0.25 {
                    // On the edge, to within how the rasterizer cuts it.
                    continue;
                }
                let drawn = window.pixel(x, y);
                if depth < 0.0 {
                    assert_eq!(drawn, BACKGROUND, "({x}, {y}), {:.2} outside", -depth);
                    // The antialiased fill and border reach this one.
                    cut += usize::from(depth > -0.7);
                } else if depth < 1.0 {
                    assert_ne!(drawn, BACKGROUND, "({x}, {y}), {depth:.2} inside");
                }
            }
        }
    }
    assert!(cut >= 4, "{cut} pixels cut");
}

/// The backdrop's colors either side of its edge (8-bit exact, muted
/// enough that the saturation boost clamps nothing).
const LEFT: Color = Color::from_rgb8(153, 128, 102);
const RIGHT: Color = Color::from_rgb8(77, 102, 128);

/// `LEFT` up to column `edge`, `RIGHT` from it on, filling the window.
fn backdrop<'a, M: 'a>(edge: f32) -> Element<'a, M> {
    iced::widget::row![
        container(Space::new().width(edge).height(Length::Fill)).style(theme::fill(LEFT)),
        container(Space::new().width(Length::Fill).height(Length::Fill)).style(theme::fill(RIGHT)),
    ]
    .into()
}

#[test]
fn the_toolbar_blurs_what_lies_behind_it() {
    let mut window = window();
    window.board = Board(Some("Z".into()));
    let value = "alpha beta gamma";
    let anchor = Point::new(TEXT_X + width("alpha be"), MIDDLE);
    let menu = toolbar::place(anchor, toolbar::size(3), window.size);
    // A sharp edge behind the menu, well right of its labels.
    let edge = (menu.x + 150.0).round();
    let view = || iced::widget::stack![backdrop::<Msg>(edge), field(value)];
    window.run(view(), anchor, &right_click(), Instant::now());
    window.run(view(), Point::new(390.0, 190.0), &[], Instant::now());
    // In the box's bottom padding, where the blur reaches no higher than
    // the backdrop below the field (and the box's shadow is the same all
    // along the row): the edge comes out as a ramp over many pixels.
    let y = menu.y + toolbar::size(3).height - 3.0;
    assert!(
        y - 3.0 * toolbar::BLUR_SIGMA > 46.0,
        "the field is out of reach"
    );
    let at = |dx: f32| window.pixel((edge + dx) as u32, y as u32);
    let ramp: Vec<Color> = [-40.0, -20.0, 0.0, 20.0, 40.0]
        .into_iter()
        .map(at)
        .collect();
    for pair in ramp.windows(2) {
        // Blue rises and red falls across it, step by step.
        assert!(pair[0].b < pair[1].b && pair[0].r > pair[1].r, "{ramp:?}");
    }
    // Either side of the edge itself, the two columns barely differ, where
    // unblurred they would show the two colors under the fill.
    let step = (at(-1.0).r - at(0.0).r).abs();
    let sharp = over(toolbar::BACKGROUND, LEFT).r - over(toolbar::BACKGROUND, RIGHT).r;
    assert!(step < 4.0 / 255.0 && sharp > 20.0 / 255.0, "{step} {sharp}");
}

/// A window filled by [`backdrop`] at `edge`, filtered as the toolbar
/// filters (`BackdropFilter`) inside `clip`.
struct Probe {
    edge: f32,
    clip: Rectangle,
}

impl iced::advanced::Widget<(), iced::Theme, Renderer> for Probe {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut iced::advanced::widget::Tree,
        _renderer: &Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        iced::advanced::layout::Node::new(limits.max())
    }

    fn draw(
        &self,
        _tree: &iced::advanced::widget::Tree,
        renderer: &mut Renderer,
        _theme: &iced::Theme,
        _style: &core_renderer::Style,
        layout: iced::advanced::Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &iced::Rectangle,
    ) {
        use crate::ui::core::widgets::opacity_layer::{BackdropFilter, Effects as _};
        use iced::advanced::Renderer as _;
        let bounds = layout.bounds();
        let (left, right) = (
            Rectangle {
                width: self.edge,
                ..bounds
            },
            Rectangle {
                x: self.edge,
                width: bounds.width - self.edge,
                ..bounds
            },
        );
        for (bounds, color) in [(left, LEFT), (right, RIGHT)] {
            renderer.fill_quad(
                core_renderer::Quad {
                    bounds,
                    ..core_renderer::Quad::default()
                },
                color,
            );
        }
        renderer.draw_backdrop_filter(
            &iced::widget::canvas::Path::rectangle(self.clip.position(), self.clip.size()),
            BackdropFilter {
                blur_sigma: toolbar::BLUR_SIGMA,
                color_matrix: BackdropFilter::saturation_matrix(toolbar::SATURATION),
            },
        );
    }
}

/// What the filter makes of column `x` of [`backdrop`] at `edge` in a
/// window `width` wide, computed afresh: a Gaussian blur of standard
/// deviation 20 cut off at 3 of them (each column beyond the window's
/// edge the nearest one inside), then Cupertino's saturation matrix at 3.
fn filtered(x: i32, edge: i32, width: i32) -> Color {
    let sigma = f64::from(toolbar::BLUR_SIGMA);
    let reach = (3.0 * sigma).ceil() as i32;
    let weight = |k: i32| (-f64::from(k * k) / (2.0 * sigma * sigma)).exp();
    let total: f64 = (-reach..=reach).map(weight).sum();
    let mut sum = [0.0_f64; 3];
    for k in -reach..=reach {
        let column = (x + k).clamp(0, width - 1);
        let c = if column < edge { LEFT } else { RIGHT };
        for (s, v) in sum.iter_mut().zip([c.r, c.g, c.b]) {
            *s += weight(k) / total * f64::from(v);
        }
    }
    let s = f64::from(toolbar::SATURATION);
    let (r, g, b) = (0.213 * (1.0 - s), 0.715 * (1.0 - s), 0.072 * (1.0 - s));
    let row = |m: [f64; 3]| (m[0] * sum[0] + m[1] * sum[1] + m[2] * sum[2]).clamp(0.0, 1.0) as f32;
    Color::from_rgb(row([r + s, g, b]), row([r, g + s, b]), row([r, g, b + s]))
}

#[test]
fn the_backdrop_filter_is_a_gaussian_blur_then_the_saturation_matrix() {
    let size = Size::new(200.0, 100.0);
    let (edge, clip) = (
        60.0,
        Rectangle::new(Point::new(0.0, 30.0), Size::new(150.0, 40.0)),
    );
    let mut frames = crate::ui::core::widgets::animated::testing::Frames::new(size);
    frames.frame(
        Element::<()>::new(Probe { edge, clip }),
        &[],
        Instant::now(),
    );
    // Inside the clip: the filtered backdrop, near the window's edge (where
    // the blur takes the edge column) as well as across the colors' edge.
    for x in [0, 2, 20, 45, 59, 60, 61, 80, 120, 149] {
        let drawn = frames.pixel(Point::new(x as f32, 50.0));
        let expected = filtered(x, edge as i32, size.width as i32);
        assert!(
            near(drawn, expected),
            "column {x}: {drawn:?}, not {expected:?}"
        );
    }
    // A column well away from the edge is its color saturated threefold.
    assert!(!near(frames.pixel(Point::new(2.0, 50.0)), LEFT));
    // Outside it, the backdrop as it was.
    assert_eq!(frames.pixel(Point::new(170.0, 50.0)), RIGHT);
    assert_eq!(frames.pixel(Point::new(20.0, 10.0)), LEFT);
    assert_eq!(frames.pixel(Point::new(100.0, 90.0)), RIGHT);
}

#[test]
fn the_saturation_matrix_is_cupertinos() {
    use crate::ui::core::widgets::opacity_layer::BackdropFilter;
    // `_matrixWithSaturation(3)`.
    let m = BackdropFilter::saturation_matrix(3.0);
    let expected = [
        2.574, -1.43, -0.144, 0.0, 0.0, //
        -0.426, 1.57, -0.144, 0.0, 0.0, //
        -0.426, -1.43, 2.856, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0,
    ];
    for (a, b) in m.iter().zip(expected) {
        assert!((a - b).abs() < 1e-5, "{m:?}");
    }
}

/// The macOS system UI font Flutter set the labels in, on macOS, where the
/// font system loads `/System/Library/Fonts`: its face is there, under a
/// family name (a hidden one, `.SF NS`) that a query by that name finds.
#[cfg(target_os = "macos")]
#[test]
fn the_system_ui_font_is_reachable_by_its_family_name() {
    use iced::advanced::graphics::text::cosmic_text::fontdb::{Family, Query, Source};
    use iced::advanced::graphics::text::font_system;
    let mut fonts = font_system().write().expect("the font system");
    let db = fonts.raw().db();
    let path = |source: &Source| match source {
        Source::File(path) | Source::SharedFile(path, _) => Some(path.clone()),
        Source::Binary(_) => None,
    };
    let sf = db
        .faces()
        .find(|face| {
            path(&face.source).is_some_and(|p| p.ends_with("System/Library/Fonts/SFNS.ttf"))
        })
        .expect("SFNS.ttf, the system UI font, loaded from /System/Library/Fonts");
    let family = sf.families[0].0.clone();
    let found = db
        .query(&Query {
            families: &[Family::Name(&family)],
            ..Query::default()
        })
        .and_then(|id| db.face(id))
        .expect("a face of the family");
    assert_eq!(path(&found.source), path(&sf.source), "{family}");
}
