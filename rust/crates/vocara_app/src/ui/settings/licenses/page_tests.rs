//! The license page as it is laid out, drawn and moved: a window that
//! rebuilds the page's view each frame into the last frame's widget tree
//! (as the app does), hands the page what it reports, and can be read back
//! pixel by pixel or asked where a text is.

use std::borrow::Cow;
use std::sync::Once;
use std::time::Duration;

use iced::advanced::clipboard;
use iced::advanced::renderer::{self as core_renderer, Headless};
use iced::advanced::widget::operation::{self, Outcome};
use iced::advanced::widget::Operation;
use iced::time::Instant;
use iced::{mouse, window, Color, Event, Point, Rectangle, Renderer, Size};
use iced_test::runtime::user_interface::{self, UserInterface};
use iced_test::selector::{Candidate, Selector, Target};
use iced_winit::gesture::{self, Phase, Wheel};

use super::list_tile::{two_line, SUBTITLE_SIZE, TITLE_SIZE};
use super::sheet::SCROLLED_UNDER_TINT;
use super::*;
use crate::ui::core::icons;
use crate::ui::core::theme::{app_theme, font, DEFAULT_TEXT_SIZE};
use crate::ui::core::widgets::animated::testing::{near, over};
use crate::ui::core::widgets::pan_zoom::testing::{set_event_time, Sent, Trackpad};
use crate::ui::home::testing::run_task;

/// A millisecond.
const MS: Duration = Duration::from_millis(1);

/// A 1440 × 900 window: the page is all of it.
const SIZE: Size = Size::new(1440.0, 900.0);

/// Where the detail card starts: right under the page's toolbar.
fn card_top() -> f32 {
    crate::ui::core::widgets::toolbar::HEIGHT
}

/// The card's left and right edges: from the master's 320 to the gutter
/// and the card's margin (24 + 4) off the right.
const CARD_LEFT: f32 = 320.0;
const CARD_RIGHT: f32 = 1440.0 - 28.0;

/// The about block's height: 24 + 20 + 14 + 18 + 12 + 18 + 14 + 24.
const ABOUT: f32 = 144.0;

/// The app bar's height: the page's toolbar and the 56 px action row.
const APP_BAR: f32 = crate::ui::core::widgets::toolbar::HEIGHT + 56.0;

/// "first" (three licenses, the first long enough to scroll), "second"
/// (two) and enough more packages (one each) to scroll the list.
fn data() -> &'static LicenseData {
    let long: String = (0..60)
        .map(|i| format!("Paragraph {i} of a long license.\n\n"))
        .collect();
    let entry = |package: &str, text: &str| LicenseEntry {
        packages: vec![package.to_string()],
        text: text.to_string(),
    };
    let mut entries = vec![
        entry("first", &long),
        entry("first", "Extra one"),
        entry("first", "Extra two"),
        entry("second", "Second license"),
        entry("second", "Another"),
    ];
    for i in 3..30 {
        entries.push(entry(&format!("z-{i:02}"), "MIT"));
    }
    Box::leak(Box::new(LicenseData::collect(entries)))
}

fn lines(y: f32) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x: 0.0, y },
    })
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

/// The `n`-th text reading `label`.
fn nth(label: &str, n: usize) -> impl FnMut(Candidate<'_>) -> Option<Target> + Send {
    let label = label.to_string();
    let mut seen = 0;
    move |candidate: Candidate<'_>| {
        let hit = matches!(&candidate, Candidate::Text { content, .. } if *content == label);
        if !hit {
            return None;
        }
        seen += 1;
        (seen == n + 1).then(|| Target::from(candidate))
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.05
}

/// See the module docs.
struct Window {
    renderer: Renderer,
    cache: user_interface::Cache,
    cursor: Point,
    pixels: Vec<u8>,
}

impl Window {
    fn new() -> Self {
        static FONTS: Once = Once::new();
        FONTS.call_once(|| {
            font::install();
            iced_test::renderer::graphics::text::font_system()
                .write()
                .expect("the font system")
                .load_font(Cow::Borrowed(icons::FONT_BYTES));
        });
        let renderer = iced_test::futures::futures::executor::block_on(
            <Renderer as Headless>::new(font::REGULAR, DEFAULT_TEXT_SIZE.into(), None),
        )
        .expect("a headless renderer");
        Self {
            renderer,
            cache: user_interface::Cache::default(),
            cursor: Point::new(-100.0, -100.0),
            pixels: Vec::new(),
        }
    }

    fn point_at(&mut self, at: Point) {
        self.cursor = at;
    }

