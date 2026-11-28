//! The transport seam (port of `lib/data/services/playback_engine.dart`): what
//! the song page's view model drives, independent of the engine behind it. The
//! native graph engine (`graph_audio_playback_service`) implements it; tests
//! fake it, and [`NullPlaybackEngine`] is the silent stand-in.
//!
//! Dart's `Stream<Duration> position` and `Stream<bool> playing` were
//! broadcast streams: each listener got the events added after it subscribed,
//! in order, and a done event when the engine was disposed. [`Broadcast`]
//! keeps exactly that: [`PlaybackEngine::position`] and
//! [`PlaybackEngine::playing`] hand out a fresh [`Subscription`] per call, an
//! unbounded, lossless, ordered receiver usable from async code (an iced
//! subscription) or synchronously (`try_recv`, `blocking_recv`).

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx, TrackSend};

/// Playback spec for one track, derived from the domain model. `start_ms` is
/// the track's offset on the shared timeline; `duration_ms` its audio length.
/// A clip partly covered by one stacked above it on its lane plays as one
/// track per stretch of it that is heard (see
/// `vocara_model::models::audio_track::lane_visible_spans`): each starts
/// `src_offset_ms` into the file and lasts `duration_ms`, and fades in or out
/// where it is cut.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackPlayback {
    pub id: String,
    pub path: String,
    pub start_ms: i64,
    pub duration_ms: i64,
    /// Where in the file the track starts playing (0: its first frame).
    pub src_offset_ms: i64,
    /// Whether the track's start and end are cuts, where a clip stacked
    /// above takes over or lets go, and fade; a clip's own ends play as
    /// recorded, and an end that is not a cut plays to the file's end.
    pub fade_in: bool,
    pub fade_out: bool,
    pub volume: f64,
    pub muted: bool,
    /// Stereo pan in [-1, 1] (0 = center).
    pub pan: f64,
    /// Vocal takes get the effect chain (EQ/comp/reverb send); backing tracks
    /// stay dry.
    pub is_vocal: bool,
    /// The vocal take's effect settings (re-applied to the engine after a
    /// graph rebuild).
    pub fx: Option<TrackFx>,
    /// Output routing: the FX bus id this track feeds, or `None` = master.
    pub bus_id: Option<String>,
    /// Post-fader aux sends to FX buses.
    pub sends: Vec<TrackSend>,
    /// The lane (take) this clip belongs to: every clip of one lane plays
    /// through ONE engine chain, so a compressor or gate runs across its clip
    /// boundaries instead of restarting cold on each clip. `None`: the clip
    /// gets a chain of its own.
    pub lane_id: Option<String>,
}

impl TrackPlayback {
    /// `TrackPlayback(id:, path:)` with Dart's defaults: at 0, no length, the
    /// whole file, unity volume, unmuted, centered, backing (dry), no FX, to the
    /// master, no sends, a chain of its own.
    pub fn new(id: impl Into<String>, path: impl Into<String>) -> TrackPlayback {
        TrackPlayback {
            id: id.into(),
            path: path.into(),
            start_ms: 0,
            duration_ms: 0,
            src_offset_ms: 0,
            fade_in: false,
            fade_out: false,
            volume: 1.0,
            muted: false,
            pan: 0.0,
            is_vocal: false,
            fx: None,
            bus_id: None,
            sends: Vec::new(),
            lane_id: None,
        }
    }
}

/// Playback spec for one FX bus (group/aux), derived from the domain model.
#[derive(Clone, Debug, PartialEq)]
pub struct BusPlayback {
    pub id: String,
    /// The bus's insert chain (EQ + comp) and reverb send level.
    pub fx: TrackFx,
    pub volume: f64,
    pub muted: bool,
    /// Stereo pan in [-1, 1] (0 = center).
    pub pan: f64,
    /// Output routing: another bus's id, or `None` = master.
    pub output_bus_id: Option<String>,
    /// Post-fader aux sends to other FX buses.
    pub sends: Vec<TrackSend>,
}

impl BusPlayback {
    /// `BusPlayback(id:, fx:)` with Dart's defaults: unity volume, unmuted,
    /// centered, to the master, no sends.
    pub fn new(id: impl Into<String>, fx: TrackFx) -> BusPlayback {
        BusPlayback {
            id: id.into(),
            fx,
            volume: 1.0,
            muted: false,
            pan: 0.0,
            output_bus_id: None,
            sends: Vec::new(),
        }
    }
}

