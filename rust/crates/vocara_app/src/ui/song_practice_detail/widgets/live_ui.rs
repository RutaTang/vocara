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
        let mut ui = self.build(view);
        let mut messages = Vec::new();
        // One event at a time, each with the cursor where it is then: a
        // move puts it where it went.
        for event in events {
            if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
                self.cursor = mouse::Cursor::Available(position);
            }
            let _ = ui.update(
                std::slice::from_ref(&event),
                self.cursor,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
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
