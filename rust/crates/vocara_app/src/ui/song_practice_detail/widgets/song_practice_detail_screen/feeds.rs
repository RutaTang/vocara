//! The page's wake-ups from other threads, as subscriptions: a quit's
//! requests (the recording controller's waker, called on the quit's thread)
//! and the playback engine's events while the transport is paused.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use iced::futures::channel::mpsc::{unbounded, UnboundedSender};
use iced::futures::stream::{self, BoxStream, StreamExt};
use iced::Subscription;
use vocara_services::playback_engine::PlaybackEngine;

/// A wake-up any thread can ring; each subscription listening to it hears
/// every ring after it subscribed (and one at its start, so a ring before
/// the page listened is never lost: the screen's answer to a ring with
/// nothing waiting does nothing).
#[derive(Default)]
pub struct WakeFeed {
    listeners: Mutex<Vec<UnboundedSender<()>>>,
}

impl WakeFeed {
    fn listeners(&self) -> MutexGuard<'_, Vec<UnboundedSender<()>>> {
        self.listeners.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Rings every listener, forgetting the ones that are gone.
    pub fn wake(&self) {
        self.listeners()
            .retain(|listener| listener.unbounded_send(()).is_ok());
    }

    pub(super) fn subscribe(&self) -> BoxStream<'static, ()> {
        let (tx, rx) = unbounded();
        let _ = tx.unbounded_send(());
        self.listeners().push(tx);
        rx.boxed()
    }
}

/// A feed as a subscription key: one stream per feed.
struct WakeKey(Arc<WakeFeed>);

impl Hash for WakeKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

/// The rings of `feed`.
pub fn wakes(feed: &Arc<WakeFeed>) -> Subscription<()> {
    Subscription::run_with(WakeKey(Arc::clone(feed)), |key| key.0.subscribe())
}

/// An engine as a subscription key: one stream per engine.
pub struct EngineFeed(pub Arc<dyn PlaybackEngine>);

impl Hash for EngineFeed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as *const ()).hash(state);
    }
}

/// A ring for each position or playing event the engine adds (the view
/// model's own subscriptions hold the events; the screen drains them).
pub fn engine_events(feed: EngineFeed) -> Subscription<()> {
    Subscription::run_with(feed, |feed| {
        let positions = stream::unfold(feed.0.position(), |mut rx| async move {
            rx.recv().await.map(|_| ((), rx))
        });
        let playing = stream::unfold(feed.0.playing(), |mut rx| async move {
            rx.recv().await.map(|_| ((), rx))
        });
        stream::select(positions, playing).boxed()
    })
}
