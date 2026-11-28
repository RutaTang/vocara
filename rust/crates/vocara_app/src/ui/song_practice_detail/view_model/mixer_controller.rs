//! Everything the engine hears (port of
//! `ui/song_practice_detail/view_model/mixer_controller.dart`): audibility
//! (mute/solo resolution incl. bus routing), track/bus volume-pan-FX live
//! edits + commits, aux sends, the master bus, FX-bus CRUD/routing with cycle
//! refusal, reverb, gain-reduction meters, composite bus/master display
//! waveforms, backing A/B, and [`MixerController::resync_playback`] — the
//! full engine graph sync.
//!
//! # How the view model drives it
//!
//! The controller owns no persistent state beyond the transient backing A/B
//! flag; all mix state lives in the practice model, which the view model
//! owns and persists. Dart reached it through closures onto the view model;
//! here those are a [`MixerHost`] the view model hands each call, read and
//! written at the moment of the call as the closures were.
//!
//! - A Dart method that ended in `await _save()` returns a [`SaveRequest`]
//!   (or `Option<SaveRequest>`, `None` where Dart returned before saving):
//!   the view model persists the model, and its save's tail resyncs the
//!   engine with what it saved, so a routing edit here needs no sync of its
//!   own. Everything before the save (the model edit, the live engine push,
//!   the notification) has happened when the method returns, as it had when
//!   Dart's future first yielded.
//! - [`MixerController::resync_playback`] queues the sync on the graph
//!   engine's worker at once and hands back a [`PendingSync`] to wait for
//!   off the UI thread. Queuing it in the call itself is what keeps Dart's
//!   ordering: the engine takes every live edit made before the call to be in
//!   the model it was handed, and replays every later one over it (see
//!   `GraphAudioPlaybackService::enqueue_sync`). Built here and queued later
//!   from a task, a fader moved in between would be lost.
//! - `notifyListeners` bumps [`MixerController::revision`]; the widgets
//!   compare it between builds. The composite lane envelopes are
//!   [`Arc`]s, the same one while nothing they show changed, so a lane can
//!   compare them by identity ([`Arc::ptr_eq`]) as Dart's did.

#[cfg(test)]
mod live_edit_tests;
#[cfg(test)]
mod tests;

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use vocara_model::models::audio_fx::{
    FxKind, ReverbSettings, TrackFx, TrackSend, MAX_SENDS, SEND_OFF_DB,
};
use vocara_model::models::audio_track::{lane_visible_spans, AudioClip, AudioTrack};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_num::{clamp_f64, clamp_i64, floor_to_i64, pow_f64};
use vocara_model::util::dart_string;
use vocara_model::util::id::new_id;
use vocara_services::graph_audio_playback_service::{
    GraphAudioPlaybackService, SyncError, SyncHandle,
};
use vocara_services::playback_engine::{BusPlayback, PlaybackEngine, TrackPlayback};

/// One stretch of a clip that a lane plays (see `MixerController::heard_pieces`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Piece {
    start_ms: i64,
    duration_ms: i64,
    src_offset_ms: i64,
    fade_in: bool,
    fade_out: bool,
}

/// Whether a track is heard given its mute/solo and whether any track is
/// soloed. Mute *always* silences a track; solo only isolates which tracks
/// can be heard — so muting a soloed track still silences it (matches user
/// intent and avoids "M does nothing while a solo is active").
pub fn compute_audible(soloed: bool, muted: bool, any_solo: bool) -> bool {
    (if any_solo { soloed } else { true }) && !muted
}

/// The resolved solo state of the whole mix: which tracks and buses are part
/// of the current solo. When [`SoloResolution::any_solo`] is false (the
/// [`SoloResolution::all`] case) every source is "in solo" and only its own
/// mute matters. Built by [`resolve_solo`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoloResolution {
    tracks: HashSet<String>,
    buses: HashSet<String>,
    any_solo: bool,
}

impl SoloResolution {
    /// A solo in effect: exactly `tracks` and `buses` are heard.
    pub fn new(tracks: HashSet<String>, buses: HashSet<String>) -> SoloResolution {
        SoloResolution {
            tracks,
            buses,
            any_solo: true,
        }
    }

    /// Nothing soloed: every source is in solo.
    pub fn all() -> SoloResolution {
        SoloResolution {
            tracks: HashSet::new(),
            buses: HashSet::new(),
            any_solo: false,
        }
    }

    pub fn any_solo(&self) -> bool {
        self.any_solo
    }

    pub fn track_in_solo(&self, id: &str) -> bool {
        !self.any_solo || self.tracks.contains(id)
    }

    pub fn bus_in_solo(&self, id: &str) -> bool {
        !self.any_solo || self.buses.contains(id)
    }
}

/// Every bus a signal reaches from its main output `start_bus_id` and
/// `sends`, following bus outputs and further aux sends onward to the
/// master. A visited set guards routing cycles.
fn reachable_buses<'a>(
    bus_by_id: &HashMap<&'a str, &'a FxBus>,
    start_bus_id: Option<&str>,
    sends: &[TrackSend],
) -> HashSet<&'a str> {
    fn visit<'a>(
        bus_by_id: &HashMap<&'a str, &'a FxBus>,
        id: Option<&str>,
        result: &mut HashSet<&'a str>,
        queue: &mut Vec<&'a str>,
    ) {
        if let Some((&key, _)) = id.and_then(|id| bus_by_id.get_key_value(id)) {
            if result.insert(key) {
                queue.push(key);
            }
        }
    }
    // A send turned all the way down routes nothing (see `route_gain_to_bus`):
    // counted, soloing its bus kept the source's dry path playing too.
    let live = |s: &&TrackSend| s.level_db > SEND_OFF_DB;

    let mut result = HashSet::new();
    let mut queue = Vec::new();
    visit(bus_by_id, start_bus_id, &mut result, &mut queue);
    for s in sends.iter().filter(live) {
        visit(bus_by_id, Some(&s.bus_id), &mut result, &mut queue);
    }
    while let Some(id) = queue.pop() {
        let b = bus_by_id[id];
        visit(
            bus_by_id,
            b.output_bus_id.as_deref(),
            &mut result,
            &mut queue,
        );
        for s in b.sends.iter().filter(live) {
            visit(bus_by_id, Some(&s.bus_id), &mut result, &mut queue);
        }
    }
    result
}

/// `sends` less those into `bus_id` (all of them kept for the master, which
/// no send targets): what a channel keeps when its output moves to that bus.
fn without_sends_to(sends: &[TrackSend], bus_id: Option<&str>) -> Vec<TrackSend> {
    sends
        .iter()
        .filter(|s| Some(s.bus_id.as_str()) != bus_id)
        .cloned()
        .collect()
}

/// `{for (final b in buses) b.id: b}`: a later bus wins a duplicate id.
fn buses_by_id<B: Borrow<FxBus>>(buses: &[B]) -> HashMap<&str, &FxBus> {
    buses
        .iter()
        .map(|b| {
            let b = b.borrow();
            (b.id.as_str(), b)
        })
        .collect()
}

/// Resolves the whole mix's solo state in one pass. Solo is a view-model
/// concept the engine only ever sees as per-source mutes: with nothing
/// soloed everything (un-muted) plays; once any track *or* bus is soloed,
/// only the soloed sources and the buses that route them to the master stay
/// audible. Crucially, soloing a bus keeps the takes feeding it audible —
/// otherwise the bus would have no input and the solo would be silent.
///
/// Routing follows BOTH the main output (busId → outputBusId) and post-fader
/// aux sends (track/bus `sends`). A reverb / return bus reached only by a
/// send is part of the path, so soloing a take keeps its reverb tail and
/// soloing a send-fed return keeps its feeders audible (a muted source zeroes
/// its sends too, so the return would otherwise go silent). Pure — so it's
/// unit-tested.
pub fn resolve_solo<T: Borrow<AudioTrack>, B: Borrow<FxBus>>(
    tracks: &[T],
    buses: &[B],
) -> SoloResolution {
    let soloed_bus_ids: HashSet<&str> = buses
        .iter()
        .map(Borrow::borrow)
        .filter(|b| b.soloed)
        .map(|b| b.id.as_str())
        .collect();
    let any_solo = tracks.iter().any(|t| t.borrow().soloed) || !soloed_bus_ids.is_empty();
    if !any_solo {
        return SoloResolution::all();
    }

    let bus_by_id = buses_by_id(buses);
    let mut audible_tracks = HashSet::new();
    let mut audible_buses: HashSet<String> = HashSet::new();
    for t in tracks {
        let t = t.borrow();
        let reach = reachable_buses(&bus_by_id, t.bus_id.as_deref(), &t.sends);
        let feeds_soloed_bus = reach.iter().any(|b| soloed_bus_ids.contains(b));
        // Reuse the pure track rule, treating "feeds a soloed bus" as soloed.
        if compute_audible(t.soloed || feeds_soloed_bus, false, true) {
            audible_tracks.insert(t.id.clone());
            // Carry this source (dry + sends) onward.
            audible_buses.extend(reach.into_iter().map(str::to_string));
        }
    }
    // A soloed bus is heard itself plus everything it routes into, even with
    // no source feeding it yet.
    for id in &soloed_bus_ids {
        audible_buses.insert((*id).to_string());
        if let Some(b) = bus_by_id.get(id) {
            let reach = reachable_buses(&bus_by_id, b.output_bus_id.as_deref(), &b.sends);
            audible_buses.extend(reach.into_iter().map(str::to_string));
        }
    }
    SoloResolution::new(audible_tracks, audible_buses)
}

