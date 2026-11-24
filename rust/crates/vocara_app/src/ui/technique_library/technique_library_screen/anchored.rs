//! A trigger with, while it is open, a menu anchored under it: the tag
//! filter's dropdown (`_TagMenuRoute`, a `PopupRoute` laid out by
//! `_TagMenuDelegate`).
//!
//! The menu right-aligns under the trigger, 6 px below it, kept 8 px inside
//! the window; it is laid out within the window less those 8 px all round.
//! Like the popup route, it is modal with no barrier color: a tap outside
//! the menu (any button, landing on its release within the tap slop:
//! `gesture::AnyTap`) or Escape dismisses it (`barrierDismissible`), and
//! while it is open nothing beneath sees the pointer or the keyboard. The menu is drawn
//! from the current state on every view, so a reload landing while it is
//! open updates it (Dart rebuilt it with a `ListenableBuilder` for that).

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::{
    keyboard, mouse, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
};

use crate::ui::core::widgets::gesture::{any_tap_event, AnyTap};

/// The gap between the trigger's bottom and the menu.
pub const GAP: f32 = 6.0;
/// How close the menu may come to the window's edges.
pub const MARGIN: f32 = 8.0;

/// See the module docs.
pub struct Anchored<'a, M> {
    trigger: Element<'a, M, Theme, Renderer>,
    menu: Option<Element<'a, M, Theme, Renderer>>,
    on_dismiss: M,
}

impl<'a, M> Anchored<'a, M> {
    /// `trigger`, with `menu` open under it when `Some`; a tap outside the
    /// menu or Escape reports `on_dismiss`.
    pub fn new(
        trigger: impl Into<Element<'a, M, Theme, Renderer>>,
        menu: Option<Element<'a, M, Theme, Renderer>>,
        on_dismiss: M,
    ) -> Self {
        Self {
            trigger: trigger.into(),
            menu,
            on_dismiss,
        }
    }
}

