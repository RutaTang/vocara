//! All analysis compute + caches for the song detail screen, and their view
//! chrome (port of `ui/song_practice_detail/view_model/analysis_cache.dart`):
//! per-path spectrogram/metrics/profile/LTAS/register caches with
//! loading/unavailable latches, whole-lane aggregate merging for multi-clip
//! takes, the wide-range EQ display spectrogram, the inline fold-out lanes,
//! the bottom dock (open/tab/height), and live-vs-take switching (owns the
//! live-monitor flag and the 30 ms capture pump feeding the monitor). Not
//! Dart's: the "Vocal tract" view's estimates, a take's track computed on a
//! worker from its metrics and memoized like the profile and register, and
//! the live one the monitor keeps (see `AnalysisCache::ensure_vocal_tract`).
//!
//! Selection and the playhead come from the host ([`AnalysisHost`], Dart's
//! injected closures) — this cache owns no model or selection state. Every
//! call that reads them takes the host; [`AnalysisCache::source`] binds the
//! two into the [`AnalysisSource`] the dock and inspector views read (Dart's
//! cache implemented it directly).
//!
//! # How Dart's async maps here
//!
//! Dart ran this class as a web of `async` functions on one isolate, and
//! most of its rules are about what each sees after an `await`. Here the
//! cache is a state machine with the same rules at the same points:
//!
//! - A heavy compute (`computeBothFromFile`, a lane merge in its isolate) is
//!   an [`AnalysisEffect::Run`] carrying an [`AnalysisJob`]: the screen runs
//!   it on a worker thread and hands its [`JobDone`] to
//!   [`AnalysisCache::job_done`], where the Dart code after the `await` runs
//!   (the generation and dispose checks, the latches, the `finally`).
//! - Every other `await` — joining a compute already in flight, a lane merge
//!   waiting for the one ahead of it, `Future.wait` over a clip's two halves,
//!   a heal waiting for the merge it asked for — is a continuation the cache
//!   keeps. A completer becomes a signal; its waiters resume, in order, from
//!   a microtask queue drained before every call returns, so they run after
//!   the synchronous code that completed them, as Dart's microtasks did (a
//!   joiner sees the rerun a compute's `finally` started).
//! - The mic permission prompt, a toast and the lane-heal retry timer are
//!   effects too; the capture pump is a 30 ms tick the screen subscribes to
//!   while [`AnalysisCache::capture_pump_active`].
//! - `notifyListeners` bumps [`AnalysisCache::revision`].
//!
//! Every call that can start work returns the effects to run (`#[must_use]`).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vocara_model::config::app_config::AppConfig;
use vocara_model::models::audio_track::{AudioClip, AudioTrack};
use vocara_model::models::spectrogram::{Spectrogram, SpectrogramPart};
use vocara_model::models::voice_metrics::{
    analyze_register, analyze_voice_profile, compute_ltas, Ltas, RegisterMap, RegisterOptions,
    VoiceMetrics, VoiceMetricsPart, VoiceProfile, VoiceProfileOptions,
};
use vocara_model::util::analysis_gate::{analysis_gate, AnalysisGate};
use vocara_model::util::dart_num::clamp_f64;
use vocara_model::util::lru_bytes_cache::{analysis_cache_budget, LruBytesBudget, LruBytesCache};
use vocara_services::audio_dsp::AnalysisParams;
use vocara_services::graph_audio_playback_service::GraphAudioPlaybackService;
use vocara_services::mic_permission;
use vocara_services::monitor_service::{downmix_interleaved_stereo, LiveMonitor, MonitorService};
use vocara_services::spectrogram_service::SpectrogramAnalyses;
use vocara_services::vocal_tract::{suggested_body, VocalTractState, VocalTractTrack};
use vocara_tract::anatomy::BodyMix;

use super::analysis_source::AnalysisSource;

mod job;

pub use job::{AnalysisJob, JobDone, JobKind, MergedLane, WantedProbe};
use job::{Outcome, Work};

#[cfg(test)]
mod tests;

/// The tabs of the collapsible bottom dock — one analysis feature each, plus
/// the mixer. The spectral ones and the vocal tract can also show the live
/// mic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockTab {
    AnalysisSummary,
    Spectrogram,
    Power,
    Ltas,
    Vowel,
    Pitch,
    Vibrato,
    Timbre,
    Registers,
    Range,
    Dynamics,
    /// How the vocal tract moves (a view of its own, not in the Dart app).
    VocalTract,
    Mixer,
}

impl DockTab {
    /// Every tab, in declaration order (Dart's `DockTab.values`, with the
    /// vocal tract added before the mixer).
    pub const ALL: [DockTab; 13] = [
        DockTab::AnalysisSummary,
        DockTab::Spectrogram,
        DockTab::Power,
        DockTab::Ltas,
        DockTab::Vowel,
        DockTab::Pitch,
        DockTab::Vibrato,
        DockTab::Timbre,
        DockTab::Registers,
        DockTab::Range,
        DockTab::Dynamics,
        DockTab::VocalTract,
        DockTab::Mixer,
    ];

    /// Dart's enum `name` (what the `DOCK_TAB` demo hook names).
    pub fn name(self) -> &'static str {
        match self {
            DockTab::AnalysisSummary => "analysisSummary",
            DockTab::Spectrogram => "spectrogram",
            DockTab::Power => "power",
            DockTab::Ltas => "ltas",
            DockTab::Vowel => "vowel",
            DockTab::Pitch => "pitch",
            DockTab::Vibrato => "vibrato",
            DockTab::Timbre => "timbre",
            DockTab::Registers => "registers",
            DockTab::Range => "range",
            DockTab::Dynamics => "dynamics",
            DockTab::VocalTract => "vocalTract",
            DockTab::Mixer => "mixer",
        }
    }
}

/// The inline analysis sub-lanes a vocal take can fold out (Logic-style).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AnalysisLane {
    Pitch,
    Loudness,
    Spectrogram,
    Power,
    Notes,
    Technique,
}

impl AnalysisLane {
    /// Every lane, in declaration order (Dart's `AnalysisLane.values`).
    pub const ALL: [AnalysisLane; 6] = [
        AnalysisLane::Pitch,
        AnalysisLane::Loudness,
        AnalysisLane::Spectrogram,
        AnalysisLane::Power,
        AnalysisLane::Notes,
        AnalysisLane::Technique,
    ];
}

/// A clip's (or a lane's) spectrogram and metrics, as one analysis (Dart's
/// `TakeAnalysis` record).
#[derive(Clone, Debug, Default)]
pub struct TakeAnalysis {
    pub spectrogram: Option<Arc<Spectrogram>>,
    pub metrics: Option<Arc<VoiceMetrics>>,
}

/// The pre-EQ input spectrum column at the playhead and its bin width (Dart's
/// `({Float32List col, double binHz})`), holding the display spectrogram it
/// reads from.
#[derive(Clone, Debug)]
pub struct EqInputColumn {
    spectrogram: Arc<Spectrogram>,
    seconds: f64,
}

impl EqInputColumn {
    /// dB per bin (`Spectrogram::column_at_time`).
    pub fn col(&self) -> &[f32] {
        self.spectrogram.column_at_time(self.seconds)
    }

    /// Hz per bin.
    pub fn bin_hz(&self) -> f64 {
        self.spectrogram.bin_hz()
    }
}

/// What the cache reads from its host view model: Dart's injected closures.
/// The selection and the playhead are the host's; the cache only follows
/// them.
pub trait AnalysisHost {
    /// The selected track (any kind), if any.
    fn selected_track(&self) -> Option<Arc<AudioTrack>>;
    /// The selected clip within the selected lane; `None` when the lane is
    /// selected via its header.
    fn selected_clip_id(&self) -> Option<&str>;
    /// The track `id` as the model holds it now.
    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>>;
    /// Whether a clip of take `track_id` is being moved live (see
    /// `SongPracticeDetailViewModel.isMovingLive`). Each tick of the move
    /// re-keys a multi-clip lane's merge, and nothing asks for its analysis
    /// meanwhile ([`AnalysisCache::lacks_analysis`],
    /// [`AnalysisCache::lane_lacks_data`]): the lanes, the inspector and the
    /// dock each merged and cached every position the clip passed through.
    /// The commit asks for where it landed.
    fn is_moving_live(&self, _track_id: &str) -> bool {
        false
    }
    /// Whether the transport runs.
    fn is_playing(&self) -> bool;
    /// Recording owns the capture — the live view can't start during a take,
    /// including REC's arm window.
    fn is_recording(&self) -> bool;
    /// Pauses playback before the live view starts (capture and playback
    /// share the engine's input focus). Returns at once, as the engine's
    /// pause does.
    fn pause_playback(&self);
    /// The shared playhead.
    fn position(&self) -> Duration;
}

/// Work the screen runs for the cache.
pub enum AnalysisEffect {
    /// Run on a worker thread; hand the result to [`AnalysisCache::job_done`].
    Run(AnalysisJob),
    /// Ask for the microphone ([`mic_permission::request`], blocking: off the
    /// UI thread) and hand the answer to
    /// [`AnalysisCache::mic_permission_answered`]. A request that failed
    /// counts as granted, as Dart's did (no channel: don't block capture).
    RequestMicPermission,
    /// Show this toast.
    Toast(String),
    /// Call [`AnalysisCache::lane_heal_retry_due`] after this long (Dart's
    /// one-shot `Timer`).
    LaneHealRetry { after: Duration },
}

impl fmt::Debug for AnalysisEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnalysisEffect::Run(job) => f.debug_tuple("Run").field(job).finish(),
            AnalysisEffect::RequestMicPermission => f.write_str("RequestMicPermission"),
            AnalysisEffect::Toast(m) => f.debug_tuple("Toast").field(m).finish(),
            AnalysisEffect::LaneHealRetry { after } => f
                .debug_struct("LaneHealRetry")
                .field("after", after)
                .finish(),
        }
    }
}

/// The cache's test seams (Dart's `@visibleForTesting` parameters) and its
/// one replaceable collaborator.
#[derive(Default)]
pub struct AnalysisCacheOptions {
    /// The driving half of the monitor (default: the monitor itself).
    pub live_monitor: Option<Arc<dyn LiveMonitor>>,
    /// The byte budget the caches share (default: the app-wide
    /// [`analysis_cache_budget`]).
    pub cache_budget: Option<LruBytesBudget>,
    /// The clock the lane-heal intervals read (default: [`Instant::now`]).
    pub now: Option<Box<dyn Fn() -> Instant + Send + Sync>>,
    /// The gate lane merges queue at (default: the app-wide
    /// [`analysis_gate`]).
    pub gate: Option<&'static AnalysisGate>,
}

// --- Sizes and constants ------------------------------------------------------

// The heavy caches are byte-budgeted LRUs: a long session used to keep every
// take/lane/EQ result ever opened resident. Reads refresh recency, so the
// on-screen subject is never the eviction victim. Sizes are the typed-data
// payloads (the dominant term).
fn spec_bytes(s: &Spectrogram) -> usize {
    s.db.len() * 4
}

// Budgeted per frame: the metrics payload plus the score curve its memoized
// RegisterMap keeps alive beside it — that curve outlives eviction in the
// plain derived map, so a budget that ignored it would understate what an
// entry actually costs the process.
const REGISTER_SCORE_BYTES_PER_FRAME: usize = 4;

fn metrics_bytes(m: &VoiceMetrics) -> usize {
    m.frames * (VoiceMetrics::BYTES_PER_FRAME + REGISTER_SCORE_BYTES_PER_FRAME)
}

// Whole-lane aggregate analysis for multi-clip takes: spectrogram and metrics
// share ONE entry so eviction can never keep half a subject: a merged
// spectrogram costs ~856 B per frame against the metrics' 41, so any split
// budget drops the spectrogram first and leaves behind a half that still
// answers `contains_key` — which the ready latch reads as "done", so the
// missing half was never recomputed and the view stayed empty without even a
// loading state. [`LruBytesCache`] keeps a lone oversized entry — for a pair
// that means both halves stay, never one.
fn agg_bytes(a: &TakeAnalysis) -> usize {
    a.spectrogram.as_deref().map_or(0, spec_bytes) + a.metrics.as_deref().map_or(0, metrics_bytes)
}

// Full-range (to ~18 kHz) display spectrogram per clip file, for the FX EQ's
// input/output spectrum analyzer. Kept separate from the 5 kHz analysis
// spectrogram, which is tuned for voice metrics rather than a wide display.
const EQ_DISPLAY_MAX_HZ: f64 = 18000.0;

/// The body the vocal tract is estimated for until the view sets one:
/// halfway between the adult male and the adult female CHIMERA calibrated,
/// which misses either by less than the other preset would.
pub const DEFAULT_TRACT_BODY: BodyMix = BodyMix {
    female: 0.5,
    child: 0.0,
};

/// How a failed track is latched: its body and subject.
fn tract_failure_key(body: BodyMix, key: &str) -> String {
    format!("{},{}|{key}", body.female, body.child)
}

const MIN_DOCK_HEIGHT: f64 = 160.0;
const MAX_DOCK_HEIGHT: f64 = 760.0;

/// How many heals in a row (each within [`LANE_HEAL_QUIET`] of the one
/// before) earn a retry of their own (see [`AnalysisCache::heal_lane`]).
const LANE_HEAL_RETRIES: u32 = 3;
const LANE_HEAL_QUIET: Duration = Duration::from_secs(2 * 60);

/// How soon [`AnalysisCache::heal_lane`] asks again for the same subject.
/// More analysis on screen than the byte budget holds would otherwise have
/// the lanes and the dock evict each other's in turn, one decode after
/// another, for good.
const LANE_HEAL_INTERVAL: Duration = Duration::from_secs(30);

// --- Internals ----------------------------------------------------------------

/// Everything memoized FROM an analysis: the profile and register map derive
/// from the metrics, the LTAS from the spectrogram and metrics; the `agg_*`
/// maps for the whole-lane aggregates. Their lifetime is the SOURCE entry's:
/// the caches' eviction hooks drop them, because the budget can drop a source
/// on any insert and a derived value keyed off a gone source has no owner
/// left to drop it. Shared with those hooks, which run inside the caches.
#[derive(Default)]
struct Derived {
    profile: HashMap<String, Arc<VoiceProfile>>,
    ltas: HashMap<String, Arc<Ltas>>,
    register: HashMap<String, Arc<RegisterMap>>,
    agg_profile: HashMap<String, Arc<VoiceProfile>>,
    agg_ltas: HashMap<String, Arc<Ltas>>,
    agg_register: HashMap<String, Arc<RegisterMap>>,
    /// The vocal-tract tracks (not Dart's), computed on a worker from the
    /// metrics under the same key, for the body each names.
    tract: HashMap<String, Arc<VocalTractTrack>>,
    agg_tract: HashMap<String, Arc<VocalTractTrack>>,
}