    /// A frame at `at`: the page moves on to it, its view gets the pointer
    /// and `events` and the frame, and is drawn; then the page handles
    /// what the view reported (which it hands back).
    fn frame(&mut self, page: &mut LicensesPage, events: &[Event], at: Instant) -> Vec<Message> {
        self.frame_with(page, events, &[], at)
    }

    /// [`Window::frame`] for `sent`, wheel events with the gestures the run
    /// loop hands along, handled at `at`.
    fn frame_sent(&mut self, page: &mut LicensesPage, sent: &[Sent], at: Instant) -> Vec<Message> {
        let events: Vec<Event> = sent.iter().map(|(event, _)| event.clone()).collect();
        let wheels: Vec<Option<Wheel>> = sent
            .iter()
            .filter(|(event, _)| matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. })))
            .map(|(_, wheel)| *wheel)
            .collect();
        set_event_time(Some(at));
        let messages = self.frame_with(page, &events, &wheels, at);
        set_event_time(None);
        messages
    }

    fn frame_with(
        &mut self,
        page: &mut LicensesPage,
        events: &[Event],
        wheels: &[Option<Wheel>],
        at: Instant,
    ) -> Vec<Message> {
        page.frame(at);
        let cursor = mouse::Cursor::Available(self.cursor);
        let mut ui = UserInterface::build(
            page.view(),
            SIZE,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut messages = Vec::new();
        let mut all = vec![Event::Mouse(mouse::Event::CursorMoved {
            position: self.cursor,
        })];
        all.extend_from_slice(events);
        let renderer = &mut self.renderer;
        let _ = gesture::update_each(&all, wheels, |events| {
            ui.update(
                events,
                gesture::cursor(cursor),
                renderer,
                &mut clipboard::Null,
                &mut messages,
            )
        });
        let _ = ui.update(
            &[Event::Window(window::Event::RedrawRequested(at))],
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
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
            Size::new(SIZE.width as u32, SIZE.height as u32),
            1.0,
            Color::BLACK,
        );
        self.cache = ui.into_cache();
        for message in &messages {
            page.update(message.clone());
        }
        messages
    }

    /// Where the `n`-th text reading `label` shows.
    fn find(&mut self, page: &LicensesPage, label: &str, n: usize) -> Rectangle {
        let mut ui = UserInterface::build(
            page.view(),
            SIZE,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut finder = nth(label, n).find();
        ui.operate(&self.renderer, &mut operation::black_box(&mut finder));
        self.cache = ui.into_cache();
        match finder.finish() {
            Outcome::Some(Some(target)) => target
                .visible_bounds()
                .unwrap_or_else(|| panic!("{label} #{n} is out of sight")),
            _ => panic!("{label} #{n} is not on the page"),
        }
    }

    fn pixel(&self, at: Point) -> Color {
        let (x, y) = (at.x as usize, at.y as usize);
        let i = (y * SIZE.width as usize + x) * 4;
        let p = &self.pixels[i..i + 4];
        Color::from_rgba8(p[0], p[1], p[2], f32::from(p[3]) / 255.0)
    }

    /// The brightest channel drawn anywhere in `from`..`to`.
    fn ink(&self, from: Point, to: Point) -> u8 {
        let mut most = 0;
        for y in from.y as usize..to.y as usize {
            for x in from.x as usize..to.x as usize {
                let [r, g, b, _] = self.pixel(Point::new(x as f32, y as f32)).into_rgba8();
                most = most.max(r).max(g).max(b);
            }
        }
        most
    }
}

/// The page over [`data`], its first package's detail all in and its
/// licenses filled in, at the returned time.
fn settled(window: &mut Window) -> (LicensesPage, Instant) {
    let mut page = LicensesPage::over(data());
    let t0 = Instant::now();
    // A license fills in a frame.
    for i in 0..3 {
        window.frame(&mut page, &[], t0 + i * MS);
    }
    let t = t0 + DETAIL_TRANSITION;
    window.frame(&mut page, &[], t);
    assert!(!page.is_animating(t), "settled");
    (page, t)
}

// ------------------------------------------------------------ the titles

// The page's title sits in its toolbar after the back button (as every
// page's does); the detail's `SliverAppBar` centres its own (macOS centres
// the title of an app bar with fewer than two actions).
#[test]
fn the_toolbar_and_the_detail_header_place_their_titles() {
    use crate::ui::core::widgets::toolbar::{HEIGHT, LEADING_INSET};
    let mut window = Window::new();
    let (page, _) = settled(&mut window);
    let title = window.find(&page, "Licenses", 0);
    assert!(close(title.x, LEADING_INSET + 28.0 + 8.0), "{title:?}");
    assert!(
        (title.center_y() - (HEIGHT - 1.0) / 2.0).abs() < 1.0,
        "{title:?}"
    );

    // The header's column: the package over its count, no gap, both at the
    // column's start, the column centred on the card and in the toolbar.
    let name = window.find(&page, "first", 1);
    let count = window.find(&page, "3 licenses.", 1);
    assert!(close(name.x, count.x), "{name:?} {count:?}");
    let column = name.width.max(count.width);
    let card_centre = (CARD_LEFT + CARD_RIGHT) / 2.0;
    assert!(
        (name.x + column / 2.0 - card_centre).abs() < 0.5,
        "{name:?}"
    );
    assert!(close(name.y, card_top() + 12.0), "{name:?}");
    assert!(close(count.y, name.y + 18.0), "{count:?}");
}

// --------------------------------------------------------------- divider

// `Divider()` takes forui's `dividerTheme` color, `secondary`.
#[test]
fn the_divider_is_forui_secondary() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    window.frame(&mut page, &[], t + 16 * MS);
    // Its line: 18 + 7.5 into the body, under the 56 px header.
    let line = card_top() + 56.0 + 25.5;
    let row = line.ceil();
    let drawn = window.pixel(Point::new(800.0, row));
    let [r, g, b, _] = drawn.into_rgba8();
    let [sr, sg, sb, _] = Studio::SURFACE.into_rgba8();
    let [lr, lg, lb, _] = Zinc::SECONDARY.into_rgba8();
    for (c, s, l) in [(r, sr, lr), (g, sg, lg), (b, sb, lb)] {
        assert!(
            c >= s.min(l) && c <= s.max(l),
            "{drawn:?} between the card and the line"
        );
    }
    assert!(r >= sr + 4, "a line is drawn: {drawn:?}");
    // Its ends: the body's 24 padding and the divider's 18.
    assert_eq!(
        window.pixel(Point::new(CARD_LEFT + 24.0 + 18.0 - 2.0, row)),
        window.pixel(Point::new(CARD_LEFT + 10.0, row)),
        "none in the padding"
    );
}

