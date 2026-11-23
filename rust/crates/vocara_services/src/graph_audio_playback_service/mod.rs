//! The native real-time audio engine behind the [`PlaybackEngine`] seam:
//! the port of `lib/data/services/graph_audio_playback_service.dart`'s
//! `GraphAudioPlaybackService` and `SyncChain`. The file's top level (the
//! decode policy, the probe, the device lists) is [`crate::engine::media`]
//! and [`crate::engine`].
//!
//! [`GraphAudioPlaybackService::sync`] commits one timeline-aligned source per
//! track; a file's decode runs with the service released (no UI freeze) and is
//! cached by the engine, so the graph only rebuilds when the topology changes.
//! Volume, mute and the A/B toggle are instant per-track gain changes. The
//! playhead is read from the engine's atomic frame counter. Besides the
//! transport, the service is the song page's door to the engine's devices,
//! monitor, capture and recording (the mic recording controller and the
//! settings drive those).
//!
//! # Threading
//!
//! Dart ran all of this on one isolate: a sync job's code between two awaits
//! ran without anything else interleaving, and the awaits (the staging
//! decode, which ran on another isolate) were where a live fader move, a
//! transport call or the page closing got in. The port keeps exactly those
//! interleavings:
//!
//! - **The state lock is the isolate.** One mutex holds what the Dart object
//!   owned: the committed index maps, the live-edit log, the committed
//!   signature, the latest model, the shared reverb and master gain, the
//!   transport flag and the position poll's generation. Every method that
//!   touches them holds it for its whole body, and a sync job holds it for
//!   each of its synchronous segments, releasing it only around the staging
//!   decode (Dart's `await _decode(...)`). So a live edit lands before a
//!   job's segment or after it, never inside one, and a dispose made during a
//!   decode is seen by the job when it takes the lock back, as Dart's
//!   `_available` check saw it.
//! - **Syncs run on the [`SyncChain`]'s worker thread**, one at a time, in the
//!   order they were enqueued. [`GraphAudioPlaybackService::enqueue_sync`]
//!   records the latest model and its live-edit watermark and appends the job
//!   in one step (under a lock of its own, so concurrent callers cannot
//!   enqueue in one order and record in the other), which is what Dart's
//!   synchronous `sync()` prologue guaranteed. A degraded commit appends its
//!   retry from inside the running job, so it lands after every sync queued
//!   before that point. [`PlaybackEngine::sync`] enqueues and waits; it is
//!   blocking and belongs off the UI thread.
//! - **The position poll is a thread per play**: every 16 ms (Dart's
//!   `Timer.periodic`) it takes the state lock, checks it is still the
//!   current poll, and emits the playhead. Pause, a new play and dispose bump
//!   the generation and wake it, and it ends without another emission, as a
//!   cancelled timer fired no more. A seek emits its position under the same
//!   lock, so a tick read before the seek can never land after it.
//! - **The device, monitor, meter and recording calls do not take the state
//!   lock**: they touch none of that state, only the engine, whose own mutex
//!   orders them against every other engine call (see [`crate::engine`]).
//!   They check availability first, as Dart's did; one racing a dispose
//!   reaches an engine that already answers as unavailable.
//!
//! Lock order: the enqueue lock, then the state lock, then the chain's queue
//! lock; the broadcasts' locks are leaves. Nothing takes them the other way.

mod sync_chain;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_fx::{FxKind, ReverbSettings, TrackFx, TrackSend};

use crate::engine::media::{self, MediaError};
use crate::engine::{self, events, EngineBindings, Pcm};
use crate::playback_engine::{Broadcast, BusPlayback, PlaybackEngine, Subscription, TrackPlayback};

pub use sync_chain::{SyncChain, SyncTicket};

/// The staging decode: `path` to interleaved-stereo PCM at the engine's rate,
/// within the playback budgets ([`media::decode_for_playback`] in production).
/// `Ok(None)` is a file that decoded to nothing (skipped); a [`MediaError`]
/// refusal skips the track too; anything else fails the sync. Injectable so
/// the sync pipeline's skip, degraded and retry policies run without the
/// native engine or real media files.
pub type StagingDecode = Arc<dyn Fn(&str, u32) -> Result<Option<Pcm>, MediaError> + Send + Sync>;

/// Why a sync job failed: what the Dart `sync()` future completed with when
/// `_doSync` threw. Every other outcome (a skipped track, a rejected
/// topology, a disposed engine) is a successful sync.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    /// A staging decode failed with something that is not a refusal (no
    /// engine to decode with): Dart caught only the refusals, and this
    /// escaped the sync as a programming error.
    #[error("a staging decode failed: {0}")]
    Decode(MediaError),
    /// The job panicked: a programming error (or a failing fake in a test).
    /// A commit staged before the panic was still published.
    #[error("the sync failed: {0}")]
    Panicked(String),
}

/// A queued sync, to wait for (Dart's `sync()` future).
pub struct SyncHandle(SyncTicket<Result<(), SyncError>>);

impl SyncHandle {
    /// Blocks until this sync (and every one queued before it) is done.
    pub fn wait(self) -> Result<(), SyncError> {
        match self.0.wait() {
            Ok(outcome) => outcome,
            Err(payload) => Err(SyncError::Panicked(sync_chain::panic_message(&*payload))),
        }
    }
}

/// The position poll's period while playing (Dart's 16 ms `Timer.periodic`).
const POSITION_POLL_PERIOD: Duration = Duration::from_millis(16);

/// Frames pulled per [`GraphAudioPlaybackService::pull_input_frames`] call.
const INPUT_PULL_FRAMES: usize = 4096;

/// The rate reported before (or without) an engine.
const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;

/// The native engine (`vocara_engine`) behind the [`PlaybackEngine`]
/// interface. See the module doc for what it does and its threading.
pub struct GraphAudioPlaybackService {
    shared: Arc<Shared>,
}

/// Everything the sync worker and the position poll share with the service.
struct Shared {
    /// The engine; `None` when none could be created.
    engine: Option<Arc<dyn EngineBindings>>,
    /// The staging decode ([`media::decode_for_playback`]; injectable in tests).
    decode: StagingDecode,
    sample_rate: f64,
    /// Whether the native engine started (a device was opened) and is not
    /// disposed. Written only under the state lock.
    available: AtomicBool,
    state: Mutex<State>,
    /// Wakes a sleeping position poll that was cancelled (with `state`).
    poll_wake: Condvar,
    chain: SyncChain,
    /// Held while a sync records its model and joins the chain.
    enqueue: Mutex<()>,
    position: Broadcast<Duration>,
    playing: Broadcast<bool>,
    /// Reusable buffer for pulling captured frames (interleaved stereo).
    input_pull: Mutex<Vec<f32>>,
}

/// What the Dart object owned (see the module doc).
struct State {
    is_playing: bool,
    /// The committed graph's topology signature (paths, routing, bus set); a
    /// change triggers a rebuild, otherwise a sync only pushes live values.
    committed_sig: String,
    track_index: HashMap<String, i32>,
    /// Each committed track's chain owner: its own index, or for a clip that
    /// plays through an earlier clip's chain (see [`TrackPlayback::lane_id`])
    /// that leader's. Pan, FX and sends address the chain whichever clip they
    /// are sent for.
    chain_owner: HashMap<String, i32>,
    /// What each chain last received per setter since the last full push
    /// ([`State::push_live_params`]). The mixer sends a lane's value to every
    /// one of its clips, all onto one chain: a copy per clip filled the
    /// engine's command ring on a many-clip lane, and each FX drag tick then
    /// reached the audio a few blocks late, holding every other live edit up
    /// behind it. Deduplicated per chain rather than by skipping followers,
    /// so the lane is still reached when its leader clip has left the model
    /// (a delete still saving) but not yet the graph that plays.
    chain_sent: HashMap<(ChainKind, i32), Sent>,
    /// Each FX bus id's index into the committed graph (live bus FX, volume,
    /// meter).
    bus_index: HashMap<String, i32>,
    /// Each live setter's newest value, stamped with `live_seq`. A sync job
    /// pushes the model it was handed, which is older than any edit made
    /// while the job waited on the chain or on a decode: the edit was heard
    /// at once on the old graph, then the job's push put the old value back.
    /// An A/B backing flip during the page-open decode stayed wrong for the
    /// session, since nothing saves or resyncs after it. Each job re-applies
    /// the edits newer than its snapshot right after its own push
    /// ([`State::replay_live_edits`]). In the order the edits were made.
    live_seq: u64,
    live_edits: Vec<(u64, LiveEdit)>,
    /// The `live_seq` the model in `latest_tracks` / `latest_buses` carries.
    latest_as_of: u64,
    /// The shared reverb settings, re-applied after every graph rebuild (a
    /// rebuild resets nodes).
    reverb: Option<ReverbSettings>,
    /// The master bus gain, re-applied after every graph rebuild.
    master_gain: f64,
    /// The model most recently handed to a sync: what the degraded retry
    /// rebuilds from. A sync enqueued while the degraded job runs lands on
    /// the chain BEFORE the retry, so the retry must read the model when IT
    /// runs; replaying the degraded job's own lists would re-commit a
    /// topology that newer sync already replaced.
    latest_tracks: Arc<Vec<TrackPlayback>>,
    latest_buses: Arc<Vec<BusPlayback>>,
    /// The current position poll; bumped to cancel it.
    poll_gen: u64,
}