impl Derived {
    /// Dart's `_dropDerived`.
    fn drop_derived(&mut self, key: &str) {
        self.profile.remove(key);
        self.ltas.remove(key);
        self.register.remove(key);
        self.tract.remove(key);
    }

    /// Dart's `_dropAggDerived`.
    fn drop_agg_derived(&mut self, key: &str) {
        self.agg_profile.remove(key);
        self.agg_ltas.remove(key);
        self.agg_register.remove(key);
        self.agg_tract.remove(key);
    }

    fn len(&self) -> usize {
        self.profile.len()
            + self.ltas.len()
            + self.register.len()
            + self.agg_profile.len()
            + self.agg_ltas.len()
            + self.agg_register.len()
            + self.tract.len()
            + self.agg_tract.len()
    }
}

fn lock(derived: &Mutex<Derived>) -> MutexGuard<'_, Derived> {
    derived.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One half of a clip's analysis, naming its in-flight map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Half {
    Spectrogram,
    Metrics,
}

/// A Dart completer: waiters registered on it resume when it fires.
type SignalId = u64;

/// What runs once an awaited thing completes (the rest of the Dart function
/// that awaited it).
enum Cont {
    /// Nothing: the caller did not await, or ignored the result.
    Done,
    /// One member of a `Future.wait` finished.
    Group(u64),
    /// `_ensureClipAnalysis`'s fallback, after both halves: return the
    /// clip's pair (`_clipAnalysis(path)`) to `then`.
    ClipPair { path: String, then: Box<Cont> },
    /// The aggregate loop of run `run` got its current clip's pair.
    AggPart { run: u64 },
    /// `healSelectedAnalysis` after the lane merge it asked for.
    HealSelectedAgg { key: String },
    /// `healLane` after the lane merge it asked for.
    HealLaneAgg { key: String },
    /// `rerun.whenComplete(queued.done.complete)`.
    Fire(SignalId),
}

/// A continuation parked on a signal. A joiner (`_joinInFlight`) re-checks
/// its in-flight map as it resumes and parks again on whatever replaced the
/// compute it waited out.
struct Waiter {
    join: Option<(Half, String)>,
    then: Cont,
}

/// A `Future.wait`: `then` runs once `remaining` members finished.
struct Group {
    remaining: usize,
    then: Cont,
}

enum Microtask {
    Resume(Waiter),
    Complete(Cont, TakeAnalysis),
}

/// A clip compute in flight (the Dart function parked on its `await`).
struct Compute {
    kind: JobKind,
    path: String,
    generation: u64,
    /// The `done` completer joiners wait on (none for the EQ spectrogram).
    signal: Option<SignalId>,
    /// The starter's caller.
    then: Cont,
}

/// What a job id in flight belongs to.
enum Pending {
    Compute(Compute),
    Merge { run: u64 },
    VocalTract(TractCompute),
}

/// A vocal-tract track in flight: for which subject, from which metrics, for
/// which body, and the flag an eviction of the subject's audio sets (its
/// `wanted`).
struct TractCompute {
    key: String,
    aggregate: bool,
    metrics: Arc<VoiceMetrics>,
    body: BodyMix,
    disowned: Arc<AtomicBool>,
}

/// One `_ensureAggregateAnalysis` run in flight: its loop over the clips, the
/// parts it HOLDS, and its merge.
struct AggRun {
    track: Arc<AudioTrack>,
    key: String,
    parts: HashMap<String, TakeAnalysis>,
    /// The clip the loop is on.
    next: usize,
    /// Set as an eviction disowns this run's key (the merge's `wanted`).
    disowned: Arc<AtomicBool>,
    then: Cont,
}

/// An ask for a newer key of a lane whose merge is running.
struct Queued {
    key: String,
    done: SignalId,
}

/// How many times in a row [`AnalysisCache::heal_lane`] asked for a subject,
/// and when it last did.
#[derive(Clone, Copy)]
struct LaneHeals {
    count: u32,
    at: Instant,
}

/// The current analysis subject (Dart's `_analysisSubject` record).
struct Subject {
    key: String,
    aggregate: bool,
    start_ms: i64,
    duration_ms: i64,
}

/// What the dock's tab or a lane needs of a clip's analysis.
#[derive(Clone, Copy)]
struct Needs {
    spectrogram: bool,
    metrics: bool,
}

/// All analysis compute + caches for the song detail screen (see the module
/// docs).
pub struct AnalysisCache {
    spectrograms: Arc<dyn SpectrogramAnalyses>,
    monitor: Arc<MonitorService>,
    live: Arc<dyn LiveMonitor>,
    /// The native engine when active — the only backend with live input
    /// capture.
    graph_engine: Option<Arc<GraphAudioPlaybackService>>,
    gate: &'static AnalysisGate,
    now: Box<dyn Fn() -> Instant + Send + Sync>,

    /// Set in [`AnalysisCache::dispose`]. Guards async cache computes
    /// resolving after the view model (and this cache) are gone: every
    /// compute helper refuses to START once it is set, and re-checks after
    /// its await before touching a cache. What that buys is not the wasted
    /// writes — heavy computes queue through the app-wide analysis gate's two
    /// slots, so a multi-clip aggregate that kept feeding clips into it after
    /// its screen closed made the NEXT screen's analysis wait behind results
    /// no one would read. An already-admitted compute cannot be recalled; the
    /// guarantee is that no FURTHER one is admitted.
    disposed: bool,
    /// [`AnalysisCache::disposed`] for the jobs' `wanted` probes.
    disposed_flag: Arc<AtomicBool>,
    revision: u64,

    // --- Caches ---
    spectrogram_cache: LruBytesCache<Arc<Spectrogram>>,
    metrics_cache: LruBytesCache<Arc<VoiceMetrics>>,
    derived: Arc<Mutex<Derived>>,
    spectrogram_loading: HashSet<String>,
    metrics_loading: HashSet<String>,
    // The in-flight compute per path, so a second caller JOINS it instead of
    // skipping ahead. Kept in lockstep with the loading sets above (which
    // drive the spinner); these carry the signal the joiner awaits. Without
    // them a caller that arrived mid-compute returned instantly with nothing,
    // and the aggregate merge — which refuses to cache a partial lane,
    // correctly — threw the whole lane away with no retry scheduled, leaving
    // the dock blank until the user changed the selection.
    spectrogram_in_flight: HashMap<String, SignalId>,
    metrics_in_flight: HashMap<String, SignalId>,
    /// Bumped by [`AnalysisCache::evict_analysis`] per path (the lane merges
    /// folding that path are disowned through `agg_disowned`). A compute
    /// captures the generation it started under and, if it changed by the
    /// time the result lands, that result describes bytes the eviction has
    /// already declared stale: it must not be cached, and must not latch
    /// "unavailable" over the new file. Re-analyze is exactly this window —
    /// it evicts each clip while the dock may still be computing it — and
    /// without the check the stale result was written back under a key
    /// nothing invalidates again, so the take showed the OLD audio's analysis
    /// (or a permanent "unavailable") for the rest of the session. Atomics,
    /// so a job's `wanted` reads them on its worker.
    path_generation: HashMap<String, Arc<AtomicU64>>,
    /// Paths whose latest [`AnalysisCache::evict_analysis`] asked for a stale
    /// in-flight compute to be REDONE (re-analyze: the same path now holds
    /// new bytes). Every other eviction is for a file that left the model — a
    /// deleted take, a split or merged-away clip — where redoing it only
    /// re-decoded a dead file into one of the app-wide analysis slots.
    recompute_on_stale: HashSet<String>,
    // Paths whose compute finished but produced nothing (non-WAV imports,
    // missing or truncated files). Latched so a null result is never retried
    // — otherwise the always-on inspector would re-launch the read + compute
    // every rebuild.
    spectrogram_unavailable: HashSet<String>,
    metrics_unavailable: HashSet<String>,
    // Whole-lane aggregate analysis for multi-clip takes: each clip's
    // per-path result merged in timeline order, keyed by
    // [`AnalysisCache::aggregate_key_for`] (which embeds the ordered clip
    // ids, so the key self-invalidates when the clip set changes). Only
    // multi-clip takes ever populate these.
    agg_analysis_cache: LruBytesCache<TakeAnalysis>,
    agg_loading: HashSet<String>,
    agg_unavailable: HashSet<String>,
    /// Lane merges in flight (keys in `agg_loading`) that an
    /// [`AnalysisCache::evict_analysis`] of a file they fold has disowned:
    /// their result describes bytes already declared stale, so it is neither
    /// cached nor latched, and the lane is merged again if a view still shows
    /// it (see `shows_lane`).
    agg_disowned: HashSet<String>,
    /// The lane merge in flight per lane (track id → its key). A lane's key
    /// moves with every tick of a clip drag (it embeds the clip starts), and
    /// the inspector asks for each new one: every frame started a merge of
    /// its own, copying all the lane's clip analyses into a fresh isolate.
    /// One runs per lane; an ask for a newer key waits in `agg_queued`
    /// (loading meanwhile) and is answered by ONE rerun for the lane as it is
    /// when the running merge lands.
    agg_in_flight: HashMap<String, String>,
    agg_queued: HashMap<String, Queued>,
    agg_runs: HashMap<u64, AggRun>,
    eq_spectro_cache: LruBytesCache<Arc<Spectrogram>>,
    eq_spectro_loading: HashSet<String>,
    eq_spectro_unavailable: HashSet<String>,
    // --- The vocal tract (not Dart's) ---
    /// The body the tracks (and the live estimate) are for.
    vocal_tract_body: BodyMix,
    /// The subject whose suggested body was last followed.
    tract_body_followed: Option<String>,
    /// Subjects whose track computes, by job id.
    tract_in_flight: HashMap<String, u64>,
    /// Subjects (key and body) whose track's compute failed: not retried.
    tract_failed: HashSet<String>,
    // Inline analysis sub-lanes (view state, not persisted): which tracks are
    // folded open (in the order they were), and which lanes each shows.
    expanded_tracks: Vec<String>,
    track_lanes: HashMap<String, BTreeSet<AnalysisLane>>,

    // --- Bottom dock ---
    dock_open: bool,
    dock_tab: DockTab,
    dock_height: f64,

    // --- Live monitor ---
    live_monitor: bool,
    /// The capture pump's engine while it runs (Dart's periodic `Timer`):
    /// pulls captured frames from the engine into the live-analysis monitor.
    /// Recording never uses this pump (its capture belongs to the take).
    capture_engine: Option<Arc<GraphAudioPlaybackService>>,
    /// Intent latch for a live start parked on its permission prompt: set at
    /// [`AnalysisCache::set_live_monitor`]`(true)`'s entry, cleared when the
    /// start arms, aborts, or ANY `set_live_monitor(false)` arrives. Without
    /// it, startRecording's `set_live_monitor(false)` would no-op on the
    /// `live_monitor == on` early-return while the start is still parked,
    /// and the capture pump would arm DURING the take (a later Live-off would
    /// then stop the capture underneath the recording).
    live_start_pending: bool,

    // --- Heals ---
    /// The lane aggregate a heal merged and could not cache, which is not
    /// asked for again until a merge caches it or a file it folds changes:
    /// the inspector asks on every rebuild, and merged the lane again each
    /// time. A clip needs no such bound: a compute's insert never drops the
    /// clip's other half (see [`LruBytesCache::partner_with`]), so a heal
    /// leaves what it asked for.
    heal_blocked: Option<String>,
    /// Lane aggregates a heal merged and could not cache: not asked for
    /// again, or every rebuild of the lane merged it once more.
    lane_heal_blocked: HashSet<String>,
    /// When each lane subject (a clip's path, or a lane aggregate's key) was
    /// last asked again by [`AnalysisCache::heal_lane`].
    lane_healed_at: HashMap<String, Instant>,
    /// A rebuild of the lanes is due once a heal the interval refused may
    /// run (the timer is the screen's, see [`AnalysisEffect::LaneHealRetry`]).
    lane_heal_retry: bool,
    /// How many times in a row [`AnalysisCache::heal_lane`] asked for each
    /// subject, each within [`LANE_HEAL_QUIET`] of the one before, and when
    /// it last did. A run of heals the quiet ends starts over: counted for
    /// the life of the screen, three heals an hour apart left the next lane
    /// the budget let go empty for good.
    lane_heals: HashMap<String, LaneHeals>,

    // --- The async machinery (see the module docs) ---
    next_id: u64,
    pending: HashMap<u64, Pending>,
    waiters: HashMap<SignalId, Vec<Waiter>>,
    groups: HashMap<u64, Group>,
    microtasks: VecDeque<Microtask>,
    effects: Vec<AnalysisEffect>,
}

/// The cache bound to its host: the song screen's [`AnalysisSource`].
pub struct CacheSource<'a> {
    cache: &'a AnalysisCache,
    host: &'a dyn AnalysisHost,
}

impl AnalysisSource for CacheSource<'_> {
    fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        self.host.selected_track()
    }

    fn analysis_start_ms(&self) -> i64 {
        self.cache.analysis_start_ms(self.host)
    }

    fn analysis_duration_ms(&self) -> i64 {
        self.cache.analysis_duration_ms(self.host)
    }

    fn selected_metrics(&self) -> Option<Arc<VoiceMetrics>> {
        self.cache.selected_metrics(self.host)
    }

    fn selected_spectrogram(&self) -> Option<Arc<Spectrogram>> {
        self.cache.selected_spectrogram(self.host)
    }

    fn selected_profile(&self) -> Option<Arc<VoiceProfile>> {
        self.cache.selected_profile(self.host)
    }

    fn selected_ltas(&self) -> Option<Arc<Ltas>> {
        self.cache.selected_ltas(self.host)
    }

    fn selected_register(&self) -> Option<Arc<RegisterMap>> {
        self.cache.selected_register(self.host)
    }

    fn spectrogram_loading(&self) -> bool {
        self.cache.spectrogram_loading(self.host)
    }

    fn metrics_loading(&self) -> bool {
        self.cache.metrics_loading(self.host)
    }

    fn live_monitor(&self) -> bool {
        self.cache.live_monitor()
    }

    fn monitor(&self) -> &MonitorService {
        self.cache.monitor()
    }

    fn position(&self) -> Duration {
        self.host.position()
    }

    fn revision(&self) -> u64 {
        self.cache.revision()
    }

    fn selected_vocal_tract(&self) -> Option<Arc<VocalTractTrack>> {
        self.cache.selected_vocal_tract(self.host)
    }

    fn vocal_tract_loading(&self) -> bool {
        self.cache.vocal_tract_loading(self.host)
    }
}

