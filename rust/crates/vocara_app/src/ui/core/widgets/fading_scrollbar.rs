//! The scrollbar Flutter's desktop scroll behavior put on every scrollable
//! (Material's `Scrollbar` over `RawScrollbar`), as it moves: the thumb is
//! hidden until the view scrolls (by the wheel, a drag of the thumb or a
//! program) or the pointer comes over the bar; it then fades in over 300 ms
//! and, 600 ms after the scrolling stops or the pointer leaves the bar (and
//! no drag holds it), fades out over 300 ms, both on `Curves.fastOutSlowIn`
//! (`fadeDuration`, `timeToFade`). Under the pointer the thumb's color
//! moves to its hovered one over 200 ms, linear (Material's hover
//! animation); dragged, it takes the dragged one at once. A view with
//! nothing to scroll shows none.
//!
//! [`fading_scrollbar`] wraps an iced scrollable: it reads the scrollable's
//! offset after each event (an operation that stops at the scrollable), so
//! any scroll shows the bar, and hands the style the fade and the hover's
//! progress, which iced's status-only style could not know. The time comes
//! from the window's frames, so a test injects it; it asks for frames only
//! while something moves, or for one when the bar is due to fade.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::operation::Scrollable as ScrollableState;
use iced::advanced::widget::{tree, Id, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::time::Instant;
use iced::widget::scrollable::{self, Rail, Scrollable, Status, Style};
use iced::{
    mouse, window, Background, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
};

use crate::ui::core::motion::{Controller, Curve, Lerp};
use crate::ui::core::theme::{self, faded};

/// How long the thumb takes to fade in or out (`_kScrollbarFadeDuration`).
pub const FADE: Duration = Duration::from_millis(300);

/// How long after the scrolling stops the thumb starts to fade
/// (`_kScrollbarTimeToFade`).
pub const TIME_TO_FADE: Duration = Duration::from_millis(600);

/// How long the thumb's color takes to its hovered one
/// (`_hoverAnimationController`).
pub const HOVER: Duration = Duration::from_millis(200);

/// How thick iced's default scrollbar is: the strip at the scrollable's
/// edge where the pointer is over the bar.
const BAR: f32 = 10.0;

/// `scrollable` under the app's scrollbar (`theme::scrollbar`), fading as
/// the module docs say.
pub fn fading_scrollbar<'a, M: 'a>(scrollable: Scrollable<'a, M>) -> Element<'a, M> {
    fading_scrollbar_with(scrollable, theme::scrollbar)
}

/// `scrollable` under the scrollbar `style` gives, fading as the module
/// docs say.
pub fn fading_scrollbar_with<'a, M: 'a>(
    scrollable: Scrollable<'a, M>,
    style: impl Fn(&Theme, Status) -> Style + 'a,
) -> Element<'a, M> {
    let shown = Rc::new(Shown::default());
    let seen = Rc::clone(&shown);
    let scrollable = scrollable.style(move |theme, status| thumbs(&style, theme, status, &seen));
    Element::new(FadingScrollbar {
        scrollable: scrollable.into(),
        shown,
    })
}

/// A scrollable that will fade its scrollbar, still taking its size: what
/// [`Fading::fading`] makes of one, to finish as the scrollable was.
pub struct FadingScrollable<'a, M> {
    scrollable: Scrollable<'a, M>,
    style: scrollable::StyleFn<'a, Theme>,
}

impl<M> FadingScrollable<'_, M> {
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.scrollable = self.scrollable.width(width);
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.scrollable = self.scrollable.height(height);
        self
    }
}

impl<'a, M: 'a> From<FadingScrollable<'a, M>> for Element<'a, M> {
    fn from(fading: FadingScrollable<'a, M>) -> Self {
        fading_scrollbar_with(fading.scrollable, fading.style)
    }
}

/// `.fading()` where an iced scrollable took `.style(theme::scrollbar)`.
pub trait Fading<'a, M> {
    /// Under the app's scrollbar, fading (see the module docs).
    fn fading(self) -> FadingScrollable<'a, M>;

    /// Under the scrollbar `style` gives, fading.
    fn fading_with(self, style: impl Fn(&Theme, Status) -> Style + 'a) -> FadingScrollable<'a, M>;
}

