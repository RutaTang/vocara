//! Material's `Tooltip` as the app's mouse met it on the desktop: a plain
//! text tip below its target (theme's colors, `bodySmall`), shown while the
//! pointer is over the target.
//!
//! It comes and goes as `TooltipState` drove it: the pointer entering the
//! target fades it in over 150 ms; leaving, the tip stays for 100 ms (the
//! hover exit duration) and then fades out over 75 ms, both on
//! `Curves.fastOutSlowIn`; a press anywhere fades it out at once, and it
//! stays away until the pointer enters the target again. The pointer on the
//! tip itself does not hold it (Flutter's tip ignored the pointer). The
//! time comes from the window's frames, so a test injects it, and it asks
//! for frames only while it moves (or for one at the end of the exit
//! delay). iced has no subtree opacity: the tip's fill and text take the
//! fade's opacity.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::time::Instant;
use iced::widget::{container, text};
use iced::{
    mouse, window, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
};

use crate::ui::core::motion::{Controller, Curve, Status};
use crate::ui::core::theme::{self, faded_container};

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
        let opacity = Rc::new(Cell::new(0.0_f32));
        let shown = Rc::clone(&opacity);
        let mut tip = container(text(tooltip.tip).size(theme::TOOLTIP_TEXT_SIZE))
            .padding(theme::TOOLTIP_PADDING)
            .style(move |t| faded_container(theme::tooltip(t), shown.get()));
        if let Some(width) = tooltip.max_width {
            tip = tip.max_width(width);
        }
        Element::new(Tooltip {
            target: tooltip.target,
            tip: tip.into(),
            opacity,
            gap: tooltip.gap,
        })
    }
}

struct Tooltip<'a, M> {
    target: Element<'a, M>,
    tip: Element<'a, M>,
    /// The tip's opacity as drawn, which its style reads.
    opacity: Rc<Cell<f32>>,
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
            Event::Window(window::Event::RedrawRequested(at)) => state.frame(*at, shell),
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
            self.opacity.set(state.opacity());
            overlay::Element::new(Box::new(Tip {
                tip: &mut self.tip,
                tree: &mut tip_tree[0],
                target: layout.bounds() + translation,
                gap: self.gap,
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

/// The tip, laid out below its target, inside the window.
struct Tip<'a, 'b, M> {
    tip: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    target: Rectangle,
    gap: f32,
}

impl<M> overlay::Overlay<M, Theme, Renderer> for Tip<'_, '_, M> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let node = self.tip.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );
        let size = node.size();
        let x = self.target.x + (self.target.width - size.width) / 2.0;
        let y = self.target.y + self.target.height + self.gap;
        let at = Point::new(
            x.clamp(0.0, (bounds.width - size.width).max(0.0)),
            y.clamp(0.0, (bounds.height - size.height).max(0.0)),
        );
        node.move_to(at)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.tip.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &layout.bounds(),
        );
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
    use crate::ui::core::motion::Lerp;
    use crate::ui::core::theme::Zinc;
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
                a: Zinc::FOREGROUND.a * opacity,
                ..Zinc::FOREGROUND
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
    fn its_fill_and_text_fade_together() {
        let style = |o: f32| faded_container(theme::tooltip(&Theme::Dark), o);
        let half = style(0.5);
        assert_eq!(half.text_color.map(|c| c.a), Some(Zinc::BACKGROUND.a * 0.5));
        assert_eq!(
            half.background,
            Some(iced::Background::Color(Zinc::FOREGROUND.lerp(
                Color {
                    a: 0.0,
                    ..Zinc::FOREGROUND
                },
                0.5
            )))
        );
    }
}
