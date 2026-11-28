//! The plumbing the mixer UI's two widgets (`fx_panel`, `mixer_strip`)
//! share, where Flutter gave it to them for free:
//!
//! - [`PopupMenu`]: the instant studio menu (`showStudioMenu` with
//!   `menuAnchorBelow`, and the click-anchored context menu), as
//!   `track_context_menu.dart`'s `_InstantMenuRoute` / `_MenuLayoutDelegate`
//!   / `_MenuRow` draw it. The open menu is the trigger's own state (Dart
//!   pushed a popup route), so the host keeps nothing for it; a picked item
//!   reports its messages, in order, as the item's `onTap` called the view
//!   model.
//! - [`tap`]: a `GestureDetector(onTap: ...)` that may report more than one
//!   message (an FX toggle is a live edit AND its commit).
//! - [`Keyed`]: a row whose children keep their state by key
//!   (Flutter's `ValueKey`s on the strips and insert cards). iced's own keyed
//!   column only follows a single contiguous insert or removal; a reorder, or
//!   a lane removed before the one being dragged, handed one child's state
//!   to another.

#[cfg(test)]
pub mod testing;

use std::cell::Cell;
use std::rc::Rc;

use crate::ui::core::text;
use iced::advanced::layout::{self, flex, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, Column, Space};
use iced::{
    keyboard, mouse, Alignment, Background, Border, Element, Event, Length, Padding, Point,
    Rectangle, Renderer, Shadow, Size, Theme, Vector,
};

use super::track_context_menu::{MenuRow as StudioRow, SELECTED_CHECK};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, height, radius, type_scale, Studio};
use crate::ui::core::widgets::animated::{animated_container, Decoration};
use crate::ui::core::widgets::fading_scrollbar::{no_scrollbar, scrollable};
use crate::ui::core::widgets::gesture::{any_tap_event, AnyTap};
use crate::ui::core::widgets::line_icon::{glyphs, line_icon_stroked};
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::wrap::Wrap;

// ------------------------------------------------------------- pop-up face

/// A pop-up button's face (`DESIGN.md`, Pop-up button: the look of
/// `controls::popup_button`) for a [`PopupMenu`]'s trigger, which takes the
/// press itself (a button would capture it): 22 px, radius 5,
/// `SURFACE_RAISED` (`CONTROL_ACTIVE` under the pointer) with an inset
/// `BORDER_STRONG` outline, the label in 11 px (`TEXT_SECONDARY` when it is
/// a placeholder, else `TEXT_PRIMARY`) and an up-down chevron in
/// `TEXT_FAINT` at the right. `width`: `Shrink` hugs the label.
pub fn popup_face<'a, M: 'a>(
    label: impl Into<std::borrow::Cow<'a, str>>,
    placeholder: bool,
    width: Length,
) -> Element<'a, M> {
    let label = studio_text(label.into())
        .size(type_scale::LABEL)
        .line_height(iced::Pixels(type_scale::LINE))
        .color(if placeholder {
            Studio::TEXT_SECONDARY
        } else {
            Studio::TEXT_PRIMARY
        })
        .ellipsis(1);
    let content = row![
        container(label).width(if width == Length::Shrink {
            Length::Shrink
        } else {
            Length::Fill
        }),
        line_icon_stroked(glyphs::UP_DOWN, 10.0, Studio::TEXT_FAINT, 1.0),
    ]
    .spacing(8)
    .align_y(Vertical::Center);
    animated_container(content, std::time::Duration::ZERO)
        .width(width)
        .height(height::CONTROL)
        .padding(Padding {
            top: 0.0,
            right: 6.0,
            bottom: 0.0,
            left: 8.0,
        })
        .align((-1.0, 0.0))
        .style(|hovered| {
            Decoration::boxed(
                if hovered {
                    Studio::CONTROL_ACTIVE
                } else {
                    Studio::SURFACE_RAISED
                },
                radius::CONTROL,
                Studio::BORDER_STRONG,
                1.0,
            )
        })
        .into()
}

// ------------------------------------------------------------------- tap

