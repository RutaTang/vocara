//! A page sliding sideways in a page transition (Flutter's `SlideTransition`
//! inside `CupertinoPageTransition`), with the faint edge shadow the
//! incoming page casts on the one it covers (`_CupertinoEdgeShadowDecoration`).
//!
//! The slide moves the page's layout, not only its drawing: a tap lands where
//! the page is drawn, as Flutter's `SlideTransition` hit-tested its child
//! where it painted it, and the page is clipped to its box. Each frame of a
//! slide rebuilds the view, and iced lays a rebuilt view out anew, so the
//! layout follows the slide frame by frame. A page sliding away takes no
//! input at all and is out of reach of operations (`IgnorePointer` on a
//! route going away); [`inert`] makes anything so.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::{
    mouse, Background, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
};

use std::time::Duration;

use iced::time::Instant;

use crate::ui::core::motion::{Controller, Curve, Lerp};

/// How long a page takes to come in or go (`CupertinoRouteTransitionMixin`'s
/// `kTransitionDuration`: a `MaterialPageRoute` on macOS slides as on iOS).
pub const PAGE_TRANSITION: Duration = Duration::from_millis(500);

/// `CupertinoPageTransition` on a route animation at `now`: the moving
/// page's offset (in its widths, rightward: in from the right edge on
/// `Curves.fastEaseInToSlowEaseOut`, back on its flip), the covered page's
/// (a third of the way out to the left on `Curves.linearToEaseOut`, back on
/// `Curves.easeInToLinear`), and the moving page's edge shadow's strength
/// (`Curves.linearToEaseOut`).
pub fn cupertino(route: &Controller, now: Instant) -> (f32, f32, f32) {
    let primary = route.curved(
        Curve::FastEaseInToSlowEaseOut,
        Some(Curve::FastEaseInToSlowEaseOutFlipped),
        now,
    );
    let secondary = route.curved(Curve::LinearToEaseOut, Some(Curve::EaseInToLinear), now);
    let shadow = route.curved(Curve::LinearToEaseOut, None, now);
    (1.0 - primary, -secondary / 3.0, shadow)
}

/// The edge shadow's colors, at its full strength: from the page's edge
/// outward (`<Color>[Color(0x04000000), CupertinoColors.transparent]`).
const SHADOW: [Color; 2] = [Color::from_rgba8(0, 0, 0, 4.0 / 255.0), Color::TRANSPARENT];

/// See the module docs.
pub struct Slide<'a, M> {
    content: Element<'a, M>,
    /// How far right its drawing moves, in its own widths (a
    /// `SlideTransition`'s fractional offset).
    dx: f32,
    /// How far down, in its own heights.
    dy: f32,
    /// The edge shadow's strength, 0 to 1 (none at 0).
    shadow: f32,
    /// Whether it takes input (a route coming in, or the one on top).
    interactive: bool,
}

/// `content`, taking no input and out of reach of operations (a dialog
/// going away), where it is.
pub fn inert<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    slide(content, 0.0).inert().into()
}

/// `content`, laid out and drawn `dx` of its width to the right.
pub fn slide<'a, M>(content: impl Into<Element<'a, M>>, dx: f32) -> Slide<'a, M> {
    Slide {
        content: content.into(),
        dx,
        dy: 0.0,
        shadow: 0.0,
        interactive: true,
    }
}

impl<M> Slide<'_, M> {
    /// Also `dy` of its height down.
    pub fn down(mut self, dy: f32) -> Self {
        self.dy = dy;
        self
    }

    /// Casts the edge shadow at `strength` (0 to 1) left of the page.
    pub fn shadow(mut self, strength: f32) -> Self {
        self.shadow = strength;
        self
    }

    /// Takes no input and is out of reach of operations: a page going away.
    pub fn inert(mut self) -> Self {
        self.interactive = false;
        self
    }
}

impl<M> Widget<M, Theme, Renderer> for Slide<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let page = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let size = page.size();
        let moved = Vector::new(self.dx * size.width, self.dy * size.height);
        layout::Node::with_children(size, vec![page.translate(moved)])
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
        if self.interactive {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                page(layout),
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
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
        let bounds = layout.bounds();
        let page = page(layout);
        let cursor = if self.interactive {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        let draw = |renderer: &mut Renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                page,
                cursor,
                viewport,
            );
        };
        if self.dx == 0.0 && self.dy == 0.0 && self.shadow <= 0.0 {
            draw(renderer);
            return;
        }
        renderer.with_layer(bounds, |renderer| {
            if self.shadow > 0.0 {
                draw_shadow(renderer, bounds, page.bounds().x, self.shadow);
            }
            draw(renderer);
        });
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if self.interactive {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                page(layout),
                renderer,
                operation,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if !self.interactive {
            return mouse::Interaction::None;
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            page(layout),
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
        if !self.interactive {
            return None;
        }
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            page(layout),
            renderer,
            viewport,
            translation,
        )
    }
}

