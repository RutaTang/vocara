//! The app's playback engine (port of
//! `lib/data/services/playback_engine_factory.dart`).

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::graph_audio_playback_service::GraphAudioPlaybackService;

/// How long an engine built ahead waits to be taken: one older than this is
/// let go (the outputs may have changed meanwhile) and a new one built.
const PREWARMED_FOR: Duration = Duration::from_secs(30);

/// How long [`create_playback_engine`] waits for an engine being built
/// ahead, at most, before it builds its own.
const PREWARM_WAIT: Duration = Duration::from_secs(5);

/// The engine built ahead of the page that will take it.
enum Prewarm {
    Empty,
    /// Being built on a thread of its own.
    Building,
    Ready(Arc<GraphAudioPlaybackService>, Instant),
}

static PREWARM: Mutex<Prewarm> = Mutex::new(Prewarm::Empty);
static PREWARMED: Condvar = Condvar::new();

fn prewarm_slot() -> MutexGuard<'static, Prewarm> {
    PREWARM.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts building the next page's engine on a thread of its own: building
/// one opens the default output and waits for it to come up, which takes a
/// moment, and longer for Bluetooth headphones waking up. Called as Home
/// opens a practice: the "Loading practice" hold covers the build, and the
/// page that follows takes the engine ([`create_playback_engine`]) instead
/// of building one on the UI thread as its transition begins. At most one
/// is built ahead at a time.
pub fn prewarm_playback_engine() {
    let mut slot = prewarm_slot();
    match &*slot {
        Prewarm::Building => return,
        Prewarm::Ready(_, at) if at.elapsed() < PREWARMED_FOR => return,
        _ => {}
    }
    // A stale one is let go here; its teardown runs on a thread of its own.
    *slot = Prewarm::Building;
    drop(slot);
    let built = std::thread::Builder::new()
        .name("vocara-engine-prewarm".into())
        .spawn(|| {
            let engine = Arc::new(GraphAudioPlaybackService::new());
            let mut slot = prewarm_slot();
            if matches!(*slot, Prewarm::Building) {
                *slot = Prewarm::Ready(engine, Instant::now());
            }
            PREWARMED.notify_all();
        });
    if built.is_err() {
        *prewarm_slot() = Prewarm::Empty;
        PREWARMED.notify_all();
    }
}

/// The engine built ahead, if one is ready and fresh, waiting (at most
/// [`PREWARM_WAIT`]) for one being built: never a second engine beside it.
fn take_prewarmed() -> Option<Arc<GraphAudioPlaybackService>> {
    let deadline = Instant::now() + PREWARM_WAIT;
    let mut slot = prewarm_slot();
    loop {
        match std::mem::replace(&mut *slot, Prewarm::Empty) {
            Prewarm::Empty => return None,
            Prewarm::Ready(engine, at) => {
                return (at.elapsed() < PREWARMED_FOR).then_some(engine);
            }
            Prewarm::Building => {
                *slot = Prewarm::Building;
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    // Still building: it is let go when it lands, and the
                    // caller builds its own.
                    *slot = Prewarm::Empty;
                    return None;
                }
                slot = PREWARMED
                    .wait_timeout(slot, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
}

/// Constructs the app's playback engine: the native real-time graph engine
/// ([`GraphAudioPlaybackService`]). If no output device opens, the engine
/// reports [`GraphAudioPlaybackService::is_available`] false and every call
/// no-ops, so the app stays usable (just silent) instead of failing.
///
/// Dart returned it typed as the `PlaybackEngine` interface and the song
/// page's view model checked `is GraphAudioPlaybackService` to reach the
/// device, mic and settings calls; the concrete `Arc` is returned here so the
/// view model keeps both views without a downcast (it coerces to
/// `Arc<dyn PlaybackEngine>` where only the transport is wanted).
///
/// The engine [`prewarm_playback_engine`] built ahead, when there is one.
pub fn create_playback_engine() -> Arc<GraphAudioPlaybackService> {
    take_prewarmed().unwrap_or_else(|| Arc::new(GraphAudioPlaybackService::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback_engine::PlaybackEngine;

    /// The tests here share the process's slot: one at a time.
    static SLOT: Mutex<()> = Mutex::new(());

    fn slot_to_myself() -> MutexGuard<'static, ()> {
        SLOT.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn the_factory_makes_the_graph_engine_behind_the_seam() {
        let _slot = slot_to_myself();
        let engine = create_playback_engine();
        let seam: Arc<dyn PlaybackEngine> = engine.clone();
        assert!(seam.supports_effects());
        // Without the device feature there is no engine: silent, not failing.
        #[cfg(not(feature = "device"))]
        assert!(!engine.is_available());
        seam.dispose();
    }

    /// One test, as the slot is the process's: an engine built ahead is the
    /// one the next page takes, waited for while it builds, and taken once.
    #[test]
    fn an_engine_built_ahead_is_taken_once_and_never_built_twice() {
        let _slot = slot_to_myself();
        prewarm_playback_engine();
        prewarm_playback_engine(); // already building or built: nothing more
        let taken = take_prewarmed().expect("the engine built ahead");
        assert!(take_prewarmed().is_none(), "taken once");
        assert!(matches!(*prewarm_slot(), Prewarm::Empty));
        // With none built ahead, the factory builds one, and leaves the slot empty.
        let built = create_playback_engine();
        assert!(!Arc::ptr_eq(&taken, &built));
        assert!(matches!(*prewarm_slot(), Prewarm::Empty));
        // A stale one is let go, not handed over.
        *prewarm_slot() = Prewarm::Ready(
            Arc::new(GraphAudioPlaybackService::new()),
            Instant::now() - PREWARMED_FOR - Duration::from_secs(1),
        );
        assert!(take_prewarmed().is_none());
        taken.dispose();
        built.dispose();
    }
}