/// A listener's end of a [`Broadcast`]: the events added after it subscribed,
/// in order, none dropped; `None` from `recv` once the broadcast closed and
/// every earlier event was read. Dropping it unsubscribes.
pub type Subscription<T> = mpsc::UnboundedReceiver<T>;

/// A broadcast event source: Dart's `StreamController.broadcast()`. Events
/// reach every current subscriber in the order they were added (one lock
/// orders concurrent adds the same way for all), a late subscriber sees only
/// what follows its subscription, and [`Broadcast::close`] ends every
/// subscription after the events already added.
pub struct Broadcast<T> {
    state: Mutex<State<T>>,
}

struct State<T> {
    subscribers: Vec<mpsc::UnboundedSender<T>>,
    closed: bool,
}

impl<T: Clone> Broadcast<T> {
    pub fn new() -> Broadcast<T> {
        Broadcast {
            state: Mutex::new(State {
                subscribers: Vec::new(),
                closed: false,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new subscription. On a closed broadcast it is already at its end, as
    /// a listen on a closed Dart controller got only its done event.
    pub fn subscribe(&self) -> Subscription<T> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = self.state();
        if !state.closed {
            state.subscribers.push(tx);
        }
        rx
    }

    /// Delivers `event` to every current subscriber, forgetting the ones that
    /// were dropped. After [`Broadcast::close`] the event is dropped (Dart
    /// threw a `StateError` there; nothing reaches it, since every add sits
    /// behind the engine's availability check).
    pub fn add(&self, event: T) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.subscribers.retain(|s| s.send(event.clone()).is_ok());
    }

    /// Ends every subscription (after the events already added); later adds
    /// are dropped and later subscriptions start at their end. Idempotent.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state.subscribers.clear();
    }

    pub fn is_closed(&self) -> bool {
        self.state().closed
    }
}

impl<T: Clone> Default for Broadcast<T> {
    fn default() -> Broadcast<T> {
        Broadcast::new()
    }
}

/// The transport surface the song page's view model drives, independent of
/// the backing implementation. The native graph engine implements it; the
/// view model holds one made by the playback engine factory.
///
/// Every method takes `&self` and the trait is `Send + Sync`, so the engine is
/// shared (`Arc<dyn PlaybackEngine>`) between the UI thread, which makes the
/// live edits, and the worker running a [`PlaybackEngine::sync`]. Dart's
/// `Future`-returning methods are blocking here: `sync` decodes and rebuilds,
/// and runs off the UI thread; the rest return at once.
pub trait PlaybackEngine: Send + Sync {
    /// Timeline positions, for the playhead (a new subscription per call).
    fn position(&self) -> Subscription<Duration>;

    /// Whether the transport is running, on every change (a new subscription
    /// per call).
    fn playing(&self) -> Subscription<bool>;

    fn current_position(&self) -> Duration;

    fn is_playing(&self) -> bool;

    /// Sets the play range; while enabled, playback wraps at `end_ms` back to
    /// `start_ms`.
    fn set_loop(&self, enabled: bool, start_ms: i64, end_ms: i64);

    /// Reconciles the engine with the current track set and FX buses (paths,
    /// offsets, volumes, mutes, routing). A structural change (files or
    /// routing) rebuilds the graph; volume/mute-only changes are pushed live.
    /// Returns once this sync (and every one queued before it) is done.
    fn sync(&self, tracks: Vec<TrackPlayback>, buses: Vec<BusPlayback>);

    fn play(&self);

    fn pause(&self);

    fn seek(&self, position: Duration);

    /// Sets a track's level; `muted` forces silence regardless of `volume`.
    fn set_volume(&self, id: &str, volume: f64, muted: bool);

    /// Sets a track's stereo pan in [-1, 1] (0 = center); a no-op without a
    /// pan node.
    fn set_track_pan(&self, id: &str, pan: f64);

    /// Sets the master bus output gain (linear, 1.0 = unity): the overall mix
    /// level (a no-op on engines without a master bus).
    fn set_master_volume(&self, gain: f64);

    /// Whether this engine can apply EQ / compressor / reverb (only the native
    /// engine can).
    fn supports_effects(&self) -> bool;

