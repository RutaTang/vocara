//! The keyboard's focus traversal: Tab and Shift+Tab move the focus to the
//! next and the previous text field, as Flutter's default shortcuts did
//! (`WidgetsApp.defaultShortcuts`' `NextFocusIntent` and
//! `PreviousFocusIntent`, which a focused field on macOS got back from the
//! platform as `insertTab:` and `insertBacktab:`), through the
//! `ReadingOrderTraversalPolicy` of the app's `FocusTraversalGroup`.
//!
//! [`focus_scope`] is a route's `FocusScope`: the top page, each dialog,
//! and an overlay that holds the keyboard ([`holds_keyboard`]) each wrap
//! their content in one. Tab moves among the fields inside it alone and
//! goes round at its ends: a dialog's scope looped
//! (`TraversalEdgeBehavior.closedLoop`, `showDialog`'s), a page's handed
//! the focus to the navigator's scope (`parentScope`), whose only
//! traversable route was the page again, and `HoldsKeyboard`'s scope
//! looped. With nothing focused, Tab takes the first field and Shift+Tab
//! the last (`findFirstFocus`, `findLastFocus`). The key is the scope's
//! (the action consumed it) whether or not a field took the focus.
//!
//! The order is the fields' order in the widget tree, which on every
//! screen with more than one field (the technique editor's form) is their
//! reading order: one column, top to bottom.
//!
//! A field the traversal focuses is focused "from outside": a single-line
//! one selects all its text (`EditableText.selectAllOnFocus`, true on
//! macOS in Flutter 3.38: `_adjustedSelectionWhenFocused`, not
//! multi-line, not an internal focus change), a multi-line one keeps its
//! selection. A disabled field takes no focus (`canRequestFocus` false),
//! a read-only one (`SelectableText`) does. The scroll views around the
//! field it lands on then show it (`Scrollable.ensureVisible`, at once):
//! moving forward, its bottom at the view's bottom if it was below it
//! (`keepVisibleAtEnd`); back, its top at the view's top if it was above
//! (`keepVisibleAtStart`); after which the field reveals its caret as its
//! focus coming does ([`text_field::input::scroll`]). (A view inside
//! another is taken as laid out, its own scroll left out of where the
//! field is: no screen has a field in nested scroll views.)
//!
//! A multi-line field focused by Tab keeps its caret where its `Content`
//! has it, the start of one it was never put elsewhere in, where Flutter
//! put a field that had had no selection yet at its end (the app's own
//! focus requests do the same).
//!
//! What else Tab stopped on in Flutter, which the port leaves out: the
//! app's own buttons (`StudioButton`, the icon buttons, the cards) were
//! `GestureDetector`s and took no focus, so a scope's only stops were its
//! text fields, but for Material's own focusable widgets, all on screens
//! with no text field: the settings page's `InkWell` rows, its `Slider`
//! and `Switch`, the license page's `BackButton` and `ListTile`s, and the
//! `InkWell` items of the tag and track menus. There Tab moved a focus
//! highlight from one to the next and Space or Enter activated it; iced's
//! buttons, sliders and switches take no keyboard focus and draw none, so
//! Tab does nothing on those screens.
//!
//! [`holds_keyboard`]: super::holds_keyboard
//! [`text_field::input::scroll`]: super::text_field::input::scroll

use std::any::Any;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::operation::scrollable::AbsoluteOffset;
use iced::advanced::widget::operation::Scrollable as ScrollableState;
use iced::advanced::widget::{tree, Id, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::keyboard::{self, key::Named};
use iced::{mouse, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

use super::text_field::{editor, input};

/// A field the traversal can move the focus to: the state of one of the
/// app's text fields, as its `operate` hands it over (`Operation::custom`,
/// with the field's text box as its bounds).
pub trait Traversable {
    /// It can take the focus (`canRequestFocus`: not disabled).
    fn can_focus(&self) -> bool;
    /// It has the focus.
    fn is_focused(&self) -> bool;
    /// The traversal focuses it (see the module docs).
    fn focus_by_traversal(&mut self);
    /// The traversal moves the focus away from it.
    fn unfocus_by_traversal(&mut self);
}

/// The traversable state behind an operation's `custom` call, if it is a
/// text field's.
fn traversable(state: &mut dyn Any) -> Option<&mut dyn Traversable> {
    if state.is::<input::State>() {
        return state
            .downcast_mut::<input::State>()
            .map(|s| s as &mut dyn Traversable);
    }
    state
        .downcast_mut::<editor::State>()
        .map(|s| s as &mut dyn Traversable)
}

/// `content` as a focus scope (see the module docs).
pub fn focus_scope<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    Element::new(FocusScope {
        content: content.into(),
    })
}

struct FocusScope<'a, M> {
    content: Element<'a, M>,
}