/// Where `_TagMenuDelegate` puts a menu of `menu` size in a window of
/// `window` size, anchored to `anchor`: right-aligned under it, then pulled
/// back inside the margin.
pub fn position(anchor: Rectangle, menu: Size, window: Size) -> Point {
    let mut x = anchor.x + anchor.width - menu.width;
    let mut y = anchor.y + anchor.height + GAP;
    if x + menu.width > window.width - MARGIN {
        x = window.width - menu.width - MARGIN;
    }
    if y + menu.height > window.height - MARGIN {
        y = window.height - menu.height - MARGIN;
    }
    Point::new(
        x.max(MARGIN).min(window.width.max(MARGIN)),
        y.max(MARGIN).min(window.height.max(MARGIN)),
    )
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for Anchored<'a, M> {
    fn size(&self) -> Size<Length> {
        self.trigger.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.trigger.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<AnyTap>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(AnyTap::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![
            Tree::new(&self.trigger),
            self.menu.as_ref().map_or_else(Tree::empty, Tree::new),
        ]
    }

    fn diff(&self, tree: &mut Tree) {
        if self.menu.is_none() {
            // The barrier's tap goes with the menu.
            *tree.state.downcast_mut::<AnyTap>() = AnyTap::default();
        }
        tree.children.resize_with(2, Tree::empty);
        tree.children[0].diff(&self.trigger);
        match &self.menu {
            Some(menu) => tree.children[1].diff(menu),
            None => tree.children[1] = Tree::empty(),
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.trigger
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
        self.trigger.as_widget().draw(
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
        self.trigger
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
        self.trigger.as_widget_mut().update(
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

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.trigger.as_widget().mouse_interaction(
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
        let [trigger_tree, menu_tree] = children.as_mut_slice() else {
            return None;
        };
        match &mut self.menu {
            Some(menu) => Some(overlay::Element::new(Box::new(Menu {
                menu,
                tree: menu_tree,
                anchor: layout.bounds() + translation,
                barrier: state.downcast_mut::<AnyTap>(),
                on_dismiss: self.on_dismiss.clone(),
            }))),
            None => self.trigger.as_widget_mut().overlay(
                trigger_tree,
                layout,
                renderer,
                viewport,
                translation,
            ),
        }
    }
}

impl<'a, M: Clone + 'a> From<Anchored<'a, M>> for Element<'a, M, Theme, Renderer> {
    fn from(anchored: Anchored<'a, M>) -> Self {
        Element::new(anchored)
    }
}

/// The open menu over the whole window.
struct Menu<'a, 'b, M> {
    menu: &'b mut Element<'a, M, Theme, Renderer>,
    tree: &'b mut Tree,
    anchor: Rectangle,
    /// A tap on the barrier in flight.
    barrier: &'b mut AnyTap,
    on_dismiss: M,
}

impl<M: Clone> overlay::Overlay<M, Theme, Renderer> for Menu<'_, '_, M> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(
            Size::ZERO,
            Size::new(
                (bounds.width - 2.0 * MARGIN).max(0.0),
                (bounds.height - 2.0 * MARGIN).max(0.0),
            ),
        );
        let node = self
            .menu
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let at = position(self.anchor, node.size(), bounds);
        // The node spans the window (the barrier); its one child is the
        // menu.
        layout::Node::with_children(bounds, vec![node.move_to(at)])
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let Some(menu) = layout.children().next() else {
            return;
        };
        self.menu.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            menu,
            cursor,
            &layout.bounds(),
        );
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        if let Some(menu) = layout.children().next() {
            self.menu
                .as_widget_mut()
                .operate(self.tree, menu, renderer, operation);
        }
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
    ) {
        let Some(menu) = layout.children().next() else {
            return;
        };
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) = event
        {
            shell.publish(self.on_dismiss.clone());
            shell.capture_event();
            return;
        }
        let bounds = menu.bounds();
        let (tapped, taken) = any_tap_event(
            self.barrier,
            event,
            |at| !bounds.contains(at),
            cursor.position(),
        );
        if tapped {
            shell.publish(self.on_dismiss.clone());
        }
        if taken {
            shell.capture_event();
            return;
        }
        self.menu.as_widget_mut().update(
            self.tree,
            event,
            menu,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
        // The barrier: nothing beneath sees the pointer or the keyboard.
        if matches!(
            event,
            Event::Mouse(_) | Event::Touch(_) | Event::Keyboard(_) | Event::InputMethod(_)
        ) {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction =
            layout
                .children()
                .next()
                .map_or_else(mouse::Interaction::default, |menu| {
                    self.menu.as_widget().mouse_interaction(
                        self.tree,
                        menu,
                        cursor,
                        &layout.bounds(),
                        renderer,
                    )
                });
        // Anything but `None` keeps the cursor from the page beneath: the
        // barrier covers the whole window.
        if interaction == mouse::Interaction::None {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_right_aligns_under_its_trigger() {
        let window = Size::new(1440.0, 900.0);
        let anchor = Rectangle::new(Point::new(1100.0, 40.0), Size::new(120.0, 34.0));
        let at = position(anchor, Size::new(240.0, 200.0), window);
        assert_eq!(at, Point::new(1220.0 - 240.0, 40.0 + 34.0 + GAP));
    }

    #[test]
    fn the_menu_stays_inside_the_window_margin() {
        let window = Size::new(400.0, 300.0);
        // Past the right edge: pulled left.
        let anchor = Rectangle::new(Point::new(380.0, 10.0), Size::new(30.0, 20.0));
        let at = position(anchor, Size::new(240.0, 100.0), window);
        assert_eq!(at.x, 400.0 - 240.0 - MARGIN);
        // Past the bottom: pulled up; past the left: held at the margin.
        let anchor = Rectangle::new(Point::new(0.0, 250.0), Size::new(30.0, 20.0));
        let at = position(anchor, Size::new(240.0, 100.0), window);
        assert_eq!(at, Point::new(MARGIN, 300.0 - 100.0 - MARGIN));
        // Taller than the window: held at the top margin.
        let at = position(anchor, Size::new(240.0, 400.0), window);
        assert_eq!(at.y, MARGIN);
    }
}