/// `kTouchSlop`: how far a press may travel and still be a tap
/// (`TapGestureRecognizer`'s pre-accept slop).
pub const TAP_SLOP: f32 = 18.0;

/// `GestureDetector(onTap: ...)` around `content`: a primary press the
/// content did not take, released within [`TAP_SLOP`] of where it went down,
/// reports `on_tap` in order. `cursor` is the `MouseRegion`'s, if any.
pub fn tap<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    on_tap: Vec<M>,
    cursor: Option<mouse::Interaction>,
) -> Element<'a, M> {
    let wrap = Wrap::<M, Option<Point>>::new(
        content,
        Box::new(move |down, event, bounds, at, shell| match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if shell.is_event_captured() {
                    return;
                }
                if let Some(p) = at.position_over(bounds) {
                    *down = Some(p);
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some(from), Some(p)) = (*down, at.position()) {
                    if from.distance(p) > TAP_SLOP {
                        *down = None;
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let tapped = down.take().is_some() && at.is_over(bounds);
                if tapped {
                    for m in &on_tap {
                        shell.publish(m.clone());
                    }
                    shell.capture_event();
                }
            }
            _ => {}
        }),
    );
    match cursor {
        Some(interaction) => wrap
            .interaction(Box::new(move |_, bounds, at| {
                at.is_over(bounds).then_some(interaction)
            }))
            .into(),
        None => wrap.into(),
    }
}

// ------------------------------------------------------------------ keyed

/// Reconciles `trees` (the children last shown under `old_keys`) with
/// `children` under `keys`: each child takes the state of the old child with
/// its key, wherever that one was, and a new key starts afresh.
pub fn diff_keyed<'a, K: PartialEq, M>(
    old_keys: &[K],
    trees: &mut Vec<Tree>,
    keys: &[K],
    children: &[Element<'a, M>],
) {
    if old_keys.len() == trees.len() && old_keys == keys {
        // Nothing moved: diff in place.
        for (tree, child) in trees.iter_mut().zip(children) {
            tree.diff(child);
        }
        return;
    }
    let mut old: Vec<Option<Tree>> = std::mem::take(trees).into_iter().map(Some).collect();
    for (key, child) in keys.iter().zip(children) {
        let kept = old_keys
            .iter()
            .position(|k| k == key)
            .and_then(|i| old.get_mut(i).and_then(Option::take));
        trees.push(match kept {
            Some(mut tree) => {
                tree.diff(child);
                tree
            }
            None => Tree::new(child),
        });
    }
}

/// A row of children that keep their widget state by key: see the module
/// docs.
pub struct Keyed<'a, K: Copy + PartialEq + 'static, M> {
    keys: Vec<K>,
    children: Vec<Element<'a, M>>,
    vertical: bool,
    width: Length,
    height: Length,
    padding: Padding,
    align: Alignment,
}

/// A keyed row.
pub fn keyed_row<'a, K: Copy + PartialEq + 'static, M>() -> Keyed<'a, K, M> {
    Keyed {
        keys: Vec::new(),
        children: Vec::new(),
        vertical: false,
        width: Length::Shrink,
        height: Length::Shrink,
        padding: Padding::ZERO,
        align: Alignment::Start,
    }
}

/// A keyed column (the song page's lane list, whose rows Dart keyed by
/// take, bus and section).
pub fn keyed_column<'a, K: Copy + PartialEq + 'static, M>() -> Keyed<'a, K, M> {
    Keyed {
        vertical: true,
        ..keyed_row()
    }
}

