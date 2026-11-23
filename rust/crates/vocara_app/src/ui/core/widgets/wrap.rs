//! A widget that lays out, draws and operates exactly as its content does,
//! and adds its own event handling and cursor on top: the counterpart of the
//! `MouseRegion` / `GestureDetector` / `Listener` / `FocusScope` wrappers the
//! Dart widgets put around what they draw.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::{mouse, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

/// Handles an event after the content has: `(state, event, bounds, cursor,
/// shell)`.
pub type OnEvent<'a, M, S> =
    Box<dyn FnMut(&mut S, &Event, Rectangle, mouse::Cursor, &mut Shell<'_, M>) + 'a>;

/// The cursor over the wrapper, given its state, bounds and the cursor.
pub type Interaction<'a, S> =
    Box<dyn Fn(&S, Rectangle, mouse::Cursor) -> Option<mouse::Interaction> + 'a>;

/// See the module docs. `S` is the wrapper's own state, kept in the widget
/// tree across view rebuilds.
pub struct Wrap<'a, M, S: Default + 'static> {
    content: Element<'a, M, Theme, Renderer>,
    on_event: OnEvent<'a, M, S>,
    interaction: Option<Interaction<'a, S>>,
}

impl<'a, M, S: Default + 'static> Wrap<'a, M, S> {
    pub fn new(
        content: impl Into<Element<'a, M, Theme, Renderer>>,
        on_event: OnEvent<'a, M, S>,
    ) -> Self {
        Self {
            content: content.into(),
            on_event,
            interaction: None,
        }
    }

    /// Overrides the cursor wherever `interaction` returns one.
    pub fn interaction(mut self, interaction: Interaction<'a, S>) -> Self {
        self.interaction = Some(interaction);
        self
    }
}

impl<M, S: Default + 'static> Widget<M, Theme, Renderer> for Wrap<'_, M, S> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
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

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Slot<S>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Slot(S::default()))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
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
        let state = &mut tree.state.downcast_mut::<Slot<S>>().0;
        (self.on_event)(state, event, layout.bounds(), cursor, shell);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if let Some(interaction) = &self.interaction {
            let state = &tree.state.downcast_ref::<Slot<S>>().0;
            if let Some(i) = interaction(state, layout.bounds(), cursor) {
                return i;
            }
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a, S: Default + 'static> From<Wrap<'a, M, S>> for Element<'a, M, Theme, Renderer> {
    fn from(wrap: Wrap<'a, M, S>) -> Self {
        Element::new(wrap)
    }
}

/// The wrapper's state in the tree. A newtype, so a stateless wrapper
/// (`S = ()`) still gets a tag of its own: `Tag::of::<()>()` is iced's
/// "stateless" tag, under which a reused tree keeps no state at all.
struct Slot<S>(S);
