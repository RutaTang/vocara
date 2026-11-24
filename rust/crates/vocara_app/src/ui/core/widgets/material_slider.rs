//! A Material 3 `Slider` (Flutter 3.38, the 2023 design) drawn over an
//! iced slider, which takes the input and draws nothing itself. It paints
//! what `_RenderSlider.paint` did, in its order:
//!
//! - the track (`RoundedRectSliderTrackShape`): the part after the thumb
//!   `track_height` tall, the part before it 2 px taller
//!   (`additionalActiveTrackHeight`), each a pill, the two meeting half a
//!   track height past the thumb's centre;
//! - the overlay (`RoundSliderOverlayShape`): the round glow that grows from
//!   the thumb's centre when the pointer comes over the thumb or a drag
//!   starts, and shrinks when both are over, over 100 ms
//!   (`kRadialReactionDuration`) on `Curves.fastOutSlowIn`. Its color is
//!   Material 3's (the scheme's primary at 8 % while the pointer is over the
//!   slider, 10 % while dragging, nothing otherwise), taken as it is at each
//!   frame, as Flutter resolved it on each build;
//! - the thumb (`RoundSliderThumbShape`): a disc on a shadow whose
//!   elevation rises from 1 to 6 with the overlay. The shadow is the
//!   engine's spot shadow for a light 600 px up and 800 px wide (`drawShadow`:
//!   offset down by about the elevation, blurred by 4/3 of it, black at
//!   25 %), its faint ambient part left out.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::time::Instant;
use iced::widget::slider;
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Point, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

use crate::ui::core::motion::{Controller, Curve, Lerp};
use crate::ui::core::theme::Zinc;

/// How long the overlay takes to grow or shrink (`kRadialReactionDuration`).
pub const REACTION: Duration = Duration::from_millis(100);

/// How much taller the track is before the thumb
/// (`additionalActiveTrackHeight`).
const ADDITIONAL_ACTIVE_TRACK_HEIGHT: f32 = 2.0;

/// The thumb's elevation at rest and under the full overlay
/// (`RoundSliderThumbShape`'s `elevation`, `pressedElevation`).
const THUMB_ELEVATION: f32 = 1.0;
const PRESSED_THUMB_ELEVATION: f32 = 6.0;

/// The iced slider's style under [`material_slider`]: nothing drawn, its
/// handle `thumb_radius` round (which sets where a click lands).
pub fn clear_style(thumb_radius: f32) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_, _| slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(Color::TRANSPARENT),
                Background::Color(Color::TRANSPARENT),
            ),
            width: 0.0,
            border: Border::default(),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle {
                radius: thumb_radius,
            },
            background: Background::Color(Color::TRANSPARENT),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
        },
    }
}

/// What one frame draws.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Paint {
    inactive_track: Option<Rectangle>,
    active_track: Option<Rectangle>,
    overlay: Option<(Rectangle, Color)>,
    thumb: Rectangle,
    thumb_shadow: Shadow,
}

/// The spot shadow `Canvas.drawShadow` casts at `elevation` under the
/// engine's light (600 px up, 600 px above the shape, 800 px across):
/// offset down by `600 e / (600 - e)`, blurred by `800 e / (600 - e)`,
/// black at 25 %.
fn spot_shadow(elevation: f32) -> Shadow {
    const LIGHT_HEIGHT: f32 = 600.0;
    const LIGHT_RADIUS: f32 = 800.0;
    let share = elevation / (LIGHT_HEIGHT - elevation);
    Shadow {
        color: Color::from_rgba(0.0, 0.0, 0.0, 0.25),
        offset: Vector::new(0.0, LIGHT_HEIGHT * share),
        blur_radius: LIGHT_RADIUS * share,
    }
}

/// How the slider is drawn (`SliderThemeData`, as the Dart call set it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliderLook {
    pub track_height: f32,
    pub active_track: Color,
    pub inactive_track: Color,
    pub thumb: Color,
    pub thumb_radius: f32,
    pub overlay_radius: f32,
}

/// `slider` (drawing nothing of its own) drawn as `look`, its thumb (its
/// centre `look.thumb_radius` in from each end) showing `value` (0 to 1).
pub fn material_slider<'a, M: 'a>(
    slider: impl Into<Element<'a, M>>,
    value: f32,
    look: SliderLook,
) -> Element<'a, M> {
    Element::new(MaterialSlider {
        slider: slider.into(),
        value: value.clamp(0.0, 1.0),
        look,
    })
}

struct MaterialSlider<'a, M> {
    slider: Element<'a, M>,
    value: f32,
    look: SliderLook,
}

