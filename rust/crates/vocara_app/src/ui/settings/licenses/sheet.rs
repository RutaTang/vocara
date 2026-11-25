//! The license page's lateral detail as Flutter laid it out and moved it:
//!
//! - [`Sheet`]: `_DetailView`'s `DraggableScrollableSheet` and the card on
//!   it. The sheet's size is a fraction of the page's height, starting at
//!   (and never under) `(screenHeight - kToolbarHeight) / screenHeight` of
//!   the *window's* height (`MediaQuery`), while the page it sits in is the
//!   window less the drag bar above it: so its top is `56 · h / (h + 28)`
//!   down the page, about 54 px. A trackpad's (or a Magic Mouse's)
//!   two-finger swipe that starts over the card is a pan-zoom gesture,
//!   which the detail's scroll view took as a drag, alone in its arena
//!   (`pan_zoom`): each move goes through
//!   `_DraggableScrollableSheetScrollPosition.applyUserOffset`, which,
//!   while the list is at its top, first grows the sheet to the page's
//!   full height (and back down to its least), and only then scrolls the
//!   list, within its range (clamping physics). When the fingers lift, the
//!   drag's velocity (`MacOSScrollViewFlingVelocityTracker`, a fling past
//!   18 px and 50 px/s) flings the sheet on a `ClampingScrollSimulation`
//!   in its pixels and, once it is full (or least) with velocity left, the
//!   list on its own (`goBallistic`, `scroll_fling`); a new swipe on it, a
//!   wheel that scrolls the list, or a finger touching the trackpad over
//!   it while macOS's momentum runs (an inertia cancel, `pan_zoom`: the
//!   list's `pointerScroll(0)`, whose `beginActivity` stopped the sheet's
//!   own simulation too) stops that where the last frame left it. The
//!   momentum macOS sends once the fingers lift moves nothing. A scroll
//!   signal (a mouse wheel's,
//!   Flutter's `pointerScroll`) only ever scrolls the list. The pinned
//!   `SliverAppBar` over the list takes the scheme's surface tint at
//!   elevation 3 (8 % of `primary`) once the list is scrolled under it.
//! - [`KeyedStack`]: the `AnimatedSwitcher`'s stack, whose children keep
//!   their state by key: a detail going away keeps its scroll and its
//!   sheet's size while it fades out, as the switcher's outgoing child did.
//! - [`ScrollWatch`]: the app bar's `ScrollNotificationObserver`: reports
//!   whether the list under it is scrolled (`scrolledUnder`).

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::operation::Scrollable as ScrollableState;
use iced::advanced::widget::{tree, Id, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::border::Radius;
use iced::time::Instant;
use iced::widget::scrollable::AbsoluteOffset;
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Point, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

use crate::ui::core::theme::Zinc;
use crate::ui::core::widgets::pan_zoom::{self, Held, Scroll};
use crate::ui::core::widgets::scroll_fling::{
    self, Ballistic, ClampingScrollSimulation, MacOsVelocityTracker, Tolerance,
};
use crate::ui::core::widgets::window_drag_bar;

/// `kToolbarHeight`: the sheet's least size leaves this much of the window
/// above it, and the pinned header is this tall.
const TOOLBAR_HEIGHT: f32 = 56.0;

/// `_kCardElevation`: the card's margin either side of the sheet.
const CARD_ELEVATION: f32 = 4.0;

/// The card's top corners (`BorderRadius.vertical(top: Radius.circular(3))`).
const CARD_RADIUS: f32 = 3.0;

/// The surface tint's opacity at Material 3's scrolled-under elevation, 3
/// (`ElevationOverlay`'s table).
pub const SCROLLED_UNDER_TINT: f32 = 0.08;

/// The sheet's least (and first) size on a page `page_height` tall under
/// the window's drag bar: `(screenHeight - kToolbarHeight) / screenHeight`.
pub fn least_size(page_height: f32) -> f32 {
    let screen = page_height + window_drag_bar::HEIGHT;
    if screen <= 0.0 {
        return 1.0;
    }
    ((screen - TOOLBAR_HEIGHT) / screen).clamp(0.0, 1.0)
}

/// `_DraggableScrollableSheetScrollPosition.applyUserOffset`: where a drag
/// of `delta` (positive toward the list's top) takes a sheet `size` big
/// (`least`..1) over a page `page_height` tall whose list is scrolled
/// `pixels` down; `None` when the list takes it instead.
pub fn sheet_takes(
    size: f32,
    least: f32,
    pixels: f32,
    delta: f32,
    page_height: f32,
) -> Option<f32> {
    let list_should_scroll = pixels > 0.0;
    let at_min = least >= size;
    let at_max = 1.0 <= size;
    let sheet = !list_should_scroll
        && (!(at_min || at_max) || (at_min && delta < 0.0) || (at_max && delta > 0.0));
    if !sheet {
        return None;
    }
    // `addPixelDelta(-delta)`: `availablePixels` is the page's height
    // (`maxChildSize` 1).
    if page_height <= 0.0 {
        return Some(size);
    }
    Some((size - delta / page_height).clamp(least, 1.0))
}

/// Reads the offset of the first scrollable in a widget tree.
#[derive(Default)]
struct Offset(Option<Vector>);

impl Operation for Offset {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        if self.0.is_none() {
            operate(self);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn ScrollableState,
    ) {
        self.0.get_or_insert(translation);
    }
}

/// How far the first scrollable in `content` is scrolled down (0 with none).
fn scrolled<M>(
    content: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
) -> f32 {
    let mut offset = Offset::default();
    content
        .as_widget_mut()
        .operate(tree, layout, renderer, &mut offset);
    offset.0.map_or(0.0, |o| o.y)
}

/// Reads where the first scrollable in a widget tree is scrolled down to
/// and how far down it can go.
#[derive(Default)]
struct Metrics(Option<(f32, f32)>);

impl Operation for Metrics {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        if self.0.is_none() {
            operate(self);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn ScrollableState,
    ) {
        self.0.get_or_insert((
            translation.y,
            (content_bounds.height - bounds.height).max(0.0),
        ));
    }
}

/// Scrolls the first scrollable in a widget tree to an offset down.
struct ScrollListTo(Option<f32>);

impl Operation for ScrollListTo {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        if self.0.is_some() {
            operate(self);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        _translation: Vector,
        state: &mut dyn ScrollableState,
    ) {
        if let Some(y) = self.0.take() {
            state.scroll_to(AbsoluteOffset {
                x: None,
                y: Some(y),
            });
        }
    }
}

/// The list in the card: its offset down and its range, read and set.
struct List<'a, 'b, M> {
    card: &'a mut Element<'b, M>,
    tree: &'a mut Tree,
    layout: Layout<'a>,
    renderer: &'a Renderer,
}

impl<M> List<'_, '_, M> {
    /// Where it is scrolled down to, and how far down it can go
    /// (`pixels`, `maxScrollExtent`).
    fn metrics(&mut self) -> (f32, f32) {
        let mut metrics = Metrics::default();
        self.card
            .as_widget_mut()
            .operate(self.tree, self.layout, self.renderer, &mut metrics);
        metrics.0.unwrap_or((0.0, 0.0))
    }

    /// Scrolls it to `y` down.
    fn scroll_to(&mut self, y: f32) {
        self.card.as_widget_mut().operate(
            self.tree,
            self.layout,
            self.renderer,
            &mut ScrollListTo(Some(y)),
        );
    }
}

/// `double.sign`: 0 for 0.
fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

// -------------------------------------------------------------------- Sheet

/// See the module docs: `card` on the sheet, at `opacity` (the header's
/// tint fades with the rest of the detail).
pub struct Sheet<'a, M> {
    card: Element<'a, M>,
    opacity: f32,
}