/// A mixer edit's ask of the view model: persist the model (Dart's `await
/// _save()`). The save's tail resyncs the engine with what it saved
/// ([`MixerController::resync_playback`]), failed or not.
#[must_use = "the view model saves the edited model (Dart awaited `_save()`)"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveRequest;

/// The engine the mixer drives: the transport seam for the live edits, and
/// how a graph sync is queued. The native graph engine queues its syncs on
/// its worker ([`GraphAudioPlaybackService::enqueue_sync`]); any other engine
/// (the null engine, a test fake) has no queue, and is synced in place,
/// which for those returns at once. It is also the view model's answer to
/// Dart's `_playback is GraphAudioPlaybackService`.
#[derive(Clone)]
pub enum EngineHandle {
    Graph(Arc<GraphAudioPlaybackService>),
    Other(Arc<dyn PlaybackEngine>),
}

impl EngineHandle {
    /// The transport seam.
    pub fn transport(&self) -> &dyn PlaybackEngine {
        match self {
            EngineHandle::Graph(g) => &**g,
            EngineHandle::Other(e) => &**e,
        }
    }

    /// The transport seam, shared (for the playback controller and the
    /// sibling controllers).
    pub fn shared(&self) -> Arc<dyn PlaybackEngine> {
        match self {
            EngineHandle::Graph(g) => Arc::clone(g) as Arc<dyn PlaybackEngine>,
            EngineHandle::Other(e) => Arc::clone(e),
        }
    }

    /// The native engine, when it is one (its devices, monitor, capture).
    pub fn graph(&self) -> Option<&Arc<GraphAudioPlaybackService>> {
        match self {
            EngineHandle::Graph(g) => Some(g),
            EngineHandle::Other(_) => None,
        }
    }

    /// Queues a sync to `tracks` and `buses` now (Dart's synchronous `sync()`
    /// prologue) and hands back its wait.
    pub fn queue_sync(&self, tracks: Vec<TrackPlayback>, buses: Vec<BusPlayback>) -> PendingSync {
        match self {
            EngineHandle::Graph(g) => PendingSync(Some(g.enqueue_sync(tracks, buses))),
            EngineHandle::Other(e) => {
                e.sync(tracks, buses);
                PendingSync(None)
            }
        }
    }
}

impl From<Arc<GraphAudioPlaybackService>> for EngineHandle {
    fn from(engine: Arc<GraphAudioPlaybackService>) -> EngineHandle {
        EngineHandle::Graph(engine)
    }
}

impl From<Arc<dyn PlaybackEngine>> for EngineHandle {
    fn from(engine: Arc<dyn PlaybackEngine>) -> EngineHandle {
        EngineHandle::Other(engine)
    }
}

impl fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EngineHandle::Graph(_) => "EngineHandle::Graph",
            EngineHandle::Other(_) => "EngineHandle::Other",
        })
    }
}

/// A queued graph sync (Dart's `sync()` future): wait for it off the UI
/// thread. Dropping it leaves the sync queued (Dart's `unawaited`).
#[must_use = "dropping it leaves the sync queued; wait (off the UI thread) where Dart awaited"]
pub struct PendingSync(Option<SyncHandle>);

impl PendingSync {
    /// Nothing to wait for.
    pub fn done() -> PendingSync {
        PendingSync(None)
    }

    /// Whether there is nothing to wait for (no practice, or an engine synced
    /// in place).
    pub fn is_done(&self) -> bool {
        self.0.is_none()
    }

    /// Blocks until this sync (and every one queued before it) is done, and
    /// says how it ended (Dart's future completing, or completing with its
    /// error).
    pub fn wait(self) -> Result<(), SyncError> {
        match self.0 {
            None => Ok(()),
            Some(handle) => handle.wait(),
        }
    }
}

impl fmt::Debug for PendingSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.is_done() {
            "PendingSync::Done"
        } else {
            "PendingSync::Queued"
        })
    }
}

/// What the mixer reads and edits on the view model that owns it (Dart's
/// constructor closures). The track helpers default to the view model's own
/// (`_trackById`, `_allTracks`, `_updateTrack`) over
/// [`MixerHost::practice`] and [`MixerHost::set_practice`].
pub trait MixerHost {
    /// The live practice model.
    fn practice(&self) -> Option<&SongPractice>;

    /// Replaces the model (the view model stays its single owner and
    /// persister).
    fn set_practice(&mut self, practice: SongPractice);

    /// A lane by id: the accompaniment, the reference, or a vocal take of
    /// any interpretation.
    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>> {
        let p = self.practice()?;
        if let Some(a) = p.accompaniment.as_ref().filter(|a| a.id == id) {
            return Some(Arc::clone(a));
        }
        if let Some(r) = p.reference.as_ref().filter(|r| r.id == id) {
            return Some(Arc::clone(r));
        }
        p.vocal_tracks.iter().find(|t| t.id == id).cloned()
    }

    /// The lanes loaded into playback and mixing: the backings and only the
    /// active interpretation's takes.
    fn all_tracks(&self) -> Vec<Arc<AudioTrack>> {
        let Some(p) = self.practice() else {
            return Vec::new();
        };
        let mut out: Vec<Arc<AudioTrack>> = Vec::new();
        out.extend(p.accompaniment.iter().cloned());
        out.extend(p.reference.iter().cloned());
        out.extend(p.active_vocal_tracks());
        out
    }

    /// Replaces lane `id` with `update` of it (in the model; nothing is
    /// saved).
    fn update_track(&mut self, id: &str, update: &dyn Fn(&AudioTrack) -> AudioTrack) {
        let Some(p) = self.practice() else {
            return;
        };
        let mut next = p.clone();
        if let Some(a) = next.accompaniment.as_ref().filter(|a| a.id == id) {
            next.accompaniment = Some(Arc::new(update(a)));
        } else if let Some(r) = next.reference.as_ref().filter(|r| r.id == id) {
            next.reference = Some(Arc::new(update(r)));
        } else {
            next.vocal_tracks = next
                .vocal_tracks
                .iter()
                .map(|t| {
                    if t.id == id {
                        Arc::new(update(t))
                    } else {
                        Arc::clone(t)
                    }
                })
                .collect();
        }
        self.set_practice(next);
    }

    fn toast(&mut self, message: &str);

    /// Selects a just-created/duplicated bus (bus and track selection are
    /// mutually exclusive — the view model clears the track/clip selection).
    fn select_bus_after_mutation(&mut self, bus_id: &str);

    /// Clears the bus selection when the deleted bus was selected.
    fn clear_bus_selection_if(&mut self, bus_id: &str);

    // Recording state: the take being punched into must be silent (you can't
    // monitor what you're laying down) and the live take folds into the
    // master display waveform while recording.

    fn is_recording(&self) -> bool;

    fn recording_target_id(&self) -> Option<&str>;

    fn recording_peaks(&self) -> &[f64];

    fn recording_start_ms(&self) -> i64;

    fn recording_duration_ms(&self) -> i64;

    /// The shared playhead, for the clip-under-playhead gain-reduction meter.
    fn position(&self) -> Duration;
}

/// The bus list with `bus_id`'s entries replaced by `update` of them.
fn with_bus(
    buses: &[Arc<FxBus>],
    bus_id: &str,
    update: impl Fn(&FxBus) -> FxBus,
) -> Vec<Arc<FxBus>> {
    buses
        .iter()
        .map(|b| {
            if b.id == bus_id {
                Arc::new(update(b))
            } else {
                Arc::clone(b)
            }
        })
        .collect()
}