impl AnalysisCache {
    /// A cache over the app's services: the analyses, the live monitor (the
    /// view model owns it and disposes it after this cache), and the native
    /// engine when it is the active backend.
    pub fn new(
        spectrograms: Arc<dyn SpectrogramAnalyses>,
        monitor: Arc<MonitorService>,
        graph_engine: Option<Arc<GraphAudioPlaybackService>>,
    ) -> Self {
        Self::with_options(
            spectrograms,
            monitor,
            graph_engine,
            AnalysisCacheOptions::default(),
        )
    }

    /// [`AnalysisCache::new`] with its seams set.
    pub fn with_options(
        spectrograms: Arc<dyn SpectrogramAnalyses>,
        monitor: Arc<MonitorService>,
        graph_engine: Option<Arc<GraphAudioPlaybackService>>,
        options: AnalysisCacheOptions,
    ) -> Self {
        let budget = options
            .cache_budget
            .unwrap_or_else(|| analysis_cache_budget().clone());
        let agg_analysis_cache = LruBytesCache::shared(&budget, agg_bytes);
        let spectrogram_cache =
            LruBytesCache::shared(&budget, |s: &Arc<Spectrogram>| spec_bytes(s));
        let metrics_cache =
            LruBytesCache::shared(&budget, |m: &Arc<VoiceMetrics>| metrics_bytes(m));
        let eq_spectro_cache = LruBytesCache::shared(&budget, |s: &Arc<Spectrogram>| spec_bytes(s));
        let derived = Arc::new(Mutex::new(Derived::default()));
        // Derived results are memoized under the SAME key as the entry they
        // came from, so the budget dropping a source leaves them with no
        // owner: the hooks drop them with it.
        {
            let d = Arc::clone(&derived);
            spectrogram_cache.set_on_evict(Some(move |key: &str| lock(&d).drop_derived(key)));
        }
        {
            let d = Arc::clone(&derived);
            metrics_cache.set_on_evict(Some(move |key: &str| lock(&d).drop_derived(key)));
        }
        // A clip's two halves are one analysis: neither insert evicts the
        // other. Not the EQ display: it follows the clip under the playhead,
        // not the selected one, and pinned with a selected clip's halves it
        // outlived the display on screen.
        metrics_cache.partner_with(&spectrogram_cache);
        {
            let d = Arc::clone(&derived);
            agg_analysis_cache.set_on_evict(Some(move |key: &str| lock(&d).drop_agg_derived(key)));
        }
        let live = options
            .live_monitor
            .unwrap_or_else(|| Arc::clone(&monitor) as Arc<dyn LiveMonitor>);
        AnalysisCache {
            spectrograms,
            monitor,
            live,
            graph_engine,
            gate: options.gate.unwrap_or_else(analysis_gate),
            now: options.now.unwrap_or_else(|| Box::new(Instant::now)),
            disposed: false,
            disposed_flag: Arc::new(AtomicBool::new(false)),
            revision: 0,
            spectrogram_cache,
            metrics_cache,
            derived,
            spectrogram_loading: HashSet::new(),
            metrics_loading: HashSet::new(),
            spectrogram_in_flight: HashMap::new(),
            metrics_in_flight: HashMap::new(),
            path_generation: HashMap::new(),
            recompute_on_stale: HashSet::new(),
            spectrogram_unavailable: HashSet::new(),
            metrics_unavailable: HashSet::new(),
            agg_analysis_cache,
            agg_loading: HashSet::new(),
            agg_unavailable: HashSet::new(),
            agg_disowned: HashSet::new(),
            agg_in_flight: HashMap::new(),
            agg_queued: HashMap::new(),
            agg_runs: HashMap::new(),
            eq_spectro_cache,
            eq_spectro_loading: HashSet::new(),
            eq_spectro_unavailable: HashSet::new(),
            vocal_tract_body: DEFAULT_TRACT_BODY,
            tract_body_followed: None,
            tract_in_flight: HashMap::new(),
            tract_failed: HashSet::new(),
            expanded_tracks: Vec::new(),
            track_lanes: HashMap::new(),
            // Defaults closed; the build flag is a dev hook for previewing
            // the dock.
            dock_open: AppConfig::VOICEPRINT_DEMO,
            dock_tab: DockTab::AnalysisSummary,
            dock_height: 300.0,
            live_monitor: false,
            capture_engine: None,
            live_start_pending: false,
            heal_blocked: None,
            lane_heal_blocked: HashSet::new(),
            lane_healed_at: HashMap::new(),
            lane_heal_retry: false,
            lane_heals: HashMap::new(),
            next_id: 0,
            pending: HashMap::new(),
            waiters: HashMap::new(),
            groups: HashMap::new(),
            microtasks: VecDeque::new(),
            effects: Vec::new(),
        }
    }

