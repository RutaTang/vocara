//! `_InsertChain`: the insert cards in signal-chain order, reordered by
//! dragging a card's handle (`ReorderableListView` with
//! `ReorderableDragStartListener` handles).
//!
//! Each card keeps its state by kind (Dart keyed the cards
//! `ValueKey(kind)`), so a reorder moves the EQ's analyzer, a slider's hold
//! or the GR meter with its card. The drag starts on the handle's press (a
//! mouse's immediate drag), the card follows the pointer above the others
//! while they part to show where it will land, and the drop reports the new
//! order once — nothing when it lands where it started.
//!
//! Dart kept the order locally so the drop showed at once while the model
//! caught up asynchronously; here the drop's message reaches the model
//! before the next frame, so the chain simply shows the order it is given.
//!
//! The cards move as `SliverReorderableList` moved them: as the gap where
//! the dragged card will land moves, each card it passes slides to its new
//! place over 250 ms (`Curves.easeInOut`, from wherever it was); on the
//! drop the card glides from where it was let go into its slot over 250 ms
//! (`Curves.easeOut`). The order changes at the drop, at once (Flutter
//! reported it once the glide ended); only the drawing follows. The time
//! comes from the window's frames, and the chain asks for them only while a
//! card moves.
//!
//! There is no auto-scroll to port: the Dart list was shrink-wrapped with
//! `NeverScrollableScrollPhysics`, so the edge auto-scroller it drove
//! (`EdgeDraggingAutoScroller` on the list's own scrollable) had nothing
//! to scroll, and the panel around it was not the scrollable it drove.

use iced::advanced::layout::{self, flex, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use std::time::Duration;

use iced::time::Instant;
use iced::{
    mouse, window, Alignment, Element, Event, Length, Padding, Point, Rectangle, Renderer, Size,
    Theme, Vector,
};
use vocara_model::models::audio_fx::FxKind;

use super::Message;
use crate::ui::core::motion::{Curve, Lerp};
use crate::ui::song_practice_detail::widgets::mixer_shared::diff_keyed;

/// How long a card takes to slide aside, and the dropped one to land
/// (`_ReorderableItemState`'s and `_DragInfo`'s 250 ms).
pub const CARD_MOTION: Duration = Duration::from_millis(250);

/// The handle's hit area: the 16 px grip and its 6 px gap, as wide as the
/// header is tall.
pub const HANDLE_WIDTH: f32 = 22.0;
pub const HEADER_HEIGHT: f32 = 18.0;

/// The index a dragged card lands at, its top at `proxy_top` (from the
/// chain's top): every other card whose middle, where it is laid out, lies
/// above the dragged card's middle stays before it. `heights` are the
/// cards' heights in order; the result indexes the order after the card is
/// taken out.
pub fn reorder_target(heights: &[f32], from: usize, proxy_top: f32) -> usize {
    let Some(&own) = heights.get(from) else {
        return from;
    };
    let middle = proxy_top + own / 2.0;
    let mut y = 0.0;
    let mut target = 0;
    for (i, &h) in heights.iter().enumerate() {
        if i != from && y + h / 2.0 < middle {
            target += 1;
        }
        y += h;
    }
    target
}

/// `order` with the card at `from` moved to `to` (`_onReorder`).
pub fn reordered(order: &[FxKind], from: usize, to: usize) -> Vec<FxKind> {
    let mut next = order.to_vec();
    if from < next.len() {
        let kind = next.remove(from);
        next.insert(to.min(next.len()), kind);
    }
    next
}

/// See the module docs.
pub struct InsertChain<'a> {
    kinds: Vec<FxKind>,
    cards: Vec<Element<'a, Message>>,
    on_reorder: Box<dyn Fn(Vec<FxKind>) -> Message + 'a>,
}