// ------------------------------------------------------------ the rows

// A two-line `ListTile` at the desktop's compact density.
#[test]
fn the_package_rows_are_64_tall_with_their_baselines_at_32_and_48() {
    let mut window = Window::new();
    let (page, _) = settled(&mut window);
    let first = window.find(&page, "first", 0);
    let count = window.find(&page, "3 licenses.", 0);
    let second = window.find(&page, "second", 0);
    let (title_y, subtitle_y, height) = two_line(TITLE_SIZE, SUBTITLE_SIZE);
    assert_eq!(height, 64.0);
    let top = APP_BAR + ABOUT;
    assert!(close(first.y, top + title_y), "{first:?}");
    assert!(close(count.y, top + subtitle_y), "{count:?}");
    assert!(close(second.y, first.y + 64.0), "{second:?}");
    assert!(close(first.x, 16.0), "{first:?}");
}

// The `InkWell`'s hover: the theme's `hoverColor` in forui's rounded tile
// shape, fading in over 50 ms, over the selected row's `highlightColor`
// too.
#[test]
fn a_row_lights_under_the_pointer_in_a_rounded_rectangle() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    let top = APP_BAR + ABOUT + 64.0; // "second", not selected
    let inside = Point::new(250.0, top + 5.0);
    let corner = Point::new(0.0, top);
    assert!(near(window.pixel(inside), Studio::SURFACE), "at rest");
    window.point_at(inside);
    window.frame(&mut page, &[], t);
    assert!(near(window.pixel(inside), Studio::SURFACE), "starts clear");
    window.frame(&mut page, &[], t + 25 * MS);
    let half = Color {
        a: TILE_HOVER.a / 2.0,
        ..TILE_HOVER
    };
    assert!(
        near(window.pixel(inside), over(half, Studio::SURFACE)),
        "half"
    );
    window.frame(&mut page, &[], t + 50 * MS);
    let hovered = over(TILE_HOVER, Studio::SURFACE);
    assert!(near(window.pixel(inside), hovered), "in");
    assert!(near(window.pixel(corner), Studio::SURFACE), "rounded 8");

    // The selected row lights the same, over its highlight.
    let selected_top = APP_BAR + ABOUT;
    let on_selected = Point::new(250.0, selected_top + 5.0);
    window.point_at(on_selected);
    window.frame(&mut page, &[], t + 100 * MS);
    window.frame(&mut page, &[], t + 150 * MS);
    let under = over(INK_HIGHLIGHT, Studio::SURFACE);
    assert!(near(window.pixel(on_selected), over(TILE_HOVER, under)));
    assert!(
        near(window.pixel(inside), Studio::SURFACE),
        "the other let go"
    );
}