impl<M> MaterialSlider<'_, M> {
    /// What it draws in `bounds` with the overlay `t` of the way grown in
    /// `color`.
    fn paint(&self, bounds: Rectangle, t: f32, color: Color) -> Paint {
        let look = self.look;
        let thumb = self.thumb(bounds);
        // `getPreferredRect`: the track runs between the thumb's two ends.
        let track = Rectangle::new(
            Point::new(
                bounds.x + look.thumb_radius,
                bounds.center_y() - look.track_height / 2.0,
            ),
            Size::new(
                (bounds.width - 2.0 * look.thumb_radius).max(0.0),
                look.track_height,
            ),
        );
        let half = look.track_height / 2.0;
        let inactive_track = (thumb.x < track.x + track.width - half).then(|| {
            Rectangle::new(
                Point::new(thumb.x - half, track.y),
                Size::new(track.x + track.width - (thumb.x - half), track.height),
            )
        });
        let active_track = (thumb.x > track.x + half).then(|| {
            Rectangle::new(
                Point::new(track.x, track.y - ADDITIONAL_ACTIVE_TRACK_HEIGHT / 2.0),
                Size::new(
                    thumb.x + half - track.x,
                    track.height + ADDITIONAL_ACTIVE_TRACK_HEIGHT,
                ),
            )
        });
        let radius = look.overlay_radius * t;
        let overlay = (radius > 0.0 && color.a > 0.0).then(|| {
            (
                Rectangle::new(
                    Point::new(thumb.x - radius, thumb.y - radius),
                    Size::new(2.0 * radius, 2.0 * radius),
                ),
                color,
            )
        });
        let elevation = THUMB_ELEVATION.lerp(PRESSED_THUMB_ELEVATION, t);
        let r = look.thumb_radius;
        Paint {
            inactive_track,
            active_track,
            overlay,
            thumb: Rectangle::new(
                Point::new(thumb.x - r, thumb.y - r),
                Size::new(2.0 * r, 2.0 * r),
            ),
            thumb_shadow: spot_shadow(elevation),
        }
    }

    /// Where the thumb's centre is in `bounds`.
    fn thumb(&self, bounds: Rectangle) -> Point {
        let inset = self.look.thumb_radius;
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

impl<M> Widget<M, Theme, Renderer> for MaterialSlider<'_, M> {
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
        let reach = self.look.overlay_radius;
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
        // The iced slider underneath is clear; it draws for completeness.
        self.slider.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        let (t, color) = tree.state.downcast_ref::<State>().shown();
        let bounds = layout.bounds();
        let paint = self.paint(bounds, t, color);
        let mut pill = |bounds: Rectangle, color: Color, shadow: Shadow| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        radius: (bounds.height.min(bounds.width) / 2.0).into(),
                        ..Border::default()
                    },
                    shadow,
                    snap: false,
                },
                Background::Color(color),
            );
        };
        if let Some(inactive) = paint.inactive_track {
            pill(inactive, self.look.inactive_track, Shadow::default());
        }
        if let Some(active) = paint.active_track {
            pill(active, self.look.active_track, Shadow::default());
        }
        if let Some((ring, color)) = paint.overlay {
            pill(ring, color, Shadow::default());
        }
        pill(paint.thumb, self.look.thumb, paint.thumb_shadow);
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

    /// Nothing but the overlay (the shadow, black, does not show on the
    /// black window).
    const CLEAR: SliderLook = SliderLook {
        track_height: 3.0,
        active_track: Color::TRANSPARENT,
        inactive_track: Color::TRANSPARENT,
        thumb: Color::TRANSPARENT,
        thumb_radius: 6.0,
        overlay_radius: 12.0,
    };

    const ACTIVE: Color = Color::from_rgb8(200, 80, 60);
    const INACTIVE: Color = Color::from_rgb8(40, 40, 90);
    const THUMB: Color = Color::from_rgb8(220, 90, 70);

    const LOOK: SliderLook = SliderLook {
        track_height: 3.0,
        active_track: ACTIVE,
        inactive_track: INACTIVE,
        thumb: THUMB,
        thumb_radius: 6.0,
        overlay_radius: 12.0,
    };

    /// A 200 × 24 slider at the middle (its thumb, 6 round, at (100, 12)).
    fn view_as<'a>(look: SliderLook) -> Element<'a, f32> {
        let s = slider(0.0..=1.0, 0.5, |v| v)
            .height(24)
            .style(clear_style(6.0));
        container(material_slider(s, 0.5, look))
            .width(200)
            .height(24)
            .into()
    }

    fn view<'a>() -> Element<'a, f32> {
        view_as(CLEAR)
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

    fn paint_at(value: f32, t: f32) -> Paint {
        let o = MaterialSlider::<()> {
            slider: Space::new().into(),
            value,
            look: LOOK,
        };
        o.paint(
            Rectangle::new(Point::ORIGIN, Size::new(200.0, 24.0)),
            t,
            Color::WHITE,
        )
    }

    #[test]
    fn its_thumb_follows_the_value_inside_the_thumbs_inset() {
        let bounds = Rectangle::new(Point::new(10.0, 0.0), Size::new(100.0, 24.0));
        let at = |value: f32| {
            let o = MaterialSlider::<()> {
                slider: Space::new().into(),
                value,
                look: LOOK,
            };
            o.thumb(bounds)
        };
        assert_eq!(at(0.0), Point::new(16.0, 12.0));
        assert_eq!(at(1.0), Point::new(104.0, 12.0));
        assert_eq!(at(0.5), Point::new(60.0, 12.0));
    }

    #[test]
    fn the_track_is_2_px_taller_before_the_thumb_and_the_two_meet_past_its_centre() {
        let paint = paint_at(0.5, 0.0);
        // The thumb at (100, 12); the track from 6 to 194.
        assert_eq!(
            paint.active_track,
            Some(Rectangle::new(Point::new(6.0, 9.5), Size::new(95.5, 5.0)))
        );
        assert_eq!(
            paint.inactive_track,
            Some(Rectangle::new(Point::new(98.5, 10.5), Size::new(95.5, 3.0)))
        );
        assert_eq!(
            paint.thumb,
            Rectangle::new(Point::new(94.0, 6.0), Size::new(12.0, 12.0))
        );
        assert_eq!(paint.overlay, None, "no overlay at rest");
        // At the ends one part is not drawn.
        assert_eq!(paint_at(0.0, 0.0).active_track, None);
        assert_eq!(paint_at(1.0, 0.0).inactive_track, None);
    }

    #[test]
    fn the_thumb_s_shadow_rises_from_elevation_1_to_6_with_the_overlay() {
        let rest = paint_at(0.5, 0.0).thumb_shadow;
        assert!((rest.offset.y - 600.0 / 599.0).abs() < 1e-4, "{rest:?}");
        assert!((rest.blur_radius - 800.0 / 599.0).abs() < 1e-4);
        assert_eq!(rest.color, Color::from_rgba(0.0, 0.0, 0.0, 0.25));
        let up = paint_at(0.5, 1.0).thumb_shadow;
        assert!((up.offset.y - 3600.0 / 594.0).abs() < 1e-4, "{up:?}");
        assert!((up.blur_radius - 4800.0 / 594.0).abs() < 1e-4);
    }

    #[test]
    fn it_draws_the_track_then_the_overlay_then_the_thumb() {
        let mut window = Frames::new(Size::new(220.0, 40.0));
        let t0 = Instant::now();
        window.frame(view_as(LOOK), &[], t0);
        assert!(near(window.pixel(Point::new(150.0, 12.0)), INACTIVE));
        // Rows 10 to 13 lie wholly inside the 5 px part (9.5 to 14.5); the
        // 3 px part (10.5 to 13.5) only half covers row 10.
        assert!(
            near(window.pixel(Point::new(50.0, 10.0)), ACTIVE),
            "5 px tall"
        );
        assert!(
            !near(window.pixel(Point::new(150.0, 10.0)), INACTIVE),
            "3 px: half"
        );
        assert!(
            near(window.pixel(Point::new(150.0, 9.0)), BACKGROUND),
            "3 px: clear"
        );
        assert!(near(window.pixel(Point::new(100.0, 12.0)), THUMB));
        // Under the pointer the overlay tints the track and not the thumb
        // (a wide overlay here, to see it on the track clear of the thumb's
        // shadow, which reaches 14 px out at elevation 6).
        let wide = SliderLook {
            overlay_radius: 40.0,
            ..LOOK
        };
        window.point_at(Point::new(100.0, 12.0));
        window.frame(view_as(wide), &[], t0);
        window.frame(view_as(wide), &[], t0 + 100 * MS);
        let tint = Color {
            a: Zinc::PRIMARY.a * 0.08,
            ..Zinc::PRIMARY
        };
        assert!(
            near(window.pixel(Point::new(130.0, 12.0)), over(tint, INACTIVE)),
            "over the track"
        );
        assert!(
            near(window.pixel(Point::new(100.0, 12.0)), THUMB),
            "under the thumb"
        );
        // The raised thumb's shadow darkens the track under it.
        let shaded = window.pixel(Point::new(100.0, 20.0));
        // (Each renderer blurs its own way: wgpu's shadow is the fainter, so
        // only that it darkens at all is asked.)
        assert!(
            shaded.r < over(tint, BACKGROUND).r - 1.5 / 255.0,
            "{shaded:?}"
        );
    }
}
