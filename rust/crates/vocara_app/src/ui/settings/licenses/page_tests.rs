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

use super::back_button::{HOVERED, PRESSED};
use super::list_tile::{two_line, SUBTITLE_SIZE, TITLE_SIZE};
use super::sheet::SCROLLED_UNDER_TINT;
use super::*;
use crate::ui::core::icons;
use crate::ui::core::theme::{app_theme, font, DEFAULT_TEXT_SIZE};
use crate::ui::core::widgets::animated::testing::{near, over};
use crate::ui::home::testing::run_task;

/// A millisecond.
const MS: Duration = Duration::from_millis(1);

/// A 1440 × 900 window: the page under the drag bar is 872 tall.
const SIZE: Size = Size::new(1440.0, 872.0);

/// Where the detail card starts: `56 · h / (h + 28)`, under the 28 px drag
/// bar.
fn card_top() -> f32 {
    56.0 * SIZE.height / (SIZE.height + 28.0)
}

/// The card's left and right edges: from the master's 320 to the gutter
/// and the card's margin (24 + 4) off the right.
const CARD_LEFT: f32 = 320.0;
const CARD_RIGHT: f32 = 1440.0 - 28.0;

/// The about block's height: 24 + 20 + 14 + 18 + 12 + 18 + 14 + 24.
const ABOUT: f32 = 144.0;

/// The app bar's height (two toolbar rows).
const APP_BAR: f32 = 112.0;

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

fn pixels(y: f32) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Pixels { x: 0.0, y },
    })
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
            let mut system = iced_test::renderer::graphics::text::font_system()
                .write()
                .expect("the font system");
            for face in font::FACES.iter().copied().chain([icons::FONT_BYTES]) {
                system.load_font(Cow::Borrowed(face));
            }
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
        let _ = ui.update(
            &all,
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
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

// `AppBar._getEffectiveCenterTitle`: macOS centres the title of an app
// bar with fewer than two actions; the page's bar has none, the detail's
// `SliverAppBar` none either.
#[test]
fn the_app_bar_and_the_detail_header_centre_their_titles() {
    let mut window = Window::new();
    let (page, _) = settled(&mut window);
    let title = window.find(&page, "Licenses", 0);
    assert!(
        (title.center_x() - SIZE.width / 2.0).abs() < 0.5,
        "{title:?}"
    );
    assert!(
        close(title.y, 19.0) && close(title.height, 18.0),
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
fn the_back_button_is_the_m3_app_bars_under_forui() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    // The 40 px button in the 56 px leading slot: 8..48 both ways.
    let left = Point::new(12.0, 28.0);
    let right = Point::new(44.0, 28.0);
    let outside = Point::new(6.0, 28.0);
    let corner = Point::new(8.0, 8.0);
    assert!(near(window.pixel(left), Studio::SURFACE), "at rest");
    // The icon: `arrow_back_ios_new_rounded`, its stroke at the tip, its
    // notch clear.
    let ink = window.ink(Point::new(23.0, 27.0), Point::new(26.0, 30.0));
    assert!(ink >= 0xD0, "the arrow's tip: {ink:#x}");
    // Its rounded ends, up and down at 16.88 of 24 (in the 24 px box at 16).
    for y in [19.0, 36.0] {
        let end = window.ink(Point::new(31.0, y), Point::new(34.0, y + 2.0));
        assert!(end >= 0xD0, "an end at {y}: {end:#x}");
    }
    assert!(near(window.pixel(Point::new(30.0, 28.0)), Studio::SURFACE));
    assert!(near(window.pixel(Point::new(36.0, 28.0)), Studio::SURFACE));

    window.point_at(Point::new(28.0, 28.0));
    window.frame(&mut page, &[], t);
    assert!(
        near(window.pixel(left), Zinc::SECONDARY),
        "forui's ghost background, at once"
    );
    window.frame(&mut page, &[], t + 25 * MS);
    let half = Color {
        a: HOVERED.a / 2.0,
        ..HOVERED
    };
    assert!(
        near(window.pixel(left), over(half, Zinc::SECONDARY)),
        "half"
    );
    window.frame(&mut page, &[], t + 50 * MS);
    let hovered = over(HOVERED, Zinc::SECONDARY);
    assert!(near(window.pixel(left), hovered), "in");
    assert!(near(window.pixel(right), hovered));
    assert!(near(window.pixel(outside), Studio::SURFACE), "40 wide");
    assert!(near(window.pixel(corner), Studio::SURFACE), "rounded 8");

    // Its tooltip, 24 below its centre, faded in over 150 ms.
    window.frame(&mut page, &[], t + 150 * MS);
    assert!(
        near(window.pixel(Point::new(28.0, 53.0)), Zinc::FOREGROUND),
        "Back"
    );

    let t1 = t + 200 * MS;
    window.frame(&mut page, &[press()], t1);
    window.frame(&mut page, &[], t1 + 200 * MS);
    assert!(
        near(window.pixel(left), over(PRESSED, hovered)),
        "pressed: 10 % over the hover"
    );
    let reported = window.frame(&mut page, &[release()], t1 + 250 * MS);
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

// `_DraggableScrollableSheetScrollPosition.applyUserOffset`: a trackpad's
// scroll grows the sheet before it scrolls the list, and shrinks it after;
// a mouse wheel's (`pointerScroll`) only scrolls the list.
#[test]
fn a_trackpad_grows_the_sheet_first_and_a_wheel_only_scrolls() {
    let mut window = Window::new();
    let (mut page, t) = settled(&mut window);
    window.point_at(Point::new(800.0, 500.0));
    let header = |window: &mut Window, page: &LicensesPage| window.find(page, "first", 1).y;
    let text = |window: &mut Window, page: &LicensesPage| {
        window.find(page, "Paragraph 20 of a long license.", 0).y
    };
    let rest = card_top() + 12.0;
    assert!(close(header(&mut window, &page), rest));
    window.frame(&mut page, &[pixels(-20.0)], t);
    assert!(close(header(&mut window, &page), rest - 20.0), "grows");
    window.frame(&mut page, &[pixels(-100.0)], t);
    assert!(
        close(header(&mut window, &page), 12.0),
        "to the page's height"
    );
    let p0 = text(&mut window, &page);
    window.frame(&mut page, &[pixels(-30.0)], t);
    assert!(close(header(&mut window, &page), 12.0));
    assert!(
        close(text(&mut window, &page), p0 - 30.0),
        "then the list scrolls"
    );
    // Back: the list first, to its top (the rest of that scroll is lost),
    // then the sheet.
    window.frame(&mut page, &[pixels(40.0)], t);
    assert!(close(text(&mut window, &page), p0));
    assert!(close(header(&mut window, &page), 12.0));
    window.frame(&mut page, &[pixels(10.0)], t);
    assert!(close(header(&mut window, &page), 22.0), "shrinks");
    // A wheel scrolls the list, the sheet stays.
    window.frame(&mut page, &[lines(-1.0)], t);
    assert!(close(header(&mut window, &page), 22.0));
    assert!(text(&mut window, &page) < p0 + 10.0 - 1.0, "the list moved");
    // Not over the card: nothing.
    window.point_at(Point::new(CARD_LEFT - 30.0, 30.0));
    window.frame(&mut page, &[pixels(5.0)], t);
    assert!(close(header(&mut window, &page), 22.0));
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
    window.frame(&mut page, &[pixels(-200.0)], t);
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