// The `InkWell`'s pressed highlight: the theme's `highlightColor` over the
// hover, in over 200 ms and out over 200 ms once the tap has picked it.
#[test]
fn a_pressed_row_highlights_and_lets_go_once_picked() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    let top = APP_BAR + ABOUT + 64.0;
    let inside = Point::new(250.0, top + 5.0);
    window.point_at(inside);
    window.frame(&mut page, &[], t);
    window.frame(&mut page, &[], t + 50 * MS);
    let t1 = t + 100 * MS;
    window.frame(&mut page, &[press()], t1);
    window.frame(&mut page, &[], t1 + 100 * MS);
    let hovered = over(TILE_HOVER, Studio::SURFACE);
    let pressed = |a: f32| {
        over(
            Color {
                a: INK_HIGHLIGHT.a * a,
                ..INK_HIGHLIGHT
            },
            hovered,
        )
    };
    assert!(near(window.pixel(inside), pressed(0.5)), "half");
    window.frame(&mut page, &[], t1 + 200 * MS);
    assert!(near(window.pixel(inside), pressed(1.0)), "in");
    let t2 = t1 + 300 * MS;
    let reported = window.frame(&mut page, &[release()], t2);
    assert_eq!(reported, vec![Message::Select(1)]);
    assert_eq!(page.selected(), 1);
    // Selected now: its highlight under the hover, the press fading out.
    window.frame(&mut page, &[], t2 + 200 * MS);
    let selected = over(TILE_HOVER, over(INK_HIGHLIGHT, Studio::SURFACE));
    assert!(
        near(window.pixel(inside), selected),
        "{:?}",
        window.pixel(inside)
    );
}

// -------------------------------------------------------- the back button

#[test]
fn the_back_button_is_the_toolbar_s() {
    use crate::ui::core::widgets::toolbar::{HEIGHT, LEADING_INSET};
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    // The 28 px icon button first in the toolbar, centred on its row.
    let middle = (HEIGHT - 1.0) / 2.0;
    let centre = Point::new(LEADING_INSET + 14.0, middle);
    let inside = Point::new(LEADING_INSET + 3.0, middle);
    // Where its tooltip's text will show, 6 under it.
    let tip = (
        Point::new(LEADING_INSET - 8.0, middle + 14.0 + 7.0),
        Point::new(LEADING_INSET + 36.0, middle + 14.0 + 22.0),
    );
    let before = window.ink(tip.0, tip.1);
    assert!(near(window.pixel(inside), Studio::TOOLBAR), "at rest");
    // The chevron, in `TEXT_SECONDARY`.
    let ink = window.ink(
        Point::new(centre.x - 4.0, centre.y - 4.0),
        Point::new(centre.x + 2.0, centre.y + 4.0),
    );
    assert!(ink >= 0x80, "the chevron: {ink:#x}");

    window.point_at(centre);
    window.frame(&mut page, &[], t);
    assert!(near(window.pixel(inside), Studio::HOVER), "the hover fill");
    assert!(
        near(
            window.pixel(Point::new(LEADING_INSET - 2.0, middle)),
            Studio::TOOLBAR
        ),
        "28 wide"
    );

    // Its tooltip, faded in over 150 ms: its text's light ink.
    window.frame(&mut page, &[], t + 150 * MS);
    let ink = window.ink(tip.0, tip.1);
    assert!(
        ink >= 0xC0 && ink > before,
        "Back: {ink:#x} over {before:#x}"
    );

    let t1 = t + 200 * MS;
    window.frame(&mut page, &[press()], t1);
    let reported = window.frame(&mut page, &[release()], t1 + 50 * MS);
    assert_eq!(reported, vec![Message::Back]);
}

// -------------------------------------------------------------- loading

