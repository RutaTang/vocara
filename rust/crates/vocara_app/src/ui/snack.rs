//! Snack bars: Dart's `ScaffoldMessenger` queue and Material 3's `SnackBar`
//! transition, shared by every page that shows them and by the app
//! (`app::messenger`).
//!
//! A [`SnackQueue`] shows its first snack bar: it enters over 250 ms, stays
//! its time counted from the end of its entrance (the messenger started its
//! timer once the entrance completed), then leaves over 250 ms; the next
//! one enters once it has left. A snack bar shown with `clearSnackBars()`
//! first sends the one showing away (from wherever its entrance got) and
//! drops the ones waiting. The queue's state changes at once — the one
//! showing is the new one from the moment it is shown — and only the
//! drawing waits: the one leaving is drawn until it has left.
//!
//! How a snack bar enters and leaves is Material 3's for its behavior
//! ([`SnackStyle`]): a floating one fades in between 40 % and 60 % of its
//! entrance (`Interval(0.4, 0.6, curve: Curves.easeInCirc)`) while it grows
//! up from its bottom edge (`Curves.easeInOutQuart`, clipped), and leaves by
//! the same fade alone; a fixed one slides up from the page's bottom edge
//! (`Curves.fastOutSlowIn`, clipped) and back down. Flutter's `Hero`
//! between the pages' scaffolds has no counterpart.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::alignment::Vertical;
use iced::time::Instant;
use iced::widget::{container, stack};
use iced::{mouse, Element, Event, Length, Rectangle, Renderer, Size, Subscription, Theme};

use crate::ui::core::motion::{self, Controller, Curve};

/// How long a snack bar takes to enter, and to leave
/// (`_snackBarTransitionDuration`).
pub const TRANSITION: Duration = Duration::from_millis(250);

/// How a snack bar enters and leaves: its `SnackBarBehavior`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnackStyle {
    /// `SnackBarBehavior.floating`: in by fading while it grows up from its
    /// bottom edge, out by the fade.
    Floating,
    /// The default, fixed bar: slides up from the page's bottom and back.
    Fixed,
}

/// Draws a snack bar's text, at an opacity, as its page does (`SnackBar`
/// reports nothing: no Dart snack bar had an action). A floating snack bar
/// includes its margin.
pub type SnackLook = for<'a> fn(&'a str, f32) -> Element<'a, Infallible>;

/// A snack bar: what it says, its time and look; a page hands its own to
/// the app ([`Screen::take_snack_bars`](crate::ui::Screen::take_snack_bars)).
#[derive(Debug, Clone)]
pub struct SnackBar {
    /// What it says.
    pub text: String,
    /// How long it still shows once it has entered: the rest of its time
    /// for the one showing, all of it for one still waiting.
    pub left: Duration,
    /// How long until its entrance is over: all of [`TRANSITION`] for one
    /// still waiting, nothing for one that has entered.
    pub entrance: Duration,
    /// Whether its page cleared the snack bars up before showing it
    /// (`clearSnackBars()`, then `showSnackBar`), or queued it behind them
    /// (`showSnackBar` alone).
    pub clears: bool,
    /// How it enters and leaves.
    pub style: SnackStyle,
    /// Its page's widget for it.
    pub look: SnackLook,
}

impl SnackBar {
    /// One waiting its turn: its whole entrance and all of `duration` still
    /// ahead of it.
    pub fn new(
        text: impl Into<String>,
        duration: Duration,
        clears: bool,
        style: SnackStyle,
        look: SnackLook,
    ) -> Self {
        Self {
            text: text.into(),
            left: duration,
            entrance: TRANSITION,
            clears,
            style,
            look,
        }
    }
}

/// When the snack bar showing (number `seq`) has had its time: the end of
/// its entrance, then its time. The subscription's identity, so the next
/// one starts its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnackTimer {
    pub seq: u64,
    pub deadline: Instant,
}

/// What a [`SnackQueue`]'s subscription reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnackEvent {
    /// Snack bar `n`'s time is up.
    Expired(u64),
    /// A frame, while one enters or leaves.
    Frame(Instant),
}

/// A snack bar's entrance and exit.
#[derive(Debug, Clone)]
struct Motion {
    controller: Controller,
    /// When its entrance ends, and its time starts.
    entered: Instant,
}

#[derive(Debug, Clone)]
struct Entry {
    seq: u64,
    bar: SnackBar,
    /// Set once it is the first in the queue.
    motion: Option<Motion>,
}

/// See the module docs.
#[derive(Debug)]
pub struct SnackQueue {
    /// The first shows (or waits for the one leaving); the rest wait their
    /// turn.
    queue: VecDeque<Entry>,
    /// The one leaving, drawn until it has left.
    leaving: Option<Box<Entry>>,
    /// Numbers the snack bars, so a stale timer takes down nothing.
    made: u64,
    /// The last frame's time, what the drawing reads.
    now: Instant,
}