impl<'a> InsertChain<'a> {
    pub fn new(
        cards: Vec<(FxKind, Element<'a, Message>)>,
        on_reorder: impl Fn(Vec<FxKind>) -> Message + 'a,
    ) -> Self {
        let (kinds, cards) = cards.into_iter().unzip();
        Self {
            kinds,
            cards,
            on_reorder: Box::new(on_reorder),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    from: usize,
    /// Where on the card it was taken, from the card's top.
    grab: f32,
    /// The pointer's y.
    y: f32,
}

struct ChainState {
    kinds: Vec<FxKind>,
    drag: Option<Drag>,
    /// The cards moving, or parted, by kind.
    motions: Vec<(FxKind, Motion)>,
    /// The last frame's time.
    now: Option<Instant>,
}

/// How a card is drawn away from where it is laid out.
#[derive(Debug, Clone, Copy)]
enum Motion {
    /// Parted around a drag's gap: its offset sliding from `from` to `to`
    /// over [`CARD_MOTION`] (`Curves.easeInOut`), from `start`.
    Aside { from: f32, to: f32, start: Instant },
    /// After a drop: from where it was drawn (its top, from the chain's
    /// top) into its place, wherever that is laid out, over
    /// [`CARD_MOTION`] along `curve`, from the first frame after.
    Landing {
        from_top: f32,
        start: Option<Instant>,
        curve: Curve,
    },
}

impl Motion {
    fn progress(start: Option<Instant>, now: Instant) -> f32 {
        start.map_or(0.0, |start| {
            let elapsed = now.saturating_duration_since(start);
            (elapsed.as_secs_f32() / CARD_MOTION.as_secs_f32()).clamp(0.0, 1.0)
        })
    }

    /// The offset at `now` of a card laid out at `top`.
    fn offset(&self, now: Instant, top: f32) -> f32 {
        match *self {
            Motion::Aside { from, to, start } => from.lerp(
                to,
                Curve::EaseInOut.transform(Self::progress(Some(start), now)),
            ),
            Motion::Landing {
                from_top,
                start,
                curve,
            } => (from_top - top) * (1.0 - curve.transform(Self::progress(start, now))),
        }
    }

    /// Where it heads, as an offset.
    fn heading(&self) -> f32 {
        match *self {
            Motion::Aside { to, .. } => to,
            Motion::Landing { .. } => 0.0,
        }
    }

    fn is_moving(&self, now: Instant) -> bool {
        match *self {
            Motion::Aside { start, .. } => Self::progress(Some(start), now) < 1.0,
            Motion::Landing { start, .. } => Self::progress(start, now) < 1.0,
        }
    }

    /// The card landing on top of the others (the dropped one).
    fn is_dropped(&self) -> bool {
        matches!(
            self,
            Motion::Landing {
                curve: Curve::EaseOut,
                ..
            }
        )
    }
}

impl ChainState {
    fn motion(&self, kind: FxKind) -> Option<&Motion> {
        self.motions
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, m)| m)
    }

    fn set(&mut self, kind: FxKind, motion: Motion) {
        match self.motions.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, m)) => *m = motion,
            None => self.motions.push((kind, motion)),
        }
    }

    /// Kind `kind`'s offset at the last frame, laid out at `top`.
    fn offset_at(&self, kind: FxKind, top: f32) -> f32 {
        match (self.motion(kind), self.now) {
            (Some(m), Some(now)) => m.offset(now, top),
            (Some(Motion::Aside { from, .. }), None) => *from,
            (Some(Motion::Landing { from_top, .. }), None) => from_top - top,
            (None, _) => 0.0,
        }
    }

    /// The dropped card while it lands.
    fn landing(&self) -> Option<FxKind> {
        let now = self.now?;
        self.motions
            .iter()
            .find(|(_, m)| m.is_dropped() && m.is_moving(now))
            .map(|(k, _)| *k)
    }

    /// The drop of `drag`: every card lands from where it is drawn (the
    /// dropped one gliding in on `Curves.easeOut`), once laid out anew.
    fn drop(&mut self, layout: Layout<'_>, drag: Drag) {
        let top = layout.bounds().y;
        let proxy = drag_offsets(layout, drag);
        let drawn: Vec<(FxKind, f32)> = self
            .kinds
            .iter()
            .zip(layout.children())
            .enumerate()
            .map(|(i, (kind, card))| {
                let laid = card.bounds().y - top;
                let dy = if i == drag.from {
                    proxy[i]
                } else {
                    self.offset_at(*kind, laid)
                };
                (*kind, laid + dy)
            })
            .collect();
        let dropped = self.kinds.get(drag.from).copied();
        for (kind, from_top) in drawn {
            let curve = if Some(kind) == dropped {
                Curve::EaseOut
            } else {
                Curve::EaseInOut
            };
            self.set(
                kind,
                Motion::Landing {
                    from_top,
                    start: None,
                    curve,
                },
            );
        }
        self.drag = None;
    }

    /// A frame at `at`: the cards head where the drag puts them, and a
    /// drop's land.
    fn frame(&mut self, layout: Layout<'_>, at: Instant, shell: &mut Shell<'_, Message>) {
        let now = self.now.map_or(at, |was| was.max(at));
        self.now = Some(now);
        for (_, motion) in &mut self.motions {
            if let Motion::Landing { start, .. } = motion {
                start.get_or_insert(now);
            }
        }
        let top = layout.bounds().y;
        let tops: Vec<f32> = layout.children().map(|c| c.bounds().y - top).collect();
        let targets = match self.drag {
            Some(drag) => drag_offsets(layout, drag),
            None => vec![0.0; self.kinds.len()],
        };
        let dragged = self.drag.map(|d| d.from);
        for (i, kind) in self.kinds.clone().into_iter().enumerate() {
            if Some(i) == dragged {
                // The dragged card follows the pointer.
                continue;
            }
            let to = targets.get(i).copied().unwrap_or(0.0);
            let heading = self.motion(kind).map_or(0.0, Motion::heading);
            if (to - heading).abs() > 0.01 {
                let laid = tops.get(i).copied().unwrap_or(0.0);
                let from = self.offset_at(kind, laid);
                self.set(
                    kind,
                    Motion::Aside {
                        from,
                        to,
                        start: now,
                    },
                );
            }
        }
        // Those at rest in their place need nothing more.
        self.motions
            .retain(|(_, m)| m.is_moving(now) || m.heading().abs() > 0.01);
        if self.motions.iter().any(|(_, m)| m.is_moving(now)) {
            shell.request_redraw();
        }
    }
}

