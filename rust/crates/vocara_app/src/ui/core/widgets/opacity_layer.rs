//! Flutter's `Opacity`: a subtree drawn as one layer at an opacity. What
//! Flutter faded through an `OpacityLayer` (`Opacity`, `AnimatedOpacity`,
//! `FadeTransition`, and the dialog, tooltip and button fades built on
//! them) fades through this: the subtree is drawn offscreen, then over what
//! lies behind it at the opacity, so where its parts overlap the topmost
//! shows, at the opacity, not a blend of each part at the opacity. Text,
//! images, quads, canvases and meshes inside fade together, and the clips
//! and transformations inside go on working (`iced_renderer::effects`, the
//! renderers' own, `rust/vendor/README.md`).
//!
//! As Flutter's: at full opacity the subtree is drawn as it is, with no
//! layer; at none it is not drawn at all (`RenderOpacity` skipped painting
//! its child); either way it is laid out and takes input as before.
//!
//! The layer is clipped to the region the subtree is drawn in (the
//! viewport iced draws it with), not to its box, so what it draws past its
//! box (a shadow) fades with it, as Flutter's layer held all its child
//! painted. Like any layer, it is drawn over what was drawn before it in
//! the same layer (iced's widgets that stack children draw each later one
//! in a later layer, so those still go over it).

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::{mouse, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

pub use iced_renderer::effects::{BackdropFilter, Effects};

/// See the module docs.
pub struct OpacityLayer<'a, M> {
    content: Element<'a, M>,
    opacity: f32,
}

/// `content` drawn as one layer at `opacity` (0 to 1).
pub fn opacity_layer<'a, M>(
    opacity: f32,
    content: impl Into<Element<'a, M>>,
) -> OpacityLayer<'a, M> {
    OpacityLayer {
        content: content.into(),
        opacity,
    }
}

impl<M> Widget<M, Theme, Renderer> for OpacityLayer<'_, M> {
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
        let draw = |renderer: &mut Renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        };
        if self.opacity >= 1.0 {
            draw(renderer);
        } else if self.opacity > 0.0 {
            renderer.with_opacity_layer(*viewport, self.opacity, draw);
        }
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

impl<'a, M: 'a> From<OpacityLayer<'a, M>> for Element<'a, M> {
    fn from(layer: OpacityLayer<'a, M>) -> Self {
        Element::new(layer)
    }
}

#[cfg(test)]
mod tests;