impl Default for SnackQueue {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            leaving: None,
            made: 0,
            now: Instant::now(),
        }
    }
}

impl SnackQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// The snack bar showing (or about to, once the one before it has
    /// left), if any.
    pub fn current(&self) -> Option<&SnackBar> {
        self.queue.front().map(|e| &e.bar)
    }

    /// Its number ([`SnackEvent::Expired`] takes it).
    pub fn current_seq(&self) -> Option<u64> {
        self.queue.front().map(|e| e.seq)
    }

    /// The snack bars up and waiting, the one showing first.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.queue.iter().map(|e| e.bar.text.as_str())
    }

    /// How many are up or waiting.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Shows `bar` at `now` (see the module docs for a clearing one).
    pub fn show(&mut self, bar: SnackBar, now: Instant) {
        if bar.clears {
            self.queue.truncate(1);
            if let Some(showing) = self.queue.pop_front() {
                self.leave(showing, now);
            }
        }
        self.made += 1;
        self.queue.push_back(Entry {
            seq: self.made,
            bar,
            motion: None,
        });
        self.start_first(now);
    }

    /// Takes in snack bars handed over, in order (each as [`Self::show`]).
    pub fn adopt(&mut self, bars: impl IntoIterator<Item = SnackBar>, now: Instant) {
        for bar in bars {
            self.show(bar, now);
        }
    }

    /// Snack bar `seq`'s time is up at `now`: it leaves, and the next one
    /// enters once it has. A stale timer takes down nothing.
    pub fn expired(&mut self, seq: u64, now: Instant) {
        if self.queue.front().is_some_and(|e| e.seq == seq) {
            if let Some(showing) = self.queue.pop_front() {
                self.leave(showing, now);
            }
            self.start_first(now);
        }
    }

    /// A frame at `now`: what is drawn moves on, and the one leaving goes
    /// once it has left.
    pub fn frame(&mut self, now: Instant) {
        self.now = self.now.max(now);
        if self
            .leaving
            .as_ref()
            .is_some_and(|e| !is_moving(e, self.now))
        {
            self.leaving = None;
        }
    }

    /// Whether one enters or leaves at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.queue.front().is_some_and(|e| is_moving(e, now))
            || self.leaving.as_ref().is_some_and(|e| is_moving(e, now))
    }

    /// The timer of the one showing: up its time after its entrance ends.
    pub fn timer(&self) -> Option<SnackTimer> {
        let first = self.queue.front()?;
        let motion = first.motion.as_ref()?;
        Some(SnackTimer {
            seq: first.seq,
            deadline: motion.entered + first.bar.left,
        })
    }

    /// The timer, and the frames while one enters or leaves.
    pub fn subscription(&self) -> Subscription<SnackEvent> {
        let frames = motion::frames(self.is_animating(Instant::now())).map(SnackEvent::Frame);
        match self.timer() {
            Some(timer) => {
                Subscription::batch([Subscription::run_with(timer, timer_stream), frames])
            }
            None => frames,
        }
    }

    /// Hands over the snack bars at `now` — the one showing with the time
    /// it has left, then the ones waiting — and stops showing them. One
    /// leaving is not handed over: it is gone with its page.
    pub fn take(&mut self, now: Instant) -> Vec<SnackBar> {
        self.leaving = None;
        let mut bars = Vec::new();
        for entry in std::mem::take(&mut self.queue) {
            let mut bar = entry.bar;
            if let Some(motion) = entry.motion {
                bar.entrance = motion.entered.saturating_duration_since(now);
                bar.left = bar
                    .left
                    .saturating_sub(now.saturating_duration_since(motion.entered));
                if bar.left.is_zero() {
                    // Its time is up; only the message saying so is late.
                    continue;
                }
            }
            bars.push(bar);
        }
        bars
    }

    /// The snack bars as drawn at the last frame: the one leaving, and the
    /// one showing, over the bottom of the page.
    pub fn view(&self) -> Option<Element<'_, Infallible>> {
        let mut layers = stack![].width(Length::Fill).height(Length::Fill);
        let mut any = false;
        for entry in self
            .leaving
            .as_deref()
            .into_iter()
            .chain(self.queue.front())
        {
            let Some(motion) = &entry.motion else {
                continue;
            };
            layers = layers.push(transition(&entry.bar, &motion.controller, self.now));
            any = true;
        }
        any.then(|| layers.into())
    }

    /// Sends `entry` away at `now`, from wherever its entrance got; one not
    /// yet in sight just goes.
    fn leave(&mut self, mut entry: Entry, now: Instant) {
        let Some(motion) = &mut entry.motion else {
            return;
        };
        if motion.controller.value(now) <= 0.0 {
            return;
        }
        motion.controller.reverse(now);
        self.leaving = Some(Box::new(entry));
    }

    /// The first snack bar starts its entrance — once the one leaving has
    /// left — if it has not yet.
    fn start_first(&mut self, now: Instant) {
        let after = self
            .leaving
            .as_ref()
            .and_then(|e| e.motion.as_ref())
            .map_or(now, |m| m.controller.ends_at(now));
        let Some(first) = self.queue.front_mut() else {
            return;
        };
        if first.motion.is_some() {
            return;
        }
        // A snack bar handed over part way through its entrance carries on
        // from there; one with more than an entrance ahead waits first.
        let entrance = first.bar.entrance;
        let ahead = entrance.min(TRANSITION);
        let start = after + entrance.saturating_sub(TRANSITION);
        let mut controller = Controller::new(TRANSITION, TRANSITION);
        controller.set(1.0 - ahead.as_secs_f32() / TRANSITION.as_secs_f32());
        controller.forward(start);
        first.motion = Some(Motion {
            controller,
            entered: start + ahead,
        });
    }
}