/// The cards' heights in a laid-out chain.
fn heights(layout: Layout<'_>) -> Vec<f32> {
    layout.children().map(|c| c.bounds().height).collect()
}

/// Where each card is drawn while `drag` is held: per card, its offset from
/// where it is laid out — the dragged one under the pointer, the others
/// parted around the gap where it will land.
fn drag_offsets(layout: Layout<'_>, drag: Drag) -> Vec<f32> {
    let top = layout.bounds().y;
    let heights = heights(layout);
    let tops: Vec<f32> = layout.children().map(|c| c.bounds().y - top).collect();
    let proxy_top = drag.y - drag.grab - top;
    let target = reorder_target(&heights, drag.from, proxy_top);
    let gap = heights.get(drag.from).copied().unwrap_or(0.0);
    let mut y = 0.0;
    let mut slot = 0;
    let mut offsets = vec![0.0; heights.len()];
    for (i, &h) in heights.iter().enumerate() {
        if i == drag.from {
            offsets[i] = proxy_top - tops[i];
            continue;
        }
        if slot == target {
            y += gap;
        }
        offsets[i] = y - tops[i];
        y += h;
        slot += 1;
    }
    offsets
}

fn pointer(cursor: mouse::Cursor) -> Option<Point> {
    match cursor {
        mouse::Cursor::Available(p) | mouse::Cursor::Levitating(p) => Some(p),
        mouse::Cursor::Unavailable => None,
    }
}

/// The card whose handle is under `p`.
fn handle_at(layout: Layout<'_>, p: Point) -> Option<usize> {
    layout.children().position(|card| {
        let b = card.bounds();
        Rectangle::new(b.position(), Size::new(HANDLE_WIDTH, HEADER_HEIGHT)).contains(p)
    })
}