impl<'a, M: 'a> Fading<'a, M> for Scrollable<'a, M> {
    fn fading(self) -> FadingScrollable<'a, M> {
        self.fading_with(theme::scrollbar)
    }

    fn fading_with(self, style: impl Fn(&Theme, Status) -> Style + 'a) -> FadingScrollable<'a, M> {
        FadingScrollable {
            scrollable: self,
            style: Box::new(style),
        }
    }
}

/// What the style reads, as drawn: the thumbs' opacity and each bar's
/// hover progress.
#[derive(Default)]
struct Shown {
    opacity: Cell<f32>,
    hover_x: Cell<f32>,
    hover_y: Cell<f32>,
}

/// `style`'s look at `status`, its thumbs at the fade and their colors
/// along the hover's progress.
fn thumbs(
    style: &dyn Fn(&Theme, Status) -> Style,
    theme: &Theme,
    status: Status,
    shown: &Shown,
) -> Style {
    let (dragged, disabled) = match status {
        Status::Active {
            is_horizontal_scrollbar_disabled: x,
            is_vertical_scrollbar_disabled: y,
        }
        | Status::Hovered {
            is_horizontal_scrollbar_disabled: x,
            is_vertical_scrollbar_disabled: y,
            ..
        } => ((false, false), (x, y)),
        Status::Dragged {
            is_horizontal_scrollbar_dragged: dx,
            is_vertical_scrollbar_dragged: dy,
            is_horizontal_scrollbar_disabled: x,
            is_vertical_scrollbar_disabled: y,
        } => ((dx, dy), (x, y)),
    };
    let now = style(theme, status);
    let idle = style(
        theme,
        Status::Active {
            is_horizontal_scrollbar_disabled: disabled.0,
            is_vertical_scrollbar_disabled: disabled.1,
        },
    );
    let hot = style(
        theme,
        Status::Hovered {
            is_horizontal_scrollbar_hovered: true,
            is_vertical_scrollbar_hovered: true,
            is_horizontal_scrollbar_disabled: disabled.0,
            is_vertical_scrollbar_disabled: disabled.1,
        },
    );
    let opacity = shown.opacity.get();
    let rail = |rail: Rail, idle: Rail, hot: Rail, dragged: bool, hover: f32| {
        let thumb = match (dragged, idle.scroller.background, hot.scroller.background) {
            (false, Background::Color(a), Background::Color(b)) => {
                Background::Color(a.lerp(b, hover))
            }
            _ => rail.scroller.background,
        };
        Rail {
            scroller: scrollable::Scroller {
                background: thumb.scale_alpha(opacity),
                border: iced::Border {
                    color: faded(rail.scroller.border.color, opacity),
                    ..rail.scroller.border
                },
            },
            ..rail
        }
    };
    Style {
        horizontal_rail: rail(
            now.horizontal_rail,
            idle.horizontal_rail,
            hot.horizontal_rail,
            dragged.0,
            shown.hover_x.get(),
        ),
        vertical_rail: rail(
            now.vertical_rail,
            idle.vertical_rail,
            hot.vertical_rail,
            dragged.1,
            shown.hover_y.get(),
        ),
        ..now
    }
}

struct FadingScrollbar<'a, M> {
    scrollable: Element<'a, M>,
    shown: Rc<Shown>,
}

/// The bar's state (`RawScrollbarState`'s and Material's controllers).
struct State {
    fade: Controller,
    /// Whether the fade last ran forward.
    on: bool,
    /// When the thumb is due to fade, while that is pending
    /// (`_fadeoutTimer`).
    fade_at: Option<Instant>,
    hover_x: Controller,
    hover_y: Controller,
    /// The pointer is over the horizontal / the vertical bar.
    over: (bool, bool),
    /// A thumb drag is held.
    dragging: bool,
    /// The view scrolled since the last frame.
    scrolled: bool,
    /// The offset last seen.
    offset: Option<Vector>,
    /// Whether there is anything to scroll, across and down.
    overflow: (bool, bool),
    now: Option<Instant>,
}