// The `FutureBuilder` waits on the registry: the about block over a
// spinner; then the list, and the first package's detail comes in.
#[test]
fn the_registry_loads_under_a_spinner_then_the_first_detail_comes_in() {
    let mut page = LicensesPage::new();
    assert!(!page.is_loaded());
    {
        let mut ui = iced_test::simulator(page.view());
        assert!(ui.find("Licenses").is_ok());
        assert!(ui.find("Powered by iced").is_ok());
        assert!(ui.find("first").is_err(), "no list yet");
    }
    // (On a page of its own: the frame is ahead of the clock.)
    let mut window = Window::new();
    window.frame(&mut LicensesPage::new(), &[], Instant::now() + 300 * MS);
    let spinner = window.ink(
        Point::new(142.0, APP_BAR + ABOUT),
        Point::new(178.0, APP_BAR + ABOUT + 36.0),
    );
    assert!(spinner >= 0xD0, "the spinner: {spinner:#x}");

    // Gathered off the UI thread.
    let gathered = run_task(LicensesPage::load());
    assert_eq!(
        gathered,
        vec![Message::Loaded(Ok(Registry(license_data())))]
    );

    let t0 = Instant::now();
    page.update(Message::Loaded(Ok(Registry(data()))));
    assert!(page.is_loaded());
    let start = Instant::now();
    assert!(page.is_animating(start), "the first detail comes in");
    page.frame(start);
    let (opacity, down) = page.upwards(page.entering.as_ref());
    assert!(
        opacity < 0.01 && down > 0.249,
        "clear, down: {opacity} {down}"
    );
    page.frame(t0 + 2 * DETAIL_TRANSITION);
    assert_eq!(page.upwards(page.entering.as_ref()), (1.0, 0.0));
    let mut ui = iced_test::simulator(page.view());
    assert!(ui.find("second").is_ok());
    // A second gathering changes nothing.
    let entering = page.entering.clone();
    page.update(Message::Loaded(Err("late".into())));
    assert!(iced_test::simulator(page.view()).find("late").is_err());
    assert_eq!(format!("{:?}", page.entering), format!("{entering:?}"));
}

#[test]
fn a_registry_that_fails_shows_why() {
    let mut page = LicensesPage::new();
    page.update(Message::Loaded(Err("no licenses".into())));
    let mut ui = iced_test::simulator(page.view());
    assert!(ui.find("no licenses").is_ok());
    assert!(ui.find("Powered by iced").is_err());
    assert!(!page.is_animating(Instant::now()), "no detail");
}

// --------------------------------------------------------------- the sheet

fn header(window: &mut Window, page: &LicensesPage) -> f32 {
    window.find(page, "first", 1).y
}

fn paragraph(window: &mut Window, page: &LicensesPage) -> f32 {
    window.find(page, "Paragraph 20 of a long license.", 0).y
}

// `_DraggableScrollableSheetScrollPosition.applyUserOffset`: a trackpad's
// drag grows the sheet before it scrolls the list, and shrinks it after;
// a wheel's scroll signal (`pointerScroll`) only scrolls the list.
#[test]
fn a_trackpad_grows_the_sheet_first_and_a_wheel_only_scrolls() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    window.point_at(Point::new(800.0, 500.0));
    let rest = card_top() + 12.0;
    assert!(close(header(&mut window, &page), rest));
    // Slow moves (no fling at the end) 20 ms apart.
    let mut pad = Trackpad::new();
    let mut at = t;
    let mut step = |window: &mut Window, page: &mut LicensesPage, sent: Sent| {
        at += 20 * MS;
        window.frame_sent(page, &[sent], at);
    };
    step(&mut window, &mut page, pad.began(t));
    step(&mut window, &mut page, pad.changed(0.0, -20.0, t + 20 * MS));
    assert!(close(header(&mut window, &page), rest - 20.0), "grows");
    step(
        &mut window,
        &mut page,
        pad.changed(0.0, -100.0, t + 40 * MS),
    );
    assert!(
        close(header(&mut window, &page), 12.0),
        "to the page's height"
    );
    let p0 = paragraph(&mut window, &page);
    step(&mut window, &mut page, pad.changed(0.0, -30.0, t + 60 * MS));
    assert!(close(header(&mut window, &page), 12.0));
    assert!(
        close(paragraph(&mut window, &page), p0 - 30.0),
        "then the list scrolls"
    );
    // Back: the list first, to its top (the rest of that move is lost),
    // then the sheet.
    step(&mut window, &mut page, pad.changed(0.0, 40.0, t + 80 * MS));
    assert!(close(paragraph(&mut window, &page), p0));
    assert!(close(header(&mut window, &page), 12.0));
    step(&mut window, &mut page, pad.changed(0.0, 10.0, t + 100 * MS));
    assert!(close(header(&mut window, &page), 22.0), "shrinks");
    // Still a while, then the fingers lift: no fling.
    at += 100 * MS;
    window.frame_sent(&mut page, &[pad.ended(at)], at);
    window.frame(&mut page, &[], at + 16 * MS);
    assert!(close(header(&mut window, &page), 22.0));
    // A wheel's lines, and a precise device's points without phases, are
    // scroll signals: they scroll the list, the sheet stays.
    window.frame_sent(&mut page, &[pad.lines(0.0, -1.0, at)], at + 32 * MS);
    assert!(close(header(&mut window, &page), 22.0));
    let p1 = paragraph(&mut window, &page);
    assert!(p1 < p0 + 10.0 - 1.0, "the list moved");
    window.frame_sent(&mut page, &[pad.pixels(0.0, -25.0, at)], at + 48 * MS);
    assert!(close(header(&mut window, &page), 22.0));
    assert!(close(paragraph(&mut window, &page), p1 - 25.0));
    // Not over the card: nothing.
    window.point_at(Point::new(CARD_LEFT - 30.0, 30.0));
    let off = at + 64 * MS;
    window.frame_sent(&mut page, &[pad.began(off)], off);
    window.frame_sent(&mut page, &[pad.changed(0.0, 5.0, off)], off);
    window.frame_sent(&mut page, &[pad.ended(off)], off);
    assert!(close(header(&mut window, &page), 22.0));
}

