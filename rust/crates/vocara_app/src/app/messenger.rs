//! The snack bars the app shows itself: Dart's `ScaffoldMessenger`, which
//! `MaterialApp` put above every page, so a snack bar outlived the page that
//! showed it.
//!
//! A page draws its own snack bars while it is on top
//! ([`Screen::take_snack_bars`](crate::ui::Screen::take_snack_bars)). The
//! app takes them over when the page stops being on top — popped, dropped
//! by a go, or covered by a push — and draws the first over the page on top
//! now, with its page's own widget, until the time it had left runs out;
//! then the next one, for all of its time. While the app shows one, a page's
//! new snack bar joins them as its page showed it in Dart: Home, the song
//! page and Settings cleared the ones up first (`clearSnackBars()`), the
//! technique pages queued it behind them (`showSnackBar` alone).
//!
//! So a page holds snack bars of its own only while it is on top and the
//! app shows none: the order in which they were shown is the order in which
//! they come and go, as in Dart's single queue. They enter and leave as
//! Material's did, a handed-over one carrying on from where its entrance
//! got ([`crate::ui::snack`]); one swiped away goes at once, and the next
//! enters (`removeCurrentSnackBar(reason: SnackBarClosedReason.swipe)`).
//!
//! # Differences from Flutter
//!
//! The app draws its snack bar over the whole page, so above what a page
//! draws over itself in place of a Dart dialog route (Home's "Loading
//! practice" dialog, the song page's note editor and technique picker),
//! which covered a snack bar in Dart; the studio dialogs still cover it. No
//! Dart snack bar had an action, so none is carried over. One leaving as its
//! page stops being on top goes at once. A snack bar being dragged (swiped)
//! as its page stops being on top is let go: the app draws it at rest, its
//! slide being the page's widget's.

use std::time::Instant;

use iced::{Element, Subscription};

use super::{Message, Page};
use crate::router::Router;
use crate::ui::snack::{SnackEvent, SnackQueue, SnackTimer};
use crate::ui::SnackBar;

/// The app's snack bars: the first shows, the rest wait their turn.
#[derive(Debug, Default)]
pub(super) struct Messenger {
    queue: SnackQueue,
}

impl Messenger {
    /// The snack bar showing, if any.
    pub fn showing(&self) -> Option<&SnackBar> {
        self.queue.current()
    }

    /// The showing snack bar's timer.
    pub fn timer(&self) -> Option<SnackTimer> {
        self.queue.timer()
    }

    /// Takes in snack bars a page handed over at `now`, in order: one its
    /// page cleared the others for takes their place, the others wait
    /// behind.
    pub fn adopt(&mut self, bars: Vec<SnackBar>, now: Instant) {
        self.queue.adopt(bars, now);
    }

    /// Takes over the snack bars of every page that is not on top, and of
    /// the one on top while the app shows one already (see the module
    /// docs).
    pub fn gather(&mut self, router: &mut Router<Page>) {
        let top = router.top().0;
        let now = Instant::now();
        for (id, page) in router.pages_mut() {
            if id != top || !self.queue.is_empty() {
                let bars = page.take_snack_bars();
                self.adopt(bars, now);
            }
        }
    }

    /// What the snack bars' subscription, or one of them, reported.
    pub fn event(&mut self, event: SnackEvent) {
        match event {
            SnackEvent::Expired(seq) => self.queue.expired(seq, Instant::now()),
            SnackEvent::Frame(at) => self.queue.frame(at),
            SnackEvent::Swiped(seq) => self.queue.swiped(seq, Instant::now()),
        }
    }

    /// The snack bars as their pages drew them, entering or leaving.
    pub fn view(&self) -> Option<Element<'_, Message>> {
        self.queue.view().map(|e| e.map(message))
    }

    /// Their timer, and the frames while one enters or leaves.
    pub fn subscription(&self) -> Subscription<Message> {
        self.queue.subscription().map(message)
    }
}

