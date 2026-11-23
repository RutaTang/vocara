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
//! Flutter animated the cards sliding apart and auto-scrolled the panel
//! near its edges; iced moves them at once and does not scroll.

use iced::advanced::layout::{self, flex, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::{
    mouse, Alignment, Element, Event, Length, Padding, Point, Rectangle, Renderer, Size, Theme,
    Vector,
};
use vocara_model::models::audio_fx::FxKind;

use super::Message;
use crate::ui::song_practice_detail::widgets::mixer_shared::diff_keyed;

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
                    tree.state.downcast_mut::<ChainState>().drag = None;
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
        let drag = tree.state.downcast_ref::<ChainState>().drag;
        let Some(drag) = drag else {
            for ((card, state), layout) in
                self.cards.iter().zip(&tree.children).zip(layout.children())
            {
                card.as_widget()
                    .draw(state, renderer, theme, style, layout, cursor, viewport);
            }
            return;
        };
        let offsets = drag_offsets(layout, drag);
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
            if i == drag.from {
                continue;
            }
            renderer.with_translation(Vector::new(0.0, dy), |renderer| {
                card.as_widget().draw(
                    state,
                    renderer,
                    theme,
                    style,
                    layout,
                    mouse::Cursor::Unavailable,
                    viewport,
                );
            });
        }
        // The dragged card above them all (its panel color occludes them).
        if let Some((_, (((card, state), layout), &dy))) = cards().nth(drag.from) {
            renderer.with_layer(*viewport, |renderer| {
                renderer.with_translation(Vector::new(0.0, dy), |renderer| {
                    card.as_widget().draw(
                        state,
                        renderer,
                        theme,
                        style,
                        layout,
                        mouse::Cursor::Unavailable,
                        viewport,
                    );
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