impl<'a, K: Copy + PartialEq + 'static, M> Keyed<'a, K, M> {
    pub fn push(mut self, key: K, child: impl Into<Element<'a, M>>) -> Self {
        let child = child.into();
        let hint = child.as_widget().size_hint();
        self.width = self.width.enclose(hint.width);
        self.height = self.height.enclose(hint.height);
        self.keys.push(key);
        self.children.push(child);
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

struct KeyedState<K> {
    keys: Vec<K>,
}

impl<K: Copy + PartialEq + 'static, M> Widget<M, Theme, Renderer> for Keyed<'_, K, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<KeyedState<K>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(KeyedState {
            keys: self.keys.clone(),
        })
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<KeyedState<K>>();
        diff_keyed(&state.keys, children, &self.keys, &self.children);
        state.keys.clone_from(&self.keys);
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let axis = if self.vertical {
            flex::Axis::Vertical
        } else {
            flex::Axis::Horizontal
        };
        flex::resolve(
            axis,
            renderer,
            limits,
            self.width,
            self.height,
            self.padding,
            0.0,
            self.align,
            &mut self.children,
            &mut tree.children,
        )
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
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
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
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
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
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, K: Copy + PartialEq + 'static, M: 'a> From<Keyed<'a, K, M>> for Element<'a, M> {
    fn from(keyed: Keyed<'a, K, M>) -> Self {
        Element::new(keyed)
    }
}

// ------------------------------------------------------------------- menu

/// How close the menu may come to the window's edges
/// (`_MenuLayoutDelegate`'s 8 px).
pub const MENU_MARGIN: f32 = 8.0;

/// The menu's tallest; a longer list scrolls inside it.
pub const MENU_MAX_HEIGHT: f32 = 440.0;

/// `menuAnchorBelow`'s default `dy`: the gap under the trigger.
pub const ANCHOR_GAP: f32 = 4.0;

/// One entry of a studio menu (`StudioMenuItem`).
#[derive(Debug, Clone)]
pub struct MenuItem<M> {
    pub label: String,
    /// What a pick reports, in order (`onTap`).
    pub on_tap: Vec<M>,
    /// A one-line description shown faintly under the label.
    pub subtitle: Option<String>,
    pub selected: bool,
    pub danger: bool,
    /// A divider above this item.
    pub divider: bool,
}

impl<M> MenuItem<M> {
    pub fn new(label: impl Into<String>, on_tap: Vec<M>) -> Self {
        Self {
            label: label.into(),
            on_tap,
            subtitle: None,
            selected: false,
            danger: false,
            divider: false,
        }
    }

    pub fn subtitle(mut self, subtitle: Option<String>) -> Self {
        self.subtitle = subtitle;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }
}

/// How a [`PopupMenu`] opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opens {
    /// A primary tap on the trigger opens it just below the trigger
    /// (`showStudioMenu(context, menuAnchorBelow(context), ...)`).
    BelowOnTap,
    /// A secondary press on the trigger opens it at the press
    /// (`onSecondaryTapDown: (d) => showStudioMenu(context,
    /// d.globalPosition, ...)`).
    AtSecondaryPress,
}

/// Where `_MenuLayoutDelegate` puts a menu of `menu` size asked for at `at`
/// in a window of `window` size: at the point, pulled back inside the 8 px
/// margin at the right and the bottom, and never above or left of it.
pub fn menu_position(at: Point, menu: Size, window: Size) -> Point {
    let mut x = at.x;
    let mut y = at.y;
    if x + menu.width > window.width - MENU_MARGIN {
        x = window.width - menu.width - MENU_MARGIN;
    }
    if y + menu.height > window.height - MENU_MARGIN {
        y = window.height - menu.height - MENU_MARGIN;
    }
    Point::new(
        x.max(MENU_MARGIN).min(window.width.max(MENU_MARGIN)),
        y.max(MENU_MARGIN).min(window.height.max(MENU_MARGIN)),
    )
}

/// A trigger that opens a studio menu of items: see the module docs.
pub struct PopupMenu<'a, M> {
    trigger: Element<'a, M>,
    menu: Element<'a, M>,
    picked: Rc<Cell<bool>>,
    opens: Opens,
    /// What it reports as the menu opens.
    on_open: Vec<M>,
}