    /// Applies a vocal track's effect chain immediately (a no-op on engines
    /// without effects).
    fn set_track_fx(&self, track_id: &str, fx: &TrackFx);

    /// Applies an FX bus's effect chain immediately (a no-op on engines without
    /// effects).
    fn set_bus_fx(&self, bus_id: &str, fx: &TrackFx);

    /// Sets an FX bus's fader level; `muted` forces silence (a no-op without
    /// buses).
    fn set_bus_volume(&self, bus_id: &str, volume: f64, muted: bool);

    /// Sets an FX bus's stereo pan in [-1, 1] (a no-op without buses).
    fn set_bus_pan(&self, bus_id: &str, pan: f64);

    /// The bus compressor's current gain reduction (dB, >= 0), polled for a
    /// meter.
    fn bus_gain_reduction_db(&self, bus_id: &str) -> f64;

    /// Sets a track's aux-send level (dB) for send `slot` (a no-op without
    /// sends).
    fn set_track_send_level(&self, track_id: &str, slot: usize, level_db: f64);

    /// Sets a bus's aux-send level (dB) for send `slot`.
    fn set_bus_send_level(&self, bus_id: &str, slot: usize, level_db: f64);

    /// Applies the shared reverb bus settings (a no-op on engines without
    /// effects).
    fn set_reverb(&self, reverb: &ReverbSettings);

    /// The vocal compressor's current gain reduction (dB, >= 0) for a track,
    /// polled for a meter; 0 on engines without effects.
    fn track_gain_reduction_db(&self, track_id: &str) -> f64;

    /// Releases the engine; the position and playing subscriptions end.
    fn dispose(&self);
}

/// The silent engine: what the native graph engine is when no output device
/// opened (`isAvailable == false`), every call a no-op. The playhead stays at
/// zero, the transport never runs and no event is ever emitted; the meters
/// read 0. It reports effects as supported, as the graph engine does whether
/// or not its device opened.
pub struct NullPlaybackEngine {
    position: Broadcast<Duration>,
    playing: Broadcast<bool>,
}

impl NullPlaybackEngine {
    pub fn new() -> NullPlaybackEngine {
        NullPlaybackEngine {
            position: Broadcast::new(),
            playing: Broadcast::new(),
        }
    }
}

impl Default for NullPlaybackEngine {
    fn default() -> NullPlaybackEngine {
        NullPlaybackEngine::new()
    }
}

impl PlaybackEngine for NullPlaybackEngine {
    fn position(&self) -> Subscription<Duration> {
        self.position.subscribe()
    }

    fn playing(&self) -> Subscription<bool> {
        self.playing.subscribe()
    }

    fn current_position(&self) -> Duration {
        Duration::ZERO
    }

    fn is_playing(&self) -> bool {
        false
    }

    fn set_loop(&self, _enabled: bool, _start_ms: i64, _end_ms: i64) {}

    fn sync(&self, _tracks: Vec<TrackPlayback>, _buses: Vec<BusPlayback>) {}

    fn play(&self) {}

    fn pause(&self) {}

    fn seek(&self, _position: Duration) {}

    fn set_volume(&self, _id: &str, _volume: f64, _muted: bool) {}

    fn set_track_pan(&self, _id: &str, _pan: f64) {}

    fn set_master_volume(&self, _gain: f64) {}

    fn supports_effects(&self) -> bool {
        true
    }

    fn set_track_fx(&self, _track_id: &str, _fx: &TrackFx) {}

    fn set_bus_fx(&self, _bus_id: &str, _fx: &TrackFx) {}

    fn set_bus_volume(&self, _bus_id: &str, _volume: f64, _muted: bool) {}

    fn set_bus_pan(&self, _bus_id: &str, _pan: f64) {}

    fn bus_gain_reduction_db(&self, _bus_id: &str) -> f64 {
        0.0
    }

    fn set_track_send_level(&self, _track_id: &str, _slot: usize, _level_db: f64) {}

    fn set_bus_send_level(&self, _bus_id: &str, _slot: usize, _level_db: f64) {}

    fn set_reverb(&self, _reverb: &ReverbSettings) {}

    fn track_gain_reduction_db(&self, _track_id: &str) -> f64 {
        0.0
    }