/// The sheet's state.
#[derive(Debug, Default, Clone)]
struct SheetState {
    /// Its size once a drag has changed it (`hasChanged`); until then it
    /// follows the least, as `initialChildSize` does.
    size: Option<f32>,
    /// The list is scrolled under the header.
    scrolled: bool,
    /// The trackpad gesture the scroll view took last.
    held: Held,
    /// That gesture's drag, while the fingers are down.
    drag: Option<Drag>,
    /// A fling under way.
    run: Option<Run>,
}

/// A trackpad's drag of the scroll view: its gesture, its pan down so far
/// and the pan's velocity tracker.
#[derive(Debug, Clone)]
struct Drag {
    gesture: u64,
    pan: f32,
    tracker: MacOsVelocityTracker,
}

/// A fling (`goBallistic`).
#[derive(Debug, Clone, Copy)]
enum Run {
    /// The sheet's own, in its pixels (`_DraggableScrollableSheet
    /// ScrollPosition.goBallistic`'s controller): `velocity` is the
    /// fling's, `last` the simulation's place at the last frame.
    Sheet {
        simulation: ClampingScrollSimulation,
        velocity: f64,
        start: Option<Instant>,
        last: f64,
    },
    /// The list's (`ScrollPositionWithSingleContext.goBallistic`).
    List(Ballistic),
}

