//! A headless UI for widget tests that rebuild the view between events, as
//! the app does after every message: `iced_test`'s `Simulator` builds its
//! view once, so it cannot show what a widget does when the screen redraws
//! it mid-gesture (a clip moved on every live tick, a menu opened by the
//! press before). This keeps the widget tree's state (`user_interface::Cache`)
//! from one view to the next, like the runtime.

use iced::{mouse, Element, Event, Font, Pixels, Point, Size, Theme};
use iced_test::core::clipboard;
use iced_test::core::renderer::Headless;
use iced_test::core::widget;
use iced_test::renderer::Renderer;
use iced_test::runtime::user_interface::{self, UserInterface};
use iced_test::selector::Selector;
use iced_winit::gesture::{self, Wheel};

/// See the module docs.
pub struct LiveUi {
    cache: Option<user_interface::Cache>,
    renderer: Renderer,
    size: Size,
    cursor: mouse::Cursor,
}

impl LiveUi {
    /// A window of `size`, with the simulator's default font and size.
    pub fn new(size: Size) -> Self {
        let renderer = iced_test::futures::futures::executor::block_on(Renderer::new(
            Font::with_name("Fira Sans"),
            Pixels(16.0),
            None,
        ))
        .expect("a headless renderer");
        Self {
            cache: Some(user_interface::Cache::default()),
            renderer,
            size,
            cursor: mouse::Cursor::Unavailable,
        }
    }

    /// Where the cursor is (no event: see [`LiveUi::move_to`]).
    pub fn point_at(&mut self, at: Point) {
        self.cursor = mouse::Cursor::Available(at);
    }

    fn build<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
    ) -> UserInterface<'a, M, Theme, Renderer> {
        UserInterface::build(
            view,
            self.size,
            self.cache.take().unwrap_or_default(),
            &mut self.renderer,
        )
    }

    /// Builds `view` on the widget state the last view left, feeds it
    /// `events` in one batch, and keeps its state for the next view; what
    /// it published.
    pub fn run<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        events: impl IntoIterator<Item = Event>,
    ) -> Vec<M> {
        self.run_sent(view, events.into_iter().map(|event| (event, None)))
    }

    /// [`LiveUi::run`] for events and the gestures the run loop hands
    /// along with the wheel's (`pan_zoom::testing`).
    pub fn run_sent<'a, M>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        sent: impl IntoIterator<Item = (Event, Option<Wheel>)>,
    ) -> Vec<M> {
        let mut ui = self.build(view);
        let mut messages = Vec::new();
        // One event at a time, each with the cursor where it is then: a
        // move puts it where it went.
        for (event, wheel) in sent {
            if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
                self.cursor = mouse::Cursor::Available(position);
            }
            gesture::with(wheel, || {
                let _ = ui.update(
                    std::slice::from_ref(&event),
                    self.cursor,
                    &mut self.renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
            });
        }
        self.cache = Some(ui.into_cache());
        messages
    }

    /// The first target of `selector` in `view`.
    pub fn find<'a, M, S>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        selector: S,
    ) -> Option<S::Output>
    where
        S: Selector + Send,
        S::Output: Clone + Send,
    {
        use widget::Operation;
        let mut ui = self.build(view);
        let mut operation = selector.find();
        ui.operate(
            &self.renderer,
            &mut widget::operation::black_box(&mut operation),
        );
        self.cache = Some(ui.into_cache());
        match operation.finish() {
            widget::operation::Outcome::Some(output) => output,
            _ => None,
        }
    }

    /// Every target of `selector` in `view`, overlays included.
    pub fn find_all<'a, M, S>(
        &mut self,
        view: impl Into<Element<'a, M>>,
        selector: S,
    ) -> Vec<S::Output>
    where
        S: Selector + Send,
        S::Output: Clone + Send,
    {
        use widget::Operation;
        let mut ui = self.build(view);
        let mut operation = selector.find_all();
        ui.operate(
            &self.renderer,
            &mut widget::operation::black_box(&mut operation),
        );
        self.cache = Some(ui.into_cache());
        match operation.finish() {
            widget::operation::Outcome::Some(all) => all,
            _ => Vec::new(),
        }
    }
}

/// A frame rendered at one pixel per logical pixel.
pub struct Frame {
    width: usize,
    rgba: Vec<u8>,
}

impl Frame {
    /// The `[r, g, b, a]` at `(x, y)`.
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
}

impl LiveUi {
    /// Draws `view` (a frame's redraw first, as the runtime does) on the
    /// state the last view left, and reads the pixels back.
    pub fn snapshot<'a, M>(&mut self, view: impl Into<Element<'a, M>>) -> Frame {
        use iced::theme::Base;
        let theme = crate::ui::core::theme::app_theme();
        let base = theme.base();
        let mut ui = self.build(view);
        let mut messages = Vec::new();
        let _ = ui.update(
            &[redraw()],
            self.cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        ui.draw(
            &mut self.renderer,
            &theme,
            &iced::advanced::renderer::Style {
                text_color: base.text_color,
            },
            self.cursor,
        );
        self.cache = Some(ui.into_cache());
        let size = Size::new(self.size.width as u32, self.size.height as u32);
        let rgba = self.renderer.screenshot(size, 1.0, base.background_color);
        Frame {
            width: size.width as usize,
            rgba,
        }
    }
}

/// A frame: the runtime's redraw request reaching the widgets.
pub fn redraw() -> Event {
    Event::Window(iced::window::Event::RedrawRequested(
        std::time::Instant::now(),
    ))
}

/// A precise scroll of `(x, y)` pixels the run loop knew no gesture of: a
/// scroll signal to the app's widgets, a scroll to iced's scroll views
/// (`pan_zoom::testing` makes a trackpad's swipe).
pub fn scroll(x: f32, y: f32) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Pixels { x, y },
    })
}

/// A left-button press.
pub fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

/// A left-button release.
pub fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

/// A right-button press and release.
pub fn right_click() -> [Event; 2] {
    [
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right)),
    ]
}

/// A cursor move to `(x, y)`.
pub fn move_to(x: f32, y: f32) -> Event {
    Event::Mouse(mouse::Event::CursorMoved {
        position: Point::new(x, y),
    })
}