    /// Bumped on every Dart `notifyListeners` (never after dispose).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn notify(&mut self) {
        if self.disposed {
            return;
        }
        self.revision += 1;
    }

    /// The cache bound to `host`, as the dock and inspector views read it.
    pub fn source<'a>(&'a self, host: &'a dyn AnalysisHost) -> CacheSource<'a> {
        CacheSource { cache: self, host }
    }

    // --- Test hooks -----------------------------------------------------------

    /// Bytes this screen's caches currently charge to the shared analysis
    /// budget, for tests pinning that a disposed screen stops charging it.
    pub fn budget_bytes(&self) -> usize {
        self.spectrogram_cache.budget().bytes()
    }

    /// Drops one half of a clip's analysis, as the shared byte budget does
    /// under pressure (the halves are separate entries). For tests.
    pub fn debug_drop_half(&mut self, path: &str, metrics: bool) {
        if metrics {
            self.metrics_cache.remove(path);
        } else {
            self.spectrogram_cache.remove(path);
        }
    }

    /// Memoized derived results currently held, for tests pinning that
    /// derived state never outlives its source entry.
    pub fn derived_entries(&self) -> usize {
        lock(&self.derived).len()
    }

    // --- Bottom dock (Voiceprint + Mixer) -------------------------------------

    pub fn dock_open(&self) -> bool {
        self.dock_open
    }

    pub fn dock_tab(&self) -> DockTab {
        self.dock_tab
    }

    /// Every analysis tab (all but Mixer) reads the selected take's
    /// spectrogram/metrics, so opening one computes them. The view model's
    /// selection triggers and the interpretation switch gate on this too.
    pub fn dock_shows_take(&self) -> bool {
        self.dock_tab != DockTab::Mixer
    }

    /// The spectral/pitch/timbre tabs and the vocal tract can render the live
    /// mic; range/dynamics need a whole recording and the mixer isn't a view
    /// of the source.
    fn tab_supports_live(t: DockTab) -> bool {
        matches!(
            t,
            DockTab::Spectrogram
                | DockTab::Power
                | DockTab::Ltas
                | DockTab::Vowel
                | DockTab::Pitch
                | DockTab::Vibrato
                | DockTab::Timbre
                | DockTab::Registers
                | DockTab::VocalTract
        )
    }

    /// Whether the active tab can show the live mic (drives the Take/Live
    /// toggle).
    pub fn active_tab_supports_live(&self) -> bool {
        Self::tab_supports_live(self.dock_tab)
    }

    pub fn dock_height(&self) -> f64 {
        self.dock_height
    }

    /// Resizes the dock by dragging its top edge (clamped to a sane range).
    pub fn set_dock_height(&mut self, height: f64) {
        let clamped = clamp_f64(height, MIN_DOCK_HEIGHT, MAX_DOCK_HEIGHT);
        if clamped == self.dock_height {
            return;
        }
        self.dock_height = clamped;
        self.notify();
    }

    #[must_use]
    pub fn toggle_dock(&mut self, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        let open = !self.dock_open;
        self.set_dock_open(open, host)
    }

    #[must_use]
    pub fn set_dock_open(&mut self, open: bool, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        if self.dock_open == open {
            return Vec::new();
        }
        self.dock_open = open;
        self.notify();
        if open && self.dock_shows_take() {
            self.ensure_dock_analysis(host);
        } else if !open && self.live_monitor {
            self.live_off();
        }
        self.finish(host)
    }

    /// Opens the dock on `tab` (switching if needed). Switching to a tab that
    /// can't show the live mic stops it.
    #[must_use]
    pub fn show_dock_tab(&mut self, tab: DockTab, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        self.dock_tab = tab;
        self.dock_open = true;
        self.notify();
        if self.dock_shows_take() {
            self.ensure_dock_analysis(host);
        }
        if !self.active_tab_supports_live() && self.live_monitor {
            self.live_off();
        }
        self.finish(host)
    }

    // --- Live monitor (real-time spectrogram) ---------------------------------

    pub fn live_monitor(&self) -> bool {
        self.live_monitor
    }

    pub fn monitor(&self) -> &MonitorService {
        &self.monitor
    }

    /// Whether the 30 ms capture pump runs: while it does, the screen calls
    /// [`AnalysisCache::pump_capture`] every 30 ms.
    pub fn capture_pump_active(&self) -> bool {
        self.capture_engine.is_some()
    }

    /// One tick of the capture pump: pulls the engine's captured input
    /// (interleaved stereo) into the live-analysis monitor, as mono.
    pub fn pump_capture(&mut self) {
        let Some(engine) = &self.capture_engine else {
            return;
        };
        let frames = engine.pull_input_frames();
        if frames.is_empty() {
            return;
        }
        self.live.feed_samples(&downmix_interleaved_stereo(&frames));
    }

    /// Starts/stops feeding the engine's live input into the spectrogram
    /// analyzer. Can't run while recording — including REC's arm window, see
    /// the host's `is_recording` — because the engine capture is single-owner
    /// and belongs to the take; pauses playback when starting. A start asks
    /// for the microphone first ([`AnalysisEffect::RequestMicPermission`]);
    /// it arms in [`AnalysisCache::mic_permission_answered`].
    #[must_use]
    pub fn set_live_monitor(&mut self, on: bool, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        // Cancel a start parked on its permission prompt BEFORE the
        // early-return below: while parked, live_monitor is still false, so
        // an OFF request would otherwise be swallowed and the abandoned start
        // would arm anyway.
        if !on {
            self.live_start_pending = false;
        }
        if self.live_monitor == on {
            return self.finish(host);
        }
        if on {
            if host.is_recording() || self.graph_engine.is_none() {
                return self.finish(host);
            }
            self.live_start_pending = true;
            // Without the grant the capture is fed silence: ask first, as
            // Monitor and Record do, so a refusal says why the view stays
            // flat (and leaves the transport playing).
            self.effects.push(AnalysisEffect::RequestMicPermission);
            return self.finish(host);
        }
        self.live_off();
        self.finish(host)
    }

    /// `set_live_monitor(false)` past its early checks: stops the pump and
    /// the capture.
    fn live_off(&mut self) {
        self.live_start_pending = false;
        if !self.live_monitor {
            return;
        }
        self.capture_engine = None;
        if let Some(engine) = &self.graph_engine {
            engine.stop_capture();
        }
        self.live.stop();
        self.live_monitor = false;
        self.notify();
    }

    /// The answer to a live start's [`AnalysisEffect::RequestMicPermission`]:
    /// arms the capture pump unless the start was cancelled, the cache
    /// disposed or a recording started meanwhile.
    #[must_use]
    pub fn mic_permission_answered(
        &mut self,
        granted: bool,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        let Some(engine) = self.graph_engine.clone() else {
            return self.finish(host);
        };
        if !granted {
            if self.live_start_pending && !self.disposed {
                self.effects.push(AnalysisEffect::Toast(
                    mic_permission::DENIED_MESSAGE.to_string(),
                ));
            }
            self.live_start_pending = false;
            return self.finish(host);
        }
        if self.disposed || !self.live_start_pending || host.is_recording() {
            self.live_start_pending = false;
            return self.finish(host);
        }
        if host.is_playing() {
            host.pause_playback();
        }
        // Re-check after the pause too: a recording start (which owns the
        // capture) or a cancel/dispose during it aborts this start — nothing
        // was armed yet, so there is nothing to undo. (Dart's pause was an
        // await; the engine's returns at once here, but the rule stands.)
        if self.disposed || !self.live_start_pending || host.is_recording() {
            self.live_start_pending = false;
            return self.finish(host);
        }
        self.live_start_pending = false;
        engine.start_capture();
        self.live.start();
        self.live_monitor = true;
        // Two starts can race across the prompt; replacing the pump's engine
        // cancels the prior timer so the first isn't orphaned (it would keep
        // pulling input forever).
        self.capture_engine = Some(engine);
        self.notify();
        self.finish(host)
    }

    // --- AnalysisSource -------------------------------------------------------

    /// Cache key for a take's whole-lane aggregate analysis. Embeds the
    /// ordered clip ids, their timeline starts AND their files (with an
    /// `agg:` prefix that no file path uses) so the key self-invalidates
    /// whenever the clip set changes, a clip moves — the merged analysis
    /// bakes each clip's lane-relative offset into its frame placement — or a
    /// clip is re-read at another path: a synced clip whose bytes had not
    /// arrived carries an `asset://` ref that analyzes to nothing, and once a
    /// re-read hydrates it at its pool path a path-blind key kept serving the
    /// lane without it.
    pub fn aggregate_key_for(track: &AudioTrack) -> String {
        let clips: Vec<String> = track
            .clips
            .iter()
            .map(|c| format!("{}@{}:{}", c.id, c.start_ms, c.audio_file_path))
            .collect();
        format!("agg:{}#{}", track.id, clips.join(","))
    }

    /// The subject the inspector analyzes for the current selection: a single
    /// selected clip (analyze that one file), or — when the lane is selected
    /// via its header — the whole take, which is just the one clip for a
    /// single-clip take and the aggregate across all clips for a multi-clip
    /// take. `None` for no selection, a backing track, or an empty take.
    /// `aggregate` picks which cache family `key` lives in;
    /// `start_ms`/`duration_ms` are the absolute-timeline span the subject's
    /// analysis covers (the clip span for a clip subject, the lane span for
    /// the aggregate — equal for a single-clip lane, whose one clip IS the
    /// lane span). Carried here so the subject axis can never disagree with
    /// which cache the selected getters serve.
    fn analysis_subject(&self, host: &dyn AnalysisHost) -> Option<Subject> {
        let track = host.selected_track()?;
        if track.is_backing() || track.clips.is_empty() {
            return None;
        }
        let clip_subject = |clip: &AudioClip| Subject {
            key: clip.audio_file_path.clone(),
            aggregate: false,
            start_ms: clip.start_ms,
            duration_ms: clip.duration_ms,
        };
        if let Some(clip_id) = host.selected_clip_id() {
            if let Some(clip) = track.clip_by_id(clip_id) {
                return Some(clip_subject(clip));
            }
        }
        if track.clips.len() == 1 {
            return Some(clip_subject(&track.clips[0]));
        }
        Some(Subject {
            key: Self::aggregate_key_for(&track),
            aggregate: true,
            start_ms: track.start_ms(),
            duration_ms: track.duration_ms(),
        })
    }

    /// The absolute-timeline span of the current analysis subject (see
    /// [`AnalysisSource::analysis_start_ms`]): whatever the selected getters
    /// return maps over exactly this span. 0 while there is no subject;
    /// unread in live mode (the live views never map a take).
    pub fn analysis_start_ms(&self, host: &dyn AnalysisHost) -> i64 {
        self.analysis_subject(host).map_or(0, |s| s.start_ms)
    }

    /// The length (ms) of the analysis-subject span; see
    /// [`AnalysisCache::analysis_start_ms`].
    pub fn analysis_duration_ms(&self, host: &dyn AnalysisHost) -> i64 {
        self.analysis_subject(host).map_or(0, |s| s.duration_ms)
    }

    /// The computed spectrogram for the analysis subject, if ready.
    pub fn selected_spectrogram(&self, host: &dyn AnalysisHost) -> Option<Arc<Spectrogram>> {
        let s = self.analysis_subject(host)?;
        if s.aggregate {
            self.agg_analysis_cache.get(&s.key)?.spectrogram
        } else {
            self.spectrogram_cache.get(&s.key)
        }
    }

    /// The computed voice-timbre metrics (ring/tilt/CPP/formants) for the
    /// analysis subject, if ready. Computed alongside the spectrogram.
    pub fn selected_metrics(&self, host: &dyn AnalysisHost) -> Option<Arc<VoiceMetrics>> {
        let s = self.analysis_subject(host)?;
        if s.aggregate {
            self.agg_analysis_cache.get(&s.key)?.metrics
        } else {
            self.metrics_cache.get(&s.key)
        }
    }

    /// The halves of a clip's analysis the dock's tab cannot show without.
    /// The spectrogram views overlay formants when the metrics are there, and
    /// the summary its LTAS, but they show without them.
    fn dock_needs(&self) -> Needs {
        match self.dock_tab {
            DockTab::Mixer => Needs {
                spectrogram: false,
                metrics: false,
            },
            DockTab::Spectrogram | DockTab::Power => Needs {
                spectrogram: true,
                metrics: false,
            },
            DockTab::Ltas => Needs {
                spectrogram: true,
                metrics: true,
            },
            _ => Needs {
                spectrogram: false,
                metrics: true,
            },
        }
    }

    /// Whether the analysis subject lacks a part a view reads (`spectrogram`,
    /// `metrics`) that it can still get and nothing is computing it. For a
    /// clip, a half the byte budget dropped: the halves are separate entries,
    /// and nothing asked for them again — the spectrogram views said the take
    /// could not be analyzed. For a lane aggregate, the merge (its parts are
    /// one entry), unless asking is blocked (see `heal_blocked`).
    pub fn lacks_analysis(
        &self,
        spectrogram: bool,
        metrics: bool,
        host: &dyn AnalysisHost,
    ) -> bool {
        let Some(s) = self.analysis_subject(host) else {
            return false;
        };
        if self.heal_blocked.as_deref() == Some(s.key.as_str()) {
            return false;
        }
        if s.aggregate {
            if let Some(moving) = host.selected_track() {
                if host.is_moving_live(&moving.id) {
                    return false;
                }
            }
            return (spectrogram || metrics)
                && !self.agg_loading.contains(&s.key)
                && !self.agg_analysis_cache.contains_key(&s.key)
                && !self.agg_unavailable.contains(&s.key);
        }
        if self.spectrogram_loading.contains(&s.key) || self.metrics_loading.contains(&s.key) {
            return false;
        }
        (spectrogram && !self.spectrogram_settled(&s.key))
            || (metrics && !self.metrics_settled(&s.key))
    }

    /// [`AnalysisCache::lacks_analysis`] for what the dock's tab shows (see
    /// [`AnalysisCache::heal_selected_analysis`], which the dock asks).
    pub fn selected_analysis_incomplete(&self, host: &dyn AnalysisHost) -> bool {
        let needs = self.dock_needs();
        self.lacks_analysis(needs.spectrogram, needs.metrics, host)
    }

    fn spectrogram_settled(&self, path: &str) -> bool {
        self.spectrogram_cache.contains_key(path) || self.spectrogram_unavailable.contains(path)
    }

    fn metrics_settled(&self, path: &str) -> bool {
        self.metrics_cache.contains_key(path) || self.metrics_unavailable.contains(path)
    }

    /// Asks again for what [`AnalysisCache::lacks_analysis`] says the subject
    /// lacks: by default what the dock's tab shows, or the parts named (see
    /// `ensure_halves`).
    #[must_use]
    pub fn heal_selected_analysis(
        &mut self,
        spectrogram: Option<bool>,
        metrics: Option<bool>,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        let needs = self.dock_needs();
        let wants_spec = spectrogram.unwrap_or(needs.spectrogram);
        let wants_metrics = metrics.unwrap_or(needs.metrics);
        if !self.lacks_analysis(wants_spec, wants_metrics, host) {
            return self.finish(host);
        }
        let Some(s) = self.analysis_subject(host) else {
            return self.finish(host);
        };
        if s.aggregate {
            let then = Cont::HealSelectedAgg { key: s.key };
            match host.selected_track() {
                Some(track) => self.ensure_aggregate_analysis(track, then, host),
                None => self.complete(then, TakeAnalysis::default()),
            }
            return self.finish(host);
        }
        self.ensure_halves(&s.key, wants_spec, wants_metrics, Cont::Done);
        self.finish(host)
    }

    /// Lifts `heal_blocked` once the aggregate `key` is cached or known not
    /// to exist.
    fn forgive_heal(&mut self, key: &str) {
        if self.heal_blocked.as_deref() != Some(key) {
            return;
        }
        if self.agg_analysis_cache.contains_key(key) || self.agg_unavailable.contains(key) {
            self.heal_blocked = None;
        }
    }

    /// What the dock's tab shows, as it opens on it (see `ensure_halves`).
    fn ensure_dock_analysis(&mut self, host: &dyn AnalysisHost) {
        let Some(s) = self.analysis_subject(host) else {
            return;
        };
        if s.aggregate {
            self.ensure_selected(host);
            return;
        }
        let needs = self.dock_needs();
        self.ensure_halves(&s.key, needs.spectrogram, needs.metrics, Cont::Done);
    }

    /// True when the subject's metrics compute already ran and produced
    /// nothing (e.g. a non-WAV import). The inspector uses this to stop
    /// retrying.
    pub fn selected_metrics_unavailable(&self, host: &dyn AnalysisHost) -> bool {
        let Some(s) = self.analysis_subject(host) else {
            return false;
        };
        if s.aggregate {
            self.agg_unavailable.contains(&s.key)
        } else {
            self.metrics_unavailable.contains(&s.key)
        }
    }

    /// The voice profile (range / passaggio / dynamics) for the analysis
    /// subject, derived from the selected metrics and memoized.
    pub fn selected_profile(&self, host: &dyn AnalysisHost) -> Option<Arc<VoiceProfile>> {
        let s = self.analysis_subject(host)?;
        let m = if s.aggregate {
            self.agg_analysis_cache.get(&s.key)?.metrics
        } else {
            self.metrics_cache.get(&s.key)
        }?;
        let mut d = lock(&self.derived);
        let cache = if s.aggregate {
            &mut d.agg_profile
        } else {
            &mut d.profile
        };
        Some(Arc::clone(cache.entry(s.key).or_insert_with(|| {
            Arc::new(analyze_voice_profile(&m, VoiceProfileOptions::default()))
        })))
    }

    /// The long-term average spectrum for the analysis subject (needs both
    /// the spectrogram and metrics); memoized.
    pub fn selected_ltas(&self, host: &dyn AnalysisHost) -> Option<Arc<Ltas>> {
        let s = self.analysis_subject(host)?;
        // ONE lookup for the pair: the memo is keyed by subject alone, so it
        // must never be built from a spectrogram and a metrics half that did
        // not come from the same compute — compute_ltas silently drops its
        // voiced-frame gate when the metrics are missing.
        let agg = if s.aggregate {
            self.agg_analysis_cache.get(&s.key)
        } else {
            None
        };
        let spec = if s.aggregate {
            agg.as_ref()?.spectrogram.clone()
        } else {
            self.spectrogram_cache.get(&s.key)
        }?;
        let m = if s.aggregate {
            agg.and_then(|a| a.metrics)
        } else {
            self.metrics_cache.get(&s.key)
        };
        let mut d = lock(&self.derived);
        let cache = if s.aggregate {
            &mut d.agg_ltas
        } else {
            &mut d.ltas
        };
        Some(Arc::clone(cache.entry(s.key).or_insert_with(|| {
            Arc::new(compute_ltas(&spec, m.as_deref()))
        })))
    }

    /// The register map (per-frame chest↔head) for the analysis subject;
    /// memoized.
    pub fn selected_register(&self, host: &dyn AnalysisHost) -> Option<Arc<RegisterMap>> {
        let s = self.analysis_subject(host)?;
        let m = if s.aggregate {
            self.agg_analysis_cache.get(&s.key)?.metrics
        } else {
            self.metrics_cache.get(&s.key)
        }?;
        let mut d = lock(&self.derived);
        let cache = if s.aggregate {
            &mut d.agg_register
        } else {
            &mut d.register
        };
        Some(Arc::clone(cache.entry(s.key).or_insert_with(|| {
            Arc::new(analyze_register(&m, RegisterOptions::default()))
        })))
    }

    // --- The vocal tract (not Dart's) ----------------------------------------

    /// The body the vocal tract is estimated for (see
    /// [`AnalysisCache::set_vocal_tract_body`]).
    pub fn vocal_tract_body(&self) -> BodyMix {
        self.vocal_tract_body
    }

    /// Sets the body the vocal tract is estimated for: the live estimate,
    /// when it runs, restarts for it, and the subject's track is asked for
    /// again (a track of the other body stays memoized until the subject's
    /// analysis goes, unread).
    #[must_use]
    pub fn set_vocal_tract_body(
        &mut self,
        body: BodyMix,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        if self.vocal_tract_body != body {
            self.vocal_tract_body = body;
            let live = self.monitor.read().vocal_tract_body().is_some();
            if live {
                self.monitor.set_vocal_tract(Some(body));
            }
            self.notify();
        }
        self.ensure_vocal_tract(host)
    }

    /// A body to start from for the analysis subject (its metrics' F3, see
    /// `vocara_services::vocal_tract::suggested_body`), to the nearest
    /// quarter of the way between the adult male, the adult female and the
    /// child: a take's small differences from the singer's others keep their
    /// body (and its track), and few bodies' codebooks are built. `None`
    /// until its metrics are in, or when too few of its frames are voiced.
    pub fn suggested_vocal_tract_body(&self, host: &dyn AnalysisHost) -> Option<BodyMix> {
        let b = suggested_body(self.selected_metrics(host)?.as_ref())?;
        let quarter = |x: f64| (x * 4.0).round() / 4.0;
        Some(BodyMix::new(quarter(b.female), quarter(b.child)))
    }

    /// Moves the vocal tract (the tracks and the live estimate) to the body
    /// the analysis subject's voice suggests, once per subject, when its
    /// metrics are in: the view's way of fitting the singer without asking.
    #[must_use]
    pub fn follow_suggested_vocal_tract_body(
        &mut self,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        let Some(s) = self.analysis_subject(host) else {
            return Vec::new();
        };
        if self.tract_body_followed.as_deref() == Some(s.key.as_str())
            || self.selected_metrics(host).is_none()
        {
            return Vec::new();
        }
        self.tract_body_followed = Some(s.key);
        match self.suggested_vocal_tract_body(host) {
            Some(body) if body != self.vocal_tract_body => self.set_vocal_tract_body(body, host),
            _ => Vec::new(),
        }
    }

    /// The analysis subject's vocal-tract track for the current body, once
    /// computed (see [`AnalysisCache::ensure_vocal_tract`]). Its times are the
    /// subject's, as its metrics' are.
    pub fn selected_vocal_tract(&self, host: &dyn AnalysisHost) -> Option<Arc<VocalTractTrack>> {
        let s = self.analysis_subject(host)?;
        let d = lock(&self.derived);
        let map = if s.aggregate { &d.agg_tract } else { &d.tract };
        map.get(&s.key)
            .filter(|t| t.body() == self.vocal_tract_body)
            .cloned()
    }

    /// Whether the analysis subject's vocal-tract track is computing.
    pub fn vocal_tract_loading(&self, host: &dyn AnalysisHost) -> bool {
        self.analysis_subject(host)
            .is_some_and(|s| self.tract_in_flight.contains_key(&s.key))
    }

    /// Whether the analysis subject's vocal-tract track failed to compute
    /// for the current body.
    pub fn vocal_tract_failed(&self, host: &dyn AnalysisHost) -> bool {
        self.analysis_subject(host).is_some_and(|s| {
            self.tract_failed
                .contains(&tract_failure_key(self.vocal_tract_body, &s.key))
        })
    }

    /// Asks for the analysis subject's vocal-tract track. Without its metrics
    /// it asks for them (as [`AnalysisCache::heal_selected_analysis`] does)
    /// and must be called again once they are in: a view showing a take calls
    /// it as it shows one, the inspector's way. With them, the track computes
    /// on a worker (a few seconds for a long take, in an analysis-gate slot)
    /// and is memoized beside them, dropped with them. A track computed,
    /// computing, or whose compute failed for this body is not asked again.
    #[must_use]
    pub fn ensure_vocal_tract(&mut self, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        if self.disposed {
            return self.finish(host);
        }
        let Some(s) = self.analysis_subject(host) else {
            return self.finish(host);
        };
        let Some(metrics) = self.selected_metrics(host) else {
            return self.heal_selected_analysis(Some(false), Some(true), host);
        };
        let body = self.vocal_tract_body;
        if self.selected_vocal_tract(host).is_some()
            || self.tract_in_flight.contains_key(&s.key)
            || self.tract_failed.contains(&tract_failure_key(body, &s.key))
        {
            return self.finish(host);
        }
        let disowned = Arc::new(AtomicBool::new(false));
        let wanted =
            WantedProbe::not_disowned(Arc::clone(&self.disposed_flag), Arc::clone(&disowned));
        let id = self.id();
        self.tract_in_flight.insert(s.key.clone(), id);
        self.pending.insert(
            id,
            Pending::VocalTract(TractCompute {
                key: s.key,
                aggregate: s.aggregate,
                metrics: Arc::clone(&metrics),
                body,
                disowned,
            }),
        );
        self.effects.push(AnalysisEffect::Run(AnalysisJob {
            id,
            work: Work::VocalTract {
                gate: self.gate,
                metrics,
                body,
                wanted,
            },
        }));
        self.notify();
        self.finish(host)
    }

    /// The vocal tract at the playhead: the subject's track at the
    /// playhead's time within the subject's span. `None` until the track is
    /// computed.
    pub fn vocal_tract_at_playhead(&self, host: &dyn AnalysisHost) -> Option<VocalTractState> {
        let track = self.selected_vocal_tract(host)?;
        let position_ms = i64::try_from(host.position().as_millis()).unwrap_or(i64::MAX);
        let seconds = (position_ms - self.analysis_start_ms(host)) as f64 / 1000.0;
        track.state_at(seconds)
    }

    /// Starts (or stops) estimating the live vocal tract, the monitor's, for
    /// the current body: on while the view shows the live mic.
    pub fn set_live_vocal_tract(&mut self, on: bool) {
        self.monitor
            .set_vocal_tract(on.then_some(self.vocal_tract_body));
        self.notify();
    }

    /// Whether the live vocal tract is being estimated (asked for by
    /// [`AnalysisCache::set_live_vocal_tract`]).
    pub fn live_vocal_tract_on(&self) -> bool {
        self.monitor.read().vocal_tract_body().is_some()
    }

    /// The live vocal tract after the latest mic frame; `None` while it is
    /// not asked for, and until its body's codebook is built.
    pub fn live_vocal_tract(&self) -> Option<VocalTractState> {
        self.monitor.read().vocal_tract().copied()
    }

    /// A track landed: memoized under its subject while the subject's
    /// metrics are still the ones it came from.
    fn tract_landed(
        &mut self,
        id: u64,
        t: TractCompute,
        result: Result<Option<Arc<VocalTractTrack>>, String>,
    ) {
        if self.tract_in_flight.get(&t.key) == Some(&id) {
            self.tract_in_flight.remove(&t.key);
        }
        if self.disposed || t.disowned.load(Ordering::SeqCst) {
            self.notify();
            return;
        }
        match result {
            Ok(Some(track)) => {
                let current = if t.aggregate {
                    self.agg_analysis_cache.get(&t.key).and_then(|a| a.metrics)
                } else {
                    self.metrics_cache.get(&t.key)
                };
                if current.is_some_and(|m| Arc::ptr_eq(&m, &t.metrics)) {
                    let mut d = lock(&self.derived);
                    let map = if t.aggregate {
                        &mut d.agg_tract
                    } else {
                        &mut d.tract
                    };
                    map.insert(t.key, track);
                }
            }
            // no longer wanted as the gate admitted it
            Ok(None) => {}
            Err(_) => {
                self.tract_failed.insert(tract_failure_key(t.body, &t.key));
            }
        }
        self.notify();
    }

    pub fn spectrogram_loading(&self, host: &dyn AnalysisHost) -> bool {
        let Some(s) = self.analysis_subject(host) else {
            return false;
        };
        if s.aggregate {
            self.agg_loading.contains(&s.key) || self.merge_awaits_move(host, None, &s.key)
        } else {
            self.spectrogram_loading.contains(&s.key)
        }
    }

    pub fn metrics_loading(&self, host: &dyn AnalysisHost) -> bool {
        let Some(s) = self.analysis_subject(host) else {
            return false;
        };
        if s.aggregate {
            self.agg_loading.contains(&s.key) || self.merge_awaits_move(host, None, &s.key)
        } else {
            self.metrics_loading.contains(&s.key)
        }
    }

    /// Whether take `track_id`'s merge `key` is unasked only because one of
    /// its clips is being dragged (see [`AnalysisCache::lacks_analysis`],
    /// [`AnalysisCache::lane_lacks_data`]): each tick re-keys it, and it is
    /// computed once the move lands. Its views wait for it — the dock, the
    /// inspector and its own lanes, read as settled, said the take could not
    /// be analyzed for the whole drag. `None` reads the selected track.
    fn merge_awaits_move(
        &self,
        host: &dyn AnalysisHost,
        track_id: Option<&str>,
        key: &str,
    ) -> bool {
        let selected;
        let track_id = match track_id {
            Some(id) => id,
            None => match host.selected_track() {
                Some(t) => {
                    selected = t;
                    &selected.id
                }
                None => return false,
            },
        };
        host.is_moving_live(track_id)
            && !self.agg_analysis_cache.contains_key(key)
            && !self.agg_unavailable.contains(key)
    }

    // --- Per-lane analysis ----------------------------------------------------

    /// Voice metrics for an arbitrary lane (its inline sub-lanes read this):
    /// the clip's own analysis for a single-clip lane, or the whole-lane
    /// aggregate — aligned to the lane span, gaps as NaN frames — for a
    /// multi-clip lane.
    pub fn metrics_for(
        &self,
        track_id: &str,
        host: &dyn AnalysisHost,
    ) -> Option<Arc<VoiceMetrics>> {
        let t = host.track_by_id(track_id)?;
        if t.clips.len() > 1 {
            return self
                .agg_analysis_cache
                .get(&Self::aggregate_key_for(&t))?
                .metrics;
        }
        let clip = t.primary_clip()?;
        self.metrics_cache.get(&clip.audio_file_path)
    }

    pub fn metrics_loading_for(&self, track_id: &str, host: &dyn AnalysisHost) -> bool {
        let Some(t) = host.track_by_id(track_id) else {
            return false;
        };
        if t.clips.len() > 1 {
            return self.lane_merge_loading(&t, host);
        }
        t.primary_clip()
            .is_some_and(|c| self.metrics_loading.contains(&c.audio_file_path))
    }

    /// Computed spectrogram for an arbitrary lane (its inline sub-lanes read
    /// this); single vs multi-clip resolution mirrors
    /// [`AnalysisCache::metrics_for`].
    pub fn spectrogram_for(
        &self,
        track_id: &str,
        host: &dyn AnalysisHost,
    ) -> Option<Arc<Spectrogram>> {
        let t = host.track_by_id(track_id)?;
        if t.clips.len() > 1 {
            return self
                .agg_analysis_cache
                .get(&Self::aggregate_key_for(&t))?
                .spectrogram;
        }
        let clip = t.primary_clip()?;
        self.spectrogram_cache.get(&clip.audio_file_path)
    }

    pub fn spectrogram_loading_for(&self, track_id: &str, host: &dyn AnalysisHost) -> bool {
        let Some(t) = host.track_by_id(track_id) else {
            return false;
        };
        if t.clips.len() > 1 {
            return self.lane_merge_loading(&t, host);
        }
        t.primary_clip()
            .is_some_and(|c| self.spectrogram_loading.contains(&c.audio_file_path))
    }

    /// Whether multi-clip take `t`'s merge is computing, or waits for a clip
    /// being dragged to land (see `merge_awaits_move`).
    fn lane_merge_loading(&self, t: &AudioTrack, host: &dyn AnalysisHost) -> bool {
        let key = Self::aggregate_key_for(t);
        self.agg_loading.contains(&key) || self.merge_awaits_move(host, Some(&t.id), &key)
    }

    // --- Inline analysis sub-lanes (Logic-style fold-out) ---------------------

    pub fn is_track_expanded(&self, track_id: &str) -> bool {
        self.expanded_tracks.iter().any(|id| id == track_id)
    }

    pub fn lanes_for(&self, track_id: &str) -> BTreeSet<AnalysisLane> {
        self.track_lanes.get(track_id).cloned().unwrap_or_default()
    }

    /// Folds a track's analysis sub-area open/closed. First open defaults to
    /// the pitch lane. View state — not persisted.
    #[must_use]
    pub fn toggle_track_expanded(
        &mut self,
        track_id: &str,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        if let Some(i) = self.expanded_tracks.iter().position(|id| id == track_id) {
            self.expanded_tracks.remove(i);
            self.notify();
            return self.finish(host);
        }
        self.expanded_tracks.push(track_id.to_string());
        self.track_lanes
            .entry(track_id.to_string())
            .or_insert_with(|| BTreeSet::from([AnalysisLane::Pitch]));
        self.ensure_lane_data(track_id, host);
        self.notify();
        self.finish(host)
    }

    /// Turns a single sub-lane on/off for `track_id`, computing its data on
    /// demand.
    #[must_use]
    pub fn toggle_lane(
        &mut self,
        track_id: &str,
        lane: AnalysisLane,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        let set = self.track_lanes.entry(track_id.to_string()).or_default();
        if !set.remove(&lane) {
            set.insert(lane);
        }
        self.ensure_lane_data(track_id, host);
        self.notify();
        self.finish(host)
    }

    /// Drops the fold-out state of a lane that no longer exists (deleted
    /// track, merged-away take, removed interpretation).
    pub fn drop_lane_state(&mut self, track_id: &str) {
        self.expanded_tracks.retain(|id| id != track_id);
        self.track_lanes.remove(track_id);
    }

    /// Recomputes whatever every folded-open lane shows — a clip move changed
    /// the lane-relative offsets aggregate analysis bakes in. Cheap no-ops
    /// for single-clip takes and cached subjects.
    #[must_use]
    pub fn refresh_expanded_lanes(&mut self, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        for id in self.expanded_tracks.clone() {
            self.ensure_lane_data(&id, host);
        }
        self.finish(host)
    }

    /// What the folded-open lanes of `track_id` read: metrics (pitch,
    /// loudness, power) and the spectrogram (spectrogram, power).
    fn lane_needs(&self, track_id: &str) -> Needs {
        let lanes = self.track_lanes.get(track_id);
        let has = |l: AnalysisLane| lanes.is_some_and(|s| s.contains(&l));
        Needs {
            metrics: has(AnalysisLane::Pitch)
                || has(AnalysisLane::Loudness)
                || has(AnalysisLane::Power),
            spectrogram: has(AnalysisLane::Spectrogram) || has(AnalysisLane::Power),
        }
    }

    /// Whether the folded-open lanes of `track_id` lack data they can still
    /// get, with nothing computing it: the byte budget let it go (every lane,
    /// the dock and the inspector share it), or a clip moved and re-keyed the
    /// lane. Lanes are computed only when folded open or a lane is toggled,
    /// so they sat empty, saying the take could not be analyzed, until the
    /// user toggled the lane again. A lane on screen asks
    /// [`AnalysisCache::heal_lane`], as the dock asks
    /// [`AnalysisCache::heal_selected_analysis`].
    pub fn lane_lacks_data(&self, track_id: &str, host: &dyn AnalysisHost) -> bool {
        if !self.is_track_expanded(track_id) || host.is_moving_live(track_id) {
            return false;
        }
        let Some(t) = host.track_by_id(track_id) else {
            return false;
        };
        let needs = self.lane_needs(track_id);
        if !needs.metrics && !needs.spectrogram {
            return false;
        }
        if t.clips.len() > 1 {
            let key = Self::aggregate_key_for(&t);
            return !self.agg_loading.contains(&key)
                && !self.agg_analysis_cache.contains_key(&key)
                && !self.agg_unavailable.contains(&key)
                && !self.lane_heal_blocked.contains(&key);
        }
        let Some(path) = t.primary_clip().map(|c| c.audio_file_path.as_str()) else {
            return false;
        };
        if self.metrics_loading.contains(path) || self.spectrogram_loading.contains(path) {
            return false;
        }
        (needs.metrics && !self.metrics_settled(path))
            || (needs.spectrogram && !self.spectrogram_settled(path))
    }

    /// Asks again for what [`AnalysisCache::lane_lacks_data`] says the lanes
    /// of `track_id` lack.
    #[must_use]
    pub fn heal_lane(&mut self, track_id: &str, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        if self.disposed || !self.lane_lacks_data(track_id, host) {
            return self.finish(host);
        }
        let Some(t) = host.track_by_id(track_id) else {
            return self.finish(host);
        };
        let multi = t.clips.len() > 1;
        let subject = if multi {
            Self::aggregate_key_for(&t)
        } else {
            match t.primary_clip() {
                Some(c) => c.audio_file_path.clone(),
                None => return self.finish(host),
            }
        };
        let now = (self.now)();
        self.lane_healed_at
            .retain(|_, at| now.saturating_duration_since(*at) < LANE_HEAL_INTERVAL);
        self.lane_heals
            .retain(|_, h| now.saturating_duration_since(h.at) < LANE_HEAL_QUIET);
        if let Some(&last) = self.lane_healed_at.get(&subject) {
            // Asked again once the interval is up: nothing else may rebuild
            // the lane, and it sat empty on an idle screen. Not for a subject
            // already healed a few times: more analysis on screen than the
            // budget holds has every view evict another's in turn, and an
            // idle screen then decoded something every interval for as long
            // as it stayed open.
            let count = self.lane_heals.get(&subject).map_or(0, |h| h.count);
            if count < LANE_HEAL_RETRIES && !self.lane_heal_retry {
                self.lane_heal_retry = true;
                self.effects.push(AnalysisEffect::LaneHealRetry {
                    after: LANE_HEAL_INTERVAL.saturating_sub(now.saturating_duration_since(last)),
                });
            }
            return self.finish(host);
        }
        self.lane_healed_at.insert(subject.clone(), now);
        let count = self.lane_heals.get(&subject).map_or(0, |h| h.count) + 1;
        self.lane_heals
            .insert(subject.clone(), LaneHeals { count, at: now });
        if multi {
            let key = subject;
            // Only lanes as they are now: a key from an earlier layout can
            // never be asked for again.
            let current: HashSet<String> = self
                .expanded_tracks
                .iter()
                .filter_map(|id| host.track_by_id(id))
                .filter(|lane| lane.clips.len() > 1)
                .map(|lane| Self::aggregate_key_for(&lane))
                .collect();
            self.lane_heal_blocked.retain(|k| current.contains(k));
            self.ensure_aggregate_analysis(t, Cont::HealLaneAgg { key }, host);
            return self.finish(host);
        }
        let needs = self.lane_needs(track_id);
        self.ensure_halves(&subject, needs.spectrogram, needs.metrics, Cont::Done);
        self.finish(host)
    }

    /// The lane-heal retry's timer fired (see
    /// [`AnalysisEffect::LaneHealRetry`]): the lanes rebuild, and ask again.
    pub fn lane_heal_retry_due(&mut self) {
        if self.disposed || !self.lane_heal_retry {
            return;
        }
        self.lane_heal_retry = false;
        self.notify();
    }

    /// Computes whatever the currently-shown lanes need (metrics /
    /// spectrogram).
    fn ensure_lane_data(&mut self, track_id: &str, host: &dyn AnalysisHost) {
        let needs = self.lane_needs(track_id);
        if needs.metrics {
            self.ensure_metrics_for_track(track_id, host);
        }
        if needs.spectrogram {
            self.ensure_spectrogram_for_track(track_id, host);
        }
    }

    // --- Compute (lazy, off the UI thread) ------------------------------------

    /// Computes + caches voice metrics for a lane (lazy, on a worker;
    /// WAV-only): the clip's file for a single-clip lane, or the whole-lane
    /// aggregate for a multi-clip lane. Shares the caches with the
    /// Voiceprint dock.
    #[must_use]
    pub fn ensure_metrics_for(
        &mut self,
        track_id: &str,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        self.ensure_metrics_for_track(track_id, host);
        self.finish(host)
    }

    fn ensure_metrics_for_track(&mut self, track_id: &str, host: &dyn AnalysisHost) {
        let Some(track) = host.track_by_id(track_id) else {
            return;
        };
        if track.clips.len() > 1 {
            self.ensure_aggregate_analysis(track, Cont::Done, host);
            return;
        }
        if let Some(clip) = track.primary_clip() {
            let path = clip.audio_file_path.clone();
            self.ensure_halves(&path, false, true, Cont::Done);
        }
    }

    /// Computes + caches the spectrogram for a lane (lazy); single vs
    /// multi-clip resolution mirrors [`AnalysisCache::ensure_metrics_for`].
    #[must_use]
    pub fn ensure_spectrogram_for(
        &mut self,
        track_id: &str,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        self.ensure_spectrogram_for_track(track_id, host);
        self.finish(host)
    }

    fn ensure_spectrogram_for_track(&mut self, track_id: &str, host: &dyn AnalysisHost) {
        let Some(track) = host.track_by_id(track_id) else {
            return;
        };
        if track.clips.len() > 1 {
            self.ensure_aggregate_analysis(track, Cont::Done, host);
            return;
        }
        if let Some(clip) = track.primary_clip() {
            let path = clip.audio_file_path.clone();
            self.ensure_halves(&path, true, false, Cont::Done);
        }
    }

    /// The halves of a clip's analysis a view shows (`spectrogram`,
    /// `metrics`).
    ///
    /// With neither half held or computing, both come from the one decode the
    /// dock would ask for, with the LTAS and voice profile derived on the
    /// analysis worker — asked for one at a time (the Pitch lane folded out
    /// first, the dock opened after), the two took two decodes and the dock's
    /// summary built the LTAS and profile on the UI thread, ~0.4 s for a
    /// 10-minute take. Otherwise only the halves asked for: the other one may
    /// be a half the byte budget dropped, which a lane that never shows it
    /// asked for again after every clip edit, and which a tab switch computed
    /// at the cost of the half the new tab shows.
    fn ensure_halves(&mut self, path: &str, spectrogram: bool, metrics: bool, then: Cont) {
        let spectrogram_absent = !self.spectrogram_cache.contains_key(path)
            && !self.spectrogram_loading.contains(path)
            && !self.spectrogram_unavailable.contains(path);
        let metrics_absent = !self.metrics_cache.contains_key(path)
            && !self.metrics_loading.contains(path)
            && !self.metrics_unavailable.contains(path);
        if spectrogram_absent && metrics_absent {
            self.ensure_clip_analysis(path, then);
            return;
        }
        let members = usize::from(spectrogram) + usize::from(metrics);
        if members == 0 {
            self.complete(then, TakeAnalysis::default());
            return;
        }
        let group = self.group(members, then);
        if spectrogram {
            self.ensure_half_for_path(Half::Spectrogram, path, Cont::Group(group));
        }
        if metrics {
            self.ensure_half_for_path(Half::Metrics, path, Cont::Group(group));
        }
    }

    /// Computes + caches one half of the analysis of one audio file (lazy,
    /// on a worker; WAV-only): Dart's `_ensureSpectrogramForPath` and
    /// `_ensureMetricsForPath`. Keyed by `path` so a clip's result is shared
    /// by the per-clip inspector, the aggregate merge, and the dock.
    fn ensure_half_for_path(&mut self, half: Half, path: &str, then: Cont) {
        let (held, unavailable) = match half {
            Half::Spectrogram => (
                self.spectrogram_cache.contains_key(path),
                self.spectrogram_unavailable.contains(path),
            ),
            Half::Metrics => (
                self.metrics_cache.contains_key(path),
                self.metrics_unavailable.contains(path),
            ),
        };
        if self.disposed || path.is_empty() || held || unavailable {
            self.complete(then, TakeAnalysis::default());
            return;
        }
        // Already computing (possibly as half of a paired clip compute): wait
        // for it. Returning here as if there were nothing pending is what let
        // the aggregate merge see a hole that no one would ever fill.
        if self.in_flight(half).contains_key(path) {
            self.join(half, path, then);
            return;
        }
        let signal = self.signal();
        let generation = self.generation_of(path);
        match half {
            Half::Spectrogram => {
                self.spectrogram_in_flight.insert(path.to_string(), signal);
                self.spectrogram_loading.insert(path.to_string());
            }
            Half::Metrics => {
                self.metrics_in_flight.insert(path.to_string(), signal);
                self.metrics_loading.insert(path.to_string());
            }
        }
        self.notify();
        let kind = match half {
            Half::Spectrogram => JobKind::Spectrogram,
            Half::Metrics => JobKind::Metrics,
        };
        self.start_compute(
            kind,
            path,
            AnalysisParams::default(),
            Compute {
                kind,
                path: path.to_string(),
                generation,
                signal: Some(signal),
                then,
            },
        );
    }

    /// Computes spectrogram + metrics for one clip's file together. When BOTH
    /// are missing (the common case: a take was just selected), they come
    /// from ONE decode in ONE compute — the old shape ran two concurrent
    /// computes that each read and decoded the same file, doubling peak
    /// memory for the whole compute. Falls back to the individual paths when
    /// only one side is missing (its twin stays cached).
    /// Hands the clip's pair to `then` so a caller can HOLD it: the per-path
    /// caches are byte-budgeted, so a later clip's insert can evict this one
    /// before the caller reads it back.
    fn ensure_clip_analysis(&mut self, path: &str, then: Cont) {
        if self.disposed || path.is_empty() {
            self.complete(then, TakeAnalysis::default());
            return;
        }
        let need_spec = !self.spectrogram_cache.contains_key(path)
            && !self.spectrogram_loading.contains(path)
            && !self.spectrogram_unavailable.contains(path);
        let need_metrics = !self.metrics_cache.contains_key(path)
            && !self.metrics_loading.contains(path)
            && !self.metrics_unavailable.contains(path);
        if !need_spec || !need_metrics {
            let group = self.group(
                2,
                Cont::ClipPair {
                    path: path.to_string(),
                    then: Box::new(then),
                },
            );
            self.ensure_half_for_path(Half::Spectrogram, path, Cont::Group(group));
            self.ensure_half_for_path(Half::Metrics, path, Cont::Group(group));
            return;
        }
        // ONE compute, registered under BOTH paths' in-flight maps: whichever
        // half a later caller asks for, it joins this work instead of starting
        // its own or giving up.
        let signal = self.signal();
        let generation = self.generation_of(path);
        self.spectrogram_in_flight.insert(path.to_string(), signal);
        self.metrics_in_flight.insert(path.to_string(), signal);
        self.spectrogram_loading.insert(path.to_string());
        self.metrics_loading.insert(path.to_string());
        self.notify();
        self.start_compute(
            JobKind::Both,
            path,
            AnalysisParams::default(),
            Compute {
                kind: JobKind::Both,
                path: path.to_string(),
                generation,
                signal: Some(signal),
                then,
            },
        );
    }

    /// Reads a clip's cached pair, refreshing its recency — a part just
    /// handed to the aggregate merge is the newest, not the next eviction
    /// victim.
    fn clip_analysis(&self, path: &str) -> TakeAnalysis {
        TakeAnalysis {
            spectrogram: self.spectrogram_cache.get(path),
            metrics: self.metrics_cache.get(path),
        }
    }

    /// The pre-EQ input spectrum column at the playhead for `track_id`'s
    /// currently sounding clip, plus its bin width (Hz) — drives the FX EQ
    /// analyzer. `None` when no clip of the take is under the playhead, or
    /// its display spectrogram isn't computed yet (kicking off that compute
    /// on the way: the effects).
    #[must_use]
    pub fn eq_input_column(
        &mut self,
        track_id: &str,
        host: &dyn AnalysisHost,
    ) -> (Option<EqInputColumn>, Vec<AnalysisEffect>) {
        let Some(track) = host.track_by_id(track_id) else {
            return (None, self.finish(host));
        };
        let pos_ms = i64::try_from(host.position().as_millis()).unwrap_or(i64::MAX);
        let Some(clip) = track
            .clips
            .iter()
            .find(|c| pos_ms >= c.start_ms && pos_ms < c.end_ms())
        else {
            return (None, self.finish(host));
        };
        let Some(spec) = self.eq_spectro_cache.get(&clip.audio_file_path) else {
            let path = clip.audio_file_path.clone();
            self.ensure_eq_spectrogram_for_path(&path);
            return (None, self.finish(host));
        };
        let column = EqInputColumn {
            spectrogram: spec,
            seconds: (pos_ms - clip.start_ms) as f64 / 1000.0,
        };
        (Some(column), self.finish(host))
    }

    /// Computes + caches the wide-range display spectrogram for one clip
    /// file.
    fn ensure_eq_spectrogram_for_path(&mut self, path: &str) {
        if self.disposed
            || path.is_empty()
            || self.eq_spectro_cache.contains_key(path)
            || self.eq_spectro_loading.contains(path)
            || self.eq_spectro_unavailable.contains(path)
        {
            return;
        }
        let generation = self.generation_of(path);
        self.eq_spectro_loading.insert(path.to_string());
        self.start_compute(
            JobKind::EqSpectrogram,
            path,
            AnalysisParams {
                max_hz: EQ_DISPLAY_MAX_HZ,
                ..AnalysisParams::default()
            },
            Compute {
                kind: JobKind::EqSpectrogram,
                path: path.to_string(),
                generation,
                signal: None,
                then: Cont::Done,
            },
        );
    }

    /// Computes the whole-lane aggregate for a multi-clip `track`: analyzes
    /// each clip's file (reusing the per-path caches, so selecting a clip
    /// later is already warm), then merges the per-clip results at their
    /// lane-relative offsets — so the aggregate spans exactly the track's
    /// `start_ms..end_ms`, with gaps between clips as silence. Clips that
    /// fail or aren't WAV are simply skipped; the aggregate is built from
    /// whatever analyzed.
    fn ensure_aggregate_analysis(
        &mut self,
        track: Arc<AudioTrack>,
        then: Cont,
        host: &dyn AnalysisHost,
    ) {
        let key = Self::aggregate_key_for(&track);
        if self.disposed
            || self.agg_analysis_cache.contains_key(&key)
            || self.agg_loading.contains(&key)
            || self.agg_unavailable.contains(&key)
        {
            self.complete(then, TakeAnalysis::default());
            return;
        }
        if self.agg_in_flight.contains_key(&track.id) {
            let queued = self.agg_queued.remove(&track.id);
            if let Some(q) = &queued {
                self.agg_loading.remove(&q.key);
            }
            let done = match queued {
                Some(q) => q.done,
                None => self.signal(),
            };
            self.agg_queued.insert(
                track.id.clone(),
                Queued {
                    key: key.clone(),
                    done,
                },
            );
            self.agg_loading.insert(key);
            self.notify();
            self.wait(done, Waiter { join: None, then });
            return;
        }
        self.agg_in_flight.insert(track.id.clone(), key.clone());
        self.agg_loading.insert(key.clone());
        self.notify();
        let run = self.id();
        self.agg_runs.insert(
            run,
            AggRun {
                track,
                key,
                parts: HashMap::new(),
                next: 0,
                disowned: Arc::new(AtomicBool::new(false)),
                then,
            },
        );
        self.agg_next_clip(run, host);
    }

    /// The aggregate loop: the next clip, or the merge once every clip is
    /// in.
    ///
    /// Sequential on purpose: each clip's analysis holds its file bytes plus
    /// its PCM on a worker, so N clips at once meant N concurrent full-file
    /// copies with no upper bound. One at a time keeps the peak at a single
    /// clip; the per-path caches make later clips warm for every other
    /// consumer either way.
    ///
    /// Each clip's parts are HELD as the loop produces them. The per-path
    /// caches are byte-budgeted, so a later clip's insert can evict an
    /// earlier one before the merge below reads it — and a merge treats a
    /// missing part as SILENCE, which would bake a lane analysis quietly
    /// missing a clip under a key only a clip-set change can invalidate.
    fn agg_next_clip(&mut self, run: u64, host: &dyn AnalysisHost) {
        let Some(r) = self.agg_runs.get(&run) else {
            return;
        };
        if let Some(clip) = r.track.clips.get(r.next) {
            let path = clip.audio_file_path.clone();
            self.ensure_clip_analysis(&path, Cont::AggPart { run });
            return;
        }
        self.agg_merge(run, host);
    }

    /// The aggregate loop got its current clip's pair.
    fn agg_part(&mut self, run: u64, part: TakeAnalysis, host: &dyn AnalysisHost) {
        let Some(r) = self.agg_runs.get_mut(&run) else {
            return;
        };
        if let Some(clip) = r.track.clips.get(r.next) {
            r.parts.insert(clip.audio_file_path.clone(), part);
        }
        r.next += 1;
        // Between clips is the only place this loop can be abandoned: every
        // further clip claims an analysis-gate slot, and the merge allocates
        // a lane-length result, for a cache with no reader left.
        if self.disposed {
            self.agg_finally(run, host);
            return;
        }
        self.agg_next_clip(run, host);
    }

    /// The aggregate's merge, once every clip's analysis is held.
    fn agg_merge(&mut self, run: u64, host: &dyn AnalysisHost) {
        let Some(r) = self.agg_runs.get(&run) else {
            return;
        };
        // Offsets are relative to the lane's span start (min over ALL clips,
        // even ones whose analysis failed), so the merged frames line up with
        // the lane axis (track start/duration) every consumer maps against.
        let lane_start_ms = r.track.start_ms();
        let mut metrics = Vec::new();
        let mut specs = Vec::new();
        let mut complete = true;
        for c in &r.track.clips {
            let path = &c.audio_file_path;
            let part = r.parts.get(path);
            // The cached re-read is belt-and-braces now that the clip
            // analysis JOINS an in-flight compute instead of returning ahead
            // of it: the part is already filled. It still covers a
            // byte-budget eviction between the loop and this merge.
            let m = part
                .and_then(|p| p.metrics.clone())
                .or_else(|| self.metrics_cache.get(path));
            let spec = part
                .and_then(|p| p.spectrogram.clone())
                .or_else(|| self.spectrogram_cache.get(path));
            // Fold a clip in only when BOTH analyses are present and
            // non-empty, kept as a pair — an empty/degenerate clip yields
            // spec.frames=1 but metrics.frames=0, which would desync the two
            // merged framings (LTAS gates spectrogram frames by the metrics'
            // frame index).
            match (m, spec) {
                (Some(m), Some(spec)) if m.frames > 0 && spec.frames > 0 => {
                    let offset_ms = c.start_ms - lane_start_ms;
                    metrics.push(VoiceMetricsPart {
                        offset_ms,
                        metrics: m,
                    });
                    specs.push(SpectrogramPart {
                        offset_ms,
                        spectrogram: spec,
                    });
                }
                _ => {
                    if !self.spectrogram_unavailable.contains(path)
                        && !self.metrics_unavailable.contains(path)
                    {
                        // A clip that could not analyze at all is a
                        // permanent, honest gap; one merely absent here is a
                        // transient miss the merge must not freeze into the
                        // lane result.
                        complete = false;
                    }
                }
            }
        }
        // A merge nothing will keep (a part missing, or an eviction disowned
        // it: neither is cached below) is not run: it held an analysis slot,
        // every clip's analysis copied to a worker, for nothing.
        if !complete || self.agg_disowned.contains(&r.key) {
            self.agg_finally(run, host);
            return;
        }
        // Off the UI thread: the merges and the views derived from them are
        // lane-length work (a 10-minute lane's LTAS and profile alone ran
        // ~250 ms in the dock's getters). Asked again once a slot admits it:
        // one queued while the screen closed took the slot from the next
        // screen's analysis.
        let wanted =
            WantedProbe::not_disowned(Arc::clone(&self.disposed_flag), Arc::clone(&r.disowned));
        let id = self.id();
        self.pending.insert(id, Pending::Merge { run });
        self.effects.push(AnalysisEffect::Run(AnalysisJob {
            id,
            work: Work::Merge {
                gate: self.gate,
                metrics,
                specs,
                wanted,
            },
        }));
    }

    /// The lane merge landed.
    fn agg_merged(
        &mut self,
        run: u64,
        result: Result<Option<MergedLane>, String>,
        host: &dyn AnalysisHost,
    ) {
        let Some(key) = self.agg_runs.get(&run).map(|r| r.key.clone()) else {
            return;
        };
        match result {
            Err(_) => {
                if !self.agg_disowned.contains(&key) {
                    self.agg_unavailable.insert(key);
                }
            }
            Ok(None) => {}
            Ok(Some(_)) if self.disposed => {}
            Ok(Some(lane)) => {
                // A lane whose parts did not all land is not cached (the
                // merge was not run): leaving the key absent makes the next
                // ensure recompute against now-warm per-clip caches, where a
                // cached partial would be permanent (the key only changes
                // with the clip set). See `agg_disowned`: an eviction landed
                // while the clips were being analyzed, so this merge folds
                // bytes that are already stale.
                if !self.agg_disowned.contains(&key) {
                    // Stored as ONE entry: both merges describe the same
                    // clips at the same offsets, every derived view (LTAS,
                    // profile, register) is only sound against that exact
                    // pair, and the guard above takes a cached entry to be
                    // the whole answer. A missing spectrogram beside live
                    // metrics is a SETTLED answer, not a miss —
                    // merge_spectrograms refuses lane spans past its cell
                    // budget where the metrics merge still succeeds, and the
                    // pitch and timbre views remain worth showing on their
                    // own.
                    if lane.metrics.is_some() || lane.spectrogram.is_some() {
                        self.agg_analysis_cache.insert(
                            key.clone(),
                            TakeAnalysis {
                                spectrogram: lane.spectrogram,
                                metrics: lane.metrics,
                            },
                        );
                        // Seeded only while the pair is held: the eviction
                        // hook drops these memos with it, and an entry the
                        // byte budget refused at once must not leave them
                        // behind.
                        if self.agg_analysis_cache.contains_key(&key) {
                            let mut d = lock(&self.derived);
                            if let Some(ltas) = lane.ltas {
                                d.agg_ltas.insert(key.clone(), ltas);
                            }
                            if let Some(profile) = lane.profile {
                                d.agg_profile.insert(key.clone(), profile);
                            }
                        }
                    } else {
                        self.agg_unavailable.insert(key);
                    }
                }
            }
        }
        self.agg_finally(run, host);
    }

    /// The aggregate's `finally`.
    fn agg_finally(&mut self, run: u64, host: &dyn AnalysisHost) {
        let Some(r) = self.agg_runs.remove(&run) else {
            return;
        };
        self.agg_loading.remove(&r.key);
        self.agg_in_flight.remove(&r.track.id);
        let disowned = self.agg_disowned.remove(&r.key);
        // A queued ask never started: its key leaves with it, and the rerun
        // below answers it for wherever the lane is now.
        let queued = self.agg_queued.remove(&r.track.id);
        if let Some(q) = &queued {
            self.agg_loading.remove(&q.key);
            self.agg_disowned.remove(&q.key);
        }
        self.forgive_heal(&r.key);
        self.notify();
        // Merge the lane as it is now for a queued ask, and for a disowned
        // merge a view still shows: it left the lane with no result and no
        // spinner, and nothing else asks again (the folded-out lanes compute
        // only on these pushes). The queued asks wait for that rerun.
        let rerun_then = match &queued {
            Some(q) => Cont::Fire(q.done),
            None => Cont::Done,
        };
        let mut rerun = Some(rerun_then);
        if !self.disposed && (queued.is_some() || (disowned && self.shows_lane(&r.track.id, host)))
        {
            if let Some(live) = host.track_by_id(&r.track.id) {
                if live.clips.len() > 1 {
                    if let Some(then) = rerun.take() {
                        self.ensure_aggregate_analysis(live, then, host);
                    }
                }
            }
        }
        if let Some(then) = rerun {
            // `Future.value().whenComplete(...)`: answered at once.
            self.complete(then, TakeAnalysis::default());
        }
        self.complete(r.then, TakeAnalysis::default());
    }

    // --- Eviction / refresh -------------------------------------------------

    /// Drops the cached spectrogram + voice metrics for an audio `path` whose
    /// file left the model (delete/split/merge) or, with `recompute`, whose
    /// CONTENT was replaced under the same path (re-analyze) — only then is a
    /// compute still in flight for it redone against the new bytes.
    pub fn evict_analysis(&mut self, path: &str, recompute: bool) {
        // Disown any compute still in flight for this path: it read the file
        // BEFORE this eviction, so its result (or its "unavailable" latch)
        // must not land on top of the replacement. See `path_generation`.
        self.bump_generation(path);
        if recompute {
            self.recompute_on_stale.insert(path.to_string());
        } else {
            self.recompute_on_stale.remove(path);
        }
        // remove fires the eviction hook, which drops the derived memos with
        // it.
        self.spectrogram_cache.remove(path);
        self.metrics_cache.remove(path);
        // Clear the "unavailable" latches so the replacement file can
        // re-analyze.
        self.spectrogram_unavailable.remove(path);
        self.metrics_unavailable.remove(path);
        self.eq_spectro_cache.remove(path);
        self.eq_spectro_unavailable.remove(path);
        self.evict_aggregates_folding(path);
        // the vocal-tract tracks in flight over it describe the old audio
        for p in self.pending.values() {
            if let Pending::VocalTract(t) = p {
                if t.key == path || (t.aggregate && Self::aggregate_folds(&t.key, path)) {
                    t.disowned.store(true, Ordering::SeqCst);
                }
            }
        }
        self.tract_failed
            .retain(|k| !(k.ends_with(path) || Self::aggregate_folds(k, path)));
    }

    /// Drops the whole-lane aggregates that folded `path` in. Aggregate keys
    /// embed the clip files so a changed clip set self-invalidates, but a
    /// file's *content* can change at the same path (re-analyze), so a file
    /// mutation drops the lanes playing it — and ONLY those: clearing every
    /// lane blanked other takes' folded-out lanes, which nothing asked for
    /// again.
    fn evict_aggregates_folding(&mut self, path: &str) {
        // Disown the merges in flight over it too (see `agg_disowned`).
        let disowned: Vec<String> = self
            .agg_loading
            .iter()
            .filter(|key| Self::aggregate_folds(key, path))
            .cloned()
            .collect();
        for key in disowned {
            for r in self.agg_runs.values() {
                if r.key == key {
                    r.disowned.store(true, Ordering::SeqCst);
                }
            }
            self.agg_disowned.insert(key);
        }
        // remove fires the eviction hook, which drops the derived memos with
        // it.
        for key in self.agg_analysis_cache.keys() {
            if Self::aggregate_folds(&key, path) {
                self.agg_analysis_cache.remove(&key);
            }
        }
        self.agg_unavailable
            .retain(|key| !Self::aggregate_folds(key, path));
        if self
            .heal_blocked
            .as_deref()
            .is_some_and(|key| Self::aggregate_folds(key, path))
        {
            self.heal_blocked = None;
        }
    }

    /// Whether the aggregate `key` (see [`AnalysisCache::aggregate_key_for`])
    /// folds the clip file `path` in: each clip ends `:<path>`,
    /// comma-separated.
    fn aggregate_folds(key: &str, path: &str) -> bool {
        key.ends_with(&format!(":{path}")) || key.contains(&format!(":{path},"))
    }

    /// After a track's audio file changes (re-analyze/split/merge), refresh
    /// any open Voiceprint dock and inline sub-lanes that point at it.
    #[must_use]
    pub fn on_track_audio_changed(
        &mut self,
        track_id: &str,
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        if self.dock_open && self.dock_shows_take() {
            self.ensure_selected(host);
        }
        if self.is_track_expanded(track_id) {
            self.ensure_lane_data(track_id, host);
        }
        self.finish(host)
    }

    /// Computes (and caches) the spectrogram + metrics for the current
    /// analysis subject: a single selected clip, or — for a header selection
    /// — the one clip of a single-clip take or the aggregate of a multi-clip
    /// take. The dock, inspector and inline lanes share the per-path caches,
    /// so a clip analyzed here is reused everywhere (no double work).
    #[must_use]
    pub fn ensure_selected_spectrogram(&mut self, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        self.ensure_selected(host);
        self.finish(host)
    }

    fn ensure_selected(&mut self, host: &dyn AnalysisHost) {
        let Some(track) = host.selected_track() else {
            return;
        };
        // Backing tracks (original/accompaniment) carry no voice analysis —
        // don't spend a file read + compute on metrics the inspector never
        // shows.
        if track.is_backing() || track.clips.is_empty() {
            return;
        }
        if let Some(clip_id) = host.selected_clip_id() {
            if let Some(clip) = track.clip_by_id(clip_id) {
                let path = clip.audio_file_path.clone();
                self.ensure_clip_analysis(&path, Cont::Done);
                return;
            }
        }
        if track.clips.len() == 1 {
            let path = track.clips[0].audio_file_path.clone();
            self.ensure_clip_analysis(&path, Cont::Done);
            return;
        }
        self.ensure_aggregate_analysis(track, Cont::Done, host);
    }

    // --- Dev/demo hooks -------------------------------------------------------

    /// Applies the compile-time demo toggles after the practice loads (see
    /// [`AppConfig`]): the Live voiceprint layout, a specific dock tab by
    /// name, and folding out every vocal take with all lanes for screenshots.
    #[must_use]
    pub fn apply_demo_hooks(
        &mut self,
        vocal_tracks: &[Arc<AudioTrack>],
        host: &dyn AnalysisHost,
    ) -> Vec<AnalysisEffect> {
        // Dev hook: show the Live voiceprint layout (without starting the
        // mic).
        if AppConfig::LIVE_DEMO {
            self.live_monitor = true;
        }
        // Dev hook for screenshots: open a specific dock tab by name.
        if !AppConfig::DOCK_TAB.is_empty() {
            for t in DockTab::ALL {
                if t.name() == AppConfig::DOCK_TAB {
                    self.dock_tab = t;
                }
            }
            // Keep the two dev hooks consistent: a non-live tab can't be
            // live.
            if !Self::tab_supports_live(self.dock_tab) {
                self.live_monitor = false;
            }
        }
        // Dev hook for screenshots: fold out every vocal take with all lanes.
        if AppConfig::EXPAND_PITCH {
            for t in vocal_tracks {
                if !self.is_track_expanded(&t.id) {
                    self.expanded_tracks.push(t.id.clone());
                }
                self.track_lanes
                    .insert(t.id.clone(), AnalysisLane::ALL.into_iter().collect());
                self.ensure_lane_data(&t.id, host);
            }
        }
        self.finish(host)
    }

    /// Tears the cache down: no further compute is admitted, the capture
    /// pump and the heal retry stop, the live capture closes, and the caches
    /// leave the shared budget.
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.disposed_flag.store(true, Ordering::SeqCst);
        self.capture_engine = None;
        self.lane_heal_retry = false;
        // Stop the engine's capture if the live view was on; the owner
        // disposes the engine right after, this just closes the pump's
        // source first.
        if self.live_monitor {
            if let Some(engine) = &self.graph_engine {
                engine.stop_capture();
            }
        }
        // The shared budget outlives this screen, so a cache that does not
        // LEAVE it keeps this practice's whole analysis payload resident for
        // the life of the process.
        self.spectrogram_cache.dispose();
        self.metrics_cache.dispose();
        self.agg_analysis_cache.dispose();
        self.eq_spectro_cache.dispose();
        // Deliberately no monitor stop: the view model owns the monitor and
        // disposes it after the controllers.
    }

    // --- Landing --------------------------------------------------------------

    /// A job's result: the rest of the Dart function that awaited it.
    #[must_use]
    pub fn job_done(&mut self, done: JobDone, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        match self.pending.remove(&done.id) {
            Some(Pending::Compute(c)) => self.compute_landed(c, done.outcome),
            Some(Pending::Merge { run }) => {
                let result = match done.outcome {
                    Outcome::Merge(result) => result,
                    _ => Err("not a merge".to_string()),
                };
                self.agg_merged(run, result, host);
            }
            Some(Pending::VocalTract(t)) => {
                let result = match done.outcome {
                    Outcome::VocalTract(result) => result,
                    _ => Err("not a vocal-tract track".to_string()),
                };
                self.tract_landed(done.id, t, result);
            }
            None => {}
        }
        self.finish(host)
    }

    fn compute_landed(&mut self, c: Compute, outcome: Outcome) {
        match c.kind {
            JobKind::Spectrogram => {
                let result = match outcome {
                    Outcome::Spectrogram(r) => r,
                    _ => Err("not a spectrogram".to_string()),
                };
                self.half_landed(Half::Spectrogram, c, result.map(|s| s.map(Landed::Spec)));
            }
            JobKind::Metrics => {
                let result = match outcome {
                    Outcome::Metrics(r) => r,
                    _ => Err("not metrics".to_string()),
                };
                self.half_landed(Half::Metrics, c, result.map(|m| m.map(Landed::Metrics)));
            }
            JobKind::Both => {
                let result = match outcome {
                    Outcome::Both(r) => r,
                    _ => Err("not a pair".to_string()),
                };
                self.both_landed(c, result);
            }
            JobKind::EqSpectrogram => {
                let result = match outcome {
                    Outcome::Spectrogram(r) => r,
                    _ => Err("not a spectrogram".to_string()),
                };
                self.eq_landed(c, result);
            }
            // never a clip compute's
            JobKind::MergeLane | JobKind::VocalTract => {}
        }
    }

    /// `_ensureSpectrogramForPath` / `_ensureMetricsForPath` after their
    /// await.
    fn half_landed(&mut self, half: Half, c: Compute, result: Result<Option<Landed>, String>) {
        let path = c.path;
        let mut stale = false;
        match result {
            // See `disposed`: no reader is left for this cache.
            _ if self.disposed && result.is_ok() => {}
            // See `path_generation`: evicted mid-compute, so this describes
            // bytes that are already gone.
            Ok(_) if c.generation != self.generation_of(&path) => stale = true,
            Ok(Some(Landed::Spec(spec))) => {
                self.spectrogram_cache.insert(path.clone(), spec);
            }
            Ok(Some(Landed::Metrics(m))) => {
                self.metrics_cache.insert(path.clone(), m);
                // An LTAS read while these were computing was built ungated
                // (see `selected_ltas`); drop it so the next read applies the
                // voiced gate.
                lock(&self.derived).ltas.remove(&path);
            }
            Ok(None) => match half {
                Half::Spectrogram => {
                    self.spectrogram_unavailable.insert(path.clone());
                }
                Half::Metrics => {
                    self.metrics_unavailable.insert(path.clone());
                }
            },
            Err(_) => {
                // Compute failed; latch so it isn't retried. The lane shows
                // unavailable.
                if c.generation == self.generation_of(&path) {
                    match half {
                        Half::Spectrogram => self.spectrogram_unavailable.insert(path.clone()),
                        Half::Metrics => self.metrics_unavailable.insert(path.clone()),
                    };
                }
            }
        }
        // finally: removed in the SAME place the loading flag is, so a failed
        // or disposed compute can never leave a signal nobody will fire.
        match half {
            Half::Spectrogram => {
                self.spectrogram_loading.remove(&path);
                self.spectrogram_in_flight.remove(&path);
            }
            Half::Metrics => {
                self.metrics_loading.remove(&path);
                self.metrics_in_flight.remove(&path);
            }
        }
        if let Some(signal) = c.signal {
            self.fire(signal);
        }
        self.notify();
        // Redo the discarded work against the current file, now that the
        // in-flight slot is free (a joiner of the dropped compute got
        // nothing) — when the eviction asked for it; see
        // `recompute_on_stale`.
        if stale && self.recompute_on_stale.contains(&path) {
            self.ensure_half_for_path(half, &path, Cont::Done);
        }
        self.complete(c.then, TakeAnalysis::default());
    }

    /// `_ensureClipAnalysis` after its await.
    fn both_landed(
        &mut self,
        c: Compute,
        result: Result<Option<vocara_services::spectrogram_service::TakeAnalysisBundle>, String>,
    ) {
        let path = c.path;
        let mut stale = false;
        // What the function returns (`None`: read the pair after the finally).
        let mut ret: Option<TakeAnalysis> = None;
        match result {
            _ if self.disposed && result.is_ok() => ret = Some(self.clip_analysis(&path)),
            // See `path_generation`: evicted mid-compute, so this pair
            // describes bytes the eviction already declared stale.
            Ok(_) if c.generation != self.generation_of(&path) => {
                stale = true;
                ret = Some(TakeAnalysis::default());
            }
            Ok(both) => {
                let both = both.unwrap_or_default();
                match both.spectrogram {
                    Some(spec) => self.spectrogram_cache.insert(path.clone(), spec),
                    None => {
                        self.spectrogram_unavailable.insert(path.clone());
                    }
                }
                match both.metrics {
                    Some(m) => {
                        self.metrics_cache.insert(path.clone(), m);
                        // See half_landed's identical drop.
                        lock(&self.derived).ltas.remove(&path);
                    }
                    None => {
                        self.metrics_unavailable.insert(path.clone());
                    }
                }
                // The pair's LTAS and voice profile were derived on the
                // analysis worker (see TakeAnalysisBundle), so the getters
                // find them memoized instead of building them on the UI
                // thread. Seeded only while every source is still resident —
                // an insert above can evict — so a derived memo never
                // outlives what it was derived from.
                let spec_held = self.spectrogram_cache.contains_key(&path);
                let metrics_held = self.metrics_cache.contains_key(&path);
                let mut d = lock(&self.derived);
                if let Some(ltas) = both.ltas {
                    if spec_held && metrics_held {
                        d.ltas.insert(path.clone(), ltas);
                    }
                }
                if let Some(profile) = both.profile {
                    if metrics_held {
                        d.profile.insert(path.clone(), profile);
                    }
                }
            }
            Err(_) => {
                // Compute failed; latch both so neither is retried (matching
                // the individual paths' latch discipline).
                if c.generation == self.generation_of(&path) {
                    self.spectrogram_unavailable.insert(path.clone());
                    self.metrics_unavailable.insert(path.clone());
                }
            }
        }
        // finally
        self.spectrogram_loading.remove(&path);
        self.metrics_loading.remove(&path);
        self.spectrogram_in_flight.remove(&path);
        self.metrics_in_flight.remove(&path);
        if let Some(signal) = c.signal {
            self.fire(signal);
        }
        self.notify();
        if stale && self.recompute_on_stale.contains(&path) {
            self.ensure_clip_analysis(&path, Cont::Done);
        }
        let ret = ret.unwrap_or_else(|| self.clip_analysis(&path));
        self.complete(c.then, ret);
    }

    /// `_ensureEqSpectrogramForPath` after its await.
    fn eq_landed(&mut self, c: Compute, result: Result<Option<Arc<Spectrogram>>, String>) {
        let path = c.path;
        match result {
            // See `path_generation`. No retry is scheduled here:
            // eq_input_column re-asks on the next frame it is painted, which
            // starts a fresh compute once the loading flag clears below.
            Ok(_) if self.disposed || c.generation != self.generation_of(&path) => {}
            Ok(Some(spec)) => self.eq_spectro_cache.insert(path.clone(), spec),
            Ok(None) => {
                self.eq_spectro_unavailable.insert(path.clone());
            }
            Err(_) => {
                if c.generation == self.generation_of(&path) {
                    self.eq_spectro_unavailable.insert(path.clone());
                }
            }
        }
        self.eq_spectro_loading.remove(&path);
        self.notify();
    }

    // --- The async machinery --------------------------------------------------

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn signal(&mut self) -> SignalId {
        self.id()
    }

    fn group(&mut self, remaining: usize, then: Cont) -> u64 {
        let id = self.id();
        self.groups.insert(id, Group { remaining, then });
        id
    }

    fn in_flight(&self, half: Half) -> &HashMap<String, SignalId> {
        match half {
            Half::Spectrogram => &self.spectrogram_in_flight,
            Half::Metrics => &self.metrics_in_flight,
        }
    }

    fn generation_of(&self, path: &str) -> u64 {
        self.path_generation
            .get(path)
            .map_or(0, |g| g.load(Ordering::SeqCst))
    }

    fn generation_cell(&mut self, path: &str) -> Arc<AtomicU64> {
        Arc::clone(
            self.path_generation
                .entry(path.to_string())
                .or_insert_with(|| Arc::new(AtomicU64::new(0))),
        )
    }

    fn bump_generation(&mut self, path: &str) {
        self.generation_cell(path).fetch_add(1, Ordering::SeqCst);
    }

    /// Starts a clip compute on a worker: Dart's call into the service, whose
    /// `wanted` asks — once the gate admits it — whether this cache is still
    /// open and the file unchanged, so a song left with several lanes open
    /// doesn't run their queued analyses for no one.
    fn start_compute(&mut self, kind: JobKind, path: &str, params: AnalysisParams, c: Compute) {
        let wanted = WantedProbe::generation(
            Arc::clone(&self.disposed_flag),
            self.generation_cell(path),
            c.generation,
        );
        let id = self.id();
        self.pending.insert(id, Pending::Compute(c));
        self.effects.push(AnalysisEffect::Run(AnalysisJob {
            id,
            work: Work::Compute {
                kind,
                service: Arc::clone(&self.spectrograms),
                path: path.to_string(),
                params,
                wanted,
            },
        }));
    }

    /// `_joinInFlight`: waits out `path`'s compute in `half`'s in-flight map,
    /// then whatever replaced it as it ended. A compute evicted mid-flight
    /// lands empty and, for a re-analyze, starts its rerun as it ends (see
    /// `recompute_on_stale`): a caller that joined it read the hole the
    /// dropped compute left, and a lane merge asked for in that window was
    /// left blank with no spinner — nothing asked again.
    fn join(&mut self, half: Half, path: &str, then: Cont) {
        match self.in_flight(half).get(path).copied() {
            Some(signal) => self.wait(
                signal,
                Waiter {
                    join: Some((half, path.to_string())),
                    then,
                },
            ),
            None => self.complete(then, TakeAnalysis::default()),
        }
    }

    fn wait(&mut self, signal: SignalId, waiter: Waiter) {
        self.waiters.entry(signal).or_default().push(waiter);
    }

    /// Completes a Dart completer: its waiters resume, in order, as
    /// microtasks.
    fn fire(&mut self, signal: SignalId) {
        for w in self.waiters.remove(&signal).unwrap_or_default() {
            self.microtasks.push_back(Microtask::Resume(w));
        }
    }

    /// An awaited future completed with `value`: its awaiter resumes as a
    /// microtask (an `await` always yields).
    fn complete(&mut self, then: Cont, value: TakeAnalysis) {
        if matches!(then, Cont::Done) {
            return;
        }
        self.microtasks.push_back(Microtask::Complete(then, value));
    }

    /// Runs the queued microtasks (Dart's microtask queue) until none is
    /// left, then hands back the effects the call produced.
    fn finish(&mut self, host: &dyn AnalysisHost) -> Vec<AnalysisEffect> {
        while let Some(task) = self.microtasks.pop_front() {
            match task {
                Microtask::Resume(Waiter {
                    join: Some((half, path)),
                    then,
                }) => {
                    // `_joinInFlight`'s loop: another compute for the path
                    // took the finished one's place (a rerun its `finally`
                    // started): wait that one out too.
                    self.join(half, &path, then);
                }
                Microtask::Resume(Waiter { join: None, then }) => {
                    self.run_cont(then, TakeAnalysis::default(), host);
                }
                Microtask::Complete(then, value) => self.run_cont(then, value, host),
            }
        }
        std::mem::take(&mut self.effects)
    }

    fn run_cont(&mut self, cont: Cont, value: TakeAnalysis, host: &dyn AnalysisHost) {
        match cont {
            Cont::Done => {}
            Cont::Group(id) => {
                let Some(group) = self.groups.get_mut(&id) else {
                    return;
                };
                group.remaining = group.remaining.saturating_sub(1);
                if group.remaining == 0 {
                    if let Some(group) = self.groups.remove(&id) {
                        self.complete(group.then, TakeAnalysis::default());
                    }
                }
            }
            Cont::ClipPair { path, then } => {
                let pair = self.clip_analysis(&path);
                self.complete(*then, pair);
            }
            Cont::AggPart { run } => self.agg_part(run, value, host),
            Cont::HealSelectedAgg { key } => {
                if !self.disposed && !self.agg_analysis_cache.contains_key(&key) {
                    self.heal_blocked = Some(key);
                }
            }
            Cont::HealLaneAgg { key } => {
                if !self.disposed
                    && !self.agg_analysis_cache.contains_key(&key)
                    && !self.agg_unavailable.contains(&key)
                {
                    self.lane_heal_blocked.insert(key);
                }
            }
            Cont::Fire(signal) => self.fire(signal),
        }
    }

    /// Whether a view still shows `track_id`'s whole-lane analysis: its
    /// folded-out sub-lanes, or the dock and inspector with the lane itself
    /// (not one of its clips) as the subject.
    fn shows_lane(&self, track_id: &str, host: &dyn AnalysisHost) -> bool {
        self.is_track_expanded(track_id)
            || (host.selected_track().is_some_and(|t| t.id == track_id)
                && self.analysis_subject(host).is_some_and(|s| s.aggregate))
    }
}

/// A clip compute's half.
enum Landed {
    Spec(Arc<Spectrogram>),
    Metrics(Arc<VoiceMetrics>),
}
