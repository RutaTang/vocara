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
use iced::border::Radius;
use iced::time::Instant;
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Padding, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

use crate::ui::core::motion::{Controller, Curve, Implicit, Lerp};

/// What an [`AnimatedContainer`] paints behind its child: a
/// `BoxDecoration`'s color, its uniform border and its corner radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoration {
    pub background: Color,
    pub border: Color,
    pub border_width: f32,
    pub radius: Radius,
    /// The shadow under the box (`BoxDecoration.boxShadow`'s one): none by
    /// default.
    pub shadow: Shadow,
}

impl Decoration {
    /// A fill with no border.
    pub fn fill(background: Color, radius: impl Into<Radius>) -> Self {
        Self {
            background,
            border: Color::TRANSPARENT,
            border_width: 0.0,
            radius: radius.into(),
            shadow: Shadow::default(),
        }
    }

    /// A fill inside a `width` border.
    pub fn boxed(background: Color, radius: impl Into<Radius>, border: Color, width: f32) -> Self {
        Self {
            background,
            border,
            border_width: width,
            radius: radius.into(),
            shadow: Shadow::default(),
        }
    }

    /// The same box casting `shadow`.
    pub fn with_shadow(mut self, shadow: Shadow) -> Self {
        self.shadow = shadow;
        self
    }
}

/// `BoxDecoration.lerp`: the color, the border's color and width, the
/// radius, the shadow.
impl Lerp for Decoration {
    fn lerp(self, to: Self, t: f32) -> Self {
        let (a, b) = (self.shadow, to.shadow);
        Self {
            shadow: Shadow {
                color: a.color.lerp(b.color, t),
                offset: Vector::new(
                    a.offset.x.lerp(b.offset.x, t),
                    a.offset.y.lerp(b.offset.y, t),
                ),
                blur_radius: a.blur_radius.lerp(b.blur_radius, t).max(0.0),
            },
            background: self.background.lerp(to.background, t),
            border: self.border.lerp(to.border, t),
            border_width: self.border_width.lerp(to.border_width, t).max(0.0),
            radius: Radius {
                top_left: self.radius.top_left.lerp(to.radius.top_left, t),
                top_right: self.radius.top_right.lerp(to.radius.top_right, t),
                bottom_right: self.radius.bottom_right.lerp(to.radius.bottom_right, t),
                bottom_left: self.radius.bottom_left.lerp(to.radius.bottom_left, t),
            },
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
        shadow: Shadow {
            color: Color {
                a: 0.0,
                ..d.shadow.color
            },
            ..d.shadow
        },
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

    /// A frame at `at`: moves to `target` along `curve`, and asks for the
    /// next frame while it moves. Returns what it shows now.
    fn frame<M>(&mut self, target: T, curve: Curve, at: Instant, shell: &mut Shell<'_, M>) -> T {
        let now = self.now.map_or(at, |was| was.max(at));
        self.now = Some(now);
        self.motion.retarget_along(target, curve, now);
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
        padding: Padding::ZERO,
        align: (-1.0, -1.0),
        width: size.width.fluid(),
        height: size.height.fluid(),
        duration,
        curve: Curve::Linear,
    }
}

/// How long an `InkWell`'s hover highlight takes to fade in or out (its
/// `hoverDuration`'s default).
pub const INK_HOVER: Duration = Duration::from_millis(50);

/// How long an `InkWell`'s pressed highlight takes to fade in or out
/// (`InkResponse.getFadeDurationForType`, pressed).
pub const INK_PRESS: Duration = Duration::from_millis(200);

/// The pressed highlight's color: the theme's `highlightColor`, a dark
/// `ThemeData`'s default (`Color(0x40CCCCCC)`; forui's approximate Material
/// theme sets none).
pub const INK_HIGHLIGHT: Color = Color::from_rgba8(0xCC, 0xCC, 0xCC, 0x40 as f32 / 255.0);

/// How far a press may wander before it is no longer a tap (`kTouchSlop`,
/// a tap recognizer's slop).
const TOUCH_SLOP: f32 = 18.0;

/// An `InkWell(hoverColor: color)` around `content` (a transparent button),
/// its highlights `radius` round: the hover one fades in under the pointer
/// and out after it over [`INK_HOVER`], and the pressed one
/// ([`INK_HIGHLIGHT`], over it) fades in while a press is down and out
/// after it over [`INK_PRESS`], each linear (`InkHighlight`'s alpha).
/// Splashes are off app-wide (forui's theme sets `NoSplash`).
pub fn ink_hover<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    color: Color,
    radius: impl Into<Radius>,
) -> Element<'a, M> {
    ink_well(content, color, INK_HIGHLIGHT, radius)
}