impl<'a, M: Clone + 'a> PopupMenu<'a, M> {
    /// `trigger`, opening `items` in a menu `width` wide as `opens` says.
    pub fn new(
        trigger: impl Into<Element<'a, M>>,
        items: Vec<MenuItem<M>>,
        width: f32,
        opens: Opens,
    ) -> Self {
        let picked = Rc::new(Cell::new(false));
        Self {
            trigger: trigger.into(),
            menu: menu_card(items, width, &picked),
            picked,
            opens,
            on_open: Vec::new(),
        }
    }

    /// Reports `messages` as the menu opens, before any pick (a strip
    /// selects its channel as its right-click menu opens).
    pub fn on_open(mut self, messages: Vec<M>) -> Self {
        self.on_open = messages;
        self
    }
}

/// The menu card (`_showStudioMenu`'s `Material` + `Container`): rows in a
/// scroll view capped at [`MENU_MAX_HEIGHT`], on `Studio.surfaceRaised`,
/// radius 10, a `Studio.border` outline.
fn menu_card<'a, M: Clone + 'a>(
    items: Vec<MenuItem<M>>,
    width: f32,
    picked: &Rc<Cell<bool>>,
) -> Element<'a, M> {
    let mut rows = Column::new().width(Length::Fill);
    for item in items {
        if item.divider {
            rows = rows.push(menu_divider());
        }
        rows = rows.push(MenuRow::new(item, Rc::clone(picked)));
    }
    container(no_scrollbar(scrollable(rows)))
        .width(width)
        .max_height(MENU_MAX_HEIGHT)
        // Inside the 1 px border (the decoration's padding).
        .padding(1)
        .style(theme::menu)
        .clip(true)
        .into()
}

/// `_MenuDivider`: `Divider(height: 9, thickness: 1, color: Studio.border)`.
fn menu_divider<'a, M: 'a>() -> Element<'a, M> {
    container(
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(theme::fill(Studio::BORDER)),
    )
    .height(9)
    .align_y(Vertical::Center)
    .into()
}