// Flutter's own fling, not macOS's momentum: the fingers lift fast, the
// sheet flies on (`ClampingScrollSimulation`) to its full height and the
// list takes the velocity left; the momentum events move nothing.
#[test]
fn a_trackpad_flick_flings_the_sheet_then_the_list_and_its_momentum_moves_nothing() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    window.point_at(Point::new(800.0, 500.0));
    let rest = card_top() + 12.0;
    let mut pad = Trackpad::new();
    // Up 6 px each 8 ms (750 px/s), 30 px in all.
    window.frame_sent(&mut page, &[pad.began(t)], t);
    let mut at = t;
    for _ in 0..5 {
        at += 8 * MS;
        window.frame_sent(&mut page, &[pad.changed(0.0, -6.0, at)], at);
    }
    assert!(close(header(&mut window, &page), rest - 30.0));
    // The list's scroll: how far the text has come up its card.
    let gap =
        |window: &mut Window, page: &LicensesPage| paragraph(window, page) - header(window, page);
    let g0 = gap(&mut window, &page);
    let up = at + 2 * MS;
    window.frame_sent(&mut page, &[pad.ended(up)], up);
    // The momentum macOS sends then, which would undo it all: nothing.
    window.frame_sent(
        &mut page,
        &[pad.momentum(Phase::Began, 0.0, 300.0, up)],
        up + MS,
    );
    // The fling's first frame starts it; it then grows the sheet.
    window.frame(&mut page, &[], up + 16 * MS);
    window.frame(&mut page, &[], up + 32 * MS);
    let grown = header(&mut window, &page);
    assert!(grown < rest - 30.0 - 1.0, "flung on: {grown}");
    window.frame_sent(
        &mut page,
        &[pad.momentum(Phase::Changed, 0.0, 300.0, up)],
        up + 40 * MS,
    );
    // A 750 px/s fling would travel 118 px: the sheet's 24 px of room,
    // then the list flung afresh with the velocity left (some 640 px/s,
    // about 90 px).
    let mut now = up + 48 * MS;
    for _ in 0..30 {
        window.frame(&mut page, &[], now);
        now += 32 * MS;
    }
    assert!(close(header(&mut window, &page), 12.0), "full height");
    let scrolled = g0 - gap(&mut window, &page);
    assert!(
        scrolled > 70.0 && scrolled < 110.0,
        "the list took the rest: {scrolled}"
    );
    window.frame_sent(&mut page, &[pad.momentum(Phase::Ended, 0.0, 0.0, now)], now);
    window.frame(&mut page, &[], now + 32 * MS);
    assert!(close(g0 - gap(&mut window, &page), scrolled), "settled");
}

