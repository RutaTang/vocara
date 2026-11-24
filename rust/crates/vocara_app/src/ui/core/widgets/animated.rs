//! Flutter's implicitly animated widgets: [`AnimatedContainer`] (its
//! decoration, padding and alignment; `AnimatedAlign` is one with only an
//! alignment) and [`Animated`], which builds its content from a value that
//! animates (`AnimatedOpacity`, `AnimatedRotation`, a width).
//!
//! Each keeps an [`Implicit`] in the widget tree: the view gives it its
//! target, and it moves there from what it shows over its duration along
//! its curve, as Flutter's did. The first target shows at once. The time
//! comes from the window's frames (`RedrawRequested`), so a test injects it
//! with that event; a widget asks for the next frame only while it moves,
//! and relays out only while what it lays out moves.
//!
//! An [`AnimatedContainer`] can follow the pointer itself (`MouseRegion`'s
//! hover, as the Dart widgets set it around their `AnimatedContainer`): its
//! style gets whether the pointer is over it.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::time::Instant;
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Padding, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

use crate::ui::core::motion::{Curve, Implicit, Lerp};

/// What an [`AnimatedContainer`] paints behind its child: a
/// `BoxDecoration`'s color, its uniform border and its corner radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoration {
    pub background: Color,
    pub border: Color,
    pub border_width: f32,
    pub radius: f32,
}

impl Decoration {
    /// A fill with no border.
    pub fn fill(background: Color, radius: f32) -> Self {
        Self {
            background,
            border: Color::TRANSPARENT,
            border_width: 0.0,
            radius,
        }
    }

    /// A fill inside a `width` border.
    pub fn boxed(background: Color, radius: f32, border: Color, width: f32) -> Self {
        Self {
            background,
            border,
            border_width: width,
            radius,
        }
    }
}

/// `BoxDecoration.lerp`: the color, the border's color and width, the
/// radius.
impl Lerp for Decoration {
    fn lerp(self, to: Self, t: f32) -> Self {
        Self {
            background: self.background.lerp(to.background, t),
            border: self.border.lerp(to.border, t),
            border_width: self.border_width.lerp(to.border_width, t).max(0.0),
            radius: self.radius.lerp(to.radius, t),
        }
    }
}

/// What an [`AnimatedContainer`] animates: its decoration, its padding and
/// its child's alignment (Flutter's `Alignment`: -1 start, 0 centre, 1 end).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Look {
    decoration: Option<Decoration>,
    padding: Padding,
    align: (f32, f32),
}

impl Lerp for Look {
    fn lerp(self, to: Self, t: f32) -> Self {
        let decoration = match (self.decoration, to.decoration) {
            (Some(a), Some(b)) => Some(a.lerp(b, t)),
            // `Decoration.lerp` with a null end fades the other.
            (Some(a), None) => Some(a.lerp(faded(a), t)),
            (None, Some(b)) => Some(faded(b).lerp(b, t)),
            (None, None) => None,
        };
        Self {
            decoration,
            padding: self.padding.lerp(to.padding, t),
            align: self.align.lerp(to.align, t),
        }
    }
}

fn faded(d: Decoration) -> Decoration {
    Decoration {
        background: Color {
            a: 0.0,
            ..d.background
        },
        border: Color { a: 0.0, ..d.border },
        ..d
    }
}

/// An animation's state in the widget tree.
struct State<T> {
    motion: Implicit<T>,
    /// The last frame's time (the latest seen: a test may inject a later
    /// one than the frame the simulator draws).
    now: Option<Instant>,
    /// Whether the pointer is over the widget.
    hovered: bool,
    /// The value the content was last laid out with.
    laid_out: Option<T>,
}

impl<T: Lerp> State<T> {
    fn new(value: T, duration: Duration, curve: Curve) -> Self {
        Self {
            motion: Implicit::new(value, duration, curve),
            now: None,
            hovered: false,
            laid_out: None,
        }
    }

    /// What it shows at the last frame.
    fn shown(&self) -> T {
        self.now
            .map_or_else(|| self.motion.target(), |now| self.motion.value(now))
    }

    /// A frame at `at`: moves to `target`, and asks for the next frame
    /// while it moves. Returns what it shows now.
    fn frame<M>(&mut self, target: T, at: Instant, shell: &mut Shell<'_, M>) -> T {
        let now = self.now.map_or(at, |was| was.max(at));
        self.now = Some(now);
        self.motion.retarget(target, now);
        if self.motion.is_animating(now) {
            shell.request_redraw();
        }
        self.motion.value(now)
    }
}

// --------------------------------------------------------- AnimatedContainer