impl State {
    fn frame<M>(&mut self, at: Instant, shell: &mut Shell<'_, M>) {
        let now = self.now.map_or(at, |was| was.max(at));
        self.now = Some(now);
        let held = self.over.0 || self.over.1 || self.dragging;
        if !(self.overflow.0 || self.overflow.1) {
            // Nothing to scroll: no bar.
            self.scrolled = false;
            self.fade_at = None;
            if self.on {
                self.on = false;
                self.fade.reverse(now);
            }
        } else if self.scrolled || held {
            if !self.on {
                self.on = true;
                self.fade.forward(now);
            }
            // A scroll ends at once (a wheel's step); the hover and a drag
            // hold the bar until they end.
            self.fade_at = (!held).then_some(now + TIME_TO_FADE);
            self.scrolled = false;
        } else if self.on && self.fade_at.is_none() {
            self.fade_at = Some(now + TIME_TO_FADE);
        }
        if let Some(due) = self.fade_at.filter(|due| *due <= now) {
            self.fade_at = None;
            self.on = false;
            self.fade.reverse(due);
        }
        for (hover, over) in [
            (&mut self.hover_x, self.over.0),
            (&mut self.hover_y, self.over.1),
        ] {
            let forward = hover.status(now) == crate::ui::core::motion::Status::Forward
                || hover.status(now) == crate::ui::core::motion::Status::Completed;
            if over && !forward {
                hover.forward(now);
            } else if !over && forward {
                hover.reverse(now);
            }
        }
        if self.fade.is_animating(now)
            || self.hover_x.is_animating(now)
            || self.hover_y.is_animating(now)
        {
            shell.request_redraw();
        } else if let Some(due) = self.fade_at {
            shell.request_redraw_at(due);
        }
    }
}

/// Reads a scrollable's offset and extents, stopping at it.
#[derive(Default)]
struct Offset(Option<(Vector, Rectangle, Rectangle)>);

impl Operation for Offset {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn ScrollableState,
    ) {
        self.0.get_or_insert((translation, bounds, content_bounds));
    }
}