impl State {
    fn new() -> State {
        State {
            is_playing: false,
            committed_sig: String::new(),
            track_index: HashMap::new(),
            chain_owner: HashMap::new(),
            chain_sent: HashMap::new(),
            bus_index: HashMap::new(),
            live_seq: 0,
            live_edits: Vec::new(),
            latest_as_of: 0,
            reverb: None,
            master_gain: 1.0,
            latest_tracks: Arc::new(Vec::new()),
            latest_buses: Arc::new(Vec::new()),
            poll_gen: 0,
        }
    }
}

/// A chain-level setter, per committed chain owner (see `State::chain_sent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ChainKind {
    Pan,
    Fx,
    Send(usize),
}

/// The value a chain last received.
#[derive(Clone, Debug)]
enum Sent {
    Level(f64),
    Fx(Box<TrackFx>),
}

/// A value about to be sent, compared against what was.
#[derive(Clone, Copy)]
enum SentRef<'a> {
    Level(f64),
    Fx(&'a TrackFx),
}

impl SentRef<'_> {
    /// Dart's `identical(sent, value) || sent == value`: the FX the chain
    /// already got (the mixer hands every clip of a lane the same object, and
    /// `TrackFx` equality is by value), or the same level (Dart's `identical`
    /// on doubles compares bits, so a NaN matches itself too).
    fn was_sent(self, sent: &Sent) -> bool {
        match (self, sent) {
            (SentRef::Level(v), Sent::Level(s)) => v == *s || v.to_bits() == s.to_bits(),
            (SentRef::Fx(v), Sent::Fx(s)) => v == &**s,
            _ => false,
        }
    }

    fn stored(self) -> Sent {
        match self {
            SentRef::Level(v) => Sent::Level(v),
            SentRef::Fx(fx) => Sent::Fx(Box::new(fx.clone())),
        }
    }
}

/// One live setter call, as recorded for replay (Dart's `_live` closure).
#[derive(Clone, Debug)]
enum LiveEdit {
    Volume {
        id: String,
        volume: f64,
        muted: bool,
    },
    Pan {
        id: String,
        pan: f64,
    },
    Fx {
        id: String,
        fx: TrackFx,
    },
    BusFx {
        id: String,
        fx: TrackFx,
    },
    BusVolume {
        id: String,
        volume: f64,
        muted: bool,
    },
    BusPan {
        id: String,
        pan: f64,
    },
    Send {
        id: String,
        slot: usize,
        level_db: f64,
    },
    BusSend {
        id: String,
        slot: usize,
        level_db: f64,
    },
}

