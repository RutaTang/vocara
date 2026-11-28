//! The page's keyboard shortcuts: Space plays or pauses (what the transport's
//! play button does, refusals and toasts included, and nothing while that
//! button is dead), Delete or Backspace deletes the selected clip (asked
//! first, as the clip menu's Delete is), Escape lets go of the selected clip.
//!
//! The page hands a key to its shortcuts only once everything on it has had
//! the key and left it: a text field with the focus (the note editor's, the
//! lyrics editor's) takes its keys, and one with the focus before the key
//! keeps the key whatever it does with it (the lyrics editor lets Escape go
//! by as it gives up the focus); the lyrics' sync view takes Space to stamp
//! a line; the open menu, the note editor, the technique picker and the
//! lyrics Focus view hold the keyboard while they are up
//! (`holds_keyboard`), and the app's dialogs over the page take every key
//! before the page sees one. Escape closes the open menu that way: the menu
//! takes it. A key held down does not repeat a shortcut, and none takes a
//! modifier (Cmd+Backspace in a field is the field's).

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::keyboard::{self, key::Named};
use iced::{mouse, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

use crate::ui::core::widgets::focus_traversal::holds_focus;

/// A shortcut of the page's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Space: the transport's play / pause.
    PlayPause,
    /// Delete or Backspace: delete the selected clip.
    DeleteClip,
    /// Escape: let go of the selected clip.
    Deselect,
}

/// The shortcut a key press is, if any: a first press (not a repeat) with
/// no modifier.
pub fn shortcut(event: &keyboard::Event) -> Option<Shortcut> {
    let keyboard::Event::KeyPressed {
        key,
        modifiers,
        repeat: false,
        ..
    } = event
    else {
        return None;
    };
    if modifiers.control() || modifiers.shift() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    match key {
        keyboard::Key::Named(Named::Space) => Some(Shortcut::PlayPause),
        keyboard::Key::Named(Named::Delete | Named::Backspace) => Some(Shortcut::DeleteClip),
        keyboard::Key::Named(Named::Escape) => Some(Shortcut::Deselect),
        _ => None,
    }
}

/// `content` with the page's shortcuts: `on_key` says what each does now
/// (`None` when it does nothing, and the key goes on unanswered).
pub fn page_shortcuts<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    on_key: impl Fn(Shortcut) -> Option<M> + 'a,
) -> Element<'a, M> {
    Element::new(PageShortcuts {
        content: content.into(),
        on_key: Box::new(on_key),
    })
}

struct PageShortcuts<'a, M> {
    content: Element<'a, M>,
    on_key: Box<dyn Fn(Shortcut) -> Option<M> + 'a>,
}

impl<M> Widget<M, Theme, Renderer> for PageShortcuts<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
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
        let key = match event {
            Event::Keyboard(key) => shortcut(key),
            _ => None,
        };
        // Asked before the field has the key (see the module docs).
        let typing = key.is_some()
            && holds_focus(&mut self.content, &mut tree.children[0], layout, renderer);
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
        if typing || shell.is_event_captured() {
            return;
        }
        if let Some(message) = key.and_then(|key| (self.on_key)(key)) {
            shell.publish(message);
            shell.capture_event();
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