/// [`ink_hover`] whose pressed highlight is `pressed` rather than
/// [`INK_HIGHLIGHT`]: an `InkWell` whose `overlayColor` resolves the hovered
/// and the pressed states to `color` and `pressed` (a Material 3 button's).
pub fn ink_well<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    color: Color,
    pressed: Color,
    radius: impl Into<Radius>,
) -> Element<'a, M> {
    let radius = radius.into();
    let pressed = InkPress {
        content: content.into(),
        color: pressed,
        radius,
    };
    animated_container(Element::new(pressed), INK_HOVER)
        .style(move |hovered| {
            let a = if hovered { color.a } else { 0.0 };
            Decoration::fill(Color { a, ..color }, radius)
        })
        .into()
}

/// An `InkWell`'s pressed highlight, in `color`, under `content` (see
/// [`ink_hover`]).
struct InkPress<'a, M> {
    content: Element<'a, M>,
    color: Color,
    radius: Radius,
}

/// The pressed highlight's state: its `AnimationController` (forward as a
/// press comes down, back as it goes, from wherever it got) and the press.
struct PressState {
    highlight: Controller,
    /// Whether the highlight is on (the controller's last direction).
    active: bool,
    /// Where the press that holds the highlight came down.
    down: Option<iced::Point>,
    /// The last frame's time.
    now: Option<Instant>,
}

impl PressState {
    fn shown(&self) -> f32 {
        self.now.map_or(0.0, |now| self.highlight.value(now))
    }
}

impl<M> Widget<M, Theme, Renderer> for InkPress<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<PressState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(PressState {
            highlight: Controller::new(INK_PRESS, INK_PRESS),
            active: false,
            down: None,
            now: None,
        })
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
        let state = tree.state.downcast_mut::<PressState>();
        let was = state.down;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.down = cursor.position_over(layout.bounds());
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.down = None;
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                // Wandered off: the tap is cancelled.
                if state
                    .down
                    .is_some_and(|at| at.distance(*position) > TOUCH_SLOP)
                {
                    state.down = None;
                }
            }
            Event::Window(window::Event::RedrawRequested(at)) => {
                let now = state.now.map_or(*at, |was| was.max(*at));
                state.now = Some(now);
                let on = state.down.is_some();
                if on != state.active {
                    state.active = on;
                    if on {
                        state.highlight.forward(now);
                    } else {
                        state.highlight.reverse(now);
                    }
                }
                if state.highlight.is_animating(now) {
                    shell.request_redraw();
                }
            }
            _ => {}
        }
        if state.down != was {
            shell.request_redraw();
        }
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
        let shown = tree.state.downcast_ref::<PressState>().shown();
        if shown > 0.0 {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        radius: self.radius,
                        ..Border::default()
                    },
                    shadow: Shadow::default(),
                    snap: false,
                },
                Background::Color(Color {
                    a: self.color.a * shown,
                    ..self.color
                }),
            );
        }
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
            let look = state.frame(self.look(state.hovered), self.curve, *at, shell);
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
        let decoration = state.shown().decoration;
        let bounds = layout.bounds();
        let quad = |renderer: &mut Renderer, background: Color, border: Border, shadow: Shadow| {
            if background.a > 0.0
                || (border.width > 0.0 && border.color.a > 0.0)
                || shadow.color.a > 0.0
            {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border,
                        shadow,
                        snap: false,
                    },
                    Background::Color(background),
                );
            }
        };
        let border = |d: Decoration| Border {
            color: d.border,
            width: d.border_width,
            radius: d.radius,
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
            quad(renderer, d.background, under, d.shadow);
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
        // A layer of its own: the child may have drawn in layers above this
        // one (a `hover`'s top face does). Clipped to what is in view: a
        // layer is not clipped by the one it is drawn in, and the outlines
        // of Home's cards scrolled under its toolbar were drawn over it.
        let clip = bounds.intersection(viewport);
        if let (Some(d), Some(clip)) = (decoration.filter(|_| self.border_above), clip) {
            renderer.with_layer(clip, |renderer| {
                quad(renderer, Color::TRANSPARENT, border(d), Shadow::default());
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
            let value = state.frame(self.target, self.curve, *at, shell);
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
