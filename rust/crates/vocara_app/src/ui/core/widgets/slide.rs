//! A page sliding sideways in a page transition (Flutter's `SlideTransition`
//! inside `CupertinoPageTransition`), with the faint edge shadow the
//! incoming page casts on the one it covers (`_CupertinoEdgeShadowDecoration`).
//!
//! The slide is drawn, not laid out: the page keeps its place for its input,
//! its widgets' state and a test's lookups, and only its drawing moves (and
//! is clipped to the box). A page sliding away takes no input at all and is
//! out of reach of operations (`IgnorePointer` on a route going away).

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::{
    mouse, Background, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
};

use crate::ui::core::motion::Lerp;

/// The edge shadow's colors, at its full strength: from the page's edge
/// outward (`<Color>[Color(0x04000000), CupertinoColors.transparent]`).
const SHADOW: [Color; 2] = [Color::from_rgba8(0, 0, 0, 4.0 / 255.0), Color::TRANSPARENT];

/// See the module docs.
pub struct Slide<'a, M> {
    content: Element<'a, M>,
    /// How far right its drawing moves, in its own widths (a
    /// `SlideTransition`'s fractional offset).
    dx: f32,
    /// The edge shadow's strength, 0 to 1 (none at 0).
    shadow: f32,
    /// Whether it takes input (a route coming in, or the one on top).
    interactive: bool,
}

/// `content`, drawn `dx` of its width to the right.
pub fn slide<'a, M>(content: impl Into<Element<'a, M>>, dx: f32) -> Slide<'a, M> {
    Slide {
        content: content.into(),
        dx,
        shadow: 0.0,
        interactive: true,
    }
}

impl<M> Slide<'_, M> {
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
        self.content
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
        if self.interactive {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout,
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
        let cursor = if self.interactive {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        if self.dx == 0.0 && self.shadow <= 0.0 {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
            return;
        }
        let dx = self.dx * bounds.width;
        renderer.with_layer(bounds, |renderer| {
            if self.shadow > 0.0 {
                draw_shadow(renderer, bounds, dx, self.shadow);
            }
            let moved = Vector::new(dx, 0.0);
            renderer.with_translation(moved, |renderer| {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor,
                    &(*viewport - moved),
                );
            });
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
                layout,
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
        if !self.interactive {
            return None;
        }
        let dx = self.dx * layout.bounds().width;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation + Vector::new(dx, 0.0),
        )
    }
}

/// `_CupertinoEdgeShadowPainter`: a band 5 % of the page wide, left of its
/// edge at `bounds.x + dx`, drawn as Flutter drew it, in 1 px columns, each
/// the colors lerped across the band, at `strength`.
fn draw_shadow(renderer: &mut Renderer, bounds: Rectangle, dx: f32, strength: f32) {
    let width = 0.05 * bounds.width;
    let edge = bounds.x + dx;
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
    use iced::time::Instant;
    use iced::widget::{button, container, text, Space};
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