/// A send's gain (linear), 0 when it is off.
fn send_gain(s: &TrackSend) -> f64 {
    if s.level_db <= SEND_OFF_DB {
        0.0
    } else {
        pow_f64(10.0, s.level_db / 20.0)
    }
}

/// What a lane envelope was folded from (Dart's `_envelope` key): the
/// timeline length, the master gain, the A/B backing, the recording state,
/// each bus's routing and each track's level and routing. Buses and tracks
/// are kept as the model's own [`Arc`]s: the same `Arc` is the same value,
/// and a new one is compared by the fields the envelopes read.
#[derive(Clone)]
struct EnvelopeKey {
    timeline: Option<i64>,
    master: f64,
    play_reference: bool,
    recording: bool,
    /// The take being laid down (silenced while recording).
    target: Option<String>,
    /// The live take (master lane only, while recording): peaks, start,
    /// duration.
    live: Option<(Vec<f64>, i64, i64)>,
    buses: Vec<Arc<FxBus>>,
    tracks: Vec<Arc<AudioTrack>>,
}

impl EnvelopeKey {
    /// Dart's `_sameKey`: values by value; a bus or track by identity, or —
    /// a new object, as every edit makes one — by the fields an envelope
    /// reads, which is never staler than Dart's identity test on its lists.
    fn matches(&self, other: &EnvelopeKey) -> bool {
        fn same_bus(a: &Arc<FxBus>, b: &Arc<FxBus>) -> bool {
            Arc::ptr_eq(a, b)
                || (a.id == b.id
                    && a.muted == b.muted
                    && a.soloed == b.soloed
                    && a.output_bus_id == b.output_bus_id
                    && a.sends == b.sends)
        }
        fn same_track(a: &Arc<AudioTrack>, b: &Arc<AudioTrack>) -> bool {
            Arc::ptr_eq(a, b)
                || (a.id == b.id
                    && a.muted == b.muted
                    && a.soloed == b.soloed
                    && a.volume == b.volume
                    && a.bus_id == b.bus_id
                    && a.sends == b.sends
                    && a.clips == b.clips)
        }
        self.timeline == other.timeline
            && self.master == other.master
            && self.play_reference == other.play_reference
            && self.recording == other.recording
            && self.target == other.target
            && self.live == other.live
            && self.buses.len() == other.buses.len()
            && self
                .buses
                .iter()
                .zip(&other.buses)
                .all(|(a, b)| same_bus(a, b))
            && self.tracks.len() == other.tracks.len()
            && self
                .tracks
                .iter()
                .zip(&other.tracks)
                .all(|(a, b)| same_track(a, b))
    }
}

/// A folded lane envelope and what it was folded from.
struct Envelope {
    key: EnvelopeKey,
    peaks: Arc<[f64]>,
}

/// Bins in a composite lane envelope.
const ENVELOPE_BINS: usize = 1000;

/// Everything the engine hears: audibility (mute/solo resolution incl. bus
/// routing), track/bus volume-pan-FX live edits + commits, aux sends, the
/// master bus, FX-bus CRUD/routing with cycle refusal, reverb,
/// gain-reduction meters, composite bus/master display waveforms, backing
/// A/B, and [`MixerController::resync_playback`] — the full engine graph
/// sync. See the module doc for how the view model drives it.
pub struct MixerController {
    engine: EngineHandle,

    /// When true the reference (original song) is the audible backing
    /// instead of the accompaniment.
    play_reference: bool,

    /// Set in [`MixerController::dispose`]. Guards notifications racing the
    /// tear-down.
    disposed: bool,

    revision: u64,

    /// The lane envelopes last folded, by bus id (`None`: the master), with
    /// what they were folded from. The master and bus lanes rebuild on every
    /// notification of the view model — a fader, knob or clip-drag tick, a
    /// pan, a selection — and each rebuild re-folded every clip's peaks: 36k
    /// samples over 12 three-clip lanes, ~0.5 ms in a debug build. Behind a
    /// lock because the lanes read it from `&self` (the view).
    envelopes: Mutex<HashMap<Option<String>, Envelope>>,

    /// Turns the envelope cache off (every read folds afresh): the oracle
    /// for tests that check the cache returns what a fresh fold would.
    no_envelope_cache: bool,
}