impl<M> Widget<M, Theme, Renderer> for FadingScrollbar<'_, M> {
    fn size(&self) -> Size<Length> {
        self.scrollable.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.scrollable.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            fade: Controller::new(FADE, FADE),
            on: false,
            fade_at: None,
            hover_x: Controller::new(HOVER, HOVER),
            hover_y: Controller::new(HOVER, HOVER),
            over: (false, false),
            dragging: false,
            scrolled: false,
            offset: None,
            overflow: (false, false),
            now: None,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.scrollable)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.scrollable));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.scrollable
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(at)) = event {
            tree.state.downcast_mut::<State>().frame(*at, shell);
        }
        self.scrollable.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let mut offset = Offset::default();
        self.scrollable.as_widget_mut().operate(
            &mut tree.children[0],
            layout,
            renderer,
            &mut offset,
        );
        let state = tree.state.downcast_mut::<State>();
        let was = (state.over, state.dragging, state.scrolled);
        if let Some((translation, bounds, content)) = offset.0 {
            state.overflow = (
                content.width > bounds.width + 0.5,
                content.height > bounds.height + 0.5,
            );
            if state.offset.is_some_and(|last| last != translation) {
                state.scrolled = true;
            }
            state.offset = Some(translation);
        }
        if let Event::Mouse(mouse_event) = event {
            let b = layout.bounds();
            let at = cursor.position_over(b);
            state.over = (
                state.overflow.0 && at.is_some_and(|p| p.y >= b.y + b.height - BAR),
                state.overflow.1 && at.is_some_and(|p| p.x >= b.x + b.width - BAR),
            );
            match mouse_event {
                mouse::Event::ButtonPressed(mouse::Button::Left)
                    if state.over.0 || state.over.1 =>
                {
                    state.dragging = true;
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) => state.dragging = false,
                _ => {}
            }
        }
        if (state.over, state.dragging, state.scrolled) != was {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let (opacity, hover_x, hover_y) = state.now.map_or((0.0, 0.0, 0.0), |now| {
            (
                state.fade.curved(Curve::FastOutSlowIn, None, now),
                state.hover_x.value(now),
                state.hover_y.value(now),
            )
        });
        self.shown.opacity.set(opacity);
        self.shown.hover_x.set(hover_x);
        self.shown.hover_y.set(hover_y);
        self.scrollable.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.scrollable
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.scrollable.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.scrollable.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::{column, scrollable, Space};
    use iced::{Color, Point};

    use super::*;
    use crate::ui::core::theme::Zinc;
    use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};

    /// A 100 × 100 view of a 300 px column: its thumb a third of the bar.
    fn view<'a>() -> Element<'a, ()> {
        fading_scrollbar(
            scrollable(column![Space::new().width(100).height(300)])
                .width(100)
                .height(100),
        )
    }

    fn thumb(alpha: f32) -> Color {
        over(
            Color {
                a: Zinc::FOREGROUND.a * alpha,
                ..Zinc::FOREGROUND
            },
            BACKGROUND,
        )
    }

    fn wheel() -> Event {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -30.0 },
        })
    }

    /// Mid-thumb once the view has scrolled 30 px (the thumb spans 10..43).
    const ON_THUMB: Point = Point::new(95.0, 26.0);

    #[test]
    fn the_thumb_shows_on_a_scroll_and_fades_600_ms_after_it() {
        let mut window = Frames::new(Size::new(120.0, 120.0));
        let t0 = Instant::now();
        window.point_at(Point::new(40.0, 50.0));
        assert!(!window.frame(view(), &[], t0), "idle");
        assert!(
            near(window.pixel(Point::new(95.0, 10.0)), BACKGROUND),
            "hidden"
        );
        assert!(window.frame(view(), &[wheel()], t0), "a scroll: frames");
        assert!(near(window.pixel(ON_THUMB), BACKGROUND), "start");
        window.frame(view(), &[], t0 + 150 * MS);
        let middle = Curve::FastOutSlowIn.transform(0.5);
        assert!(near(window.pixel(ON_THUMB), thumb(0.30 * middle)), "middle");
        assert!(!window.frame(view(), &[], t0 + 300 * MS), "in: waits");
        assert!(near(window.pixel(ON_THUMB), thumb(0.30)), "end");
        // 600 ms after the scroll it fades out.
        window.frame(view(), &[], t0 + 599 * MS);
        assert!(near(window.pixel(ON_THUMB), thumb(0.30)), "still");
        assert!(window.frame(view(), &[], t0 + 600 * MS), "fading");
        window.frame(view(), &[], t0 + 750 * MS);
        assert!(near(window.pixel(ON_THUMB), thumb(0.30 * middle)), "going");
        assert!(!window.frame(view(), &[], t0 + 900 * MS), "rests");
        assert!(near(window.pixel(ON_THUMB), BACKGROUND), "gone");
    }

    #[test]
    fn the_pointer_on_the_bar_holds_it_and_warms_its_color_over_200_ms() {
        let mut window = Frames::new(Size::new(120.0, 120.0));
        let t0 = Instant::now();
        window.point_at(Point::new(95.0, 90.0));
        window.frame(view(), &[], t0);
        window.frame(view(), &[], t0 + 100 * MS);
        // The thumb sits at the top (0..33).
        let on = Point::new(95.0, 16.0);
        let mid = Curve::FastOutSlowIn.transform(1.0 / 3.0);
        let warming = 0.30_f32.lerp(0.50, 0.5);
        assert!(
            near(window.pixel(on), thumb(warming * mid)),
            "both on the way"
        );
        window.frame(view(), &[], t0 + 300 * MS);
        assert!(near(window.pixel(on), thumb(0.50)), "in, hovered");
        // Held while the pointer stays.
        assert!(!window.frame(view(), &[], t0 + 2000 * MS));
        assert!(near(window.pixel(on), thumb(0.50)), "held");
        // Off the bar: back to idle over 200 ms, and gone 600 ms on.
        window.point_at(Point::new(40.0, 50.0));
        let t1 = t0 + 2100 * MS;
        window.frame(view(), &[], t1);
        window.frame(view(), &[], t1 + 200 * MS);
        assert!(near(window.pixel(on), thumb(0.30)), "cooled");
        window.frame(view(), &[], t1 + 600 * MS);
        assert!(!window.frame(view(), &[], t1 + 900 * MS));
        assert!(near(window.pixel(on), BACKGROUND), "gone");
    }

    #[test]
    fn a_view_with_nothing_to_scroll_shows_no_bar() {
        let mut window = Frames::new(Size::new(120.0, 120.0));
        let t0 = Instant::now();
        let short = || -> Element<'_, ()> {
            fading_scrollbar(
                scrollable(column![Space::new().width(100).height(50)])
                    .width(100)
                    .height(100),
            )
        };
        window.point_at(Point::new(95.0, 20.0));
        assert!(!window.frame(short(), &[], t0));
        assert!(!window.frame(short(), &[wheel()], t0 + 10 * MS));
    }
}