    fn dispose(&self) {
        self.position.close();
        self.playing.close();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use mpsc::error::TryRecvError;

    #[test]
    fn specs_default_as_in_dart() {
        let t = TrackPlayback::new("t1", "/a.wav");
        assert_eq!((t.id.as_str(), t.path.as_str()), ("t1", "/a.wav"));
        assert_eq!((t.start_ms, t.duration_ms), (0, 0));
        assert_eq!(
            (t.volume, t.muted, t.pan, t.is_vocal),
            (1.0, false, 0.0, false)
        );
        assert!(t.fx.is_none() && t.bus_id.is_none() && t.lane_id.is_none());
        assert!(t.sends.is_empty());
        let b = BusPlayback::new("b1", TrackFx::flat());
        assert_eq!(b.fx, TrackFx::flat());
        assert_eq!((b.volume, b.muted, b.pan), (1.0, false, 0.0));
        assert!(b.output_bus_id.is_none() && b.sends.is_empty());
    }

    #[test]
    fn a_broadcast_reaches_every_subscriber_in_order_from_its_subscription_on() {
        let b = Broadcast::new();
        let mut early = b.subscribe();
        b.add(1);
        let mut late = b.subscribe();
        b.add(2);
        b.add(3);
        assert_eq!(early.try_recv(), Ok(1));
        assert_eq!(early.try_recv(), Ok(2));
        assert_eq!(early.try_recv(), Ok(3));
        assert_eq!(late.try_recv(), Ok(2));
        assert_eq!(late.try_recv(), Ok(3));
        assert_eq!(late.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn closing_ends_subscriptions_after_their_pending_events() {
        let b = Broadcast::new();
        let mut s = b.subscribe();
        b.add(Duration::from_millis(5));
        b.close();
        b.add(Duration::from_millis(9));
        assert!(b.is_closed());
        assert_eq!(s.try_recv(), Ok(Duration::from_millis(5)));
        assert_eq!(s.try_recv(), Err(TryRecvError::Disconnected));
        // A subscription to a closed broadcast is already at its end.
        assert_eq!(b.subscribe().try_recv(), Err(TryRecvError::Disconnected));
        b.close();
    }

    #[test]
    fn a_dropped_subscription_is_forgotten() {
        let b = Broadcast::new();
        let gone = b.subscribe();
        let mut kept = b.subscribe();
        drop(gone);
        b.add(true);
        assert_eq!(b.state().subscribers.len(), 1);
        assert_eq!(kept.try_recv(), Ok(true));
    }

    #[test]
    fn concurrent_adds_reach_every_subscriber_in_one_order() {
        let b = Arc::new(Broadcast::new());
        let mut x = b.subscribe();
        let mut y = b.subscribe();
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let b = Arc::clone(&b);
                std::thread::spawn(move || {
                    for i in 0..250 {
                        b.add(w * 1000 + i);
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let drain = |s: &mut Subscription<i32>| {
            let mut v = Vec::new();
            while let Ok(e) = s.try_recv() {
                v.push(e);
            }
            v
        };
        let (xs, ys) = (drain(&mut x), drain(&mut y));
        assert_eq!(xs.len(), 1000);
        assert_eq!(xs, ys);
    }

    #[test]
    fn the_null_engine_is_silent_and_ends_its_streams_on_dispose() {
        let engine: Arc<dyn PlaybackEngine> = Arc::new(NullPlaybackEngine::new());
        let mut position = engine.position();
        let mut playing = engine.playing();
        engine.sync(
            vec![TrackPlayback::new("t", "/t.wav")],
            vec![BusPlayback::new("b", TrackFx::flat())],
        );
        engine.play();
        engine.seek(Duration::from_secs(3));
        engine.set_volume("t", 0.5, true);
        engine.set_track_fx("t", &TrackFx::flat());
        engine.set_reverb(&ReverbSettings::DEFAULT);
        assert_eq!(engine.current_position(), Duration::ZERO);
        assert!(!engine.is_playing());
        assert!(engine.supports_effects());
        assert_eq!(engine.bus_gain_reduction_db("b"), 0.0);
        assert_eq!(engine.track_gain_reduction_db("t"), 0.0);
        assert_eq!(position.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(playing.try_recv(), Err(TryRecvError::Empty));
        engine.dispose();
        assert_eq!(position.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(playing.try_recv(), Err(TryRecvError::Disconnected));
    }
}
