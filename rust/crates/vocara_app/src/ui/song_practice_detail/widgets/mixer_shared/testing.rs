//! What the mixer UI's widget tests share: the app's fonts, finding texts,
//! the pointer events, and an interface rebuilt across frames the way the
//! app rebuilds it.

use std::borrow::Cow;
use std::sync::Once;

use iced::advanced::clipboard;
use iced::advanced::renderer::{self as core_renderer, Headless};
use iced::{mouse, window, Color, Element, Event, Point, Rectangle, Size};
use iced_test::runtime::user_interface::{self, UserInterface};
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;

use crate::ui::core::icons;
use crate::ui::core::theme::{app_theme, font};

/// The app's settings and fonts (Inter, and the Lucide icons), so text
/// measures as it does in the app.
pub fn settings() -> iced::Settings {
    let mut settings = crate::app::settings();
    settings.fonts = font::FACES
        .iter()
        .copied()
        .chain([icons::FONT_BYTES])
        .map(Cow::Borrowed)
        .collect();
    settings
}

/// Loads the app's fonts into the shared font system (a [`Simulator`] does
/// on creation).
fn load_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        font::install();
        iced_test::renderer::graphics::text::font_system()
            .write()
            .expect("the font system")
            .load_font(Cow::Borrowed(icons::FONT_BYTES));
    });
}

/// A simulator of `view` at `size`, with the app's fonts.
pub fn simulator<'a, M>(size: Size, view: impl Into<Element<'a, M>>) -> Simulator<'a, M> {
    Simulator::with_size(settings(), size, view)
}

/// The `n`-th text reading `label` (0 first), in tree order.
pub fn nth(label: &str, n: usize) -> impl FnMut(Candidate<'_>) -> Option<Target> + Send {
    let label = label.to_string();
    let mut seen = 0;
    move |candidate: Candidate<'_>| {
        let Candidate::Text { content, .. } = &candidate else {
            return None;
        };
        if *content != label {
            return None;
        }
        seen += 1;
        (seen == n + 1).then(|| Target::from(candidate))
    }
}

/// Where the `n`-th text reading `label` is laid out, if it is.
pub fn text_at<M>(ui: &mut Simulator<'_, M>, label: &str, n: usize) -> Option<Rectangle> {
    ui.find(nth(label, n)).ok().map(|t| t.bounds())
}

/// How many texts read `label`.
pub fn count<M>(ui: &mut Simulator<'_, M>, label: &str) -> usize {
    (0..)
        .take_while(|&n| ui.find(nth(label, n)).is_ok())
        .count()
}

/// The text an icon is drawn as.
pub fn glyph(icon: icons::Icon) -> String {
    char::from(icon).to_string()
}

pub fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

pub fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

pub fn right_press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
}

pub fn right_release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right))
}

pub fn move_to(p: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position: p })
}

/// A precise scroll step of `(x, y)` points the run loop knew no gesture
/// of (a scroll signal to the app's widgets; `pan_zoom::testing` makes a
/// trackpad's swipe): what iced's scroll views scroll by.
pub fn swipe(x: f32, y: f32) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Pixels { x, y },
    })
}

/// A mouse wheel's notch (line deltas; a positive `y` scrolls up).
pub fn wheel(x: f32, y: f32) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x, y },
    })
}

pub fn redraw(at: iced::time::Instant) -> Event {
    Event::Window(window::Event::RedrawRequested(at))
}

/// Drives `ui` with `events`, the pointer at `at` for each (moves are sent
/// as moves to their own point first).
pub fn drive<M>(ui: &mut Simulator<'_, M>, steps: &[(Point, Event)]) {
    for (at, event) in steps {
        ui.point_at(*at);
        let _ = ui.simulate([event.clone()]);
    }
}

/// A press at `from`, moves through `path`, and the release at the last
/// point.
pub fn drag_steps(from: Point, path: &[Point]) -> Vec<(Point, Event)> {
    let mut steps = vec![(from, press())];
    for &p in path {
        steps.push((p, move_to(p)));
    }
    steps.push((*path.last().unwrap_or(&from), release()));
    steps
}

/// An interface rebuilt across frames as the app does: a new view each
/// frame, diffed into the last frame's widget tree, so widget state (a held
/// fader, an analyzer) carries over.
pub struct Frames {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
    size: Size,
}

/// What one frame's events produced.
pub struct Frame<M> {
    pub messages: Vec<M>,
    pub redraw: window::RedrawRequest,
}

impl Frames {
    pub fn new(size: Size) -> Self {
        load_fonts();
        let renderer =
            iced_test::futures::futures::executor::block_on(<iced::Renderer as Headless>::new(
                font::REGULAR,
                crate::ui::core::theme::DEFAULT_TEXT_SIZE.into(),
                None,
            ))
            .expect("a headless renderer");
        Self {
            renderer,
            cache: user_interface::Cache::default(),
            size,
        }
    }

    /// Builds `view`, feeds it `events` with the pointer at `cursor`, and
    /// draws it.
    pub fn run<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        cursor: Point,
        events: &[Event],
    ) -> Frame<M> {
        let cursor = mouse::Cursor::Available(cursor);
        let mut ui = UserInterface::build(
            view,
            self.size,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut messages = Vec::new();
        let (state, _) = ui.update(
            events,
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let redraw = match state {
            user_interface::State::Updated { redraw_request, .. } => redraw_request,
            user_interface::State::Outdated => window::RedrawRequest::NextFrame,
        };
        ui.draw(
            &mut self.renderer,
            &app_theme(),
            &core_renderer::Style {
                text_color: Color::WHITE,
            },
            cursor,
        );
        self.cache = ui.into_cache();
        Frame { messages, redraw }
    }
}
