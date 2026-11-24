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
//! they come and go, as in Dart's single queue.
//!
//! # Differences from Flutter
//!
//! The app draws its snack bar over the whole page, so above what a page
//! draws over itself in place of a Dart dialog route (Home's "Loading
//! practice" dialog, the song page's note editor and technique picker),
//! which covered a snack bar in Dart; the studio dialogs still cover it. No
//! Dart snack bar had an action, so none is carried over. A snack bar comes
//! and goes at once (Flutter slid it in and out).

use std::collections::VecDeque;
use std::time::Duration;

use iced::{Element, Subscription};

use super::{Message, Page};
use crate::router::Router;
use crate::ui::{self, SnackBar};

/// The timer of the snack bar the app shows: its number and the time it
/// has left. The subscription's identity, so the next one starts its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnackTimer {
    seq: u64,
    /// How long the snack bar has left to show.
    pub left: Duration,
}

impl SnackTimer {
    /// What the timer sends when the time is up.
    pub fn message(self) -> Message {
        Message::SnackBarExpired(self.seq)
    }
}

/// The app's snack bars: the first shows, the rest wait their turn.
#[derive(Debug, Default)]
pub(super) struct Messenger {
    queue: VecDeque<(u64, SnackBar)>,
    /// Numbers the snack bars, so a stale timer takes down nothing.
    made: u64,
}

impl Messenger {
    /// The snack bar showing, if any.
    pub fn showing(&self) -> Option<&SnackBar> {
        self.queue.front().map(|(_, bar)| bar)
    }

    /// The showing snack bar's timer.
    pub fn timer(&self) -> Option<SnackTimer> {
        self.queue.front().map(|(seq, bar)| SnackTimer {
            seq: *seq,
            left: bar.left,
        })
    }

    /// Takes in snack bars a page handed over, in order: one its page
    /// cleared the others for takes their place, the others wait behind.
    pub fn adopt(&mut self, bars: Vec<SnackBar>) {
        for bar in bars {
            if bar.clears {
                self.queue.clear();
            }
            self.made += 1;
            self.queue.push_back((self.made, bar));
        }
    }

    /// Takes over the snack bars of every page that is not on top, and of
    /// the one on top while the app shows one already (see the module
    /// docs).
    pub fn gather(&mut self, router: &mut Router<Page>) {
        let top = router.top().0;
        for (id, page) in router.pages_mut() {
            if id != top || !self.queue.is_empty() {
                self.adopt(page.take_snack_bars());
            }
        }
    }

    /// Snack bar `seq`'s time is up: the next one shows.
    pub fn expired(&mut self, seq: u64) {
        if self.queue.front().is_some_and(|(s, _)| *s == seq) {
            self.queue.pop_front();
        }
    }

    /// The snack bar showing, as its page drew it.
    pub fn view(&self) -> Option<Element<'_, Message>> {
        self.showing()
            .map(|bar| (bar.look)(&bar.text).map(|never| match never {}))
    }

    /// Its timer, while one shows.
    pub fn subscription(&self) -> Subscription<Message> {
        self.timer().map_or_else(Subscription::none, |timer| {
            Subscription::run_with(timer, snack_timer)
        })
    }
}

/// The showing snack bar's one-shot timer.
fn snack_timer(timer: &SnackTimer) -> impl iced::futures::Stream<Item = Message> {
    ui::one_shot(timer.left, timer.message())
}

#[cfg(test)]
mod tests {
    use iced::futures::StreamExt;

    use super::*;

    fn look(text: &str) -> Element<'_, std::convert::Infallible> {
        iced::widget::text(text).into()
    }

    fn bar(text: &str, left_ms: u64, clears: bool) -> SnackBar {
        SnackBar::waiting(
            text.to_string(),
            Duration::from_millis(left_ms),
            clears,
            look,
        )
    }

    fn texts(m: &Messenger) -> Vec<&str> {
        m.queue.iter().map(|(_, b)| b.text.as_str()).collect()
    }

    #[test]
    fn a_clearing_snack_bar_takes_the_place_of_those_up_and_a_queued_one_waits() {
        let mut m = Messenger::default();
        m.adopt(vec![bar("a", 100, false), bar("b", 4000, false)]);
        assert_eq!(texts(&m), ["a", "b"]);
        m.adopt(vec![bar("c", 4000, false)]);
        assert_eq!(texts(&m), ["a", "b", "c"], "queued behind");
        m.adopt(vec![bar("d", 2800, true)]);
        assert_eq!(texts(&m), ["d"], "cleared the others");
        assert_eq!(m.timer().map(|t| t.left), Some(Duration::from_millis(2800)));
    }

    #[test]
    fn only_the_showing_snack_bars_timer_takes_it_down() {
        let mut m = Messenger::default();
        m.adopt(vec![bar("a", 100, false), bar("b", 4000, false)]);
        let first = m.timer().expect("a timer");
        m.adopt(vec![bar("c", 4000, false)]);
        assert_eq!(m.timer(), Some(first), "the one showing runs on");
        m.expired(first.seq + 1);
        assert_eq!(texts(&m), ["a", "b", "c"], "not its timer");
        m.expired(first.seq);
        assert_eq!(texts(&m), ["b", "c"]);
        let second = m.timer().expect("the next one's");
        assert_ne!(second, first);
        assert_eq!(second.left, Duration::from_millis(4000), "all its time");
        m.expired(first.seq);
        assert_eq!(texts(&m), ["b", "c"], "a stale timer takes down nothing");
    }

    #[test]
    fn the_timer_sends_once_its_time_is_up_then_nothing() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a runtime");
        let timer = SnackTimer {
            seq: 7,
            left: Duration::from_millis(60),
        };
        rt.block_on(async {
            let started = std::time::Instant::now();
            let mut stream = std::pin::pin!(snack_timer(&timer));
            assert!(matches!(
                stream.next().await,
                Some(Message::SnackBarExpired(7))
            ));
            assert!(started.elapsed() >= timer.left);
            let more = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
            assert!(more.is_err(), "then nothing more");
        });
    }
}