/// See [`Sheet`].
pub fn sheet<'a, M>(card: impl Into<Element<'a, M>>, opacity: f32) -> Sheet<'a, M> {
    Sheet {
        card: card.into(),
        opacity,
    }
}

impl<M> Widget<M, Theme, Renderer> for Sheet<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SheetState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(SheetState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.card)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.card));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let area = limits.max();
        let least = least_size(area.height);
        let state = tree.state.downcast_mut::<SheetState>();
        // A new least (the window resized) clamps a changed size; an
        // unchanged one follows it (`_DraggableSheetExtent.copyWith`).
        state.size = state.size.map(|s| s.clamp(least, 1.0));
        let size = state.size.unwrap_or(least);
        let height = size * area.height;
        let width = (area.width - 2.0 * CARD_ELEVATION).max(0.0);
        let card_size = Size::new(width, height);
        let card = self.card.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(card_size, card_size),
        );
        // `FractionallySizedBox(alignment: Alignment.bottomCenter)` in the
        // card's margin.
        layout::Node::with_children(
            area,
            vec![card.move_to(Point::new(CARD_ELEVATION, area.height - height))],
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
        let card = layout.children().next().expect("the card's layout");
        let page = layout.bounds().height;
        let least = least_size(page);
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<SheetState>();
        let child = &mut children[0];
        let mut list = List {
            card: &mut self.card,
            tree: child,
            layout: card,
            renderer,
        };
        let mut pass = true;
        // A scroll signal the list takes stops a fling (`pointerScroll`'s
        // `goIdle`).
        let mut signal = None;
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                state.frame(&mut list, *now, page, least, shell);
            }
            Event::Mouse(mouse::Event::WheelScrolled { .. }) => {
                let over = cursor.is_over(card.bounds());
                let scroll = pan_zoom::current();
                match scroll {
                    Scroll::Signal => {
                        if over {
                            signal = Some(list.metrics().0);
                        }
                    }
                    // The scroll view's drag recognizer, alone in its
                    // arena: it holds the position (a fling stops) and
                    // drags from the start.
                    Scroll::Start { gesture, time } if over => {
                        state.held.take(gesture);
                        state.run = None;
                        let mut tracker = MacOsVelocityTracker::default();
                        tracker.add(time, 0.0, pan_zoom::event_time());
                        state.drag = Some(Drag {
                            gesture,
                            pan: 0.0,
                            tracker,
                        });
                        pass = false;
                    }
                    Scroll::Update {
                        gesture,
                        delta,
                        time,
                    } if state.held.holds(gesture) => {
                        if let Some(drag) = state.drag.as_mut().filter(|d| d.gesture == gesture) {
                            drag.pan += delta.y;
                            drag.tracker.add(time, drag.pan, pan_zoom::event_time());
                            state.apply_user_offset(&mut list, delta.y, page, least, shell);
                        }
                        pass = false;
                    }
                    Scroll::End { gesture } if state.held.holds(gesture) => {
                        if let Some(drag) = state.drag.take_if(|d| d.gesture == gesture) {
                            let estimate = drag.tracker.estimate(pan_zoom::event_time());
                            let velocity = scroll_fling::drag_end(estimate);
                            state.go_ballistic(&mut list, velocity, page, least, shell);
                        }
                        pass = false;
                    }
                    // Its momentum reached nothing.
                    Scroll::Nothing { .. } if state.held.owns(&scroll) => pass = false,
                    // A finger on the trackpad during macOS's momentum: the
                    // list's `pointerScroll(0)` (or its shown bar's
                    // `jumpTo`) goes idle, whose `beginActivity` stops the
                    // sheet's ballistic controllers too, so the sheet and
                    // the list stay where the last frame left them. The
                    // list's view, which it goes on to, stops its own.
                    Scroll::InertiaCancel if over => state.run = None,
                    _ => {}
                }
                if !pass {
                    shell.capture_event();
                }
            }
            _ => {}
        }
        if pass {
            self.card.as_widget_mut().update(
                child, event, card, cursor, renderer, clipboard, shell, viewport,
            );
        }
        let mut list = List {
            card: &mut self.card,
            tree: child,
            layout: card,
            renderer,
        };
        let pixels = list.metrics().0;
        if signal.is_some_and(|before| before != pixels) {
            state.run = None;
        }
        let under = pixels > 0.0;
        if under != state.scrolled {
            state.scrolled = under;
            shell.request_redraw();
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
        let card = layout.children().next().expect("the card's layout");
        self.card.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            card,
            cursor,
            viewport,
        );
        if tree.state.downcast_ref::<SheetState>().scrolled {
            // The header's surface tint. Drawn over the header's text, which
            // is in the tint's own color (`primary`, the foreground), it
            // shows as it did under it.
            let bounds = card.bounds();
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        height: TOOLBAR_HEIGHT.min(bounds.height),
                        ..bounds
                    },
                    border: Border {
                        radius: Radius {
                            top_left: CARD_RADIUS,
                            top_right: CARD_RADIUS,
                            bottom_right: 0.0,
                            bottom_left: 0.0,
                        },
                        ..Border::default()
                    },
                    shadow: Shadow::default(),
                    snap: false,
                },
                Background::Color(Color {
                    a: SCROLLED_UNDER_TINT * self.opacity,
                    ..Zinc::PRIMARY
                }),
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
        let card = layout.children().next().expect("the card's layout");
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.card
                .as_widget_mut()
                .operate(&mut tree.children[0], card, renderer, operation);
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
        let card = layout.children().next().expect("the card's layout");
        self.card
            .as_widget()
            .mouse_interaction(&tree.children[0], card, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let card = layout.children().next().expect("the card's layout");
        self.card.as_widget_mut().overlay(
            &mut tree.children[0],
            card,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<Sheet<'a, M>> for Element<'a, M> {
    fn from(sheet: Sheet<'a, M>) -> Self {
        Element::new(sheet)
    }
}

impl SheetState {
    /// `applyUserOffset(delta)` of a drag `delta` down: the sheet's, or
    /// the list's within its range.
    fn apply_user_offset<M>(
        &mut self,
        list: &mut List<'_, '_, M>,
        delta: f32,
        page: f32,
        least: f32,
        shell: &mut Shell<'_, M>,
    ) {
        let (pixels, max) = list.metrics();
        let size = self.size.unwrap_or(least);
        if let Some(to) = sheet_takes(size, least, pixels, delta, page) {
            self.size = Some(to);
            shell.invalidate_layout();
        } else {
            // `super.applyUserOffset`, clamped at the list's ends.
            let to = (pixels - delta).clamp(0.0, max);
            if to != pixels {
                list.scroll_to(to);
            }
        }
        shell.request_redraw();
    }

    /// `goBallistic(velocity)`: the scroll's velocity, positive toward the
    /// list's end (the sheet growing).
    fn go_ballistic<M>(
        &mut self,
        list: &mut List<'_, '_, M>,
        velocity: f64,
        page: f32,
        least: f32,
        shell: &mut Shell<'_, M>,
    ) {
        let (pixels, max) = list.metrics();
        let size = self.size.unwrap_or(least);
        let at_max = 1.0 <= size;
        self.run =
            if velocity == 0.0 || (velocity < 0.0 && pixels > 0.0) || (velocity > 0.0 && at_max) {
                Ballistic::new(f64::from(pixels), f64::from(max), velocity).map(Run::List)
            } else {
                // The sheet's own fling, in its pixels (`currentPixels`).
                let position = f64::from(size * page);
                Some(Run::Sheet {
                    simulation: ClampingScrollSimulation::new(position, velocity),
                    velocity,
                    start: None,
                    last: position,
                })
            };
        if self.run.is_some() {
            shell.request_redraw();
        }
    }

    /// A frame at `now`: the fling's step.
    fn frame<M>(
        &mut self,
        list: &mut List<'_, '_, M>,
        now: Instant,
        page: f32,
        least: f32,
        shell: &mut Shell<'_, M>,
    ) {
        match &mut self.run {
            Some(Run::Sheet {
                simulation,
                velocity,
                start,
                last,
            }) => {
                let t = now
                    .saturating_duration_since(*start.get_or_insert(now))
                    .as_secs_f64();
                let value = simulation.x(t);
                let delta = value - *last;
                *last = value;
                // `extent.addPixelDelta(delta)`.
                let size = self.size.unwrap_or(least);
                if page > 0.0 {
                    let to = (size + (delta / f64::from(page)) as f32).clamp(least, 1.0);
                    self.size = Some(to);
                    if to != size {
                        shell.invalidate_layout();
                    }
                }
                let size = self.size.unwrap_or(least);
                let (velocity, simulation) = (*velocity, *simulation);
                if (velocity > 0.0 && 1.0 <= size) || (velocity < 0.0 && least >= size) {
                    // The list takes the velocity left, and enough more to
                    // go on (`toleranceFor(this).velocity`).
                    let dx = simulation.dx(t);
                    let velocity = dx + Tolerance::of_window().velocity * sign(dx);
                    let (pixels, max) = list.metrics();
                    self.run =
                        Ballistic::new(f64::from(pixels), f64::from(max), velocity).map(Run::List);
                } else if simulation.is_done(t) {
                    // `super.goBallistic(0)`: the list is still.
                    self.run = None;
                }
            }
            Some(Run::List(ballistic)) => {
                let (pixels, max) = list.metrics();
                let (to, more) = ballistic.frame(now, f64::from(max));
                let to = to as f32;
                if to != pixels {
                    list.scroll_to(to);
                }
                if !more {
                    self.run = None;
                }
            }
            None => {}
        }
        if self.run.is_some() {
            shell.request_redraw();
        }
    }
}

// --------------------------------------------------------------- KeyedStack

/// Children stacked over one another, each filling it, the last on top,
/// keeping their state by key whatever their place (an `AnimatedSwitcher`'s
/// `Stack` of keyed transitions).
pub struct KeyedStack<'a, M> {
    keys: Vec<u64>,
    children: Vec<Element<'a, M>>,
}

/// An empty [`KeyedStack`].
pub fn keyed_stack<'a, M>() -> KeyedStack<'a, M> {
    KeyedStack {
        keys: Vec::new(),
        children: Vec::new(),
    }
}

impl<'a, M> KeyedStack<'a, M> {
    /// Adds `child`, keyed `key`, on top.
    pub fn push(mut self, key: u64, child: impl Into<Element<'a, M>>) -> Self {
        self.keys.push(key);
        self.children.push(child.into());
        self
    }
}

impl<M> Widget<M, Theme, Renderer> for KeyedStack<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Vec<u64>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(self.keys.clone())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let keys = tree.state.downcast_mut::<Vec<u64>>();
        let mut old: Vec<Option<Tree>> = std::mem::take(&mut tree.children)
            .into_iter()
            .map(Some)
            .collect();
        tree.children = self
            .keys
            .iter()
            .zip(&self.children)
            .map(|(key, child)| {
                let kept = keys
                    .iter()
                    .position(|k| k == key)
                    .and_then(|i| old.get_mut(i).and_then(Option::take));
                match kept {
                    Some(mut kept) => {
                        kept.diff(child.as_widget());
                        kept
                    }
                    None => Tree::new(child.as_widget()),
                }
            })
            .collect();
        keys.clone_from(&self.keys);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let children = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .map(|(child, tree)| {
                child
                    .as_widget_mut()
                    .layout(tree, renderer, &layout::Limits::new(Size::ZERO, size))
            })
            .collect();
        layout::Node::with_children(size, children)
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
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .rev()
            .zip(tree.children.iter_mut().rev())
            .zip(layout.children().rev())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
            if shell.is_event_captured() {
                return;
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
        // Each a layer of its own, over the last: a card's text must not
        // show through the card over it.
        for (i, ((child, tree), layout)) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .enumerate()
        {
            if i > 0 {
                renderer.with_layer(*viewport, |renderer| {
                    child
                        .as_widget()
                        .draw(tree, renderer, theme, style, layout, cursor, viewport);
                });
            } else {
                child
                    .as_widget()
                    .draw(tree, renderer, theme, style, layout, cursor, viewport);
            }
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
            for ((child, tree), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(tree, layout, renderer, operation);
            }
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
        self.children
            .iter()
            .rev()
            .zip(tree.children.iter().rev())
            .zip(layout.children().rev())
            .map(|((child, tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .find(|&interaction| interaction != mouse::Interaction::None)
            .unwrap_or_default()
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

impl<'a, M: 'a> From<KeyedStack<'a, M>> for Element<'a, M> {
    fn from(stack: KeyedStack<'a, M>) -> Self {
        Element::new(stack)
    }
}

// -------------------------------------------------------------- ScrollWatch

/// Reports, as `on_change(scrolled)`, each time the first scrollable in
/// `content` comes off its top or goes back to it (see the module docs).
pub struct ScrollWatch<'a, M> {
    content: Element<'a, M>,
    on_change: Box<dyn Fn(bool) -> M + 'a>,
}

/// See [`ScrollWatch`].
pub fn scroll_watch<'a, M>(
    content: impl Into<Element<'a, M>>,
    on_change: impl Fn(bool) -> M + 'a,
) -> ScrollWatch<'a, M> {
    ScrollWatch {
        content: content.into(),
        on_change: Box::new(on_change),
    }
}

impl<M> Widget<M, Theme, Renderer> for ScrollWatch<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<bool>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(false)
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
        let under = scrolled(&mut self.content, &mut tree.children[0], layout, renderer) > 0.0;
        let was = tree.state.downcast_mut::<bool>();
        if under != *was {
            *was = under;
            shell.publish((self.on_change)(under));
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

impl<'a, M: 'a> From<ScrollWatch<'a, M>> for Element<'a, M> {
    fn from(watch: ScrollWatch<'a, M>) -> Self {
        Element::new(watch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_starts_a_toolbar_of_the_window_down() {
        // A 900 px window: the page under the drag bar is 872 tall.
        let least = least_size(872.0);
        assert!((least - 844.0 / 900.0).abs() < 1e-6);
        assert!(((1.0 - least) * 872.0 - 56.0 * 872.0 / 900.0).abs() < 1e-3);
        assert_eq!(least_size(-28.0), 1.0);
    }

    // `_DraggableScrollableSheetScrollPosition.applyUserOffset`.
    #[test]
    fn a_drag_grows_the_sheet_before_the_list_scrolls_and_shrinks_it_after() {
        let (least, page) = (0.9, 1000.0);
        // At its least, the list at its top: toward the list's end grows
        // the sheet; toward its top, the list takes it (nothing to do).
        assert_eq!(sheet_takes(least, least, 0.0, -50.0, page), Some(0.95));
        assert_eq!(sheet_takes(least, least, 0.0, 50.0, page), None);
        // Between: either way moves the sheet, clamped.
        assert_eq!(sheet_takes(0.95, least, 0.0, 30.0, page), Some(0.92));
        assert_eq!(sheet_takes(0.95, least, 0.0, -80.0, page), Some(1.0));
        assert_eq!(sheet_takes(0.95, least, 0.0, 80.0, page), Some(least));
        // Full: toward the end, the list scrolls; toward the top, the
        // sheet shrinks.
        assert_eq!(sheet_takes(1.0, least, 0.0, -10.0, page), None);
        assert_eq!(sheet_takes(1.0, least, 0.0, 10.0, page), Some(0.99));
        // The list scrolled: it takes any drag.
        assert_eq!(sheet_takes(1.0, least, 5.0, 10.0, page), None);
        assert_eq!(sheet_takes(0.95, least, 5.0, -10.0, page), None);
    }
}