impl fmt::Debug for MixerController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MixerController")
            .field("engine", &self.engine)
            .field("play_reference", &self.play_reference)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl MixerController {
    pub fn new(engine: EngineHandle) -> MixerController {
        MixerController {
            engine,
            play_reference: false,
            disposed: false,
            revision: 0,
            envelopes: Mutex::new(HashMap::new()),
            no_envelope_cache: false,
        }
    }

    /// The engine this mixer drives.
    pub fn engine(&self) -> &EngineHandle {
        &self.engine
    }

    fn playback(&self) -> &dyn PlaybackEngine {
        self.engine.transport()
    }

    /// Bumped on every notification of this controller (Dart's
    /// `notifyListeners`); never after [`MixerController::dispose`].
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn notify(&mut self) {
        if self.disposed {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Silences every later notification.
    pub fn dispose(&mut self) {
        self.disposed = true;
    }

    /// Turns the envelope cache off (`true`: every read folds afresh) or back
    /// on: Dart's `debugNoEnvelopeCache`, per controller rather than global
    /// so tests running side by side cannot see each other's.
    #[doc(hidden)]
    pub fn set_debug_no_envelope_cache(&mut self, off: bool) {
        self.no_envelope_cache = off;
    }

    // --- Backing source (accompaniment ⇄ reference) -------------------------

    /// When true the reference (original song) is the audible backing
    /// instead of the accompaniment — a seamless A/B; the playhead is
    /// unaffected.
    pub fn play_reference(&self) -> bool {
        self.play_reference
    }

    /// The backing track currently selected for playback and display.
    pub fn active_backing<'h>(&self, host: &'h dyn MixerHost) -> Option<&'h Arc<AudioTrack>> {
        let p = host.practice()?;
        if self.play_reference {
            p.reference.as_ref()
        } else {
            p.accompaniment.as_ref()
        }
    }

    pub fn set_play_reference(&mut self, host: &dyn MixerHost, value: bool) {
        if self.play_reference == value {
            return;
        }
        self.play_reference = value;
        self.apply_audibility(host);
        self.notify();
    }

    /// Sets the backing A/B flag WITHOUT touching the engine or notifying —
    /// for the separation flow, whose save → resync applies the mix. (Its
    /// failure path restores the previous flag this way too, then resyncs:
    /// the failed save resynced the engine under the flipped flag, and the
    /// undo changed the lanes after it.)
    pub fn set_play_reference_silently(&mut self, value: bool) {
        self.play_reference = value;
    }

    /// Notes on the lanes the workspace is actually showing: the active
    /// backing (accompaniment or reference, per
    /// [`MixerController::play_reference`]) plus the active interpretation's
    /// takes. The hidden backing's notes stay out, so the count shown next to
    /// the take count can never exceed what the user can see. Lives here
    /// rather than on the model because the A/B choice is transient
    /// view-state owned by this controller — `SongPractice::active_note_count`
    /// deliberately counts both backings (see its doc).
    pub fn visible_note_count(&self, host: &dyn MixerHost) -> usize {
        let Some(practice) = host.practice() else {
            return 0;
        };
        let mut count = self.active_backing(host).map_or(0, |b| b.notes.len());
        for t in practice.active_vocal_tracks() {
            count += t.notes.len();
        }
        count
    }

    // --- Audibility (mute / solo resolution) ---------------------------------

    /// True if a track should be heard, given the resolved `solo` state. Only
    /// the *active* backing (accompaniment or reference, per
    /// `play_reference`) is ever audible; the other backing is always silent.
    /// While recording, the take being punched into is silenced (you can't
    /// monitor what you're laying down) but every other lane still obeys
    /// mute/solo. Otherwise mute always silences and solo isolates.
    fn audible_track(&self, host: &dyn MixerHost, t: &AudioTrack, solo: &SoloResolution) -> bool {
        if t.is_accompaniment() && self.play_reference {
            return false;
        }
        if t.is_reference() && !self.play_reference {
            return false;
        }
        if host.is_recording() && host.recording_target_id() == Some(t.id.as_str()) {
            return false;
        }
        if t.muted {
            return false;
        }
        solo.track_in_solo(&t.id)
    }

    /// True if a bus passes audio given the resolved `solo` state — muted
    /// always silences; otherwise the bus is heard only when it's part of the
    /// solo (soloed itself, fed by a soloed source, or carrying a soloed bus
    /// onward).
    fn audible_bus(b: &FxBus, solo: &SoloResolution) -> bool {
        !b.muted && solo.bus_in_solo(&b.id)
    }

    /// The mix's current solo state — see [`resolve_solo`]. The INACTIVE
    /// backing is left out: `audible_track` silences it regardless, and it is
    /// not shown anywhere, so a solo left lit on it (solo the original,
    /// switch back to the accompaniment) must not count toward "anything
    /// soloed" — that silenced every visible lane with no lit S in sight.
    fn resolve_solo(&self, host: &dyn MixerHost) -> SoloResolution {
        let tracks: Vec<Arc<AudioTrack>> = host
            .all_tracks()
            .into_iter()
            .filter(|t| {
                !(t.is_accompaniment() && self.play_reference)
                    && !(t.is_reference() && !self.play_reference)
            })
            .collect();
        resolve_solo(&tracks, self.fx_buses(host))
    }

    /// Pushes the current mute/solo state of every track *and* bus to the
    /// audio engine immediately, so M/S respond on click without waiting for
    /// the save round-trip.
    fn apply_audibility(&self, host: &dyn MixerHost) {
        let solo = self.resolve_solo(host);
        for t in host.all_tracks() {
            let muted = !self.audible_track(host, &t, &solo);
            // The engine plays one source per clip — push the lane's level to
            // each.
            for c in &t.clips {
                self.playback().set_volume(&c.id, t.volume, muted);
            }
        }
        for b in self.fx_buses(host) {
            self.playback()
                .set_bus_volume(&b.id, b.volume, !Self::audible_bus(b, &solo));
        }
    }

    pub fn toggle_mute(&mut self, host: &mut dyn MixerHost, track_id: &str) -> SaveRequest {
        host.update_track(track_id, &|t| AudioTrack {
            muted: !t.muted,
            ..t.clone()
        });
        self.apply_audibility(host);
        self.notify();
        SaveRequest
    }

    pub fn toggle_solo(&mut self, host: &mut dyn MixerHost, track_id: &str) -> SaveRequest {
        host.update_track(track_id, &|t| AudioTrack {
            soloed: !t.soloed,
            ..t.clone()
        });
        self.apply_audibility(host);
        self.notify();
        SaveRequest
    }

    // --- Track level / pan ----------------------------------------------------

    /// Applies a volume change live (model + audio) without persisting — call
    /// on every slider tick. Persist once with
    /// [`MixerController::commit_track_volume`] on release.
    pub fn set_track_volume_live(&mut self, host: &mut dyn MixerHost, track_id: &str, volume: f64) {
        host.update_track(track_id, &|t| AudioTrack {
            volume,
            ..t.clone()
        });
        if let Some(track) = host.track_by_id(track_id) {
            let muted = !self.audible_track(host, &track, &self.resolve_solo(host));
            for c in &track.clips {
                self.playback().set_volume(&c.id, track.volume, muted);
            }
        }
        self.notify();
    }

    pub fn commit_track_volume(&self) -> SaveRequest {
        SaveRequest
    }

    /// Applies a pan change live (model + audio) without persisting — call on
    /// every knob tick. Persist once with
    /// [`MixerController::commit_track_volume`] on release.
    pub fn set_track_pan_live(&mut self, host: &mut dyn MixerHost, track_id: &str, pan: f64) {
        let p = clamp_f64(pan, -1.0, 1.0);
        host.update_track(track_id, &|t| AudioTrack {
            pan: p,
            ..t.clone()
        });
        if let Some(track) = host.track_by_id(track_id) {
            for c in &track.clips {
                self.playback().set_track_pan(&c.id, p);
            }
        }
        self.notify();
    }

    // --- Effects (EQ / compressor / shared reverb) ---------------------------

    /// Whether the active playback engine can apply effects (only the native
    /// engine can).
    pub fn supports_effects(&self) -> bool {
        self.playback().supports_effects()
    }

    /// The shared reverb settings for this practice (the default until
    /// edited).
    pub fn reverb_settings(&self, host: &dyn MixerHost) -> ReverbSettings {
        host.practice()
            .and_then(|p| p.reverb)
            .unwrap_or(ReverbSettings::DEFAULT)
    }

    /// Live shared-reverb edit (model + engine) without persisting.
    pub fn set_reverb_live(&mut self, host: &mut dyn MixerHost, reverb: ReverbSettings) {
        let Some(practice) = host.practice() else {
            return;
        };
        let next = SongPractice {
            reverb: Some(reverb),
            ..practice.clone()
        };
        host.set_practice(next);
        self.playback().set_reverb(&reverb);
        self.notify();
    }

    /// Live FX edit for a track (model + engine) without persisting — call on
    /// every drag tick; persist once with [`MixerController::commit_fx`] on
    /// release.
    ///
    /// In the chain order the model holds now: only
    /// [`MixerController::reorder_track_inserts`] changes it, and an edit
    /// built from an FX panel's earlier build (an on/off clicked while a
    /// reorder's save was landing) wrote the old order back — the cards
    /// snapped back and the reorder was lost.
    pub fn set_track_fx_live(&mut self, host: &mut dyn MixerHost, track_id: &str, mut fx: TrackFx) {
        if let Some(live) = host.track_by_id(track_id).and_then(|t| t.fx.clone()) {
            fx.order = live.order;
        }
        host.update_track(track_id, &|t| AudioTrack {
            fx: Some(fx.clone()),
            ..t.clone()
        });
        // The lane's FX applies to each of its clip sources in the engine.
        if let Some(track) = host.track_by_id(track_id) {
            for c in &track.clips {
                self.playback().set_track_fx(&c.id, &fx);
            }
        }
        self.notify();
    }

    pub fn commit_fx(&self) -> SaveRequest {
        SaveRequest
    }

    /// Reorders a vocal take's insert chain (the audible plugin order).
    /// Persists + rebuilds the engine graph (the chain order is graph
    /// wiring).
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn reorder_track_inserts(
        &mut self,
        host: &mut dyn MixerHost,
        track_id: &str,
        order: Vec<FxKind>,
    ) -> Option<SaveRequest> {
        let track = host.track_by_id(track_id)?;
        let fx = TrackFx {
            order,
            ..track.fx.clone().unwrap_or_else(TrackFx::flat)
        };
        host.update_track(track_id, &|t| AudioTrack {
            fx: Some(fx.clone()),
            ..t.clone()
        });
        self.notify();
        Some(SaveRequest)
    }

    /// Reorders an FX bus's insert chain. Persists + rebuilds the graph.
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn reorder_bus_inserts(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        order: Vec<FxKind>,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        practice.bus_by_id(Some(bus_id))?;
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                fx: TrackFx {
                    order: order.clone(),
                    ..b.fx.clone()
                },
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    // --- Aux sends (post-fader, to FX buses) --------------------------------

    /// Adds a post-fader send from a take to `bus_id` (max [`MAX_SENDS`], no
    /// dupes, and never to the bus its output already feeds: the bus would
    /// hear the take twice).
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn add_track_send(
        &mut self,
        host: &mut dyn MixerHost,
        track_id: &str,
        bus_id: &str,
    ) -> Option<SaveRequest> {
        let track = host.track_by_id(track_id)?;
        if track.sends.len() >= MAX_SENDS
            || track.sends.iter().any(|s| s.bus_id == bus_id)
            || track.bus_id.as_deref() == Some(bus_id)
        {
            return None;
        }
        host.update_track(track_id, &|t| {
            let mut sends = t.sends.clone();
            sends.push(TrackSend::new(bus_id, 0.0));
            AudioTrack { sends, ..t.clone() }
        });
        self.notify();
        Some(SaveRequest)
    }

    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn remove_track_send(
        &mut self,
        host: &mut dyn MixerHost,
        track_id: &str,
        slot: usize,
    ) -> Option<SaveRequest> {
        let track = host.track_by_id(track_id)?;
        if slot >= track.sends.len() {
            return None;
        }
        let mut next = track.sends.clone();
        next.remove(slot);
        host.update_track(track_id, &|t| AudioTrack {
            sends: next.clone(),
            ..t.clone()
        });
        // Shown now, as every routing edit here is: the save queues, then
        // rebuilds the engine graph before it notifies, and a second click on
        // the same row meanwhile removed the send that had moved into its
        // slot.
        self.notify();
        Some(SaveRequest)
    }

    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn set_track_send_target(
        &mut self,
        host: &mut dyn MixerHost,
        track_id: &str,
        slot: usize,
        bus_id: &str,
    ) -> Option<SaveRequest> {
        let track = host.track_by_id(track_id)?;
        if slot >= track.sends.len() {
            return None;
        }
        // Two slots pointing at one bus feed it twice, so the bus hears the
        // sum of both faders and neither one reads what you hear.
        // `add_track_send` refuses it; the menu hides an already-used bus.
        // This is the backstop for the window where the menu's options were
        // computed from a `sends` the save behind a previous retarget has not
        // rebuilt yet.
        if track.sends[slot].bus_id == bus_id {
            return None;
        }
        // The output's bus likewise: it already hears the take.
        if track.sends.iter().any(|s| s.bus_id == bus_id) || track.bus_id.as_deref() == Some(bus_id)
        {
            return None;
        }
        let mut next = track.sends.clone();
        next[slot].bus_id = bus_id.to_string();
        host.update_track(track_id, &|t| AudioTrack {
            sends: next.clone(),
            ..t.clone()
        });
        self.notify();
        Some(SaveRequest)
    }

    /// Live send-level edit (model + engine) without persisting.
    pub fn set_track_send_level_live(
        &mut self,
        host: &mut dyn MixerHost,
        track_id: &str,
        slot: usize,
        level_db: f64,
    ) {
        let Some(track) = host.track_by_id(track_id) else {
            return;
        };
        if slot >= track.sends.len() {
            return;
        }
        let mut next = track.sends.clone();
        let was_off = next[slot].level_db <= SEND_OFF_DB;
        next[slot].level_db = level_db;
        host.update_track(track_id, &|t| AudioTrack {
            sends: next.clone(),
            ..t.clone()
        });
        for c in &track.clips {
            self.playback().set_track_send_level(&c.id, slot, level_db);
        }
        // A send turned off or back on is a route gone or come back, which a
        // soloed bus's feeders are judged by (see `resolve_solo`): left to the
        // release, the take stayed muted, or its dry path played, while
        // dragged.
        if was_off != (level_db <= SEND_OFF_DB) {
            self.apply_audibility(host);
        }
        self.notify();
    }

    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn add_bus_send(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        target_bus_id: &str,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        let bus = practice.bus_by_id(Some(bus_id))?;
        if bus_id == target_bus_id
            || bus.sends.len() >= MAX_SENDS
            || bus.sends.iter().any(|s| s.bus_id == target_bus_id)
            || bus.output_bus_id.as_deref() == Some(target_bus_id)
        {
            return None;
        }
        if self.bus_route_would_cycle(host, bus_id, Some(target_bus_id)) {
            host.toast("Can't send there — it would create a feedback loop.");
            return None;
        }
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| {
                let mut sends = b.sends.clone();
                sends.push(TrackSend::new(target_bus_id, 0.0));
                FxBus { sends, ..b.clone() }
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn remove_bus_send(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        slot: usize,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        let bus = practice.bus_by_id(Some(bus_id))?;
        if slot >= bus.sends.len() {
            return None;
        }
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| {
                let mut sends = b.sends.clone();
                sends.remove(slot);
                FxBus { sends, ..b.clone() }
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn set_bus_send_target(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        slot: usize,
        target_bus_id: &str,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        let bus = practice.bus_by_id(Some(bus_id))?;
        if bus_id == target_bus_id || slot >= bus.sends.len() {
            return None;
        }
        // Same duplicate-target backstop as `set_track_send_target`.
        if bus.sends[slot].bus_id == target_bus_id {
            return None;
        }
        if bus.sends.iter().any(|s| s.bus_id == target_bus_id)
            || bus.output_bus_id.as_deref() == Some(target_bus_id)
        {
            return None;
        }
        if self.bus_route_would_cycle(host, bus_id, Some(target_bus_id)) {
            host.toast("Can't send there — it would create a feedback loop.");
            return None;
        }
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| {
                let mut sends = b.sends.clone();
                if let Some(s) = sends.get_mut(slot) {
                    s.bus_id = target_bus_id.to_string();
                }
                FxBus { sends, ..b.clone() }
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    pub fn set_bus_send_level_live(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        slot: usize,
        level_db: f64,
    ) {
        let Some(practice) = host.practice() else {
            return;
        };
        let Some(bus) = practice.bus_by_id(Some(bus_id)) else {
            return;
        };
        if slot >= bus.sends.len() {
            return;
        }
        let was_off = bus.sends[slot].level_db <= SEND_OFF_DB;
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| {
                let mut sends = b.sends.clone();
                if let Some(s) = sends.get_mut(slot) {
                    s.level_db = level_db;
                }
                FxBus { sends, ..b.clone() }
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.playback().set_bus_send_level(bus_id, slot, level_db);
        if was_off != (level_db <= SEND_OFF_DB) {
            self.apply_audibility(host); // as in `set_track_send_level_live`
        }
        self.notify();
    }

    // --- Master bus ----------------------------------------------------------

    /// The master bus output gain (linear, 1.0 = unity) for the whole mix.
    pub fn master_volume(&self, host: &dyn MixerHost) -> f64 {
        host.practice().map_or(1.0, |p| p.master_volume)
    }

    /// Live master-volume edit (model + engine) without persisting — call on
    /// every fader tick; persist once with
    /// [`MixerController::commit_master_volume`] on release.
    pub fn set_master_volume_live(&mut self, host: &mut dyn MixerHost, gain: f64) {
        let Some(practice) = host.practice() else {
            return;
        };
        let next = SongPractice {
            master_volume: gain,
            ..practice.clone()
        };
        host.set_practice(next);
        self.playback().set_master_volume(gain);
        self.notify();
    }

    pub fn commit_master_volume(&self) -> SaveRequest {
        SaveRequest
    }

    // --- FX buses (DAW group / aux tracks) ----------------------------------

    pub fn fx_buses<'h>(&self, host: &'h dyn MixerHost) -> &'h [Arc<FxBus>] {
        host.practice().map_or(&[], |p| &p.fx_buses)
    }

    /// `name_for` of the first n from `first` on that no bus is called. The
    /// output and send menus list buses by name alone, so two buses named
    /// alike could not be told apart there: a default 'Bus N' (N the count
    /// plus one) after a delete, or a second copy of one bus (the
    /// interpretations' default names had the same fix).
    fn unused_bus_name(
        buses: &[Arc<FxBus>],
        name_for: impl Fn(i64) -> String,
        first: i64,
    ) -> String {
        let taken: HashSet<&str> = buses.iter().map(|b| b.name.as_str()).collect();
        let mut n = first;
        while taken.contains(name_for(n).as_str()) {
            n += 1;
        }
        name_for(n)
    }

    /// Creates a new (transparent) FX bus and selects it. Returns its id
    /// (`None` without a practice, where Dart returned `''` unsaved).
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn create_bus(
        &mut self,
        host: &mut dyn MixerHost,
        name: &str,
    ) -> Option<(String, SaveRequest)> {
        let practice = host.practice()?;
        let trimmed = dart_string::trim(name);
        let bus = FxBus::new(
            new_id(),
            if trimmed.is_empty() {
                Self::unused_bus_name(
                    &practice.fx_buses,
                    |n| format!("Bus {n}"),
                    practice.fx_buses.len() as i64 + 1,
                )
            } else {
                trimmed.to_string()
            },
            TrackFx::flat(),
        );
        let id = bus.id.clone();
        let mut fx_buses = practice.fx_buses.clone();
        fx_buses.push(Arc::new(bus));
        let next = SongPractice {
            fx_buses,
            ..practice.clone()
        };
        host.set_practice(next);
        host.select_bus_after_mutation(&id);
        self.notify();
        Some((id, SaveRequest))
    }

    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn rename_bus(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        name: &str,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        let trimmed = dart_string::trim(name);
        if trimmed.is_empty() {
            return None;
        }
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                name: trimmed.to_string(),
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    /// Deletes a bus: un-routes any take feeding it and any bus whose output
    /// points at it (both fall back to the master), then removes it. Triggers
    /// a rebuild.
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn delete_bus(&mut self, host: &mut dyn MixerHost, bus_id: &str) -> Option<SaveRequest> {
        let practice = host.practice()?;
        // A take or bus the delete does not touch keeps its very object (Dart
        // copied every one; the values are the same).
        let vocal_tracks = practice
            .vocal_tracks
            .iter()
            .map(|t| {
                if t.bus_id.as_deref() != Some(bus_id) && t.sends.iter().all(|s| s.bus_id != bus_id)
                {
                    return Arc::clone(t);
                }
                Arc::new(AudioTrack {
                    bus_id: if t.bus_id.as_deref() == Some(bus_id) {
                        None
                    } else {
                        t.bus_id.clone()
                    },
                    sends: t
                        .sends
                        .iter()
                        .filter(|s| s.bus_id != bus_id)
                        .cloned()
                        .collect(),
                    ..(**t).clone()
                })
            })
            .collect();
        let fx_buses = practice
            .fx_buses
            .iter()
            .filter(|b| b.id != bus_id)
            .map(|b| {
                if b.output_bus_id.as_deref() != Some(bus_id)
                    && b.sends.iter().all(|s| s.bus_id != bus_id)
                {
                    return Arc::clone(b);
                }
                Arc::new(FxBus {
                    output_bus_id: if b.output_bus_id.as_deref() == Some(bus_id) {
                        None
                    } else {
                        b.output_bus_id.clone()
                    },
                    sends: b
                        .sends
                        .iter()
                        .filter(|s| s.bus_id != bus_id)
                        .cloned()
                        .collect(),
                    ..(**b).clone()
                })
            })
            .collect();
        let next = SongPractice {
            vocal_tracks,
            fx_buses,
            ..practice.clone()
        };
        host.clear_bus_selection_if(bus_id);
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    /// Routes a vocal take's output to `bus_id` (`None` = master). A send the
    /// take had into that bus goes: the output feeds the bus now, and the
    /// send beside it fed it the take a second time. Triggers a rebuild.
    pub fn set_track_bus(
        &mut self,
        host: &mut dyn MixerHost,
        track_id: &str,
        bus_id: Option<&str>,
    ) -> SaveRequest {
        host.update_track(track_id, &|t| AudioTrack {
            bus_id: bus_id.map(str::to_string),
            sends: without_sends_to(&t.sends, bus_id),
            ..t.clone()
        });
        self.notify();
        SaveRequest
    }

    /// Whether routing `from_bus_id` into `to_bus_id` (as its output or an
    /// aux send) would close a feedback loop — i.e. `to_bus_id` can already
    /// reach `from_bus_id` through bus outputs and aux sends. The engine
    /// REJECTS a cyclic graph and silently keeps the old one, so the routing
    /// UI must refuse the edit instead.
    fn bus_route_would_cycle(
        &self,
        host: &dyn MixerHost,
        from_bus_id: &str,
        to_bus_id: Option<&str>,
    ) -> bool {
        let (Some(to_bus_id), Some(practice)) = (to_bus_id, host.practice()) else {
            return false;
        };
        if to_bus_id == from_bus_id {
            return true;
        }
        let by_id = buses_by_id(&practice.fx_buses);
        let mut seen: HashSet<&str> = HashSet::new();
        let mut queue: Vec<&str> = vec![to_bus_id];
        while let Some(id) = queue.pop() {
            if id == from_bus_id {
                return true;
            }
            if !seen.insert(id) {
                continue;
            }
            let Some(b) = by_id.get(id) else {
                continue;
            };
            if let Some(out) = &b.output_bus_id {
                queue.push(out);
            }
            for s in &b.sends {
                queue.push(&s.bus_id);
            }
        }
        false
    }

    /// Routes a bus's output to another bus, or to the master
    /// (`output_bus_id` `None`). Self-routing and feedback loops are refused;
    /// a send into the new output's bus goes, as [`Self::set_track_bus`]'s
    /// does. Rebuilds.
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn set_bus_output(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        output_bus_id: Option<&str>,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        if Some(bus_id) == output_bus_id {
            return None;
        }
        if self.bus_route_would_cycle(host, bus_id, output_bus_id) {
            host.toast("Can't route there — it would create a feedback loop.");
            return None;
        }
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                output_bus_id: output_bus_id.map(str::to_string),
                sends: without_sends_to(&b.sends, output_bus_id),
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    /// Live bus FX edit (model + engine) without persisting — persist with
    /// [`MixerController::commit_fx`]. Kept in the chain order the model
    /// holds (see [`MixerController::set_track_fx_live`]).
    pub fn set_bus_fx_live(&mut self, host: &mut dyn MixerHost, bus_id: &str, mut fx: TrackFx) {
        let Some(practice) = host.practice() else {
            return;
        };
        if let Some(live) = practice.bus_by_id(Some(bus_id)) {
            fx.order = live.fx.order.clone();
        }
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                fx: fx.clone(),
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.playback().set_bus_fx(bus_id, &fx);
        self.notify();
    }

    /// Live bus fader edit (model + engine) without persisting.
    pub fn set_bus_volume_live(&mut self, host: &mut dyn MixerHost, bus_id: &str, volume: f64) {
        let Some(practice) = host.practice() else {
            return;
        };
        let Some(bus) = practice.bus_by_id(Some(bus_id)) else {
            return;
        };
        let updated = Arc::new(FxBus {
            volume,
            ..(**bus).clone()
        });
        let next = SongPractice {
            fx_buses: practice
                .fx_buses
                .iter()
                .map(|b| {
                    if b.id == bus_id {
                        Arc::clone(&updated)
                    } else {
                        Arc::clone(b)
                    }
                })
                .collect(),
            ..practice.clone()
        };
        host.set_practice(next);
        let muted = !Self::audible_bus(&updated, &self.resolve_solo(host));
        self.playback().set_bus_volume(bus_id, volume, muted);
        self.notify();
    }

    /// Live bus pan edit (model + engine) without persisting.
    pub fn set_bus_pan_live(&mut self, host: &mut dyn MixerHost, bus_id: &str, pan: f64) {
        let Some(practice) = host.practice() else {
            return;
        };
        let p = clamp_f64(pan, -1.0, 1.0);
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                pan: p,
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.playback().set_bus_pan(bus_id, p);
        self.notify();
    }

    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn toggle_bus_mute(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        practice.bus_by_id(Some(bus_id))?;
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                muted: !b.muted,
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.apply_audibility(host);
        self.notify();
        Some(SaveRequest)
    }

    /// Toggles a bus's solo. Soloing a bus isolates it (and the takes feeding
    /// it) across the whole mix; see [`resolve_solo`].
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn toggle_bus_solo(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        practice.bus_by_id(Some(bus_id))?;
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                soloed: !b.soloed,
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.apply_audibility(host);
        self.notify();
        Some(SaveRequest)
    }

    /// Sets a bus's lane tint (ARGB). Purely visual; persisting it goes
    /// through the shared save path (which resyncs the graph, harmlessly)
    /// like the track-color edit.
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn set_bus_color(
        &mut self,
        host: &mut dyn MixerHost,
        bus_id: &str,
        value: i64,
    ) -> Option<SaveRequest> {
        let practice = host.practice()?;
        let next = SongPractice {
            fx_buses: with_bus(&practice.fx_buses, bus_id, |b| FxBus {
                color_value: Some(value),
                ..b.clone()
            }),
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify();
        Some(SaveRequest)
    }

    /// Duplicates a bus (its chain, color, level, routing and solo/mute
    /// state) and inserts the copy right after it, then selects it.
    #[must_use = "the view model saves the edited model when it is Some"]
    pub fn duplicate_bus(&mut self, host: &mut dyn MixerHost, bus_id: &str) -> Option<SaveRequest> {
        let practice = host.practice()?;
        let src = practice.bus_by_id(Some(bus_id))?;
        // Every field but the id and the name: a duplicate the user cannot
        // hear is not a duplicate. `soloed` was the one left out, so
        // duplicating a bus WHILE it was soloed produced a copy muted by the
        // solo it did not join — and duplicating a track (a struct copy) has
        // always carried it.
        let copy = FxBus {
            id: new_id(),
            name: Self::unused_bus_name(
                &practice.fx_buses,
                |n| {
                    if n == 1 {
                        format!("{} copy", src.name)
                    } else {
                        format!("{} copy {n}", src.name)
                    }
                },
                1,
            ),
            ..(**src).clone()
        };
        let copy_id = copy.id.clone();
        let mut list = practice.fx_buses.clone();
        let at = list
            .iter()
            .position(|b| b.id == bus_id)
            .map_or(list.len(), |idx| idx + 1);
        list.insert(at, Arc::new(copy));
        let next = SongPractice {
            fx_buses: list,
            ..practice.clone()
        };
        host.set_practice(next);
        host.select_bus_after_mutation(&copy_id);
        self.notify();
        Some(SaveRequest)
    }

    /// The bus compressor's current gain reduction (dB, >= 0) — for a meter.
    pub fn bus_gain_reduction_db(&self, bus_id: &str) -> f64 {
        self.playback().bus_gain_reduction_db(bus_id)
    }

    // --- Composite display waveforms -----------------------------------------

    /// The strongest gain with which a source routed to `start_bus_id` and
    /// carrying `sends` reaches `target_bus_id` — 0 when it never does.
    ///
    /// Follows BOTH edge kinds, like [`resolve_solo`]'s reachability: a main
    /// output contributes unity, an aux send contributes its level. Following
    /// only the main output left a bus fed purely by sends (the ordinary
    /// reverb return) drawing a flat line while it was plainly audible,
    /// because the real graph does pass sends to the engine.
    ///
    /// Relaxation, not plain reachability: a bus keeps the best gain seen so
    /// far and is re-visited only when that improves. Every edge gain is <=
    /// 1, so the values decrease monotonically and this terminates even on a
    /// routing cycle (which `add_bus_send` / `set_bus_send_target` already
    /// reject — this is belt and braces).
    fn route_gain_to_bus(
        &self,
        host: &dyn MixerHost,
        target_bus_id: &str,
        start_bus_id: Option<&str>,
        sends: &[TrackSend],
    ) -> f64 {
        struct Walk<'a> {
            bus_by_id: HashMap<&'a str, &'a FxBus>,
            best: HashMap<&'a str, f64>,
            queue: Vec<&'a str>,
        }
        impl<'a> Walk<'a> {
            fn relax(&mut self, id: Option<&str>, gain: f64) {
                let Some(id) = id else {
                    return;
                };
                if gain <= 0.0 {
                    return;
                }
                let Some((&key, _)) = self.bus_by_id.get_key_value(id) else {
                    return;
                };
                if self.best.get(key).is_some_and(|&prev| prev >= gain) {
                    return;
                }
                self.best.insert(key, gain);
                self.queue.push(key);
            }
        }

        let mut w = Walk {
            bus_by_id: buses_by_id(self.fx_buses(host)),
            best: HashMap::new(),
            queue: Vec::new(),
        };
        w.relax(start_bus_id, 1.0);
        for s in sends {
            w.relax(Some(&s.bus_id), send_gain(s));
        }
        while let Some(id) = w.queue.pop() {
            let g = w.best[id];
            let b = w.bus_by_id[id];
            w.relax(b.output_bus_id.as_deref(), g);
            for s in &b.sends {
                w.relax(Some(&s.bus_id), g * send_gain(s));
            }
        }
        w.best.get(target_bus_id).copied().unwrap_or(0.0)
    }

    /// The strongest gain with which a take routed to `start_bus_id` and
    /// carrying `sends` reaches the master through buses `solo` lets pass — 0
    /// when every path is muted. A take routed into a muted bus still drew in
    /// the master lane, though nothing of it was heard. Bus levels are not
    /// modelled, as in the bus lanes; a bus id that names no bus counts as
    /// the master, as it did before.
    fn route_gain_to_master(
        &self,
        host: &dyn MixerHost,
        start_bus_id: Option<&str>,
        sends: &[TrackSend],
        solo: &SoloResolution,
    ) -> f64 {
        struct Walk<'a, 's> {
            bus_by_id: HashMap<&'a str, &'a FxBus>,
            solo: &'s SoloResolution,
            master: f64,
            best: HashMap<&'a str, f64>,
            queue: Vec<&'a str>,
        }
        impl<'a> Walk<'a, '_> {
            fn relax(&mut self, id: Option<&str>, gain: f64) {
                if gain <= 0.0 {
                    return;
                }
                let Some(id) = id else {
                    if gain > self.master {
                        self.master = gain;
                    }
                    return;
                };
                let Some((&key, &b)) = self.bus_by_id.get_key_value(id) else {
                    return;
                };
                if !MixerController::audible_bus(b, self.solo) {
                    return;
                }
                if self.best.get(key).is_some_and(|&prev| prev >= gain) {
                    return;
                }
                self.best.insert(key, gain);
                self.queue.push(key);
            }
        }

        let bus_by_id = buses_by_id(self.fx_buses(host));
        let master = match start_bus_id {
            Some(id) if bus_by_id.contains_key(id) => 0.0,
            _ => 1.0,
        };
        let mut w = Walk {
            bus_by_id,
            solo,
            master,
            best: HashMap::new(),
            queue: Vec::new(),
        };
        if start_bus_id.is_some() {
            w.relax(start_bus_id, 1.0);
        }
        for s in sends {
            w.relax(Some(&s.bus_id), send_gain(s));
        }
        while let Some(id) = w.queue.pop() {
            let g = w.best[id];
            let b = w.bus_by_id[id];
            w.relax(b.output_bus_id.as_deref(), g);
            for s in &b.sends {
                w.relax(Some(&s.bus_id), g * send_gain(s));
            }
        }
        w.master
    }

    fn envelopes(&self) -> MutexGuard<'_, HashMap<Option<String>, Envelope>> {
        self.envelopes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// What lane `bus_id`'s envelope (`None`: the master's) reads now.
    fn envelope_key(&self, host: &dyn MixerHost, master_lane: bool) -> EnvelopeKey {
        let recording = host.is_recording();
        EnvelopeKey {
            timeline: host.practice().map(|p| p.timeline_duration_ms),
            master: self.master_volume(host),
            play_reference: self.play_reference,
            recording,
            // Silenced while laid down.
            target: if recording {
                host.recording_target_id().map(str::to_string)
            } else {
                None
            },
            // Only the master lane folds the live take in. Keyed for a bus
            // too, every record tick (20 a second) refolded and repainted each
            // bus lane into the same envelope.
            live: if recording && master_lane {
                Some((
                    host.recording_peaks().to_vec(),
                    host.recording_start_ms(),
                    host.recording_duration_ms(),
                ))
            } else {
                None
            },
            buses: self.fx_buses(host).to_vec(),
            tracks: host.all_tracks(),
        }
    }

    /// `fold`'s envelope for `bus_id`, refolded only when what it reads
    /// changed: the timeline length, the master gain, the A/B backing, each
    /// bus's mute, solo and routing, each track's mute, solo, level, routing
    /// and clips, and the take being recorded (its live envelope for the
    /// master only). Not a pan or FX tick, nor a bus's level, pan or FX,
    /// which no envelope shows: keyed on whole tracks and the bus list, each
    /// of those refolded the master and every bus lane at pointer rate.
    fn envelope(
        &self,
        host: &dyn MixerHost,
        bus_id: Option<&str>,
        fold: impl FnOnce() -> Vec<f64>,
    ) -> Arc<[f64]> {
        if self.no_envelope_cache {
            return fold().into();
        }
        let key = self.envelope_key(host, bus_id.is_none());
        let slot = bus_id.map(str::to_string);
        if let Some(memo) = self.envelopes().get_mut(&slot) {
            if memo.key.matches(&key) {
                // The same inputs: adopt the newer objects, so the next read
                // matches them by identity.
                memo.key = key;
                return Arc::clone(&memo.peaks);
            }
        }
        let peaks: Arc<[f64]> = fold().into();
        self.envelopes().insert(
            slot,
            Envelope {
                key,
                peaks: Arc::clone(&peaks),
            },
        );
        peaks
    }

    /// Folds `peaks` (spread over `duration_ms` from `start_ms`, scaled by
    /// `vol`) into `out` by max over a `timeline_ms` timeline. `on_timeline`
    /// skips what falls off the timeline instead of clamping it into the
    /// first or last bin.
    fn fold_into(
        out: &mut [f64],
        timeline_ms: i64,
        peaks: &[f64],
        start_ms: i64,
        duration_ms: i64,
        vol: f64,
        on_timeline: bool,
    ) {
        if peaks.is_empty() || duration_ms <= 0 || vol <= 0.0 {
            return;
        }
        let n = out.len() as i64;
        let tl = timeline_ms as f64;
        for (i, peak) in peaks.iter().enumerate() {
            let tms = start_ms as f64 + (i as f64 + 0.5) / peaks.len() as f64 * duration_ms as f64;
            if on_timeline && (tms < 0.0 || tms >= tl) {
                continue;
            }
            let b = clamp_i64(floor_to_i64(tms / tl * n as f64), 0, n - 1) as usize;
            let v = peak * vol;
            if v > out[b] {
                out[b] = v;
            }
        }
    }

    /// A composite waveform for an FX bus lane: the max-envelope of every
    /// audible take that reaches `bus_id` — by main output or by aux send,
    /// directly or through another bus — scaled by the take's volume and the
    /// gain of the route it took. Mirrors
    /// [`MixerController::master_waveform_peaks`] but filtered to the bus.
    ///
    /// Like the master lane this is the DRY source envelope, not the bus's
    /// wet output: bus effects, volume and pan are not modelled here either.
    pub fn bus_waveform_peaks(&self, host: &dyn MixerHost, bus_id: &str) -> Arc<[f64]> {
        self.envelope(host, Some(bus_id), || self.fold_bus_waveform(host, bus_id))
    }

    fn fold_bus_waveform(&self, host: &dyn MixerHost, bus_id: &str) -> Vec<f64> {
        let tl = host.practice().map_or(0, |p| p.timeline_duration_ms);
        if tl <= 0 {
            return Vec::new();
        }
        let mut out = vec![0.0; ENVELOPE_BINS];
        let solo = self.resolve_solo(host);
        for t in host.all_tracks() {
            // Backing lanes send nothing to the engine either (see
            // `resync_playback`).
            if t.is_backing() || !t.has_clip() {
                continue;
            }
            let gain = self.route_gain_to_bus(host, bus_id, t.bus_id.as_deref(), &t.sends);
            if gain <= 0.0 {
                continue;
            }
            if !self.audible_track(host, &t, &solo) {
                continue;
            }
            for c in &t.clips {
                Self::fold_into(
                    &mut out,
                    tl,
                    &c.waveform_peaks,
                    c.start_ms,
                    c.duration_ms,
                    t.volume * gain,
                    false,
                );
            }
        }
        out
    }

    /// A composite "master" waveform for the master lane: the loudest audible
    /// track envelope (scaled by each track's volume and the master gain) at
    /// every point on the timeline, plus the live take while recording. It
    /// approximates the downmix for display and recomputes reactively as
    /// tracks/levels change.
    pub fn master_waveform_peaks(&self, host: &dyn MixerHost) -> Arc<[f64]> {
        self.envelope(host, None, || self.fold_master_waveform(host))
    }

    fn fold_master_waveform(&self, host: &dyn MixerHost) -> Vec<f64> {
        let tl = host.practice().map_or(0, |p| p.timeline_duration_ms);
        if tl <= 0 {
            return Vec::new();
        }
        let mut out = vec![0.0; ENVELOPE_BINS];
        let solo = self.resolve_solo(host);
        // Off the timeline: the live take's pre-roll, which its stop cuts, or
        // a take sung on past the end (clamped, either piled into the first
        // or the last bin, as a spike of the loudest of it).
        for t in host.all_tracks() {
            if !t.has_clip() || !self.audible_track(host, &t, &solo) {
                continue;
            }
            // Backing lanes go straight to the master (see `resync_playback`).
            let gain = if t.is_backing() {
                1.0
            } else {
                self.route_gain_to_master(host, t.bus_id.as_deref(), &t.sends, &solo)
            };
            if gain <= 0.0 {
                continue;
            }
            // Fold every clip of the take (not just the primary), so trailing
            // punched-in clips of a multi-clip take show in the master
            // envelope — matching the bus lanes.
            for c in &t.clips {
                Self::fold_into(
                    &mut out,
                    tl,
                    &c.waveform_peaks,
                    c.start_ms,
                    c.duration_ms,
                    t.volume * gain,
                    true,
                );
            }
        }
        // The take being laid down isn't a track yet — fold its live envelope
        // in.
        if host.is_recording() {
            Self::fold_into(
                &mut out,
                tl,
                host.recording_peaks(),
                host.recording_start_ms(),
                host.recording_duration_ms(),
                1.0,
                true,
            );
        }

        let m = self.master_volume(host);
        if m != 1.0 {
            for v in &mut out {
                *v *= m;
            }
        }
        out
    }

    /// The vocal compressor's current gain reduction (dB) for a track — for a
    /// live meter.
    pub fn gain_reduction_db(&self, host: &dyn MixerHost, track_id: &str) -> f64 {
        // Engine sources are per-clip, so the FX meter reads the take's clip
        // heard under the playhead (the one on top there); in the gaps between
        // clips nothing of the take plays, so there is no gain reduction to
        // show.
        let Some(track) = host.track_by_id(track_id) else {
            return 0.0;
        };
        let pos_ms = i64::try_from(host.position().as_millis()).unwrap_or(i64::MAX);
        for (piece, clip) in Self::heard_pieces(&track) {
            if pos_ms >= piece.start_ms && pos_ms < piece.start_ms + piece.duration_ms {
                return self.playback().track_gain_reduction_db(&clip.id);
            }
        }
        0.0
    }

    /// What of each of a lane's clips is heard, as the engine stages it: where
    /// clips overlap only the one stacked on top plays (`lane_visible_spans`,
    /// the rule the lane's merged analysis follows too), so the one below plays
    /// as the stretches the one above leaves, each from its offset into the
    /// file and faded where it is cut. Clips with no file play nothing; a clip
    /// of unknown length (none recorded) plays whole and covers nothing, as
    /// every clip did before the rule. Each piece comes with its clip, in the
    /// lane's order, a clip's pieces in timeline order.
    fn heard_pieces(t: &AudioTrack) -> Vec<(Piece, &AudioClip)> {
        let clips: Vec<&AudioClip> = t
            .clips
            .iter()
            .filter(|c| !c.audio_file_path.is_empty())
            .collect();
        let spans: Vec<(i64, i64)> = clips
            .iter()
            .map(|c| {
                if c.duration_ms > 0 {
                    (c.start_ms, c.end_ms())
                } else {
                    (0, 0)
                }
            })
            .collect();
        let mut out: Vec<(Piece, &AudioClip)> = lane_visible_spans(&spans)
            .into_iter()
            .map(|v| {
                let c = clips[v.clip];
                let piece = Piece {
                    start_ms: v.start_ms,
                    duration_ms: v.end_ms - v.start_ms,
                    src_offset_ms: v.start_ms - c.start_ms,
                    fade_in: v.start_ms > c.start_ms,
                    fade_out: v.end_ms < c.end_ms(),
                };
                (v.clip, piece)
            })
            .map(|(i, p)| (p, clips[i]))
            .collect();
        for c in clips.iter().filter(|c| c.duration_ms <= 0) {
            out.push((
                Piece {
                    start_ms: c.start_ms,
                    duration_ms: c.duration_ms,
                    src_offset_ms: 0,
                    fade_in: false,
                    fade_out: false,
                },
                c,
            ));
        }
        out
    }

    // --- Engine graph sync ----------------------------------------------------

    /// Reconciles the engine with the whole current mix (tracks, buses,
    /// reverb, master). Called by the view model's load/save tail (which is
    /// how the routing edits here reach the engine) and by recording start
    /// (mutes the punched take). The reverb and master gain are pushed and
    /// the sync queued before this returns (see the module doc); the
    /// returned [`PendingSync`] is Dart's awaited future.
    pub fn resync_playback(&self, host: &dyn MixerHost) -> PendingSync {
        let Some(practice) = host.practice() else {
            return PendingSync::done();
        };
        let all_tracks = host.all_tracks();

        let solo = self.resolve_solo(host);
        // Flatten lanes to one engine source per piece of a clip that is heard
        // (see `heard_pieces`: where clips overlap only the top one sounds),
        // the first piece of a clip under its id and the rest under `id#n`,
        // all of a lane's through the lane's one chain (lane_id): its volume /
        // mute / FX apply to every clip, and its dynamics run across their
        // boundaries. Empty takes contribute none.
        let mut tracks = Vec::new();
        for t in &all_tracks {
            let muted = !self.audible_track(host, t, &solo);
            let mut pieces_of: HashMap<&str, usize> = HashMap::new();
            for (piece, c) in Self::heard_pieces(t) {
                let n = pieces_of.entry(c.id.as_str()).or_insert(0);
                let id = if *n == 0 {
                    c.id.clone()
                } else {
                    format!("{}#{n}", c.id)
                };
                *n += 1;
                tracks.push(TrackPlayback {
                    id,
                    path: c.audio_file_path.clone(),
                    start_ms: piece.start_ms,
                    duration_ms: piece.duration_ms,
                    src_offset_ms: piece.src_offset_ms,
                    fade_in: piece.fade_in,
                    fade_out: piece.fade_out,
                    volume: t.volume,
                    muted,
                    pan: t.pan,
                    is_vocal: !t.is_backing(),
                    fx: t.fx.clone(),
                    bus_id: if t.is_backing() {
                        None
                    } else {
                        t.bus_id.clone()
                    },
                    sends: if t.is_backing() {
                        Vec::new()
                    } else {
                        t.sends.clone()
                    },
                    lane_id: Some(t.id.clone()),
                });
            }
        }
        let buses = practice
            .fx_buses
            .iter()
            .map(|b| BusPlayback {
                id: b.id.clone(),
                fx: b.fx.clone(),
                volume: b.volume,
                muted: !Self::audible_bus(b, &solo),
                pan: b.pan,
                output_bus_id: b.output_bus_id.clone(),
                sends: b.sends.clone(),
            })
            .collect();
        self.playback().set_reverb(&self.reverb_settings(host));
        self.playback().set_master_volume(self.master_volume(host));
        self.engine.queue_sync(tracks, buses)
    }
}