// A swipe on the card holds the position: a fling under way stops.
#[test]
fn a_new_swipe_stops_a_fling() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    window.point_at(Point::new(800.0, 500.0));
    let mut pad = Trackpad::new();
    // The sheet drawn up whole, the fingers still before they lift: no
    // fling.
    window.frame_sent(
        &mut page,
        &[pad.began(t), pad.changed(0.0, -100.0, t), pad.ended(t)],
        t,
    );
    assert!(close(header(&mut window, &page), 12.0));
    // A flick of the list: 1000 px/s.
    let t1 = t + 100 * MS;
    window.frame_sent(&mut page, &[pad.began(t1)], t1);
    let mut at = t1;
    for _ in 0..3 {
        at += 8 * MS;
        window.frame_sent(&mut page, &[pad.changed(0.0, -8.0, at)], at);
    }
    let up = at + 2 * MS;
    window.frame_sent(&mut page, &[pad.ended(up)], up);
    let p0 = paragraph(&mut window, &page);
    window.frame(&mut page, &[], up + 16 * MS);
    window.frame(&mut page, &[], up + 32 * MS);
    let flung = paragraph(&mut window, &page);
    assert!(flung < p0 - 5.0, "the list flies on: {p0} to {flung}");
    // Two fingers touch it: it stops where it is.
    window.frame_sent(&mut page, &[pad.began(up + 40 * MS)], up + 40 * MS);
    let held = paragraph(&mut window, &page);
    for i in 0..5 {
        window.frame(&mut page, &[], up + (56 + 32 * i) * MS);
    }
    assert!(close(paragraph(&mut window, &page), held), "it stopped");
}

// A finger touching the trackpad while macOS's momentum runs (an inertia
// cancel): the list's `pointerScroll(0)` goes idle, and its
// `beginActivity` stops the sheet's own simulation as well as the list's.
#[test]
fn a_finger_on_the_trackpad_stops_the_sheets_fling_over_the_card_alone() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    window.point_at(Point::new(800.0, 500.0));
    let rest = card_top() + 12.0;
    let mut pad = Trackpad::new();
    // The flick that flings the sheet (750 px/s, 30 px), lifted.
    window.frame_sent(&mut page, &[pad.began(t)], t);
    let mut at = t;
    for _ in 0..5 {
        at += 8 * MS;
        window.frame_sent(&mut page, &[pad.changed(0.0, -6.0, at)], at);
    }
    let up = at + 2 * MS;
    window.frame_sent(&mut page, &[pad.ended(up)], up);
    window.frame_sent(
        &mut page,
        &[pad.momentum(Phase::Began, 0.0, 300.0, up)],
        up + 16 * MS,
    );
    // Read while the fling is still on its way: 24 ms in, short of the
    // sheet's full height (it has 22 px left to go under the toolbar).
    let changed = up + 24 * MS;
    window.frame_sent(
        &mut page,
        &[pad.momentum(Phase::Changed, 0.0, 300.0, changed)],
        changed,
    );
    let grown = header(&mut window, &page);
    assert!(
        grown < rest - 30.0 - 1.0 && grown > 12.0 + 1.0,
        "the sheet's own fling: {grown}"
    );
    // A finger lands, the pointer over the card: the sheet stays where
    // that frame left it, short of the page's height.
    let touched = changed + 20 * MS;
    let cancel = pad.touch(touched).expect("within 50 ms of the change");
    window.frame_sent(&mut page, &[cancel], touched);
    for i in 1..=5 {
        window.frame(&mut page, &[], touched + 32 * i * MS);
    }
    assert!(close(header(&mut window, &page), grown), "it stopped");

    // The sheet drawn up whole, the list flicked (1000 px/s).
    let t1 = touched + 500 * MS;
    window.frame_sent(
        &mut page,
        &[pad.began(t1), pad.changed(0.0, -100.0, t1), pad.ended(t1)],
        t1,
    );
    assert!(close(header(&mut window, &page), 12.0));
    let t2 = t1 + 100 * MS;
    window.frame_sent(&mut page, &[pad.began(t2)], t2);
    let mut at = t2;
    for _ in 0..3 {
        at += 8 * MS;
        window.frame_sent(&mut page, &[pad.changed(0.0, -8.0, at)], at);
    }
    let up = at + 2 * MS;
    window.frame_sent(&mut page, &[pad.ended(up)], up);
    let changed = up + 16 * MS;
    window.frame_sent(
        &mut page,
        &[pad.momentum(Phase::Changed, 0.0, 300.0, changed)],
        changed,
    );
    // A finger with the pointer over the master list (where AppKit saw
    // it): the list flies on.
    let flying = paragraph(&mut window, &page);
    let touched = changed + 10 * MS;
    let cancel = pad
        .touch_at(touched, Point::new(CARD_LEFT - 100.0, 500.0))
        .expect("within 50 ms of the change");
    window.frame_sent(&mut page, &[cancel], touched);
    window.frame(&mut page, &[], touched + 16 * MS);
    let on = paragraph(&mut window, &page);
    assert!(on < flying - 1.0, "flies on: {flying} to {on}");
    // The momentum changes again; a finger over the card stops it.
    let changed = touched + 20 * MS;
    window.frame_sent(
        &mut page,
        &[pad.momentum(Phase::Changed, 0.0, 300.0, changed)],
        changed,
    );
    let touched = changed + 10 * MS;
    let cancel = pad.touch(touched).expect("a cancel");
    window.frame_sent(&mut page, &[cancel], touched);
    let held = paragraph(&mut window, &page);
    for i in 1..=5 {
        window.frame(&mut page, &[], touched + 32 * i * MS);
    }
    assert!(close(paragraph(&mut window, &page), held), "it stopped");
}