impl Widget<Message, Theme, Renderer> for InsertChain<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ChainState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ChainState {
            kinds: self.kinds.clone(),
            drag: None,
            motions: Vec::new(),
            now: None,
        })
    }

    fn children(&self) -> Vec<Tree> {
        self.cards.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<ChainState>();
        diff_keyed(&state.kinds, children, &self.kinds, &self.cards);
        if state.kinds != self.kinds {
            // Another chain (or the new order): a held drag's index no
            // longer names its card.
            state.drag = None;
            state.kinds.clone_from(&self.kinds);
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        flex::resolve(
            flex::Axis::Vertical,
            renderer,
            limits,
            Length::Fill,
            Length::Shrink,
            Padding::ZERO,
            0.0,
            Alignment::Start,
            &mut self.cards,
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
            for ((card, state), layout) in self
                .cards
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                card.as_widget_mut()
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
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(at)) = event {
            tree.state
                .downcast_mut::<ChainState>()
                .frame(layout, *at, shell);
        }
        let drag = tree.state.downcast_ref::<ChainState>().drag;
        if let Some(mut drag) = drag {
            // The drag owns the pointer until the drop.
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    if let Some(p) = pointer(cursor) {
                        drag.y = p.y;
                        tree.state.downcast_mut::<ChainState>().drag = Some(drag);
                        shell.request_redraw();
                    }
                    shell.capture_event();
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    let top = layout.bounds().y;
                    let target =
                        reorder_target(&heights(layout), drag.from, drag.y - drag.grab - top);
                    // The cards land from where they are drawn as it is let
                    // go, once laid out in the new order.
                    tree.state.downcast_mut::<ChainState>().drop(layout, drag);
                    if target != drag.from {
                        shell.publish((self.on_reorder)(reordered(&self.kinds, drag.from, target)));
                    }
                    shell.request_redraw();
                    shell.capture_event();
                }
                Event::Mouse(_) => shell.capture_event(),
                _ => {}
            }
            if matches!(event, Event::Mouse(_)) {
                return;
            }
        }
        for ((card, state), layout) in self
            .cards
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            card.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
            if shell.is_event_captured() {
                return;
            }
            let Some(p) = cursor.position() else {
                return;
            };
            if let Some(from) = handle_at(layout, p) {
                let card_top = layout.children().nth(from).map_or(p.y, |c| c.bounds().y);
                tree.state.downcast_mut::<ChainState>().drag = Some(Drag {
                    from,
                    grab: p.y - card_top,
                    y: p.y,
                });
                shell.request_redraw();
                shell.capture_event();
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
        if tree.state.downcast_ref::<ChainState>().drag.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if cursor
            .position()
            .and_then(|p| handle_at(layout, p))
            .is_some()
        {
            return mouse::Interaction::Grab;
        }
        self.cards
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((card, state), layout)| {
                card.as_widget()
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
        let chain = tree.state.downcast_ref::<ChainState>();
        let top = layout.bounds().y;
        let laid: Vec<f32> = layout.children().map(|c| c.bounds().y - top).collect();
        let mut offsets: Vec<f32> = self
            .kinds
            .iter()
            .zip(&laid)
            .map(|(kind, laid)| chain.offset_at(*kind, *laid))
            .collect();
        // On top: the dragged card, or the dropped one as it lands.
        let above = match chain.drag {
            Some(drag) => {
                if let Some(dy) = drag_offsets(layout, drag).get(drag.from) {
                    offsets[drag.from] = *dy;
                }
                Some(drag.from)
            }
            None => chain
                .landing()
                .and_then(|k| self.kinds.iter().position(|x| *x == k)),
        };
        if above.is_none() && offsets.iter().all(|dy: &f32| *dy == 0.0) {
            for ((card, state), layout) in
                self.cards.iter().zip(&tree.children).zip(layout.children())
            {
                card.as_widget()
                    .draw(state, renderer, theme, style, layout, cursor, viewport);
            }
            return;
        }
        let cursor = if chain.drag.is_some() {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
        let cards = || {
            self.cards
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
                .zip(&offsets)
                .enumerate()
        };
        // The others, parted around where the card will land.
        for (i, (((card, state), layout), &dy)) in cards() {
            if Some(i) == above {
                continue;
            }
            renderer.with_translation(Vector::new(0.0, dy), |renderer| {
                card.as_widget()
                    .draw(state, renderer, theme, style, layout, cursor, viewport);
            });
        }
        // The dragged card above them all (its panel color occludes them).
        if let Some((_, (((card, state), layout), &dy))) = above.and_then(|i| cards().nth(i)) {
            renderer.with_layer(*viewport, |renderer| {
                renderer.with_translation(Vector::new(0.0, dy), |renderer| {
                    card.as_widget()
                        .draw(state, renderer, theme, style, layout, cursor, viewport);
                });
            });
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.cards,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<InsertChain<'a>> for Element<'a, Message> {
    fn from(chain: InsertChain<'a>) -> Self {
        Element::new(chain)
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::{container, Space};
    use iced::Color;

    use super::*;
    use crate::ui::core::theme;
    use crate::ui::core::widgets::animated::testing::{near, Frames, BACKGROUND, MS};

    const A: Color = Color::from_rgb(1.0, 0.0, 0.0);
    const B: Color = Color::from_rgb(0.0, 1.0, 0.0);
    const C: Color = Color::from_rgb(0.0, 0.0, 1.0);

    /// Three 40 px cards, A, B and C, in `order`.
    fn view<'a>(order: [FxKind; 3]) -> Element<'a, Message> {
        let card = |kind: FxKind| {
            let color = match kind {
                FxKind::Eq => A,
                FxKind::Comp => B,
                _ => C,
            };
            let face: Element<'a, Message> = container(Space::new())
                .width(Length::Fill)
                .height(40)
                .style(theme::fill(color))
                .into();
            (kind, face)
        };
        InsertChain::new(order.into_iter().map(card).collect(), |order| {
            Message::ReorderTrackInserts {
                id: "t".into(),
                order,
            }
        })
        .into()
    }

    const ABC: [FxKind; 3] = [FxKind::Eq, FxKind::Comp, FxKind::Gate];
    const BAC: [FxKind; 3] = [FxKind::Comp, FxKind::Eq, FxKind::Gate];

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }

    #[test]
    fn the_cards_a_drag_passes_slide_aside_over_250_ms_and_the_drop_glides_in() {
        let mut window = Frames::new(Size::new(100.0, 200.0));
        let at = |w: &Frames, y: f32| w.pixel(Point::new(50.0, y));
        let t0 = Instant::now();
        assert!(!window.frame(view(ABC), &[], t0), "idle");
        // A taken by its handle and carried 70 down: its middle passes B's.
        window.point_at(Point::new(5.0, 5.0));
        window.frame(view(ABC), &[press()], t0);
        window.point_at(Point::new(5.0, 75.0));
        let t1 = t0 + 100 * MS;
        assert!(window.frame(view(ABC), &[], t1), "B starts to slide up");
        assert!(near(at(&window, 30.0), BACKGROUND), "start: B still at 40");
        assert!(near(at(&window, 75.0), A), "A under the pointer");
        window.frame(view(ABC), &[], t1 + 125 * MS);
        // Half way on easeInOut: 20 up.
        assert!(near(at(&window, 30.0), B) && near(at(&window, 15.0), BACKGROUND));
        assert!(!window.frame(view(ABC), &[], t1 + 250 * MS), "rests");
        assert!(near(at(&window, 15.0), B), "end: B at the top");

        // Dropped: the order changes at once, and A glides from where it
        // was let go (its top at 70) into its new place (40).
        let t2 = t1 + 300 * MS;
        window.frame(view(ABC), &[release()], t2);
        assert!(near(at(&window, 55.0), BACKGROUND), "still where let go");
        assert!(window.frame(view(BAC), &[], t2), "laid out anew: it glides");
        assert!(near(at(&window, 55.0), BACKGROUND), "start");
        assert!(near(at(&window, 15.0), B), "B in its place");
        window.frame(view(BAC), &[], t2 + 125 * MS);
        // 30 × (1 − easeOut(0.5)) ≈ 9.5 left to go: its top near 49.5.
        assert!(near(at(&window, 45.0), BACKGROUND) && near(at(&window, 55.0), A));
        assert!(!window.frame(view(BAC), &[], t2 + 250 * MS), "rests");
        assert!(near(at(&window, 45.0), A), "end: in its place");
        assert!(near(at(&window, 85.0), C), "C never moved");
    }
}
