//! Material's `Tooltip` as the app's mouse met it on the desktop: a plain
//! text tip below its target (above it when only there it fits), kept 10 px
//! inside the window (theme's colors, `bodySmall`), shown while the pointer
//! is over the target.
//!
//! It comes and goes as `TooltipState` drove it: the pointer entering the
//! target fades it in over 150 ms; leaving, the tip stays for 100 ms (the
//! hover exit duration) and then fades out over 75 ms, both on
//! `Curves.fastOutSlowIn`; a press anywhere fades it out at once, and it
//! stays away until the pointer enters the target again. The pointer on the
//! tip itself does not hold it (Flutter's tip ignored the pointer). The
//! time comes from the window's frames, so a test injects it, and it asks
//! for frames only while it moves (or for one at the end of the exit
//! delay). The tip, its fill and its text, fades as one layer (the
//! `FadeTransition` around it: `opacity_layer::Effects`).

use std::time::Duration;

use crate::ui::core::text;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::time::Instant;
use iced::widget::container;
use iced::{
    mouse, window, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
};

use crate::ui::core::motion::{Controller, Curve, Status};
use crate::ui::core::theme;
use crate::ui::core::widgets::opacity_layer::Effects as _;

/// How long the tip takes to fade in (`_fadeInDuration`).
pub const FADE_IN: Duration = Duration::from_millis(150);

/// How long it takes to fade out (`_fadeOutDuration`).
pub const FADE_OUT: Duration = Duration::from_millis(75);

/// How long it stays after the pointer leaves (`_defaultHoverExitDuration`).
pub const EXIT_DELAY: Duration = Duration::from_millis(100);

/// A tooltip reading `tip` below `target`, its top `gap` below the target's
/// bottom (Flutter's 24 px below the target's centre is `24 - height / 2`).
pub fn material_tooltip<'a, M: 'a>(
    target: impl Into<Element<'a, M>>,
    tip: impl Into<String>,
    gap: f32,
) -> MaterialTooltip<'a, M> {
    MaterialTooltip {
        target: target.into(),
        tip: tip.into(),
        gap,
        max_width: None,
    }
}

/// See the module docs.
pub struct MaterialTooltip<'a, M> {
    target: Element<'a, M>,
    tip: String,
    gap: f32,
    max_width: Option<f32>,
}

impl<M> MaterialTooltip<'_, M> {
    /// Wraps the tip at `width`.
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }
}

impl<'a, M: 'a> From<MaterialTooltip<'a, M>> for Element<'a, M> {
    fn from(tooltip: MaterialTooltip<'a, M>) -> Self {
        let mut tip = container(text(tooltip.tip).size(theme::TOOLTIP_TEXT_SIZE))
            .padding(theme::TOOLTIP_PADDING)
            .style(theme::tooltip);
        if let Some(width) = tooltip.max_width {
            tip = tip.max_width(width);
        }
        Element::new(Tooltip {
            target: tooltip.target,
            tip: tip.into(),
            gap: tooltip.gap,
        })
    }
}

struct Tooltip<'a, M> {
    target: Element<'a, M>,
    tip: Element<'a, M>,
    gap: f32,
}

/// The tooltip's state: its `AnimationController` and what drives it.
struct State {
    fade: Controller,
    /// Whether the pointer is over the target.
    hovered: bool,
    /// Whether a press sent it away since the pointer entered.
    dismissed: bool,
    /// When the hover exit's delay runs out, while it runs.
    exit_at: Option<Instant>,
    /// The last frame's time.
    now: Option<Instant>,
}

impl State {
    /// The tip's opacity at the last frame.
    fn opacity(&self) -> f32 {
        self.now
            .map_or(0.0, |now| self.fade.curved(Curve::FastOutSlowIn, None, now))
    }

    /// Whether the tip is up at the last frame (coming, shown or going).
    fn is_up(&self) -> bool {
        self.now
            .is_some_and(|now| self.fade.status(now) != Status::Dismissed)
    }

    /// A frame at `at`: the tip heads where the pointer says.
    fn frame<M>(&mut self, at: Instant, shell: &mut Shell<'_, M>) {
        let now = self.now.map_or(at, |was| was.max(at));
        self.now = Some(now);
        let showing = matches!(self.fade.status(now), Status::Forward | Status::Completed);
        if self.hovered && !self.dismissed {
            self.exit_at = None;
            if !showing {
                self.fade.forward(now);
            }
        } else if showing {
            if self.dismissed {
                self.exit_at = None;
                self.fade.reverse(now);
            } else {
                let exit_at = *self.exit_at.get_or_insert(now + EXIT_DELAY);
                if exit_at <= now {
                    self.exit_at = None;
                    self.fade.reverse(exit_at);
                }
            }
        }
        if self.fade.is_animating(now) {
            shell.request_redraw();
        } else if let Some(exit_at) = self.exit_at {
            shell.request_redraw_at(exit_at);
        }
    }
}