/// A live edit's key: one entry per parameter.
#[derive(PartialEq, Eq)]
enum LiveKey<'a> {
    Volume(&'a str),
    Pan(&'a str),
    Fx(&'a str),
    BusFx(&'a str),
    BusVolume(&'a str),
    BusPan(&'a str),
    Send(&'a str, usize),
    BusSend(&'a str, usize),
}

impl LiveEdit {
    fn key(&self) -> LiveKey<'_> {
        match self {
            LiveEdit::Volume { id, .. } => LiveKey::Volume(id),
            LiveEdit::Pan { id, .. } => LiveKey::Pan(id),
            LiveEdit::Fx { id, .. } => LiveKey::Fx(id),
            LiveEdit::BusFx { id, .. } => LiveKey::BusFx(id),
            LiveEdit::BusVolume { id, .. } => LiveKey::BusVolume(id),
            LiveEdit::BusPan { id, .. } => LiveKey::BusPan(id),
            LiveEdit::Send { id, slot, .. } => LiveKey::Send(id, *slot),
            LiveEdit::BusSend { id, slot, .. } => LiveKey::BusSend(id, *slot),
        }
    }
}

/// A send slot as the engine takes it.
fn slot_index(slot: usize) -> i32 {
    i32::try_from(slot).unwrap_or(i32::MAX)
}

impl State {
    /// Applies a live edit now and records it (one entry per parameter), for
    /// the sync jobs whose snapshot predates it.
    fn live(&mut self, engine: &dyn EngineBindings, edit: LiveEdit) {
        self.apply_live(engine, &edit);
        // Re-inserted, so the log replays in the order the edits were made:
        // two clips of a lane share a chain, and the newer value must land
        // last.
        self.live_edits.retain(|(_, e)| e.key() != edit.key());
        self.live_seq += 1;
        self.live_edits.push((self.live_seq, edit));
    }

    /// Re-applies the live edits newer than a job's snapshot `as_of`, after
    /// the job pushed that snapshot. The older ones are in it and in every
    /// later job's (jobs run in the order they were queued), so they are
    /// dropped.
    fn replay_live_edits(&mut self, engine: &dyn EngineBindings, as_of: u64) {
        self.live_edits.retain(|(seq, _)| *seq > as_of);
        let edits = std::mem::take(&mut self.live_edits);
        for (_, edit) in &edits {
            self.apply_live(engine, edit);
        }
        self.live_edits = edits;
    }

    /// One live edit against the committed graph as it stands.
    fn apply_live(&mut self, engine: &dyn EngineBindings, edit: &LiveEdit) {
        match edit {
            LiveEdit::Volume { id, volume, muted } => {
                if let Some(&idx) = self.track_index.get(id) {
                    let _ = engine.set_track_volume(idx, *volume as f32, *muted);
                }
            }
            LiveEdit::Pan { id, pan } => {
                if let Some(idx) = self.chain_index(id, ChainKind::Pan, SentRef::Level(*pan)) {
                    let _ = engine.set_pan(idx, *pan as f32);
                }
            }
            LiveEdit::Fx { id, fx } => {
                if let Some(idx) = self.chain_index(id, ChainKind::Fx, SentRef::Fx(fx)) {
                    apply_fx_to_index(engine, idx, fx);
                }
            }
            LiveEdit::BusFx { id, fx } => {
                if let Some(&idx) = self.bus_index.get(id) {
                    apply_bus_fx(engine, idx, fx);
                }
            }
            LiveEdit::BusVolume { id, volume, muted } => {
                if let Some(&idx) = self.bus_index.get(id) {
                    let _ = engine.set_bus_volume(idx, *volume as f32, *muted);
                }
            }
            LiveEdit::BusPan { id, pan } => {
                if let Some(&idx) = self.bus_index.get(id) {
                    let _ = engine.set_bus_pan(idx, *pan as f32);
                }
            }
            LiveEdit::Send { id, slot, level_db } => {
                let kind = ChainKind::Send(*slot);
                if let Some(idx) = self.chain_index(id, kind, SentRef::Level(*level_db)) {
                    let _ = engine.set_send_level(idx, slot_index(*slot), *level_db as f32);
                }
            }
            LiveEdit::BusSend { id, slot, level_db } => {
                if let Some(&idx) = self.bus_index.get(id) {
                    let _ = engine.set_bus_send_level(idx, slot_index(*slot), *level_db as f32);
                }
            }
        }
    }

    /// `track_id`'s committed index, when `value` is new for the chain it
    /// plays through under `kind` (see `chain_sent`); `None` otherwise.
    fn chain_index(&mut self, track_id: &str, kind: ChainKind, value: SentRef<'_>) -> Option<i32> {
        let idx = *self.track_index.get(track_id)?;
        let owner = *self.chain_owner.get(track_id)?;
        let key = (kind, owner);
        if self
            .chain_sent
            .get(&key)
            .is_some_and(|sent| value.was_sent(sent))
        {
            return None;
        }
        self.chain_sent.insert(key, value.stored());
        Some(idx)
    }

    /// Pushes every parameter the engine can change WITHOUT rebuilding the
    /// graph: track and bus volume, mute, pan, insert-FX settings, aux send
    /// levels, the shared reverb and the master gain. Idempotent, and
    /// deliberately the single definition of "what a live parameter is":
    /// both the post-commit re-apply (a rebuild resets nodes to defaults) and
    /// the no-topology-change fast path call it, so the two can never drift
    /// apart again.
    ///
    /// Indices come from the committed maps, so a track that failed to stage
    /// is simply skipped.
    fn push_live_params(
        &mut self,
        engine: &dyn EngineBindings,
        tracks: &[TrackPlayback],
        buses: &[BusPlayback],
    ) {
        self.chain_sent.clear();
        for t in tracks {
            let Some(&idx) = self.track_index.get(&t.id) else {
                continue;
            };
            let _ = engine.set_track_volume(idx, t.volume as f32, t.muted);
            // The rest is the lane chain's, set through its leader (always in
            // the model a graph was built from).
            if self.chain_owner.get(&t.id) != Some(&idx) {
                continue;
            }
            let _ = engine.set_pan(idx, t.pan as f32);
            if let Some(fx) = &t.fx {
                apply_fx_to_index(engine, idx, fx);
            }
            for (s, send) in t.sends.iter().enumerate() {
                let _ = engine.set_send_level(idx, slot_index(s), send.level_db as f32);
            }
        }
        for b in buses {
            let Some(&idx) = self.bus_index.get(&b.id) else {
                continue;
            };
            let _ = engine.set_bus_volume(idx, b.volume as f32, b.muted);
            let _ = engine.set_bus_pan(idx, b.pan as f32);
            apply_bus_fx(engine, idx, &b.fx);
            for (s, send) in b.sends.iter().enumerate() {
                let _ = engine.set_bus_send_level(idx, slot_index(s), send.level_db as f32);
            }
        }
        if let Some(r) = &self.reverb {
            apply_reverb(engine, r);
        }
        let _ = engine.set_master(self.master_gain as f32);
    }
}

/// Pushes a vocal track's effect chain to its committed index.
fn apply_fx_to_index(engine: &dyn EngineBindings, index: i32, fx: &TrackFx) {
    for (i, b) in fx.eq.iter().enumerate() {
        let _ = engine.set_eq_band(
            index,
            slot_index(i),
            b.freq as f32,
            b.q as f32,
            b.gain_db as f32,
            fx.eq_on && b.enabled,
        );
    }
    let c = &fx.comp;
    let _ = engine.set_comp(
        index,
        c.threshold_db as f32,
        c.ratio as f32,
        c.knee_db as f32,
        c.attack_ms as f32,
        c.release_ms as f32,
        c.makeup_db as f32,
        !fx.comp_on, // bypass when the compressor is off
    );
    let _ = engine.set_send(index, fx.send_db as f32);
    let de = &fx.de_esser;
    let _ = engine.set_deesser(
        index,
        de.freq as f32,
        de.threshold_db as f32,
        de.range_db as f32,
        !fx.de_esser_on,
    );
    let g = &fx.gate;
    let _ = engine.set_gate(
        index,
        g.threshold_db as f32,
        g.range_db as f32,
        g.attack_ms as f32,
        g.release_ms as f32,
        !fx.gate_on,
    );
    let dl = &fx.delay;
    let _ = engine.set_delay(
        index,
        dl.time_ms as f32,
        dl.feedback as f32,
        dl.mix as f32,
        !fx.delay_on,
    );
    let sa = &fx.saturation;
    let _ = engine.set_saturation(index, sa.drive_db as f32, sa.mix as f32, !fx.saturation_on);
    let db = &fx.doubler;
    let _ = engine.set_doubler(
        index,
        db.delay_ms as f32,
        db.depth_ms as f32,
        db.rate_hz as f32,
        db.mix as f32,
        !fx.doubler_on,
    );
    let lm = &fx.limiter;
    let _ = engine.set_limiter(
        index,
        lm.ceiling_db as f32,
        lm.release_ms as f32,
        !fx.limiter_on,
    );
    let rv = &fx.reverb;
    let _ = engine.set_reverb_insert(
        index,
        rv.room_size as f32,
        rv.damping as f32,
        if fx.reverb_on { rv.wet as f32 } else { 0.0 },
        rv.width as f32,
    );
}

/// Pushes an FX bus's effect chain to its committed bus index (mirrors
/// [`apply_fx_to_index`] but addresses bus nodes).
fn apply_bus_fx(engine: &dyn EngineBindings, index: i32, fx: &TrackFx) {
    for (i, b) in fx.eq.iter().enumerate() {
        let _ = engine.set_bus_eq_band(
            index,
            slot_index(i),
            b.freq as f32,
            b.q as f32,
            b.gain_db as f32,
            fx.eq_on && b.enabled,
        );
    }
    let c = &fx.comp;
    let _ = engine.set_bus_comp(
        index,
        c.threshold_db as f32,
        c.ratio as f32,
        c.knee_db as f32,
        c.attack_ms as f32,
        c.release_ms as f32,
        c.makeup_db as f32,
        !fx.comp_on,
    );
    let _ = engine.set_bus_send(index, fx.send_db as f32);
    let de = &fx.de_esser;
    let _ = engine.set_bus_deesser(
        index,
        de.freq as f32,
        de.threshold_db as f32,
        de.range_db as f32,
        !fx.de_esser_on,
    );
    let g = &fx.gate;
    let _ = engine.set_bus_gate(
        index,
        g.threshold_db as f32,
        g.range_db as f32,
        g.attack_ms as f32,
        g.release_ms as f32,
        !fx.gate_on,
    );
    let dl = &fx.delay;
    let _ = engine.set_bus_delay(
        index,
        dl.time_ms as f32,
        dl.feedback as f32,
        dl.mix as f32,
        !fx.delay_on,
    );
    let sa = &fx.saturation;
    let _ = engine.set_bus_saturation(index, sa.drive_db as f32, sa.mix as f32, !fx.saturation_on);
    let db = &fx.doubler;
    let _ = engine.set_bus_doubler(
        index,
        db.delay_ms as f32,
        db.depth_ms as f32,
        db.rate_hz as f32,
        db.mix as f32,
        !fx.doubler_on,
    );
    let lm = &fx.limiter;
    let _ = engine.set_bus_limiter(
        index,
        lm.ceiling_db as f32,
        lm.release_ms as f32,
        !fx.limiter_on,
    );
    let rv = &fx.reverb;
    let _ = engine.set_bus_reverb_insert(
        index,
        rv.room_size as f32,
        rv.damping as f32,
        if fx.reverb_on { rv.wet as f32 } else { 0.0 },
        rv.width as f32,
    );
}

fn apply_reverb(engine: &dyn EngineBindings, r: &ReverbSettings) {
    // dry = 0: the bus is a wet-only send return.
    let _ = engine.set_reverb(
        r.room_size as f32,
        r.damping as f32,
        r.wet as f32,
        0.0,
        r.width as f32,
    );
}

/// Pushes a track or bus insert-chain order to the engine (after add or
/// begin, before commit). An empty order is the engine default (EQ, Comp).
fn push_chain(engine: &dyn EngineBindings, idx: i32, fx: Option<&TrackFx>, bus: bool) {
    let order: &[FxKind] = fx.map_or(&[], |f| &f.order);
    if order.is_empty() {
        return;
    }
    let tags: Vec<u8> = order.iter().map(|k| k.index() as u8).collect();
    let _ = if bus {
        engine.bus_chain(idx, &tags)
    } else {
        engine.track_chain(idx, &tags)
    };
}

/// Stages a track or bus's aux sends (target bus index, level dB) before
/// commit.
fn push_sends(
    engine: &dyn EngineBindings,
    idx: i32,
    sends: &[TrackSend],
    bus_index: &HashMap<String, i32>,
    bus: bool,
) {
    if sends.is_empty() {
        return;
    }
    let pairs: Vec<(i32, f32)> = sends
        .iter()
        .map(|s| {
            (
                bus_index.get(&s.bus_id).copied().unwrap_or(-1),
                s.level_db as f32,
            )
        })
        .collect();
    let _ = if bus {
        engine.bus_sends(idx, &pairs)
    } else {
        engine.track_sends(idx, &pairs)
    };
}

/// The insert order as part of the topology: the kinds' engine tags.
fn order_tag(fx: Option<&TrackFx>) -> String {
    fx.map_or_else(String::new, |f| {
        f.order
            .iter()
            .map(|k| k.index().to_string())
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// Send targets (not levels) drive the graph wiring; level changes are live.
fn sends_tag(sends: &[TrackSend]) -> String {
    sends
        .iter()
        .map(|s| s.bus_id.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// The graph topology that forces a rebuild: each track's file and output
/// routing, and each bus's id and output routing. Volumes, mutes and FX are
/// excluded (pushed live), so dragging a fader never rebuilds the graph.
fn topology_sig(tracks: &[TrackPlayback], buses: &[BusPlayback]) -> String {
    let mut b = String::new();
    for t in tracks {
        b.push_str(&t.path);
        // start_ms is baked into the engine source at commit (there is no
        // live setter to move a committed track), so a clip or take move must
        // force a graph rebuild.
        b.push('@');
        b.push_str(&t.start_ms.to_string());
        b.push('>');
        b.push_str(t.bus_id.as_deref().unwrap_or(""));
        b.push(':');
        b.push_str(&order_tag(t.fx.as_ref()));
        b.push(';');
        b.push_str(&sends_tag(&t.sends));
        // The lane a clip shares a chain with is wiring too.
        b.push('~');
        b.push_str(t.lane_id.as_deref().unwrap_or(""));
        b.push('|');
    }
    b.push('#');
    for x in buses {
        b.push_str(&x.id);
        b.push('>');
        b.push_str(x.output_bus_id.as_deref().unwrap_or(""));
        b.push(':');
        b.push_str(&order_tag(Some(&x.fx)));
        b.push(';');
        b.push_str(&sends_tag(&x.sends));
        b.push('|');
    }
    b
}

/// Which model a sync job reconciles to.
enum SyncModel {
    /// The model handed to `sync`, with the live-edit watermark it carries.
    Given {
        tracks: Arc<Vec<TrackPlayback>>,
        buses: Arc<Vec<BusPlayback>>,
        as_of: u64,
    },
    /// The model most recently handed to `sync`, read when the job RUNS (the
    /// degraded retry).
    Latest,
}

/// Publishes the staged commit when dropped: on every path past the commit,
/// a panic included (Dart's `finally`).
struct PublishOnDrop<'a>(&'a dyn EngineBindings);

impl Drop for PublishOnDrop<'_> {
    fn drop(&mut self) {
        let _ = self.0.tracks_publish();
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    /// The engine, while it is available.
    fn live_engine(&self) -> Option<&dyn EngineBindings> {
        if self.is_available() {
            self.engine.as_deref()
        } else {
            None
        }
    }

    fn duration_to_frames(&self, d: Duration) -> i64 {
        (d.as_micros() as f64 * self.sample_rate / 1e6).round() as i64
    }

    fn ms_to_frames(&self, ms: i64) -> i64 {
        (ms as f64 * self.sample_rate / 1000.0).round() as i64
    }

    fn frames_to_duration(&self, frames: i64) -> Duration {
        // Dart's Duration could go negative; the playhead never does.
        let micros = (frames as f64 / self.sample_rate * 1e6).round();
        Duration::from_micros(micros.max(0.0) as u64)
    }
}

/// Starts (or restarts) the 16 ms position poll.
fn start_poll(shared: &Arc<Shared>, st: &mut State) {
    st.poll_gen = st.poll_gen.wrapping_add(1);
    shared.poll_wake.notify_all();
    let gen = st.poll_gen;
    let poll = Arc::clone(shared);
    // A poll that cannot start leaves the playhead where the last seek put
    // it, as a timer that never fired would.
    let _ = thread::Builder::new()
        .name("vocara-position-poll".into())
        .spawn(move || poll_position(&poll, gen));
}

/// Cancels the position poll.
fn stop_poll(shared: &Shared, st: &mut State) {
    st.poll_gen = st.poll_gen.wrapping_add(1);
    shared.poll_wake.notify_all();
}

/// The position poll `gen`: a tick every period, from a period after its
/// start, until it is no longer the current poll.
fn poll_position(shared: &Shared, gen: u64) {
    let mut st = shared.state();
    let mut next = Instant::now() + POSITION_POLL_PERIOD;
    loop {
        if st.poll_gen != gen {
            return;
        }
        let now = Instant::now();
        if now < next {
            st = shared
                .poll_wake
                .wait_timeout(st, next - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        }
        next += POSITION_POLL_PERIOD;
        if next <= now {
            next = now + POSITION_POLL_PERIOD;
        }
        if let Some(engine) = shared.live_engine() {
            shared
                .position
                .add(shared.frames_to_duration(engine.frame_pos()));
        }
    }
}

/// Queues the single rebuild a degraded commit needs.
///
/// Called from inside the running sync job, so it must APPEND to the sync
/// chain and never wait: this job is the chain's current link, so this lands
/// after it, and after any sync enqueued while it ran. The retry therefore
/// reads the latest model at RUN time instead of taking the degraded job's
/// lists: when a newer model has already committed, the retry fast-paths on
/// its signature instead of resurrecting the stale topology over it. A
/// failing retry is swallowed like any other chain failure (its ticket is
/// dropped): the withheld signature means the next real sync rebuilds anyway.
/// Disposal makes the queued run a no-op (`available` is false by then).
fn schedule_degraded_rebuild(shared: &Arc<Shared>) {
    let retry = Arc::clone(shared);
    drop(
        shared
            .chain
            .add(move || do_sync(&retry, SyncModel::Latest, true)),
    );
}

/// One sync job (Dart's `_doSync`).
///
/// `retry` marks the one rebuild a degraded commit queues (see
/// [`schedule_degraded_rebuild`]): it never queues another, so a file that
/// stays unreadable settles instead of rebuilding the graph (and re-running
/// its decodes) on every sync for the rest of the session. A parameter rather
/// than shared state: a flag set by the degraded commit and cleared only on a
/// later successful one stayed set when a rejected commit returned early in
/// between, and the NEXT degraded commit then never got its retry.
fn do_sync(shared: &Arc<Shared>, model: SyncModel, retry: bool) -> Result<(), SyncError> {
    let mut st = shared.state();
    if !shared.is_available() {
        return Ok(());
    }
    let Some(engine) = shared.engine.clone() else {
        return Ok(());
    };
    let engine: &dyn EngineBindings = &*engine;
    let (tracks, buses, as_of) = match model {
        SyncModel::Given {
            tracks,
            buses,
            as_of,
        } => (tracks, buses, as_of),
        SyncModel::Latest => (
            Arc::clone(&st.latest_tracks),
            Arc::clone(&st.latest_buses),
            st.latest_as_of,
        ),
    };
    let sig = topology_sig(&tracks, &buses);
    if sig == st.committed_sig {
        // No structural change: re-push every live parameter. Pushing only
        // volume/mute here was a silent divergence: the signature deliberately
        // excludes pan, FX settings and send levels (a knob tick must not
        // rebuild the graph), and each of those has its own live setter that
        // the mixer calls on every tick, so this branch is the ONLY thing
        // that carries them when the model changed by some other route. A
        // failed save reverting the aggregate, and a remote sync applying
        // someone else's edit, both land exactly here: the UI showed the new
        // value while the engine kept playing the old one until the next
        // structural edit or reopen.
        st.push_live_params(engine, &tracks, &buses);
        st.replay_live_edits(engine, as_of);
        return Ok(());
    }

    // Topology changed: rebuild the graph. Stage BUSES FIRST so a track can
    // route into one by index. Build the new index maps off to the side so
    // the old ones stay valid during the track decodes.
    let mut new_bus_index: HashMap<String, i32> = HashMap::new();
    for (i, b) in buses.iter().enumerate() {
        new_bus_index.insert(b.id.clone(), slot_index(i));
    }
    let _ = engine.buses_begin();
    for (i, b) in buses.iter().enumerate() {
        let out = b
            .output_bus_id
            .as_ref()
            .map_or(-1, |o| new_bus_index.get(o).copied().unwrap_or(-1));
        let _ = engine.buses_add(b.volume as f32, b.muted, out);
        push_chain(engine, slot_index(i), Some(&b.fx), true);
        push_sends(engine, slot_index(i), &b.sends, &new_bus_index, true);
    }

    let _ = engine.tracks_begin();
    let mut new_index: HashMap<String, i32> = HashMap::new();
    let mut new_owner: HashMap<String, i32> = HashMap::new();
    // Each lane's first clip that staged: its chain is the one every later
    // clip of the lane plays through (see `TrackPlayback::lane_id`).
    let mut lane_leader: HashMap<String, i32> = HashMap::new();
    for t in tracks.iter() {
        let bus_idx = t
            .bus_id
            .as_ref()
            .map_or(-1, |b| new_bus_index.get(b).copied().unwrap_or(-1));
        // A track that fails to stage (idx < 0, e.g. a gone or corrupt file)
        // is simply left out of the graph this round; the commit still
        // succeeds for the rest and the skipped track restages on the next
        // topology change (see `committed_sig` below). The decode refusals
        // (unreadable file, unsupported rate, over-cap media) mean exactly
        // "skip this track"; anything else is a programming error and must
        // escape.
        let (relocked, staged) = stage_track(shared, engine, st, t, bus_idx);
        st = relocked;
        let idx = match staged {
            Ok(idx) => idx,
            Err(
                e @ (MediaError::TooLong { .. } | MediaError::Unsupported(_) | MediaError::Read),
            ) => {
                DiagnosticsLog::instance().log("engine", &format!("skip unstageable track: {e}"));
                -1
            }
            Err(e) => return Err(SyncError::Decode(e)),
        };
        // Disposed (the page closed) while this track decoded: the rest of
        // the rebuild is for an engine that no longer exists. Without this
        // every remaining track was still decoded to PCM (seconds of CPU and
        // up to gigabytes per hour of audio) only to be refused by a null
        // engine.
        if !shared.is_available() {
            return Ok(());
        }
        if idx >= 0 {
            new_index.insert(t.id.clone(), idx);
            new_owner.insert(t.id.clone(), idx);
            push_sends(engine, idx, &t.sends, &new_bus_index, false);
            if let Some(lane) = &t.lane_id {
                match lane_leader.get(lane) {
                    None => {
                        lane_leader.insert(lane.clone(), idx);
                    }
                    Some(&leader) => {
                        if engine.track_lane(idx, leader).is_ok() {
                            new_owner.insert(t.id.clone(), leader);
                        }
                    }
                }
            }
        }
    }
    // A refusal means the engine REJECTED this topology (e.g. a routing
    // cycle) and kept the OLD graph. Don't record it as committed or swap the
    // index maps: otherwise an identical re-sync short-circuits on
    // `committed_sig` and never retries, and the cached indices diverge from
    // the live graph. Leave the prior committed state intact so a corrected
    // topology rebuilds.
    let committed = engine.tracks_commit_staged();
    // Published whatever happens from here on: a commit the engine staged and
    // nothing published (a panic below, or an engine-side failure after the
    // stage that still reported a refusal) left the old graph playing while
    // every setter addressed the staged one: mute, faders and FX went dead
    // until the next topology change. A publish with nothing staged (a
    // rejected topology) is a no-op. Dropped before the state lock, as Dart's
    // `finally` ran in the same synchronous segment.
    let _publish = PublishOnDrop(engine);
    if committed.is_err() {
        return Ok(());
    }
    st.track_index = new_index;
    st.chain_owner = new_owner;
    st.bus_index = new_bus_index;
    // Record the committed signature even when a track was skipped (e.g. a
    // gone or corrupt file). Forcing the empty sentinel here used to
    // guarantee a retry, but because the signature excludes live volume and
    // mute, EVERY fader, mute or FX tweak then missed the fast path and
    // rebuilt the whole graph, re-running the dead clip's decode and
    // re-staging everything on each tick (an audible glitch and constant
    // re-decode). Recording the real `sig` keeps unrelated live changes on
    // the fast path; a skipped track is restaged on the next real topology
    // change (a structural edit, backing toggle or reopen, all of which
    // change `sig`), which is also when a transiently unreadable file would
    // have become readable again.
    //
    // A DEGRADED track is different from a skipped one and must not be
    // treated the same way. Staging probes a streamable WAV and then hands
    // the engine an empty buffer, trusting the commit to reopen the file;
    // when that second open fails the track is IN the graph, holds an index,
    // has gain and meters, and renders silence forever, while the commit
    // returns success and the track contributes no underruns. Recording the
    // signature over that would make it permanent for the session, so the
    // signature is withheld and the rebuild is QUEUED HERE. Withholding alone
    // was not enough: the only caller syncs on structural mix edits, and
    // nothing in play, seek or a fader move ever calls it, so a "retry on the
    // next sync" could wait for the rest of the session while the take sat
    // silent. The retry re-probes on a fresh open: if the file came back
    // non-canonical it is decoded and staged as PCM instead (it plays), and
    // if it is truly gone the stage MISSES and the track is left out; neither
    // can degrade again, so one retry is enough and there is no rebuild loop
    // (`retry` bounds it either way).
    let degraded = engine.commit_degraded_mask();
    if degraded != 0 && !retry {
        DiagnosticsLog::instance().log(
            "engine",
            &format!(
                "commit degraded {} streaming track(s) to silence; rebuilding once from a fresh probe",
                GraphAudioPlaybackService::bit_count(degraded)
            ),
        );
        st.committed_sig = String::new();
        // Appended to the sync chain rather than waited on: this job IS the
        // chain's current link, so waiting on its successor would deadlock.
        schedule_degraded_rebuild(shared);
    } else {
        st.committed_sig = sig;
    }

    // The rebuild reset every node to defaults: re-apply persisted effects
    // and levels. The new graph is still staged, so these land on it before
    // it renders anything (its first block already plays the mix as set,
    // instead of the build defaults ramping into it), and only then does it
    // go live.
    st.push_live_params(engine, &tracks, &buses);
    st.replay_live_edits(engine, as_of);
    Ok(())
}

/// Stages one track for the pending commit, routed to `bus_idx` (-1 =
/// master). Tries the engine's decoded-PCM cache first (instant; the native
/// side is the single source of truth for what is cached and never decodes
/// on this thread: a miss is refused). On a miss (first stage, or evicted at
/// an earlier commit) the file is decoded with the state lock RELEASED (the
/// one await of Dart's sync) and staged as raw samples, which re-caches it
/// under the track's path. Hands the lock back, retaken.
fn stage_track<'a>(
    shared: &'a Shared,
    engine: &dyn EngineBindings,
    st: MutexGuard<'a, State>,
    t: &TrackPlayback,
    bus_idx: i32,
) -> (MutexGuard<'a, State>, Result<i32, MediaError>) {
    let volume = t.volume as f32;
    if let Ok(idx) = engine.tracks_add(&t.path, t.start_ms, volume, t.muted, t.is_vocal, bus_idx) {
        if idx >= 0 {
            push_chain(engine, idx, t.fx.as_ref(), false);
            return (st, Ok(idx));
        }
    }
    drop(st);
    let rate = shared.sample_rate.round() as u32;
    let decoded = (shared.decode)(&t.path, rate);
    let st = shared.state();
    let pcm = match decoded {
        Ok(Some(pcm)) if !pcm.is_empty() => pcm,
        Ok(_) => return (st, Ok(-1)),
        Err(e) => return (st, Err(e)),
    };
    if !shared.is_available() {
        // Disposed during the decode: nothing will take the buffer (it is
        // freed as it drops).
        return (st, Ok(-1));
    }
    let idx = engine
        .tracks_add_pcm(
            &t.path,
            pcm.samples(),
            t.start_ms,
            volume,
            t.muted,
            t.is_vocal,
            bus_idx,
        )
        .unwrap_or(-1);
    if idx >= 0 {
        push_chain(engine, idx, t.fx.as_ref(), false);
    }
    // The engine copied the samples; the decoded buffer is freed here.
    drop(pcm);
    (st, Ok(idx))
}

impl Default for GraphAudioPlaybackService {
    fn default() -> GraphAudioPlaybackService {
        GraphAudioPlaybackService::new()
    }
}

impl GraphAudioPlaybackService {
    /// Device-event bits for [`GraphAudioPlaybackService::poll_device_events`]
    /// masks (the engine's, see [`crate::engine::events`]).
    pub const EVENT_OUTPUT_LOST: u32 = events::OUTPUT_LOST;
    pub const EVENT_INPUT_LOST: u32 = events::INPUT_LOST;
    pub const EVENT_AEC_BYPASSED: u32 = events::AEC_BYPASSED;
    pub const EVENT_INPUT_FELL_BACK: u32 = events::INPUT_FELL_BACK;
    pub const EVENT_RECORD_LIMIT: u32 = events::RECORD_LIMIT;
    pub const EVENT_OUTPUT_REROUTED: u32 = events::OUTPUT_REROUTED;

    /// [`GraphAudioPlaybackService::set_input_channel`] modes: Auto, the
    /// input carrying the mic (a mic on either input of a 2-input interface
    /// records at full level), or one input alone.
    pub const INPUT_CHANNEL_AUTO: u32 = engine::INPUT_CHANNEL_AUTO;
    pub const INPUT_CHANNEL_1: u32 = engine::INPUT_CHANNEL_1;
    pub const INPUT_CHANNEL_2: u32 = engine::INPUT_CHANNEL_2;

    /// The app's engine: creates the native one. If no output device opens,
    /// the engine reports [`GraphAudioPlaybackService::is_available`] false
    /// and every call no-ops, so the app stays usable (just silent).
    pub fn new() -> GraphAudioPlaybackService {
        match engine::create() {
            Ok(engine) => GraphAudioPlaybackService::build(Some(engine), None),
            Err(_) => GraphAudioPlaybackService::build(None, None),
        }
    }

    /// The service over injected `bindings` (and an injected staging
    /// `decode`), so the sync pipeline's skip, degraded and retry policies,
    /// and every caller of the device and recording calls, run against a
    /// fake engine without the native one or real media files. The engine is
    /// taken as available.
    pub fn with_bindings(
        bindings: Arc<dyn EngineBindings>,
        decode: Option<StagingDecode>,
    ) -> GraphAudioPlaybackService {
        GraphAudioPlaybackService::build(Some(bindings), decode)
    }

    fn build(
        engine: Option<Arc<dyn EngineBindings>>,
        decode: Option<StagingDecode>,
    ) -> GraphAudioPlaybackService {
        let sample_rate = engine
            .as_ref()
            .map_or(DEFAULT_SAMPLE_RATE, |e| e.sample_rate());
        let decode =
            decode.unwrap_or_else(|| Arc::new(media::decode_for_playback) as StagingDecode);
        GraphAudioPlaybackService {
            shared: Arc::new(Shared {
                available: AtomicBool::new(engine.is_some()),
                engine,
                decode,
                sample_rate,
                state: Mutex::new(State::new()),
                poll_wake: Condvar::new(),
                chain: SyncChain::new(),
                enqueue: Mutex::new(()),
                position: Broadcast::new(),
                playing: Broadcast::new(),
                input_pull: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Population count over all 64 bits of `mask`. The degraded mask is a
    /// full u64 (the engine folds staged indices past bit 63 into the top
    /// bit). Dart received it as a signed int and had to shift logically
    /// (`>>>`) so the count terminated with bit 63 set; a `u64` has no sign
    /// to keep.
    pub fn bit_count(mask: u64) -> u32 {
        mask.count_ones()
    }

    /// Whether the native engine started (a device was opened) and is not
    /// disposed. If false, every call no-ops.
    pub fn is_available(&self) -> bool {
        self.shared.is_available()
    }

    /// The engine's sample rate (48 kHz before an engine).
    pub fn sample_rate(&self) -> f64 {
        self.shared.sample_rate
    }

    fn live(&self) -> Option<&dyn EngineBindings> {
        self.shared.live_engine()
    }

    // --- Devices ----------------------------------------------------------------

    /// Switches the active output device by uid. True when the pick was acted
    /// on: playback is on it, or on the default output when it could not be
    /// opened, or (while the mic unit owns the output) the pick is recorded
    /// for when the mic closes; [`GraphAudioPlaybackService::actual_output_device`]
    /// says what renders. False when nothing could be opened, or the engine is
    /// down.
    pub fn set_output_device(&self, uid: &str) -> bool {
        self.live()
            .is_some_and(|e| e.set_output_device(uid).is_ok())
    }

    /// The requested output device's uid ("" if unavailable).
    pub fn current_output_device(&self) -> String {
        self.live()
            .map_or_else(String::new, |e| e.current_output_device())
    }

    /// The uid the engine is ACTUALLY rendering to ("" = nothing open).
    /// Differs from [`GraphAudioPlaybackService::current_output_device`],
    /// which reports what was REQUESTED: they diverge when the requested
    /// device could not be opened, and for as long as the duplex mic unit owns
    /// the output, where playback follows the microphone's endpoint or the
    /// system default and the app-selected device is not consulted at all.
    /// Feedback safety must be judged from this one.
    pub fn actual_output_device(&self) -> String {
        self.live()
            .map_or_else(String::new, |e| e.actual_output_device())
    }

    /// Polls and clears the pending device events selected by `mask` (the
    /// `EVENT_*` bits: output lost, input lost, mic opened without echo
    /// cancellation, selected input unavailable so capture fell back to the
    /// system default, the take reached the longest recording a WAV header
    /// can describe so capture past it is refused and the owner must
    /// finalize, playback not on the selected output so the engine has muted
    /// the monitor for as long as that holds). Only the claimed bits are
    /// cleared; bits outside the mask stay pending for their own consumer, so
    /// the song page's 1 s watch and the technique page's 50 ms level tick no
    /// longer steal each other's events. 0 when the engine is down.
    pub fn poll_device_events(&self, mask: u32) -> u32 {
        self.live().map_or(0, |e| e.poll_device_event(mask))
    }

    /// Total streaming-source underruns (frames a disk-streamed track rendered
    /// silent because its ring was empty). 0 is the healthy steady state; the
    /// device watch logs growth, the field instrument for feeder health on
    /// real hardware.
    pub fn stream_xruns(&self) -> u64 {
        self.live().map_or(0, |e| e.stream_xruns())
    }

    /// Selects the input device by uid (reopens the input stream live if
    /// monitoring). True on success; false when the engine is down.
    pub fn set_input_device(&self, uid: &str) -> bool {
        self.live().is_some_and(|e| e.set_input_device(uid).is_ok())
    }

    /// The selected input device's uid ("" if unavailable or system default).
    pub fn current_input_device(&self) -> String {
        self.live()
            .map_or_else(String::new, |e| e.current_input_device())
    }

    /// The uid the OPEN input stream is ACTUALLY bound to ("" = input closed,
    /// or bound to the system default). Selection is fire-and-forget inside
    /// the engine, so this, not
    /// [`GraphAudioPlaybackService::current_input_device`], is what the take
    /// really records from.
    pub fn actual_input_device(&self) -> String {
        self.live()
            .map_or_else(String::new, |e| e.actual_input_device())
    }

    // --- Monitoring ---------------------------------------------------------------

    /// Starts live-input monitoring (opens the mic stream and ramps the
    /// monitor in).
    pub fn start_monitor(&self) {
        if let Some(e) = self.live() {
            let _ = e.start_monitor();
        }
    }

    /// Stops live-input monitoring (ramps out and closes the mic stream).
    pub fn stop_monitor(&self) {
        if let Some(e) = self.live() {
            let _ = e.stop_monitor();
        }
    }

    /// Sets the live-input monitor gain (linear, 0 = muted).
    pub fn set_monitor_gain(&self, gain: f64) {
        if let Some(e) = self.live() {
            let _ = e.set_monitor_gain(gain as f32);
        }
    }

    /// Names the output with no acoustic path back to the mic (headphones), or
    /// "" for none. The engine passes the mic through its echo canceller
    /// untouched only while that device is the one actually rendering (see
    /// `AppSettingsService::echo_free_output`).
    pub fn set_echo_free(&self, uid: &str) {
        if let Some(e) = self.live() {
            let _ = e.set_echo_free(uid);
        }
    }

    /// Sets the record/input gain (linear): the mic-input fader's makeup gain
    /// applied to the captured voice before recording and analysis.
    pub fn set_input_gain(&self, gain: f64) {
        if let Some(e) = self.live() {
            let _ = e.set_input_gain(gain as f32);
        }
    }

    /// Chooses which input(s) of a multi-channel mic become the mono signal
    /// the monitor, the echo canceller, the meter and the recording all hear
    /// (an `INPUT_CHANNEL_*` mode). Input 2 on a device without one is
    /// input 1.
    pub fn set_input_channel(&self, mode: u32) {
        if let Some(e) = self.live() {
            let _ = e.set_input_channel(mode);
        }
    }

    /// Sets the monitor reverb (room, damping, wet, each 0..1).
    pub fn set_monitor_reverb(&self, room: f64, damping: f64, wet: f64) {
        if let Some(e) = self.live() {
            let _ = e.set_monitor_reverb(room as f32, damping as f32, wet as f32);
        }
    }

    /// The current live-input peak level (0..1) for a meter; 0 when not
    /// capturing.
    pub fn input_level(&self) -> f64 {
        self.live().map_or(0.0, |e| f64::from(e.input_level()))
    }

    /// Diagnostic and pre-roll clock: total ENGINE-RATE (48 kHz) frames the
    /// mic callback has emitted (0 = the mic is delivering nothing).
    /// Engine-rate so callers can divide by 48000 for time regardless of the
    /// device's native rate.
    pub fn input_frames_received(&self) -> u64 {
        self.live().map_or(0, |e| e.input_frames_received())
    }

    /// The host-thread-authoritative input-stream state word:
    /// `(generation << 1) | running`. Running is set only after a successful
    /// stream start and cleared BEFORE any teardown joins the old callback;
    /// the generation bumps on every start. The mic ready handshake snapshots
    /// it, verifies [`GraphAudioPlaybackService::input_frames_received`]
    /// advanced, then snapshots it AGAIN: an unchanged word with bit 0 set
    /// proves the SAME stream delivered those frames (a teardown clears the
    /// bit, a restart changes the generation), so a dying stream's final
    /// callback can't fake a new stream coming up.
    pub fn input_stream_state(&self) -> u64 {
        self.live().map_or(0, |e| e.input_stream_state())
    }

    /// Bit 0 of [`GraphAudioPlaybackService::input_stream_state`]: whether an
    /// input stream is currently up.
    pub fn input_state_running(state: u64) -> bool {
        state & 1 == 1
    }

    /// The newest published input-intent sequence. Read right after arming a
    /// recording, it is the arm's watermark for
    /// [`GraphAudioPlaybackService::applied_seq_of`].
    pub fn input_intent_seq(&self) -> u64 {
        self.live().map_or(0, |e| e.input_intent_seq())
    }

    /// The host's last SUCCESSFUL convergence, packed as
    /// `(intent_seq << 24) | input_generation`. Published only when the
    /// desired state was actually reached (a failed stream open leaves it
    /// untouched), and paired with the generation so the ready check can
    /// demand that progress, the running bit and the applied watermark all
    /// come from the SAME stream instance (see
    /// [`GraphAudioPlaybackService::applied_seq_of`] and
    /// [`GraphAudioPlaybackService::applied_gen_of`]).
    pub fn input_applied_state(&self) -> u64 {
        self.live().map_or(0, |e| e.input_applied_state())
    }

    /// The intent-sequence half of
    /// [`GraphAudioPlaybackService::input_applied_state`].
    pub fn applied_seq_of(applied: u64) -> u64 {
        applied >> 24
    }

    /// The generation half of
    /// [`GraphAudioPlaybackService::input_applied_state`] (24 bits, matching
    /// the generation carried in the stream state's high bits).
    pub fn applied_gen_of(applied: u64) -> u64 {
        applied & 0xFF_FFFF
    }

    /// The generation half of
    /// [`GraphAudioPlaybackService::input_stream_state`], truncated to the
    /// same 24 bits the applied pairing carries.
    pub fn state_gen_of(state: u64) -> u64 {
        (state >> 1) & 0xFF_FFFF
    }

    /// Record round-trip latency in milliseconds: how far behind the playhead
    /// a captured vocal lands (output IO, acoustic, input IO and the AEC
    /// block). Recorded takes shift earlier by this so they sit on the beat
    /// the singer heard. 0 until the mic has opened once.
    pub fn record_latency_ms(&self) -> f64 {
        self.live().map_or(0.0, |e| {
            f64::from(e.record_latency_frames()) / 48_000.0 * 1000.0
        })
    }

    // --- Capture, recording ---------------------------------------------------

    /// Begins capturing input frames for live analysis (opens the mic stream
    /// on the selected device).
    pub fn start_capture(&self) {
        if let Some(e) = self.live() {
            let _ = e.start_capture();
        }
    }

    /// Stops capturing input frames (closes the mic stream if nothing else
    /// needs it).
    pub fn stop_capture(&self) {
        if let Some(e) = self.live() {
            let _ = e.stop_capture();
        }
    }

    /// Pulls the latest captured frames (interleaved stereo at 48 kHz): `2 *
    /// count` samples, at most 4096 frames a call. Empty when the engine is
    /// down.
    pub fn pull_input_frames(&self) -> Vec<f32> {
        let Some(e) = self.live() else {
            return Vec::new();
        };
        let mut buf = self
            .shared
            .input_pull
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if buf.is_empty() {
            buf.resize(INPUT_PULL_FRAMES * 2, 0.0);
        }
        let n = e.input_frames(&mut buf[..]).min(INPUT_PULL_FRAMES);
        buf[..n * 2].to_vec()
    }

    /// Records the live input (selected device) to a mono 48 kHz WAV at
    /// `path`. False when the engine could not create the file (disk full,
    /// permissions), in which case nothing will be recorded, or is down.
    pub fn start_input_recording(&self, path: &str) -> bool {
        self.live()
            .is_some_and(|e| e.start_input_recording(path).is_ok())
    }

    /// Stops the input recording (finalizes the WAV). False when an IO error
    /// occurred during the recording, meaning the WAV on disk is missing,
    /// damaged or incomplete (or the engine is down).
    pub fn stop_input_recording(&self) -> bool {
        self.live()
            .is_some_and(|e| e.stop_input_recording().is_ok())
    }

    /// Frames the recording ring dropped during the current or latest take
    /// (reset at each start). Nonzero means the WAV on disk is missing that
    /// many frames (the writer thread stalled past the ring's headroom), so
    /// the take has silent holes and must not be presented as gapless.
    pub fn record_dropped_frames(&self) -> u64 {
        self.live().map_or(0, |e| e.record_dropped_frames())
    }

    /// Frames written into the recording ring for the CURRENT take
    /// (engine-rate, reset at each start): the per-take recorded-progress
    /// ack. Nonzero PROVES audio is in this take's ring: a callback that
    /// entered before the arm read the disarmed flag and never pushed, so
    /// unlike [`GraphAudioPlaybackService::input_frames_received`] this can
    /// never count frames that are not in the WAV. The mic ready handshake
    /// waits for it to leave zero; read at play time it IS the take's file
    /// offset (the pre-roll).
    pub fn record_written_frames(&self) -> u64 {
        self.live().map_or(0, |e| e.record_written_frames())
    }

    // --- Sync -----------------------------------------------------------------

    /// Queues a sync of the engine to `tracks` and `buses` and returns at once
    /// with its handle (Dart's `sync()` returned its future the same way).
    /// The model and its live-edit watermark are recorded now, in the order
    /// syncs are enqueued: every live edit made before this call is taken to
    /// be in this model (the mixer updates its model before it calls a
    /// setter), and every later one is replayed over it.
    pub fn enqueue_sync(&self, tracks: Vec<TrackPlayback>, buses: Vec<BusPlayback>) -> SyncHandle {
        let shared = &self.shared;
        let _order = shared
            .enqueue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (tracks, buses) = (Arc::new(tracks), Arc::new(buses));
        let as_of = {
            let mut st = shared.state();
            st.latest_tracks = Arc::clone(&tracks);
            st.latest_buses = Arc::clone(&buses);
            st.latest_as_of = st.live_seq;
            st.live_seq
        };
        let job = Arc::clone(shared);
        SyncHandle(shared.chain.add(move || {
            do_sync(
                &job,
                SyncModel::Given {
                    tracks,
                    buses,
                    as_of,
                },
                false,
            )
        }))
    }

    /// [`PlaybackEngine::sync`] with its outcome: blocks until this sync (and
    /// every one queued before it) is done, and says how it ended.
    pub fn sync_checked(
        &self,
        tracks: Vec<TrackPlayback>,
        buses: Vec<BusPlayback>,
    ) -> Result<(), SyncError> {
        self.enqueue_sync(tracks, buses).wait()
    }
}

impl PlaybackEngine for GraphAudioPlaybackService {
    fn position(&self) -> Subscription<Duration> {
        self.shared.position.subscribe()
    }

    fn playing(&self) -> Subscription<bool> {
        self.shared.playing.subscribe()
    }

    fn current_position(&self) -> Duration {
        self.live().map_or(Duration::ZERO, |e| {
            self.shared.frames_to_duration(e.frame_pos())
        })
    }

    fn is_playing(&self) -> bool {
        self.shared.state().is_playing
    }

    fn set_loop(&self, enabled: bool, start_ms: i64, end_ms: i64) {
        if let Some(e) = self.live() {
            let _ = e.set_loop(
                enabled,
                self.shared.ms_to_frames(start_ms),
                self.shared.ms_to_frames(end_ms),
            );
        }
    }

    /// Blocking: decodes and rebuilds on the sync worker and waits for it. A
    /// failure (see [`SyncError`]) completed Dart's future with an error that
    /// reached the zone's error handler through its awaiting caller; here the
    /// trait returns nothing, so it is logged (`engine` / `sync failed`).
    /// [`GraphAudioPlaybackService::sync_checked`] hands it back instead.
    fn sync(&self, tracks: Vec<TrackPlayback>, buses: Vec<BusPlayback>) {
        if let Err(e) = self.sync_checked(tracks, buses) {
            DiagnosticsLog::instance().log("engine", &format!("sync failed: {e}"));
        }
    }

    fn play(&self) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        let _ = e.play();
        st.is_playing = true;
        self.shared.playing.add(true);
        start_poll(&self.shared, &mut st);
    }

    fn pause(&self) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        let _ = e.pause();
        st.is_playing = false;
        self.shared.playing.add(false);
        // Stop the 60 Hz position poll: while paused the playhead doesn't
        // advance, so polling just burns engine calls every 16 ms. play()
        // restarts it (and seek() emits the playhead directly), so this
        // matches seek()'s "poll only runs while playing" contract.
        stop_poll(&self.shared, &mut st);
    }

    fn seek(&self, position: Duration) {
        let _st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        let _ = e.seek(self.shared.duration_to_frames(position));
        // Emit the new position immediately. The position poll only runs
        // while playing, so without this a seek (e.g. clicking the ruler)
        // before the first playback would move the engine but never update
        // the playhead.
        self.shared.position.add(position);
    }

    fn set_volume(&self, id: &str, volume: f64, muted: bool) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        let edit = LiveEdit::Volume {
            id: id.to_string(),
            volume,
            muted,
        };
        st.live(e, edit);
    }

    fn set_track_pan(&self, id: &str, pan: f64) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        st.live(
            e,
            LiveEdit::Pan {
                id: id.to_string(),
                pan,
            },
        );
    }

    fn set_master_volume(&self, gain: f64) {
        let mut st = self.shared.state();
        st.master_gain = gain;
        if let Some(e) = self.live() {
            let _ = e.set_master(gain as f32);
        }
    }

    fn supports_effects(&self) -> bool {
        true
    }

    fn set_track_fx(&self, track_id: &str, fx: &TrackFx) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        st.live(
            e,
            LiveEdit::Fx {
                id: track_id.to_string(),
                fx: fx.clone(),
            },
        );
    }

    fn set_bus_fx(&self, bus_id: &str, fx: &TrackFx) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        st.live(
            e,
            LiveEdit::BusFx {
                id: bus_id.to_string(),
                fx: fx.clone(),
            },
        );
    }

    fn set_bus_volume(&self, bus_id: &str, volume: f64, muted: bool) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        st.live(
            e,
            LiveEdit::BusVolume {
                id: bus_id.to_string(),
                volume,
                muted,
            },
        );
    }

    fn set_bus_pan(&self, bus_id: &str, pan: f64) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        st.live(
            e,
            LiveEdit::BusPan {
                id: bus_id.to_string(),
                pan,
            },
        );
    }

    fn bus_gain_reduction_db(&self, bus_id: &str) -> f64 {
        let st = self.shared.state();
        let Some(e) = self.live() else {
            return 0.0;
        };
        st.bus_index
            .get(bus_id)
            .map_or(0.0, |&idx| f64::from(e.bus_gr(idx)))
    }

    fn set_track_send_level(&self, track_id: &str, slot: usize, level_db: f64) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        st.live(
            e,
            LiveEdit::Send {
                id: track_id.to_string(),
                slot,
                level_db,
            },
        );
    }

    fn set_bus_send_level(&self, bus_id: &str, slot: usize, level_db: f64) {
        let mut st = self.shared.state();
        let Some(e) = self.live() else {
            return;
        };
        st.live(
            e,
            LiveEdit::BusSend {
                id: bus_id.to_string(),
                slot,
                level_db,
            },
        );
    }

    fn set_reverb(&self, reverb: &ReverbSettings) {
        let mut st = self.shared.state();
        st.reverb = Some(*reverb);
        if let Some(e) = self.live() {
            apply_reverb(e, reverb);
        }
    }

    fn track_gain_reduction_db(&self, track_id: &str) -> f64 {
        let st = self.shared.state();
        let Some(e) = self.live() else {
            return 0.0;
        };
        st.track_index
            .get(track_id)
            .map_or(0.0, |&idx| f64::from(e.track_gr(idx)))
    }

    /// Stops the position poll, destroys the engine (once), forgets the
    /// committed graph and ends the position and playing subscriptions. A
    /// sync in flight notices at its next step and stops; queued ones run as
    /// no-ops. Idempotent; dropping the service does the same.
    fn dispose(&self) {
        let shared = &self.shared;
        let mut st = shared.state();
        stop_poll(shared, &mut st);
        if shared.available.swap(false, Ordering::SeqCst) {
            if let Some(e) = &shared.engine {
                e.destroy();
            }
        }
        *shared
            .input_pull
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Vec::new();
        st.track_index.clear();
        st.chain_owner.clear();
        st.chain_sent.clear();
        st.bus_index.clear();
        st.live_edits.clear();
        st.committed_sig.clear();
        drop(st);
        shared.position.close();
        shared.playing.close();
    }
}

impl Drop for GraphAudioPlaybackService {
    fn drop(&mut self) {
        PlaybackEngine::dispose(self);
    }
}