/// The style an [`AnimatedContainer`] animates to, given whether the pointer
/// is over it.
pub type StyleFn<'a> = Box<dyn Fn(bool) -> Option<Decoration> + 'a>;

/// `AnimatedContainer`: a box that paints a decoration behind its child,
/// pads it and aligns it, and animates any change to the three (see the
/// module docs). Its size follows its child, as an iced container's does.
pub struct AnimatedContainer<'a, M> {
    content: Element<'a, M>,
    style: StyleFn<'a>,
    /// Whether the border is drawn over the child (see
    /// [`AnimatedContainer::border_above`]).
    border_above: bool,
    /// Scales the decoration's alpha, not animated (an `Opacity` around it).
    opacity: f32,
    padding: Padding,
    align: (f32, f32),
    width: Length,
    height: Length,
    duration: Duration,
    curve: Curve,
}

/// `AnimatedContainer(duration: duration, child: content)`, linear, with
/// no decoration, padding or alignment until set.
pub fn animated_container<'a, M>(
    content: impl Into<Element<'a, M>>,
    duration: Duration,
) -> AnimatedContainer<'a, M> {
    let content = content.into();
    let size = content.as_widget().size_hint();
    AnimatedContainer {
        content,
        style: Box::new(|_| None),
        border_above: false,
        opacity: 1.0,
        padding: Padding::ZERO,
        align: (-1.0, -1.0),
        width: size.width.fluid(),
        height: size.height.fluid(),
        duration,
        curve: Curve::Linear,
    }
}

/// `AnimatedAlign(alignment: align, duration: duration, curve: curve)`.
pub fn animated_align<'a, M>(
    content: impl Into<Element<'a, M>>,
    align: (f32, f32),
    duration: Duration,
    curve: Curve,
) -> AnimatedContainer<'a, M> {
    animated_container(content, duration)
        .align(align)
        .curve(curve)
        .width(Length::Fill)
        .height(Length::Fill)
}

impl<'a, M> AnimatedContainer<'a, M> {
    /// The decoration, the same whether hovered or not.
    pub fn decoration(self, decoration: Decoration) -> Self {
        self.style(move |_| decoration)
    }

    /// The decoration, given whether the pointer is over the box.
    pub fn style(mut self, style: impl Fn(bool) -> Decoration + 'a) -> Self {
        self.style = Box::new(move |hovered| Some(style(hovered)));
        self
    }

    /// Draws the border over the child rather than beneath it: for a child
    /// that fills the box with an opaque face of its own (a `BoxDecoration`'s
    /// border lies over its fill, and the child inside it).
    pub fn border_above(mut self) -> Self {
        self.border_above = true;
        self
    }