impl<M> Widget<M, Theme, Renderer> for Tooltip<'_, M> {
    fn size(&self) -> Size<Length> {
        self.target.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.target.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            fade: Controller::new(FADE_IN, FADE_OUT),
            hovered: false,
            dismissed: false,
            exit_at: None,
            now: None,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.target), Tree::new(&self.tip)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[self.target.as_widget(), self.tip.as_widget()]);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.target
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
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                // Something came over the target under a still pointer (a
                // dialog's barrier, which takes the pointer from what lies
                // beneath): the tip goes at once, as for a press, until the
                // pointer enters the target again. It stayed drawn over the
                // dimmed page until the pointer moved.
                if state.hovered && cursor.is_levitating() {
                    state.hovered = false;
                    state.dismissed = true;
                }
                state.frame(*at, shell);
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) => {
                if !state.dismissed {
                    state.dismissed = true;
                    shell.request_redraw();
                }
            }
            Event::Mouse(_) => {
                let hovered = cursor.is_over(layout.bounds());
                if hovered != state.hovered {
                    state.hovered = hovered;
                    if hovered {
                        // A new entry: a press before it no longer counts.
                        state.dismissed = false;
                    }
                    shell.request_redraw();
                }
            }
            _ => {}
        }
        self.target.as_widget_mut().update(
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
        self.target.as_widget().draw(
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
        self.target
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
        self.target.as_widget().mouse_interaction(
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
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_ref::<State>();
        let (target_tree, tip_tree) = children.split_at_mut(1);
        let target = self.target.as_widget_mut().overlay(
            &mut target_tree[0],
            layout,
            renderer,
            viewport,
            translation,
        );
        let tip = state.is_up().then(|| {
            overlay::Element::new(Box::new(Tip {
                tip: &mut self.tip,
                tree: &mut tip_tree[0],
                target: layout.bounds() + translation,
                gap: self.gap,
                opacity: state.opacity(),
            }))
        });
        match (target, tip) {
            (None, None) => None,
            (target, tip) => Some(
                overlay::Group::with_children(target.into_iter().chain(tip).collect()).overlay(),
            ),
        }
    }
}

/// Flutter's `positionDependentBox` as the tooltip calls it (preferring
/// below, 10 px from the window's edges): below the target by
/// `vertical_offset` from its centre if it fits there or fits neither way,
/// else above; across, centred on the target and kept 10 px inside the
/// window, or centred in it when it is too wide for that.
fn position_dependent_box(window: Size, tip: Size, target: Point, vertical_offset: f32) -> Point {
    const MARGIN: f32 = 10.0;
    let fits_below = target.y + vertical_offset + tip.height <= window.height - MARGIN;
    let fits_above = target.y - vertical_offset - tip.height >= MARGIN;
    let below = if fits_above == fits_below {
        true
    } else {
        fits_below
    };
    let y = if below {
        (target.y + vertical_offset).min(window.height - MARGIN)
    } else {
        (target.y - vertical_offset - tip.height).max(MARGIN)
    };
    let flexible = window.width - tip.width;
    let x = if flexible <= 2.0 * MARGIN {
        flexible / 2.0
    } else {
        (target.x - tip.width / 2.0).clamp(MARGIN, flexible - MARGIN)
    };
    Point::new(x, y)
}

/// The tip, laid out by [`position_dependent_box`], at its opacity.
struct Tip<'a, 'b, M> {
    tip: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    target: Rectangle,
    gap: f32,
    opacity: f32,
}

impl<M> overlay::Overlay<M, Theme, Renderer> for Tip<'_, '_, M> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let node = self.tip.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );
        // On whole pixels, so its outline's edges are crisp (and the layer
        // it fades in holds all of them).
        let size = node.size();
        let node = layout::Node::with_children(
            Size::new(size.width.ceil(), size.height.ceil()),
            node.children().to_vec(),
        );
        let at = position_dependent_box(
            bounds,
            node.size(),
            self.target.center(),
            self.target.height / 2.0 + self.gap,
        );
        node.move_to(Point::new(at.x.round(), at.y.round()))
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let bounds = layout.bounds();
        let draw = |renderer: &mut Renderer| {
            self.tip
                .as_widget()
                .draw(self.tree, renderer, theme, style, layout, cursor, &bounds);
        };
        if self.opacity >= 1.0 {
            draw(renderer);
        } else if self.opacity > 0.0 {
            renderer.with_opacity_layer(bounds, self.opacity, draw);
        }
    }

    fn mouse_interaction(
        &self,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::{container, Space};
    use iced::{Color, Point, Size};

    use super::*;
    use crate::ui::core::theme::Studio;
    use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};

    /// A 40 × 20 target at the top left, its tip 10 below it.
    fn view<'a>() -> Element<'a, ()> {
        container(material_tooltip(
            Space::new().width(40).height(20),
            "Tip",
            10.0,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    /// Where the tip's fill shows (clear of its text): just inside its left
    /// edge, centred under the target.
    fn on_tip(window: &Frames) -> Color {
        // The tip is centred under the 40 px target and 20 + 10 down; its
        // left edge is somewhere left of the target's middle. Its padding
        // row (4 px tall) at the top is fill only.
        window.pixel(Point::new(20.0, 31.0))
    }

    fn tip_at(opacity: f32) -> Color {
        over(
            Color {
                a: Studio::SURFACE.a * opacity,
                ..Studio::SURFACE
            },
            BACKGROUND,
        )
    }

    #[test]
    fn it_fades_in_over_150_ms_under_the_pointer() {
        let mut window = Frames::new(Size::new(200.0, 100.0));
        let t0 = Instant::now();
        assert!(!window.frame(view(), &[], t0), "idle");
        assert!(near(on_tip(&window), BACKGROUND), "none");
        window.point_at(Point::new(20.0, 10.0));
        assert!(window.frame(view(), &[], t0), "asks for frames");
        assert!(near(on_tip(&window), BACKGROUND), "start");
        window.frame(view(), &[], t0 + 75 * MS);
        let middle = Curve::FastOutSlowIn.transform(0.5);
        assert!(near(on_tip(&window), tip_at(middle)), "middle");
        assert!(!window.frame(view(), &[], t0 + 150 * MS), "rests");
        assert!(near(on_tip(&window), tip_at(1.0)), "end");
    }

    #[test]
    fn it_stays_100_ms_after_the_pointer_leaves_then_fades_out_over_75_ms() {
        let mut window = Frames::new(Size::new(200.0, 100.0));
        let t0 = Instant::now();
        window.point_at(Point::new(20.0, 10.0));
        window.frame(view(), &[], t0);
        window.frame(view(), &[], t0 + 150 * MS);
        // Away (not over the tip either).
        window.point_at(Point::new(150.0, 80.0));
        let t1 = t0 + 200 * MS;
        assert!(!window.frame(view(), &[], t1), "waits for the exit delay");
        window.frame(view(), &[], t1 + 50 * MS);
        assert!(near(on_tip(&window), tip_at(1.0)), "still there");
        window.frame(view(), &[], t1 + 100 * MS);
        assert!(near(on_tip(&window), tip_at(1.0)), "start");
        window.frame(view(), &[], t1 + 100 * MS + Duration::from_micros(37_500));
        let middle = Curve::FastOutSlowIn.transform(0.5);
        assert!(near(on_tip(&window), tip_at(middle)), "middle");
        assert!(!window.frame(view(), &[], t1 + 175 * MS), "rests");
        assert!(near(on_tip(&window), BACKGROUND), "gone");
    }

    #[test]
    fn a_press_sends_it_away_at_once_until_the_pointer_enters_again() {
        let mut window = Frames::new(Size::new(200.0, 100.0));
        let t0 = Instant::now();
        window.point_at(Point::new(20.0, 10.0));
        window.frame(view(), &[], t0);
        window.frame(view(), &[], t0 + 150 * MS);
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let t1 = t0 + 200 * MS;
        window.frame(view(), &[press], t1);
        window.frame(view(), &[], t1 + 75 * MS);
        assert!(near(on_tip(&window), BACKGROUND), "gone, still hovered");
        assert!(!window.frame(view(), &[], t1 + 200 * MS), "and stays away");
        // Out and back in: it comes again.
        window.point_at(Point::new(150.0, 80.0));
        window.frame(view(), &[], t1 + 300 * MS);
        window.point_at(Point::new(20.0, 10.0));
        window.frame(view(), &[], t1 + 400 * MS);
        window.frame(view(), &[], t1 + 550 * MS);
        assert!(near(on_tip(&window), tip_at(1.0)), "back");
    }

    #[test]
    fn a_dialog_over_a_still_pointer_sends_it_away_at_once() {
        // A layer over the target that takes the pointer, as a dialog's
        // barrier does (here a clear one), or none.
        let page = |covered: bool| -> Element<'_, ()> {
            let over: Element<'_, ()> = if covered {
                crate::ui::core::widgets::toolbar::window_backdrop(
                    Space::new().width(Length::Fill).height(Length::Fill),
                )
            } else {
                Space::new().width(Length::Fill).height(Length::Fill).into()
            };
            iced::widget::stack![view(), over].into()
        };
        let mut window = Frames::new(Size::new(200.0, 100.0));
        let t0 = Instant::now();
        window.point_at(Point::new(20.0, 10.0));
        window.frame(page(false), &[], t0);
        window.frame(page(false), &[], t0 + 150 * MS);
        assert!(near(on_tip(&window), tip_at(1.0)), "shown");
        // The dialog opens; the pointer does not move.
        let t1 = t0 + 200 * MS;
        window.frame_still(page(true), &[], t1);
        window.frame_still(page(true), &[], t1 + 75 * MS);
        assert!(near(on_tip(&window), BACKGROUND), "gone");
        // Closed again, the pointer still: it stays away.
        assert!(!window.frame_still(page(false), &[], t1 + 200 * MS));
        assert!(near(on_tip(&window), BACKGROUND));
    }

    #[test]
    fn its_fill_and_text_fade_as_one_layer() {
        // The tip all in, then halfway in: each pixel of it (its text on
        // its fill too) the first's at the fade's opacity, not the text at
        // it over the fill at it.
        let pixels = |window: &Frames| -> Vec<Color> {
            (24..44)
                .flat_map(|y| (0..60).map(move |x| (x, y)))
                .map(|(x, y)| window.pixel(Point::new(x as f32, y as f32)))
                .collect()
        };
        let mut window = Frames::new(Size::new(200.0, 100.0));
        let t0 = Instant::now();
        window.point_at(Point::new(20.0, 10.0));
        window.frame(view(), &[], t0);
        window.frame(view(), &[], t0 + 150 * MS);
        let full = pixels(&window);
        // Its text's ink, lighter than the fill, on it.
        let ink = full.iter().filter(|p| p.r > 0.5).count();
        assert!(ink > 10, "its text ({ink} pixels)");
        let mut window = Frames::new(Size::new(200.0, 100.0));
        window.point_at(Point::new(20.0, 10.0));
        window.frame(view(), &[], t0);
        window.frame(view(), &[], t0 + 75 * MS);
        let middle = Curve::FastOutSlowIn.transform(0.5);
        for (full, half) in full.iter().zip(pixels(&window)) {
            let layer = over(Color { a: middle, ..*full }, BACKGROUND);
            assert!(near(half, layer), "{half:?}, not {layer:?}");
        }
    }

    #[test]
    fn it_goes_above_its_target_when_only_there_it_fits() {
        // A 40 × 20 target on the window's bottom edge: below, the tip
        // would end past 10 px from it; above, it ends 10 px over the
        // target's top (24 px from its centre).
        let view = || -> Element<'_, ()> {
            container(material_tooltip(
                Space::new().width(40).height(20),
                "Tip",
                10.0,
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(iced::alignment::Vertical::Bottom)
            .into()
        };
        let mut window = Frames::new(Size::new(200.0, 100.0));
        let t0 = Instant::now();
        window.point_at(Point::new(20.0, 90.0));
        window.frame(view(), &[], t0);
        window.frame(view(), &[], t0 + 150 * MS);
        assert!(
            near(window.pixel(Point::new(20.0, 68.0)), tip_at(1.0)),
            "above: its bottom padding"
        );
        assert!(near(window.pixel(Point::new(20.0, 72.0)), BACKGROUND));
    }

    #[test]
    fn it_is_placed_as_flutter_s_position_dependent_box_places_it() {
        let window = Size::new(200.0, 100.0);
        let tip = Size::new(30.0, 24.0);
        // Below, centred on the target.
        let below = position_dependent_box(window, tip, Point::new(100.0, 20.0), 20.0);
        assert_eq!(below, Point::new(85.0, 40.0));
        // Kept 10 px inside the window's sides.
        let left = position_dependent_box(window, tip, Point::new(5.0, 20.0), 20.0);
        assert_eq!(left.x, 10.0);
        let right = position_dependent_box(window, tip, Point::new(198.0, 20.0), 20.0);
        assert_eq!(right.x, 160.0);
        // No room below: above.
        let above = position_dependent_box(window, tip, Point::new(100.0, 80.0), 20.0);
        assert_eq!(above, Point::new(85.0, 36.0));
        // Room neither way: below, held 10 px above the bottom edge.
        let tall = Size::new(30.0, 90.0);
        let neither = position_dependent_box(window, tall, Point::new(100.0, 50.0), 20.0);
        assert_eq!(neither.y, 70.0);
        // Too wide for the margins: centred in the window.
        let wide =
            position_dependent_box(window, Size::new(185.0, 24.0), Point::new(5.0, 20.0), 20.0);
        assert_eq!(wide.x, 7.5);
    }
}
