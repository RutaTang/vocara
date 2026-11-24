//! A window for animation tests: the view is rebuilt each frame and diffed
//! into the last frame's widget tree, as the app does, the frame's time is
//! the test's, and what was drawn can be read back pixel by pixel.

use std::borrow::Cow;
use std::sync::Once;
use std::time::Duration;

use iced::advanced::clipboard;
use iced::advanced::renderer::{self as core_renderer, Headless};
use iced::time::Instant;
use iced::{mouse, window, Color, Element, Event, Point, Renderer, Size};
use iced_test::runtime::user_interface::{self, UserInterface};

use crate::ui::core::icons;
use crate::ui::core::theme::{app_theme, font, DEFAULT_TEXT_SIZE};

/// A millisecond.
pub const MS: Duration = Duration::from_millis(1);

/// The window's background, where nothing is drawn.
pub const BACKGROUND: Color = Color::BLACK;

/// A frame at `at` (`RedrawRequested`).
pub fn redraw(at: Instant) -> Event {
    Event::Window(window::Event::RedrawRequested(at))
}

/// The pointer moved to `to`.
pub fn move_to(to: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position: to })
}

/// See the module docs.
pub struct Frames {
    renderer: Renderer,
    cache: user_interface::Cache,
    size: Size,
    cursor: Point,
    /// The last frame, drawn, as RGBA rows.
    pixels: Vec<u8>,
    /// The cursor the last frame asked for.
    interaction: mouse::Interaction,
}

impl Frames {
    /// A `size` window with the app's fonts, the pointer outside it.
    pub fn new(size: Size) -> Self {
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
            size,
            cursor: Point::new(-100.0, -100.0),
            pixels: Vec::new(),
            interaction: mouse::Interaction::None,
        }
    }

    /// Puts the pointer at `at` (moving it there in the next frame's events
    /// too).
    pub fn point_at(&mut self, at: Point) {
        self.cursor = at;
    }

    /// Builds `view`, feeds it `events` then a frame at `at`, and draws it;
    /// whether it asked for another frame.
    pub fn frame<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        events: &[Event],
        at: Instant,
    ) -> bool {
        self.frame_messages(view, events, at).0
    }

    /// [`Frames::frame`], with the messages the view published.
    pub fn frame_messages<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        events: &[Event],
        at: Instant,
    ) -> (bool, Vec<M>) {
        let cursor = mouse::Cursor::Available(self.cursor);
        let mut ui = UserInterface::build(
            view,
            self.size,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut messages = Vec::new();
        let mut events = events.to_vec();
        events.insert(0, move_to(self.cursor));
        let _ = ui.update(
            &events,
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let (state, _) = ui.update(
            &[redraw(at)],
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
            Size::new(self.size.width as u32, self.size.height as u32),
            1.0,
            BACKGROUND,
        );
        self.cache = ui.into_cache();
        if let user_interface::State::Updated {
            mouse_interaction, ..
        } = state
        {
            self.interaction = mouse_interaction;
        }
        let more = matches!(
            state,
            user_interface::State::Updated {
                redraw_request: window::RedrawRequest::NextFrame,
                ..
            }
        );
        (more, messages)
    }

    /// The cursor the last frame asked for under the pointer.
    pub fn interaction(&self) -> mouse::Interaction {
        self.interaction
    }

    /// The color drawn at `at` in the last frame.
    pub fn pixel(&self, at: Point) -> Color {
        let (x, y) = (at.x as usize, at.y as usize);
        let i = (y * self.size.width as usize + x) * 4;
        let p = &self.pixels[i..i + 4];
        Color::from_rgba8(p[0], p[1], p[2], f32::from(p[3]) / 255.0)
    }

    /// The brightest channel drawn anywhere in `from`..`to` (a glyph's ink).
    pub fn ink(&self, from: Point, to: Point) -> u8 {
        let mut most = 0;
        for y in from.y as usize..to.y as usize {
            for x in from.x as usize..to.x as usize {
                let i = (y * self.size.width as usize + x) * 4;
                most = most.max(
                    self.pixels[i]
                        .max(self.pixels[i + 1])
                        .max(self.pixels[i + 2]),
                );
            }
        }
        most
    }
}

/// `a` drawn over `b` (what a pixel of `a` at its alpha shows on `b`).
pub fn over(a: Color, b: Color) -> Color {
    Color {
        r: a.r * a.a + b.r * (1.0 - a.a),
        g: a.g * a.a + b.g * (1.0 - a.a),
        b: a.b * a.a + b.b * (1.0 - a.a),
        a: 1.0,
    }
}

/// Whether two colors match to within a step or two of 8 bits.
pub fn near(a: Color, b: Color) -> bool {
    let [ar, ag, ab, _] = a.into_rgba8();
    let [br, bg, bb, _] = b.into_rgba8();
    ar.abs_diff(br) <= 2 && ag.abs_diff(bg) <= 2 && ab.abs_diff(bb) <= 2
}