#[derive(Debug, Default)]
struct PopupState {
    /// Where the open menu is anchored, from the trigger's top-left.
    open: Option<Vector>,
    /// A primary press on the trigger that may become the opening tap.
    down: Option<Point>,
    /// A tap on the open menu's barrier in flight.
    barrier: AnyTap,
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for PopupMenu<'a, M> {
    fn size(&self) -> Size<Length> {
        self.trigger.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.trigger.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<PopupState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(PopupState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.trigger), Tree::new(&self.menu)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children.resize_with(2, Tree::empty);
        tree.children[0].diff(&self.trigger);
        tree.children[1].diff(&self.menu);
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
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<PopupState>();
        let was_open = state.open.is_some();
        let open = |state: &mut PopupState, at: Vector, shell: &mut Shell<'_, M>| {
            state.open = Some(at);
            state.barrier = AnyTap::default();
            shell.capture_event();
            shell.invalidate_layout();
            shell.request_redraw();
        };
        match (self.opens, event) {
            (Opens::BelowOnTap, Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))) => {
                if shell.is_event_captured() {
                    return;
                }
                if let Some(p) = cursor.position_over(bounds) {
                    state.down = Some(p);
                    shell.capture_event();
                }
            }
            (Opens::BelowOnTap, Event::Mouse(mouse::Event::CursorMoved { .. })) => {
                if let (Some(from), Some(p)) = (state.down, cursor.position()) {
                    if from.distance(p) > TAP_SLOP {
                        state.down = None;
                    }
                }
            }
            (
                Opens::BelowOnTap,
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ) => {
                if state.down.take().is_some() && cursor.is_over(bounds) {
                    open(state, Vector::new(0.0, bounds.height + ANCHOR_GAP), shell);
                }
            }
            (
                Opens::AtSecondaryPress,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            ) => {
                if shell.is_event_captured() {
                    return;
                }
                if let Some(p) = cursor.position_over(bounds) {
                    open(state, p - bounds.position(), shell);
                }
            }
            _ => {}
        }
        // The menu opens fresh, scrolled to its top: its tree outlived the
        // last opening, and a long list (the presets) reopened where it was
        // scrolled, so a click where a row had been picked another.
        if !was_open && state.open.is_some() {
            tree.children[1] = Tree::new(&self.menu);
            for m in &self.on_open {
                shell.publish(m.clone());
            }
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
        let inner = self.trigger.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        );
        // The dropdowns and route chips sit in `MouseRegion(cursor: click)`.
        if self.opens == Opens::BelowOnTap
            && inner == mouse::Interaction::None
            && cursor.is_over(layout.bounds())
        {
            return mouse::Interaction::Pointer;
        }
        inner
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
        let state = state.downcast_mut::<PopupState>();
        let [trigger_tree, menu_tree] = children.as_mut_slice() else {
            return None;
        };
        match state.open {
            Some(offset) => Some(overlay::Element::new(Box::new(MenuOverlay {
                menu: &mut self.menu,
                tree: menu_tree,
                state,
                at: layout.bounds().position() + offset + translation,
                picked: Rc::clone(&self.picked),
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

impl<'a, M: Clone + 'a> From<PopupMenu<'a, M>> for Element<'a, M> {
    fn from(menu: PopupMenu<'a, M>) -> Self {
        Element::new(menu)
    }
}

/// The open menu over the whole window: a barrier with no color
/// (`barrierDismissible`), so a tap outside the menu (any button, landing
/// on its release within the tap slop: `gesture::AnyTap`) or Escape closes
/// it, and nothing beneath sees the pointer or the keyboard meanwhile.
struct MenuOverlay<'a, 'b, M> {
    menu: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    state: &'b mut PopupState,
    at: Point,
    picked: Rc<Cell<bool>>,
}

impl<M> MenuOverlay<'_, '_, M> {
    fn close(&mut self, shell: &mut Shell<'_, M>) {
        self.state.open = None;
        shell.invalidate_layout();
        shell.request_redraw();
    }
}

impl<M> overlay::Overlay<M, Theme, Renderer> for MenuOverlay<'_, '_, M> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        // `BoxConstraints.loose(biggest).deflate(EdgeInsets.all(8))`.
        let limits = layout::Limits::new(
            Size::ZERO,
            Size::new(
                (bounds.width - 2.0 * MENU_MARGIN).max(0.0),
                (bounds.height - 2.0 * MENU_MARGIN).max(0.0),
            ),
        );
        let node = self
            .menu
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let at = menu_position(self.at, node.size(), bounds);
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
            self.close(shell);
            shell.capture_event();
            return;
        }
        let bounds = menu.bounds();
        let (tapped, taken) = any_tap_event(
            &mut self.state.barrier,
            event,
            |at| !bounds.contains(at),
            cursor.position(),
        );
        if tapped {
            self.close(shell);
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
        if self.picked.replace(false) {
            self.close(shell);
        }
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
        if interaction == mouse::Interaction::None {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }
}

/// `_MenuRow`: an `InkWell` row — `Studio.hover` under the pointer — with
/// the label, its optional subtitle, and a check when selected. A pick
/// reports the item's messages and closes the menu.
struct MenuRow<'a, M> {
    content: Element<'a, M>,
    on_tap: Vec<M>,
    picked: Rc<Cell<bool>>,
}

/// A row's label colour and weight: the inspector menus' (`DESIGN.md`,
/// Menus, `track_context_menu::MenuRow`), so every menu marks its current
/// item alike — the label in medium weight, an [`SELECTED_CHECK`] check at
/// its end. (Here the label went coral, there it stayed white.)
fn label_style<M>(item: &MenuItem<M>) -> (iced::Color, iced::Font) {
    let row = StudioRow::new("", ())
        .danger(item.danger)
        .selected(item.selected);
    (row.color(), row.font())
}

impl<'a, M: Clone + 'a> MenuRow<'a, M> {
    fn new(item: MenuItem<M>, picked: Rc<Cell<bool>>) -> Self {
        let (color, font) = label_style(&item);
        let mut labels = column![container(
            text(item.label)
                .size(13)
                .font(font)
                .color(color)
                .wrapping(Wrapping::None)
        )
        .width(Length::Fill)
        .clip(true)];
        if let Some(subtitle) = item.subtitle {
            labels = labels.push(Space::new().height(2)).push(
                text(subtitle)
                    .size(10.5)
                    .line_height(1.3)
                    .color(Studio::TEXT_FAINT),
            );
        }
        let mut content = row![labels.width(Length::Fill)].align_y(Vertical::Center);
        if item.selected {
            content = content.push(icons::icon(Icon::Check, 14.0, SELECTED_CHECK));
        }
        Self {
            content: container(content).padding([9, 14]).into(),
            on_tap: item.on_tap,
            picked,
        }
    }
}

impl<M: Clone> Widget<M, Theme, Renderer> for MenuRow<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RowState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(RowState::default())
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
        let limits = limits.width(Length::Fill);
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &limits);
        let size = limits.resolve(Length::Fill, Length::Shrink, node.size());
        layout::Node::with_children(size, vec![node])
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
        use iced::advanced::Renderer as _;
        if cursor.is_over(layout.bounds()) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border::default(),
                    shadow: Shadow::default(),
                    snap: false,
                },
                Background::Color(Studio::HOVER),
            );
        }
        if let Some(content) = layout.children().next() {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                content,
                cursor,
                viewport,
            );
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(content) = layout.children().next() {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                content,
                renderer,
                operation,
            );
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<RowState>();
        let bounds = layout.bounds();
        let hovered = cursor.is_over(bounds);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if hovered => {
                state.pressed = true;
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let tapped = std::mem::take(&mut state.pressed) && hovered;
                if tapped {
                    for m in &self.on_tap {
                        shell.publish(m.clone());
                    }
                    self.picked.set(true);
                    shell.capture_event();
                }
            }
            // The hover fill follows the pointer.
            Event::Mouse(mouse::Event::CursorMoved { .. }) if hovered != state.hovered => {
                state.hovered = hovered;
                shell.request_redraw();
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

#[derive(Debug, Default)]
struct RowState {
    pressed: bool,
    hovered: bool,
}

impl<'a, M: Clone + 'a> From<MenuRow<'a, M>> for Element<'a, M> {
    fn from(row: MenuRow<'a, M>) -> Self {
        Element::new(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::song_practice_detail::widgets::track_context_menu::argb;

    #[test]
    fn a_row_marks_the_current_item_as_the_inspector_menus_do() {
        // The current item: a white label in medium weight, and the
        // inspector menus' check (the label was coral here).
        let item = MenuItem::new("Master", vec![()]).selected(true);
        assert_eq!(
            label_style(&item),
            (Studio::TEXT_PRIMARY, crate::ui::core::theme::font::MEDIUM)
        );
        let plain = MenuItem::new("Bus 1", vec![()]);
        assert_eq!(
            label_style(&plain),
            (Studio::TEXT_PRIMARY, crate::ui::core::theme::font::REGULAR)
        );
        assert_eq!(SELECTED_CHECK, Studio::ACCENT_BRIGHT);
    }

    #[test]
    fn argb_reads_like_flutters_color() {
        let c = argb(0xFFE3_604E);
        assert_eq!(c.into_rgba8(), [0xE3, 0x60, 0x4E, 0xFF]);
        let half = argb(0x8000_00FF);
        assert_eq!(half.into_rgba8(), [0, 0, 0xFF, 0x80]);
    }

    #[test]
    fn the_menu_opens_at_the_point_and_stays_inside_the_window() {
        let window = Size::new(800.0, 600.0);
        let menu = Size::new(160.0, 100.0);
        assert_eq!(
            menu_position(Point::new(100.0, 50.0), menu, window),
            Point::new(100.0, 50.0)
        );
        // Past the right and bottom edges: pulled back inside the margin.
        assert_eq!(
            menu_position(Point::new(700.0, 550.0), menu, window),
            Point::new(800.0 - 160.0 - 8.0, 600.0 - 100.0 - 8.0)
        );
        // Never above or left of the margin.
        assert_eq!(
            menu_position(Point::new(2.0, 1.0), menu, window),
            Point::new(8.0, 8.0)
        );
    }
}