/// The fields in a scope, in order: whether each can take the focus, has
/// it, and its text box.
#[derive(Default)]
struct Survey {
    fields: Vec<(bool, bool, Rectangle)>,
}

impl Operation for Survey {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(&mut self, _id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
        if let Some(field) = traversable(state) {
            self.fields
                .push((field.can_focus(), field.is_focused(), bounds));
        }
    }
}

impl Survey {
    /// The field Tab (`forward`) or Shift+Tab lands on, among those that
    /// can take the focus, and the one that has it.
    fn target(&self, forward: bool) -> Option<(usize, Option<usize>)> {
        let open: Vec<usize> = (0..self.fields.len())
            .filter(|&i| self.fields[i].0)
            .collect();
        let count = open.len();
        if count == 0 {
            return None;
        }
        let focused = open.iter().position(|&i| self.fields[i].1);
        let next = match focused {
            None if forward => 0,
            None => count - 1,
            Some(at) if forward => (at + 1) % count,
            Some(at) => (at + count - 1) % count,
        };
        Some((open[next], focused.map(|at| open[at])))
    }
}

/// Moves the focus to the field at `target` (counting every field), away
/// from any other.
struct Move {
    target: usize,
    index: usize,
}

impl Operation for Move {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Any) {
        if let Some(field) = traversable(state) {
            if self.index == self.target {
                field.focus_by_traversal();
            } else if field.is_focused() {
                field.unfocus_by_traversal();
            }
            self.index += 1;
        }
    }
}

/// `Scrollable.ensureVisible` of a field's text box `rect`, at once: every
/// vertical scroll view whose content holds it scrolls, forward, until its
/// bottom is in view (`keepVisibleAtEnd`), back, until its top is
/// (`keepVisibleAtStart`).
struct EnsureVisible {
    rect: Rectangle,
    forward: bool,
}

impl EnsureVisible {
    /// The offset down a view of `viewport` height over `content` scrolled
    /// `pixels` down takes to show the field.
    fn target(&self, viewport: f32, content: Rectangle, pixels: f32) -> f32 {
        let range = (content.height - viewport).max(0.0);
        let top = self.rect.y - content.y;
        if self.forward {
            (top + self.rect.height - viewport)
                .clamp(0.0, range)
                .max(pixels)
        } else {
            top.clamp(0.0, range).min(pixels)
        }
    }
}

impl Operation for EnsureVisible {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        state: &mut dyn ScrollableState,
    ) {
        let inside = self.rect.y >= content.y
            && self.rect.y + self.rect.height <= content.y + content.height;
        if !inside || content.height <= bounds.height {
            return;
        }
        let to = self.target(bounds.height, content, translation.y);
        if to != translation.y {
            state.scroll_to(AbsoluteOffset {
                x: None,
                y: Some(to),
            });
        }
    }
}

/// Whether a text field in `content` (laid out as `layout`) has the focus:
/// it holds the keyboard, and a screen's shortcuts leave every key to it.
/// Asked before the field has the key: a multi-line field lets Escape go
/// by uncaptured as it gives up the focus.
pub fn holds_focus<M>(
    content: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
) -> bool {
    let mut survey = Survey::default();
    content
        .as_widget_mut()
        .operate(tree, layout, renderer, &mut survey);
    survey.fields.iter().any(|&(_, focused, _)| focused)
}

/// Tab (`forward`) or Shift+Tab in the scope over `content`.
fn traverse<M>(
    content: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
    forward: bool,
) {
    let widget = content.as_widget_mut();
    let mut survey = Survey::default();
    widget.operate(tree, layout, renderer, &mut survey);
    let Some((target, focused)) = survey.target(forward) else {
        return;
    };
    if focused != Some(target) {
        widget.operate(tree, layout, renderer, &mut Move { target, index: 0 });
    }
    let rect = survey.fields[target].2;
    widget.operate(tree, layout, renderer, &mut EnsureVisible { rect, forward });
}

impl<M> Widget<M, Theme, Renderer> for FocusScope<'_, M> {
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
        if shell.is_event_captured() {
            return;
        }
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(Named::Tab),
            modifiers,
            ..
        }) = event
        {
            // Plain Tab and Shift+Tab only (`SingleActivator`s).
            if modifiers.control() || modifiers.alt() || modifiers.logo() {
                return;
            }
            traverse(
                &mut self.content,
                &mut tree.children[0],
                layout,
                renderer,
                !modifiers.shift(),
            );
            shell.capture_event();
            shell.request_redraw();
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

#[cfg(test)]
mod tests;