fn is_moving(entry: &Entry, now: Instant) -> bool {
    entry
        .motion
        .as_ref()
        .is_some_and(|m| m.controller.is_animating(now))
}

/// The showing snack bar's one-shot timer.
fn timer_stream(timer: &SnackTimer) -> impl iced::futures::Stream<Item = SnackEvent> {
    use iced::futures::StreamExt;
    let (seq, deadline) = (timer.seq, timer.deadline);
    iced::futures::stream::once(async move {
        tokio::time::sleep_until(deadline.into()).await;
        SnackEvent::Expired(seq)
    })
    .chain(iced::futures::stream::pending())
}

/// How far in `bar` is at `now` on `controller`: `(opacity, height factor)`
/// (see the module docs).
pub fn progress(style: SnackStyle, controller: &Controller, now: Instant) -> (f32, f32) {
    match style {
        SnackStyle::Floating => (
            controller.curved(Curve::SnackBarM3FadeIn, None, now),
            // `reverseCurve: Threshold(0.0)`: it keeps its height leaving.
            controller.curved(Curve::EaseInOutQuart, Some(Curve::Threshold0), now),
        ),
        SnackStyle::Fixed => (1.0, controller.curved(Curve::FastOutSlowIn, None, now)),
    }
}

/// `bar` drawn at `now` on `controller`, at the bottom of the page.
fn transition<'a>(
    bar: &'a SnackBar,
    controller: &Controller,
    now: Instant,
) -> Element<'a, Infallible> {
    let (opacity, height) = progress(bar.style, controller, now);
    let anchor = match bar.style {
        SnackStyle::Floating => Anchor::Bottom,
        SnackStyle::Fixed => Anchor::Top,
    };
    container(Reveal {
        content: (bar.look)(&bar.text, opacity),
        factor: height,
        anchor,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .align_y(Vertical::Bottom)
    .into()
}

/// Which edge of a [`Reveal`]'s content stays put as it grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// `Alignment.topStart`: the content's top shows first.
    Top,
    /// `Alignment.bottomLeft`: its bottom shows first.
    Bottom,
}

/// `ClipRect(Align(heightFactor: factor))`: as tall as `factor` of its
/// content, which it clips, anchored at one edge.
struct Reveal<'a> {
    content: Element<'a, Infallible>,
    factor: f32,
    anchor: Anchor,
}

impl Widget<Infallible, Theme, Renderer> for Reveal<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(self.content.as_widget().size().width, Length::Shrink)
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
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let full = node.size();
        let height = full.height * self.factor.clamp(0.0, 1.0);
        let top = match self.anchor {
            Anchor::Top => 0.0,
            Anchor::Bottom => height - full.height,
        };
        layout::Node::with_children(
            Size::new(full.width, height),
            vec![node.move_to((0.0, top))],
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
        shell: &mut Shell<'_, Infallible>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("the content's layout"),
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
        let bounds = layout.bounds();
        if bounds.height <= 0.0 {
            return;
        }
        renderer.with_layer(bounds, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout.children().next().expect("the content's layout"),
                cursor,
                viewport,
            );
        });
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("the content's layout"),
            renderer,
            operation,
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
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("the content's layout"),
            cursor,
            viewport,
            renderer,
        )
    }
}

impl<'a> From<Reveal<'a>> for Element<'a, Infallible> {
    fn from(reveal: Reveal<'a>) -> Self {
        Element::new(reveal)
    }
}

#[cfg(test)]
mod tests;
