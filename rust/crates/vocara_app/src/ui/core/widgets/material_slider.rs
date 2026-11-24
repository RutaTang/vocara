//! A Material `Slider`'s overlay around an iced slider: the round glow
//! (`RoundSliderOverlayShape`) that grows from the thumb's centre when the
//! pointer comes over the thumb or a drag starts, and shrinks when both
//! are over, over 100 ms (`kRadialReactionDuration`) on
//! `Curves.fastOutSlowIn`. Its color is Material 3's (the scheme's primary
//! at 8 % while the pointer is over the slider, 10 % while dragging,
//! nothing otherwise), taken as it is at each frame, as Flutter resolved it
//! on each build. It is drawn under the whole slider, its track included
//! (Flutter drew it over the track and under the thumb: the track is 3 px
//! of an overlay 8 % strong).

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::time::Instant;
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Point, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

use crate::ui::core::motion::{Controller, Curve};
use crate::ui::core::theme::Zinc;

/// How long the overlay takes to grow or shrink (`kRadialReactionDuration`).
pub const REACTION: Duration = Duration::from_millis(100);

/// The overlay at full size, around `slider`, whose thumb (`thumb_radius`
/// round, its centre that far in from each end) shows `value` (0 to 1).
pub fn slider_overlay<'a, M: 'a>(
    slider: impl Into<Element<'a, M>>,
    value: f32,
    thumb_radius: f32,
    overlay_radius: f32,
) -> Element<'a, M> {
    Element::new(SliderOverlay {
        slider: slider.into(),
        value: value.clamp(0.0, 1.0),
        thumb_radius,
        overlay_radius,
    })
}

struct SliderOverlay<'a, M> {
    slider: Element<'a, M>,
    value: f32,
    thumb_radius: f32,
    overlay_radius: f32,
}

impl<M> SliderOverlay<'_, M> {
    /// Where the thumb's centre is in `bounds`.
    fn thumb(&self, bounds: Rectangle) -> Point {
        let inset = self.thumb_radius;
        Point::new(
            bounds.x + inset + self.value * (bounds.width - 2.0 * inset).max(0.0),
            bounds.center_y(),
        )
    }
}

/// The overlay's state: its `overlayController` and the pointer's.
struct State {
    reaction: Controller,
    /// Whether the controller last ran forward.
    active: bool,
    /// The pointer is over the slider (`MouseRegion`'s hover).
    hovered: bool,
    /// The pointer is over the overlay's square around the thumb
    /// (`hoveringThumb`).
    over_thumb: bool,
    /// A drag (or a tap) is held.
    dragging: bool,
    now: Option<Instant>,
}

impl State {
    /// The overlay's radius share and color at the last frame.
    fn shown(&self) -> (f32, Color) {
        let t = self.now.map_or(0.0, |now| {
            self.reaction.curved(Curve::FastOutSlowIn, None, now)
        });
        let alpha = if self.dragging {
            0.10
        } else if self.hovered {
            0.08
        } else {
            0.0
        };
        (
            t,
            Color {
                a: Zinc::PRIMARY.a * alpha,
                ..Zinc::PRIMARY
            },
        )
    }
}