/// The app's message for what its snack bars report.
fn message(event: SnackEvent) -> Message {
    match event {
        SnackEvent::Expired(seq) => Message::SnackBarExpired(seq),
        SnackEvent::Frame(at) => Message::SnackBarFrame(at),
        SnackEvent::Swiped(seq) => Message::SnackBarSwiped(seq),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::ui::snack::{SnackStyle, TRANSITION};

    fn look(text: &str) -> Element<'_, std::convert::Infallible> {
        crate::ui::core::text(text).into()
    }

    fn bar(text: &str, left_ms: u64, clears: bool) -> SnackBar {
        SnackBar::new(
            text,
            Duration::from_millis(left_ms),
            clears,
            SnackStyle::Floating,
            look,
        )
    }

    fn texts(m: &Messenger) -> Vec<&str> {
        m.queue.texts().collect()
    }

    #[test]
    fn a_clearing_snack_bar_takes_the_place_of_those_up_and_a_queued_one_waits() {
        let mut m = Messenger::default();
        let t0 = Instant::now();
        m.adopt(vec![bar("a", 100, false), bar("b", 4000, false)], t0);
        assert_eq!(texts(&m), ["a", "b"]);
        m.adopt(vec![bar("c", 4000, false)], t0);
        assert_eq!(texts(&m), ["a", "b", "c"], "queued behind");
        let t1 = t0 + Duration::from_secs(1);
        m.adopt(vec![bar("d", 2800, true)], t1);
        assert_eq!(texts(&m), ["d"], "cleared the others");
        // It enters once "a" has left: its time is up 2.8 s after that.
        let timer = m.timer().expect("a timer");
        assert_eq!(
            timer.deadline,
            t1 + TRANSITION + TRANSITION + Duration::from_millis(2800)
        );
    }

    #[test]
    fn only_the_showing_snack_bars_timer_takes_it_down() {
        let mut m = Messenger::default();
        let t0 = Instant::now();
        m.adopt(vec![bar("a", 100, false), bar("b", 4000, false)], t0);
        let first = m.timer().expect("a timer");
        m.adopt(vec![bar("c", 4000, false)], t0);
        assert_eq!(m.timer(), Some(first), "the one showing runs on");
        m.event(SnackEvent::Expired(first.seq + 1));
        assert_eq!(texts(&m), ["a", "b", "c"], "not its timer");
        m.event(SnackEvent::Expired(first.seq));
        assert_eq!(texts(&m), ["b", "c"]);
        let second = m.timer().expect("the next one's");
        assert_ne!(second, first);
        m.event(SnackEvent::Expired(first.seq));
        assert_eq!(texts(&m), ["b", "c"], "a stale timer takes down nothing");
    }

    #[test]
    fn the_subscription_carries_the_timer_and_frames_only_while_one_moves() {
        use iced::advanced::subscription::into_recipes;
        let mut m = Messenger::default();
        assert!(into_recipes(m.subscription()).is_empty(), "nothing up");
        // Up long enough to have entered: its timer alone.
        let long_ago = Instant::now() - Duration::from_secs(1);
        m.adopt(vec![bar("a", 60_000, false)], long_ago);
        assert_eq!(into_recipes(m.subscription()).len(), 1);
        // Leaving: the frames too.
        let seq = m.timer().expect("a timer").seq;
        m.event(SnackEvent::Expired(seq));
        assert_eq!(into_recipes(m.subscription()).len(), 1, "frames, no timer");
        m.event(SnackEvent::Frame(Instant::now() + TRANSITION));
        assert!(into_recipes(m.subscription()).is_empty(), "idle again");
    }

    #[test]
    fn a_snack_bar_swiped_away_goes_and_the_next_one_has_its_turn() {
        let mut m = Messenger::default();
        let long_ago = Instant::now() - Duration::from_secs(1);
        m.adopt(
            vec![bar("a", 60_000, false), bar("b", 4000, false)],
            long_ago,
        );
        let a = m.timer().expect("a's timer").seq;
        m.event(SnackEvent::Swiped(a));
        assert_eq!(texts(&m), ["b"]);
        assert_ne!(m.timer().map(|t| t.seq), Some(a), "b's timer now");
        // Its view reports the swipe as the app's message.
        let message = message(SnackEvent::Swiped(a));
        assert!(matches!(message, Message::SnackBarSwiped(n) if n == a));
    }
}