/// The page's layout, moved, inside the slide's.
fn page(layout: Layout<'_>) -> Layout<'_> {
    layout.children().next().expect("a slide lays out its page")
}

/// `_CupertinoEdgeShadowPainter`: a band 5 % of the page wide, left of its
/// edge at `edge`, drawn as Flutter drew it, in 1 px columns, each the
/// colors lerped across the band, at `strength`.
fn draw_shadow(renderer: &mut Renderer, bounds: Rectangle, edge: f32, strength: f32) {
    let width = 0.05 * bounds.width;
    let mut x = 0.0_f32;
    while x < width {
        let color = SHADOW[0].lerp(SHADOW[1], x / width);
        let color = Color {
            a: color.a * strength,
            ..color
        };
        if color.a > 0.0 {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: edge - x - 1.0,
                        y: bounds.y,
                        width: 1.0,
                        height: bounds.height,
                    },
                    ..renderer::Quad::default()
                },
                Background::Color(color),
            );
        }
        x += 1.0;
    }
}

impl<'a, M: 'a> From<Slide<'a, M>> for Element<'a, M> {
    fn from(slide: Slide<'a, M>) -> Self {
        Element::new(slide)
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::core::text;
    use iced::time::Instant;
    use iced::widget::{button, container, Space};
    use iced::{Point, Size};

    use super::*;
    use crate::ui::core::theme;
    use crate::ui::core::widgets::animated::testing::{near, Frames};

    const FACE: Color = Color::from_rgb(1.0, 0.4, 0.2);

    fn face<'a>() -> Element<'a, ()> {
        container(Space::new().width(Length::Fill).height(Length::Fill))
            .style(theme::fill(FACE))
            .into()
    }

    #[test]
    fn its_drawing_moves_by_a_share_of_its_width_and_is_clipped() {
        let mut window = Frames::new(Size::new(100.0, 40.0));
        let drawn = |w: &Frames, x: f32| near(w.pixel(Point::new(x, 20.0)), FACE);
        window.frame(slide(face(), 0.0), &[], Instant::now());
        assert!(drawn(&window, 10.0) && drawn(&window, 90.0));
        window.frame(slide(face(), 0.5), &[], Instant::now());
        assert!(!drawn(&window, 40.0) && drawn(&window, 60.0), "half off");
        window.frame(slide(face(), -1.0 / 3.0), &[], Instant::now());
        assert!(
            drawn(&window, 60.0) && !drawn(&window, 70.0),
            "a third out left"
        );
    }

    #[test]
    fn a_tap_lands_where_the_page_is_drawn() {
        // A 100 wide button at the left of a 200 wide page, the page half
        // way in: the button is drawn at 100..200.
        let view = || {
            let target = button(Space::new().width(100).height(40))
                .padding(0)
                .on_press(());
            Element::from(slide(
                container(target).width(Length::Fill).height(Length::Fill),
                0.5,
            ))
        };
        let tap = |x: f32| {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                Size::new(200.0, 40.0),
                view(),
            );
            ui.point_at(Point::new(x, 20.0));
            let _ = ui.simulate(iced_test::simulator::click());
            ui.into_messages().count()
        };
        assert_eq!(tap(50.0), 0, "where it would rest: nothing there");
        assert_eq!(tap(150.0), 1, "where it is drawn");
    }

    #[test]
    fn a_page_going_takes_no_input_and_is_out_of_reach() {
        let view = |inert: bool| {
            let s = slide(button(text("Open")).on_press(()), 0.0);
            Element::from(if inert { s.inert() } else { s })
        };
        let mut ui = iced_test::simulator(view(false));
        ui.click("Open").expect("found");
        assert_eq!(ui.into_messages().count(), 1);
        let mut ui = iced_test::simulator(view(true));
        assert!(ui.find("Open").is_err(), "out of reach");
        ui.point_at(Point::new(10.0, 10.0));
        let _ = ui.simulate(iced_test::simulator::click());
        assert_eq!(ui.into_messages().count(), 0, "no input");
    }
}