    /// Draws the decoration at `opacity` (an `Opacity` widget around the
    /// Dart `AnimatedContainer`): it applies at once, whatever animates.
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Where the child sits in the padded box (Flutter's `Alignment`).
    pub fn align(mut self, align: (f32, f32)) -> Self {
        self.align = align;
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    pub fn curve(mut self, curve: Curve) -> Self {
        self.curve = curve;
        self
    }

    fn look(&self, hovered: bool) -> Look {
        Look {
            decoration: (self.style)(hovered),
            padding: self.padding,
            align: self.align,
        }
    }
}

/// Lays `content` out padded by `padding` in a `width` × `height` box, at
/// `align` in what the padding leaves (as `layout::positioned` does for an
/// iced container).
fn positioned<M>(
    content: &mut Element<'_, M>,
    tree: &mut Tree,
    renderer: &Renderer,
    limits: &layout::Limits,
    (width, height): (Length, Length),
    padding: Padding,
    align: (f32, f32),
) -> layout::Node {
    layout::positioned(
        limits,
        width,
        height,
        padding,
        |limits| {
            content
                .as_widget_mut()
                .layout(tree, renderer, &limits.loose())
        },
        |node, space| {
            let size = node.size();
            node.translate(Vector::new(
                (space.width - size.width) * (align.0 + 1.0) / 2.0,
                (space.height - size.height) * (align.1 + 1.0) / 2.0,
            ))
        },
    )
}

impl<M> Widget<M, Theme, Renderer> for AnimatedContainer<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Look>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.look(false), self.duration, self.curve))
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
        let state = tree.state.downcast_mut::<State<Look>>();
        let look = state.shown();
        state.laid_out = Some(look);
        positioned(
            &mut self.content,
            &mut tree.children[0],
            renderer,
            limits,
            (self.width, self.height),
            look.padding,
            look.align,
        )
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
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("the child's layout"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let state = tree.state.downcast_mut::<State<Look>>();
        let hovered = cursor.is_over(layout.bounds());
        if hovered != state.hovered {
            state.hovered = hovered;
            shell.request_redraw();
        }
        if let Event::Window(window::Event::RedrawRequested(at)) = event {
            let look = state.frame(self.look(state.hovered), *at, shell);
            let moved = state
                .laid_out
                .is_some_and(|was| (was.padding, was.align) != (look.padding, look.align));
            if moved {
                shell.invalidate_layout();
            }
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
        let state = tree.state.downcast_ref::<State<Look>>();
        let opacity = self.opacity;
        let decoration = state.shown().decoration.map(|d| Decoration {
            background: Color {
                a: d.background.a * opacity,
                ..d.background
            },
            border: Color {
                a: d.border.a * opacity,
                ..d.border
            },
            ..d
        });
        let bounds = layout.bounds();
        let quad = |renderer: &mut Renderer, background: Color, border: Border| {
            if background.a > 0.0 || (border.width > 0.0 && border.color.a > 0.0) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border,
                        shadow: Shadow::default(),
                        snap: false,
                    },
                    Background::Color(background),
                );
            }
        };
        let border = |d: Decoration| Border {
            color: d.border,
            width: d.border_width,
            radius: d.radius.into(),
        };
        if let Some(d) = decoration {
            let under = if self.border_above {
                Border {
                    width: 0.0,
                    ..border(d)
                }
            } else {
                border(d)
            };
            quad(renderer, d.background, under);
        }
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().expect("the child's layout"),
            cursor,
            viewport,
        );
        if let Some(d) = decoration.filter(|_| self.border_above) {
            // A layer of its own: the child may have drawn in layers above
            // this one (a `hover`'s top face does).
            renderer.with_layer(bounds, |renderer| {
                quad(renderer, Color::TRANSPARENT, border(d));
            });
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().next().expect("the child's layout"),
                renderer,
                operation,
            );
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("the child's layout"),
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().expect("the child's layout"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<AnimatedContainer<'a, M>> for Element<'a, M> {
    fn from(container: AnimatedContainer<'a, M>) -> Self {
        Element::new(container)
    }
}

// ------------------------------------------------------------------ Animated

/// Builds the content of an [`Animated`] from the value it shows.
pub type ViewFn<'a, T, M> = Box<dyn Fn(T) -> Element<'a, M> + 'a>;

/// A widget whose content is built from a value that animates: on a new
/// target the value moves there (see the module docs) and the content is
/// rebuilt from it each frame — `AnimatedOpacity`'s opacity,
/// `AnimatedRotation`'s turns, an `AnimatedContainer`'s width.
pub struct Animated<'a, T, M> {
    target: T,
    /// What it shows when it first appears (its target, unless set).
    initial: T,
    duration: Duration,
    curve: Curve,
    view: ViewFn<'a, T, M>,
    content: Element<'a, M>,
    /// The value `content` was built from.
    built: T,
}

/// `view(value)`, `value` moving to `target` over `duration` along `curve`
/// whenever the target changes.
pub fn animated<'a, T: Lerp + 'static, M>(
    target: T,
    duration: Duration,
    curve: Curve,
    view: impl Fn(T) -> Element<'a, M> + 'a,
) -> Animated<'a, T, M> {
    let content = view(target);
    Animated {
        target,
        initial: target,
        duration,
        curve,
        view: Box::new(view),
        content,
        built: target,
    }
}

impl<'a, T: Lerp + 'static, M> Animated<'a, T, M> {
    /// Starts at `initial` when it first appears, and moves to its target
    /// from there (a transition's entrance, as a route's animation running
    /// forward from 0 when it is pushed).
    pub fn from(mut self, initial: T) -> Self {
        self.initial = initial;
        if initial != self.built {
            self.content = (self.view)(initial);
            self.built = initial;
        }
        self
    }
}

impl<T: Lerp + 'static, M> Widget<M, Theme, Renderer> for Animated<'_, T, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<T>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.initial, self.duration, self.curve))
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
        let state = tree.state.downcast_mut::<State<T>>();
        let value = state.shown();
        state.laid_out = Some(value);
        if value != self.built {
            self.content = (self.view)(value);
            self.built = value;
            tree.diff_children(std::slice::from_ref(&self.content));
        }
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
        if let Event::Window(window::Event::RedrawRequested(at)) = event {
            let state = tree.state.downcast_mut::<State<T>>();
            let value = state.frame(self.target, *at, shell);
            if state.laid_out.is_some_and(|was| was != value) {
                shell.invalidate_layout();
            }
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
        self.content.as_widget().draw(
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
        self.content
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, T: Lerp + 'static, M: 'a> From<Animated<'a, T, M>> for Element<'a, M> {
    fn from(animated: Animated<'a, T, M>) -> Self {
        Element::new(animated)
    }
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