// The pinned `SliverAppBar` takes the surface tint once the list is
// scrolled under it.
#[test]
fn the_details_header_tints_once_its_list_is_scrolled() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    let on_header = Point::new(CARD_LEFT + 10.0, card_top() + 6.0);
    assert!(near(window.pixel(on_header), Studio::SURFACE));
    window.point_at(Point::new(800.0, 500.0));
    window.frame(&mut page, &[lines(-1.0)], t);
    window.frame(&mut page, &[], t + 16 * MS);
    let tinted = over(
        Color {
            a: SCROLLED_UNDER_TINT,
            ..Zinc::PRIMARY
        },
        Studio::SURFACE,
    );
    assert!(
        near(window.pixel(on_header), tinted),
        "{:?}",
        window.pixel(on_header)
    );
    window.frame(&mut page, &[lines(5.0)], t + 32 * MS);
    window.frame(&mut page, &[], t + 48 * MS);
    assert!(
        near(window.pixel(on_header), Studio::SURFACE),
        "back at the top"
    );
}

// The app bar listens to the package list (`ScrollNotificationObserver`):
// scrolled under, its color takes the surface tint at once.
#[test]
fn the_app_bar_tints_once_the_package_list_is_scrolled() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    let on_bar = Point::new(200.0, 80.0);
    assert!(near(window.pixel(on_bar), Studio::SURFACE));
    window.point_at(Point::new(160.0, 600.0));
    let reported = window.frame(&mut page, &[lines(-1.0)], t);
    assert!(
        reported.contains(&Message::MasterScrolled(true)),
        "{reported:?}"
    );
    window.frame(&mut page, &[], t + 16 * MS);
    assert!(near(window.pixel(on_bar), scrolled_under(Studio::SURFACE)));
    assert_eq!(
        scrolled_under(Studio::SURFACE),
        over(
            Color {
                a: SCROLLED_UNDER_TINT,
                ..Zinc::PRIMARY
            },
            Studio::SURFACE
        )
    );
    let reported = window.frame(&mut page, &[lines(5.0)], t + 32 * MS);
    assert!(
        reported.contains(&Message::MasterScrolled(false)),
        "{reported:?}"
    );
    window.frame(&mut page, &[], t + 48 * MS);
    assert!(near(window.pixel(on_bar), Studio::SURFACE));
}

// The switcher's children are keyed: the detail going keeps its sheet (and
// its scroll) as it fades out; the one coming in starts afresh, a quarter
// of the page's height down (the transition slides the switcher's whole
// child).
#[test]
fn a_detail_going_keeps_its_sheet_and_the_next_comes_up_a_quarter_page() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    window.point_at(Point::new(800.0, 500.0));
    // A trackpad drag up grows its sheet to the page's height.
    let mut pad = Trackpad::new();
    window.frame_sent(
        &mut page,
        &[pad.began(t), pad.changed(0.0, -200.0, t), pad.ended(t)],
        t,
    );
    let centre = (CARD_LEFT + CARD_RIGHT) / 2.0;
    let grown_header = |window: &Window| {
        window.ink(
            Point::new(centre - 12.0, 12.0),
            Point::new(centre + 12.0, 30.0),
        )
    };
    window.frame(&mut page, &[], t + 16 * MS);
    assert!(
        grown_header(&window) >= 0xD0,
        "grown: its header at the top"
    );

    let t1 = t + 100 * MS;
    page.select(1, t1);
    window.frame(&mut page, &[], t1);
    assert!(grown_header(&window) >= 0xD0, "going, still grown");
    let coming = window.find(&page, "second", 1);
    let quarter = 0.25 * SIZE.height;
    assert!(close(coming.y, card_top() + 12.0 + quarter), "{coming:?}");

    window.frame(&mut page, &[], t1 + DETAIL_TRANSITION);
    window.frame(&mut page, &[], t1 + DETAIL_TRANSITION + 16 * MS);
    assert!(grown_header(&window) < 0x60, "gone");
    let coming = window.find(&page, "second", 1);
    assert!(close(coming.y, card_top() + 12.0), "afresh, at its least");
}