impl<M> Widget<M, Theme, Renderer> for SliderOverlay<'_, M> {
    fn size(&self) -> Size<Length> {
        self.slider.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.slider.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            reaction: Controller::new(REACTION, REACTION),
            active: false,
            hovered: false,
            over_thumb: false,
            dragging: false,
            now: None,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.slider)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.slider));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.slider
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
        let bounds = layout.bounds();
        let thumb = self.thumb(bounds);
        let reach = self.overlay_radius;
        let square = Rectangle::new(
            Point::new(thumb.x - reach, thumb.y - reach),
            Size::new(2.0 * reach, 2.0 * reach),
        );
        let state = tree.state.downcast_mut::<State>();
        let was = (state.hovered, state.over_thumb, state.dragging);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if cursor.is_over(bounds) {
                    state.dragging = true;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = false;
            }
            Event::Window(window::Event::RedrawRequested(at)) => {
                let now = state.now.map_or(*at, |was| was.max(*at));
                state.now = Some(now);
                let on = (state.hovered && state.over_thumb) || state.dragging;
                if on != state.active {
                    state.active = on;
                    if on {
                        state.reaction.forward(now);
                    } else {
                        state.reaction.reverse(now);
                    }
                }
                if state.reaction.is_animating(now) {
                    shell.request_redraw();
                }
            }
            _ => {}
        }
        if let Event::Mouse(_) = event {
            state.hovered = cursor.is_over(bounds);
            state.over_thumb = cursor.is_over(square);
        }
        if (state.hovered, state.over_thumb, state.dragging) != was {
            shell.request_redraw();
        }
        self.slider.as_widget_mut().update(
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
        let (t, color) = tree.state.downcast_ref::<State>().shown();
        let radius = self.overlay_radius * t;
        if radius > 0.0 && color.a > 0.0 {
            let centre = self.thumb(layout.bounds());
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        Point::new(centre.x - radius, centre.y - radius),
                        Size::new(2.0 * radius, 2.0 * radius),
                    ),
                    border: Border {
                        radius: radius.into(),
                        ..Border::default()
                    },
                    shadow: Shadow::default(),
                    snap: false,
                },
                Background::Color(color),
            );
        }
        self.slider.as_widget().draw(
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
        self.slider
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
        self.slider.as_widget().mouse_interaction(
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
        self.slider.as_widget_mut().overlay(
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
    use iced::widget::{container, slider, Space};

    use super::*;
    use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};

    /// A 200 × 24 slider at the middle (its thumb, 6 round, at (100, 12)),
    /// its handle and rail clear so only the overlay draws.
    fn view<'a>() -> Element<'a, f32> {
        let clear = |_: &Theme, _| slider::Style {
            rail: slider::Rail {
                backgrounds: (
                    Background::Color(Color::TRANSPARENT),
                    Background::Color(Color::TRANSPARENT),
                ),
                width: 3.0,
                border: Border::default(),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 6.0 },
                background: Background::Color(Color::TRANSPARENT),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
            },
        };
        let s = slider(0.0..=1.0, 0.5, |v| v).height(24).style(clear);
        container(slider_overlay(s, 0.5, 6.0, 12.0))
            .width(200)
            .height(24)
            .into()
    }

    fn glow(alpha: f32) -> Color {
        over(
            Color {
                a: Zinc::PRIMARY.a * alpha,
                ..Zinc::PRIMARY
            },
            BACKGROUND,
        )
    }

    #[test]
    fn it_grows_over_100_ms_under_the_pointer_on_the_thumb_and_shrinks_after() {
        let mut window = Frames::new(Size::new(220.0, 40.0));
        let t0 = Instant::now();
        // 10 px right of the thumb's centre: inside the full overlay only.
        let ring = Point::new(110.0, 12.0);
        assert!(!window.frame(view(), &[], t0));
        assert!(near(window.pixel(ring), BACKGROUND), "none");
        window.point_at(Point::new(100.0, 12.0));
        assert!(window.frame(view(), &[], t0), "asks for frames");
        assert!(near(window.pixel(ring), BACKGROUND), "start");
        window.frame(view(), &[], t0 + 50 * MS);
        // 12 × fastOutSlowIn(0.5) ≈ 9.3: not yet out to 10.
        assert!(near(window.pixel(ring), BACKGROUND), "middle: short of it");
        assert!(
            near(window.pixel(Point::new(108.0, 12.0)), glow(0.08)),
            "middle"
        );
        assert!(!window.frame(view(), &[], t0 + 100 * MS), "rests");
        assert!(near(window.pixel(ring), glow(0.08)), "end");
        // Off the thumb, still over the slider: it shrinks.
        window.point_at(Point::new(30.0, 12.0));
        let t1 = t0 + 200 * MS;
        assert!(window.frame(view(), &[], t1));
        window.frame(view(), &[], t1 + 50 * MS);
        assert!(near(window.pixel(ring), BACKGROUND), "shrinking");
        assert!(!window.frame(view(), &[], t1 + 100 * MS));
        assert!(
            near(window.pixel(Point::new(101.0, 4.0)), BACKGROUND),
            "gone"
        );
    }

    #[test]
    fn a_drag_keeps_it_at_its_stronger_color() {
        let mut window = Frames::new(Size::new(220.0, 40.0));
        let t0 = Instant::now();
        let ring = Point::new(110.0, 12.0);
        window.point_at(Point::new(100.0, 12.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        window.frame(view(), &[press], t0);
        window.frame(view(), &[], t0 + 100 * MS);
        assert!(near(window.pixel(ring), glow(0.10)), "dragging");
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        window.frame(view(), &[release], t0 + 200 * MS);
        assert!(
            near(window.pixel(ring), glow(0.08)),
            "released, still over it"
        );
        // Away off the slider: its color goes at once.
        window.point_at(Point::new(100.0, 35.0));
        window.frame(view(), &[], t0 + 210 * MS);
        assert!(near(window.pixel(ring), BACKGROUND));
    }

    #[test]
    fn its_thumb_follows_the_value_inside_the_thumbs_inset() {
        let bounds = Rectangle::new(Point::new(10.0, 0.0), Size::new(100.0, 24.0));
        let at = |value: f32| {
            let o = SliderOverlay::<()> {
                slider: Space::new().into(),
                value,
                thumb_radius: 6.0,
                overlay_radius: 12.0,
            };
            o.thumb(bounds)
        };
        assert_eq!(at(0.0), Point::new(16.0, 12.0));
        assert_eq!(at(1.0), Point::new(104.0, 12.0));
        assert_eq!(at(0.5), Point::new(60.0, 12.0));
    }
}
