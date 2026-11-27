//! The live microphone monitor behind the live Voiceprint (port of
//! `lib/data/services/monitor_service.dart`).
//!
//! It is FED mono PCM (pulled from the engine's input capture by its owner),
//! runs the incremental [`LiveVoiceAnalyzer`], and keeps a rolling window of
//! dB columns, the per-column voice metrics for the overlays, the latest
//! input level, the running session averages, the live LTAS and register
//! tallies, and the tuner's stable pitch; and, while the "Vocal tract" view
//! asks for it ([`MonitorService::set_vocal_tract`]), the singer's estimated
//! articulation ([`MonitorState::vocal_tract`]).
//!
//! # How live frames are delivered
//!
//! Dart's `MonitorService` was a `ChangeNotifier` on the UI isolate: the
//! owner's 30 ms timer pulled the engine's interleaved stereo input, averaged
//! it to mono and called `feedSamples`, and the live widgets listened. Here:
//!
//! - the owner (the song screen's analysis cache, or the mic recording
//!   controller while a technique take records) pulls the engine input on its
//!   30 ms tick, downmixes it with [`downmix_interleaved_stereo`] and calls
//!   [`MonitorService::feed_samples`]. The analysis runs synchronously inside
//!   the call (a 30 ms chunk is about three hops, each sub-millisecond), as it
//!   ran inside Dart's timer callback;
//! - the live views read the rolling state through [`MonitorService::read`],
//!   a read-only lock on [`MonitorState`] whose getters are Dart's (the
//!   columns as shared [`LiveColumn`]s, compared by identity like Dart's
//!   `Float32List`s, the histories and the voicing flags as slices aligned
//!   with them);
//! - every Dart `notifyListeners` bumps [`MonitorState::revision`], so a view
//!   that caches its drawing redraws exactly when Dart's listeners fired.
//!
//! The monitor is `Send + Sync` and shared as an `Arc`: its owner and the
//! controllers it hands it to feed and stop the same one, from the UI thread
//! or a worker. [`LiveMonitor`] is the driving half as a trait, for a test
//! fake (Dart's tests faked `start`/`stop`).

use std::borrow::Cow;
use std::f64::consts::LN_10;
use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vocara_model::models::voice_metrics::{finalize_ltas, register_score, Ltas, RegisterOptions};
use vocara_model::util::dart_num::{clamp_f64, compare_f64, floor_to_i64, pow_f64};

use vocara_tract::anatomy::BodyMix;
use vocara_tract::codebook::Codebook;

use crate::audio_dsp::{AnalysisParams, LiveVoiceAnalyzer, TunerTracker};
use crate::vocal_tract::{VocalTractState, VocalTractTracker};

/// One live spectrogram column: dB per bin, from DC up to
/// [`MonitorState::max_hz`]. Shared, so a view keeps the columns it drew and
/// tells a new one from an old one by identity (`Arc::ptr_eq`), as Dart
/// compared the `Float32List`s.
pub type LiveColumn = Arc<[f32]>;

/// The rolling window's default length in columns (Dart's `maxColumns`
/// default): about 7.7 s at the analyzer's 94 frames a second.
pub const DEFAULT_MAX_COLUMNS: usize = 720;

/// The engine captures at its fixed internal rate (48 kHz).
const SAMPLE_RATE: i64 = 48000;

/// The live dynamics over the voiced frames in the rolling window (Dart's
/// `({double softDb, double loudDb, double rangeDb})`): the 5th and 95th
/// loudness percentiles (dBFS) and their span; NaN until enough voiced
/// frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveDynamics {
    pub soft_db: f64,
    pub loud_db: f64,
    pub range_db: f64,
}

/// The monitor's rolling state, read through [`MonitorService::read`]. The
/// getters are Dart's; mutation goes through [`MonitorService`] only.
pub struct MonitorState {
    max_columns: usize,
    analyzer: LiveVoiceAnalyzer,

    columns: Vec<LiveColumn>,
    // Rolling per-column voice metrics (parallel to columns) for the overlay.
    f0: Vec<f64>,
    f1: Vec<f64>,
    f2: Vec<f64>,
    f3: Vec<f64>,
    // Rolling per-column loudness (dBFS), parallel to columns, for live
    // dynamics.
    loud: Vec<f64>,
    // Per-column voicing flag (1 = voiced), parallel to columns. The live
    // spectrogram dims unvoiced columns so silence/noise reads dark.
    act: Vec<u8>,
    // Per-column: a pitched frame, parallel to columns. `act` holds a phrase
    // a few frames past its last pitched one (the overlay's release), which
    // the averages and the dynamics must not count: each phrase's tail of
    // fading breath read as voice, a 50 dB "range" over notes sung level.
    voiced: Vec<bool>,
    active: bool,
    level: f64,
    // Latest smoothed metrics (for the readout bar / vowel dot).
    cur_f0: f64,
    cur_f1: f64,
    cur_f2: f64,
    cur_f3: f64,
    ring: f64,
    tilt: f64,
    cpp: f64,
    cur_h1h2: f64,
    // Running session averages over voiced frames.
    ring_sum: f64,
    tilt_sum: f64,
    cpp_sum: f64,
    active_n: usize,
    // Running LTAS accumulator: per-bin power sum (10^(dB/10)) over voiced
    // columns, plus the voiced-column count, for the live average spectrum.
    ltas_pow: Option<Vec<f64>>,
    ltas_n: usize,
    // Running register tally over voiced frames with a valid H1−H2, using the
    // same chest/mix/head score thresholds as the take's analyze_register.
    reg_chest: usize,
    reg_mix: usize,
    reg_head: usize,
    // A stable, heavily-smoothed pitch lock just for the tuner readout (needs
    // a few consecutive clearly-voiced frames before it shows, then
    // EMA-glides).
    tuner: TunerTracker,
    tuner_f0: f64,
    /// The live vocal tract while the view asks for it (not Dart's: new).
    tract: Option<LiveTract>,
    /// Bumped on every Dart `notifyListeners`.
    revision: u64,
}

/// The live vocal tract: the body the view asked for, and its tracker once the body's codebook
/// is built (on a worker: building it on the feeding thread would stall the capture pump for
/// a few tenths of a second).
struct LiveTract {
    body: BodyMix,
    tracker: Option<VocalTractTracker>,
}

impl MonitorState {
    fn new(max_columns: usize) -> Self {
        MonitorState {
            max_columns,
            analyzer: LiveVoiceAnalyzer::new(SAMPLE_RATE, AnalysisParams::default()),
            columns: Vec::new(),
            f0: Vec::new(),
            f1: Vec::new(),
            f2: Vec::new(),
            f3: Vec::new(),
            loud: Vec::new(),
            act: Vec::new(),
            voiced: Vec::new(),
            active: false,
            level: 0.0,
            cur_f0: f64::NAN,
            cur_f1: f64::NAN,
            cur_f2: f64::NAN,
            cur_f3: f64::NAN,
            ring: f64::NAN,
            tilt: f64::NAN,
            cpp: f64::NAN,
            cur_h1h2: f64::NAN,
            ring_sum: 0.0,
            tilt_sum: 0.0,
            cpp_sum: 0.0,
            active_n: 0,
            ltas_pow: None,
            ltas_n: 0,
            reg_chest: 0,
            reg_mix: 0,
            reg_head: 0,
            tuner: TunerTracker::default(),
            tuner_f0: f64::NAN,
            tract: None,
            revision: 0,
        }
    }

    /// Dart's `notifyListeners`.
    fn notify(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// How many times the state changed in a way Dart told its listeners
    /// about (a start, a fed chunk, a stop). A view that caches its drawing
    /// redraws when this moves.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The rolling window's length in columns.
    pub fn max_columns(&self) -> usize {
        self.max_columns
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// The rolling window of dB columns, oldest first.
    pub fn columns(&self) -> &[LiveColumn] {
        &self.columns
    }

    /// Per-column voicing flags (1 = voiced), aligned with [`Self::columns`].
    pub fn active_columns(&self) -> &[u8] {
        &self.act
    }

    /// The newest column, if any.
    pub fn latest(&self) -> Option<&LiveColumn> {
        self.columns.last()
    }

    /// Bins per column (DC up).
    pub fn bins(&self) -> usize {
        self.analyzer.bins()
    }

    pub fn bin_hz(&self) -> f64 {
        self.analyzer.bin_hz()
    }

    /// The top of the columns' frequency range the analyzer was asked for
    /// (Hz).
    pub fn max_hz(&self) -> f64 {
        self.analyzer.max_hz()
    }

    /// Columns per second (one per hop).
    pub fn frame_rate(&self) -> f64 {
        self.analyzer.frame_rate()
    }

    /// Latest H1−H2 (dB) for the live register indicator; NaN when not
    /// voiced.
    pub fn current_h1h2(&self) -> f64 {
        self.cur_h1h2
    }

    /// The running long-term average spectrum over voiced frames this
    /// session, or `None` until enough frames have accumulated for a
    /// meaningful average. Like every live analysis this is gated on
    /// sustained-voiced frames (the same gate as the live F0/formant
    /// overlays), so it is a real-time vocal fingerprint — close to, but not
    /// bit-identical to, the take's energy-gated `compute_ltas`.
    pub fn live_ltas(&self) -> Option<Ltas> {
        let pow = self.ltas_pow.as_ref()?;
        if self.ltas_n < 8 {
            return None;
        }
        Some(finalize_ltas(
            pow,
            self.ltas_n,
            pow.len(),
            self.bin_hz(),
            self.max_hz(),
        ))
    }

    // Running register tallies (voiced frames) for the live chest/mix/head
    // split.
    pub fn register_chest(&self) -> usize {
        self.reg_chest
    }

    pub fn register_mix(&self) -> usize {
        self.reg_mix
    }

    pub fn register_head(&self) -> usize {
        self.reg_head
    }

    pub fn register_voiced(&self) -> usize {
        self.reg_chest + self.reg_mix + self.reg_head
    }

    /// Input level in [0, 1] (mapped from RMS dBFS), for the meter.
    pub fn level(&self) -> f64 {
        self.level
    }

    // Live voice metrics: rolling histories (aligned with columns) for the
    // formant overlay, plus the latest smoothed values and session averages.
    pub fn f0_history(&self) -> &[f64] {
        &self.f0
    }

    pub fn f1_history(&self) -> &[f64] {
        &self.f1
    }

    pub fn f2_history(&self) -> &[f64] {
        &self.f2
    }

    pub fn f3_history(&self) -> &[f64] {
        &self.f3
    }

    pub fn current_f0(&self) -> f64 {
        self.cur_f0
    }

    pub fn current_f1(&self) -> f64 {
        self.cur_f1
    }

    pub fn current_f2(&self) -> f64 {
        self.cur_f2
    }

    pub fn current_f3(&self) -> f64 {
        self.cur_f3
    }

    pub fn ring(&self) -> f64 {
        self.ring
    }

    pub fn tilt(&self) -> f64 {
        self.tilt
    }

    pub fn cpp(&self) -> f64 {
        self.cpp
    }

    pub fn ring_avg(&self) -> f64 {
        self.average(self.ring_sum)
    }

    pub fn tilt_avg(&self) -> f64 {
        self.average(self.tilt_sum)
    }

    pub fn cpp_avg(&self) -> f64 {
        self.average(self.cpp_sum)
    }

    fn average(&self, sum: f64) -> f64 {
        if self.active_n == 0 {
            f64::NAN
        } else {
            sum / self.active_n as f64
        }
    }

    /// Rolling per-column loudness (dBFS), aligned with [`Self::columns`].
    pub fn loudness_history(&self) -> &[f64] {
        &self.loud
    }

    /// Live dynamics over the voiced frames in the rolling window: the
    /// 5th/95th loudness percentiles and their span. NaN fields until enough
    /// voiced frames.
    pub fn live_dynamics(&self) -> LiveDynamics {
        let mut v: Vec<f64> = Vec::new();
        for (i, &loud) in self.loud.iter().enumerate() {
            if i < self.voiced.len() && self.voiced[i] && loud.is_finite() {
                v.push(loud);
            }
        }
        if v.len() < 8 {
            return LiveDynamics {
                soft_db: f64::NAN,
                loud_db: f64::NAN,
                range_db: f64::NAN,
            };
        }
        v.sort_by(|a, b| compare_f64(*a, *b));
        let last = v.len() as i64 - 1;
        let at = |p: f64| v[floor_to_i64(v.len() as f64 * p).clamp(0, last) as usize];
        let (soft, loud) = (at(0.05), at(0.95));
        LiveDynamics {
            soft_db: soft,
            loud_db: loud,
            range_db: loud - soft,
        }
    }

    /// A stable, smoothed pitch (Hz) for the tuner; NaN until a clear
    /// sustained note is locked.
    pub fn tuner_f0(&self) -> f64 {
        self.tuner_f0
    }

    /// The singer's estimated vocal tract after the latest frame, while
    /// [`MonitorService::set_vocal_tract`] asks for it; `None` when it does
    /// not, and until the body's codebook is built (see
    /// [`MonitorState::vocal_tract_preparing`]).
    pub fn vocal_tract(&self) -> Option<&VocalTractState> {
        self.tract.as_ref()?.tracker.as_ref().map(|t| t.state())
    }

    /// The body the live vocal tract is estimated for, while asked for.
    pub fn vocal_tract_body(&self) -> Option<BodyMix> {
        self.tract.as_ref().map(|t| t.body)
    }

    /// Whether the live vocal tract is asked for but its body's codebook is
    /// still being built.
    pub fn vocal_tract_preparing(&self) -> bool {
        self.tract.as_ref().is_some_and(|t| t.tracker.is_none())
    }

    fn start(&mut self) {
        if self.active {
            return;
        }
        self.columns.clear();
        self.f0.clear();
        self.f1.clear();
        self.f2.clear();
        self.f3.clear();
        self.act.clear();
        self.voiced.clear();
        self.loud.clear();
        self.ring_sum = 0.0;
        self.tilt_sum = 0.0;
        self.cpp_sum = 0.0;
        self.active_n = 0;
        self.ltas_pow = None;
        self.ltas_n = 0;
        self.reg_chest = 0;
        self.reg_mix = 0;
        self.reg_head = 0;
        self.cur_f0 = f64::NAN;
        self.cur_f1 = f64::NAN;
        self.cur_f2 = f64::NAN;
        self.cur_f3 = f64::NAN;
        self.ring = f64::NAN;
        self.tilt = f64::NAN;
        self.cpp = f64::NAN;
        self.cur_h1h2 = f64::NAN;
        self.tuner.reset();
        self.tuner_f0 = f64::NAN;
        if let Some(tracker) = self.tract.as_mut().and_then(|t| t.tracker.as_mut()) {
            tracker.reset();
        }
        self.analyzer.reset();
        self.active = true;
        self.notify();
    }

    fn feed_samples(&mut self, samples: &[f32]) {
        if !self.active {
            return;
        }
        let n = samples.len();
        if n == 0 {
            return;
        }
        let pcm = finite_only(samples);
        let mut sum_sq = 0.0;
        for &s in pcm.iter() {
            let s = f64::from(s);
            sum_sq += s * s;
        }
        let rms = (sum_sq / n as f64).sqrt();
        let dbfs = 20.0 * ((rms + 1e-9).ln() / LN_10);
        self.level = clamp_f64((dbfs + 50.0) / 50.0, 0.0, 1.0);

        // The frame callback updates every field but the analyzer (Dart's
        // closure over the fields); the closure borrows those fields alone.
        let frame_rate = self.analyzer.frame_rate();
        self.analyzer.add_samples(&pcm, |f| {
            if let Some(t) = &mut self.tract {
                if t.tracker.is_none() {
                    t.tracker = Codebook::cached(t.body).map(VocalTractTracker::with_codebook);
                }
                if let Some(tracker) = &mut t.tracker {
                    tracker.push_live(&f, frame_rate);
                }
            }
            let col: LiveColumn = Arc::from(f.column);
            let voiced = f.active && f.f0_confidence > 0.0;
            self.columns.push(Arc::clone(&col));
            self.f0.push(f.f0);
            self.f1.push(f.f1);
            self.f2.push(f.f2);
            self.f3.push(f.f3);
            self.act.push(u8::from(f.active));
            self.voiced.push(voiced);
            self.loud.push(f.loudness);
            if self.columns.len() > self.max_columns {
                self.columns.remove(0);
                self.f0.remove(0);
                self.f1.remove(0);
                self.f2.remove(0);
                self.f3.remove(0);
                self.act.remove(0);
                self.voiced.remove(0);
                self.loud.remove(0);
            }
            self.cur_f0 = f.f0;
            self.cur_f1 = f.f1;
            self.cur_f2 = f.f2;
            self.cur_f3 = f.f3;
            self.ring = f.ring;
            self.tilt = f.tilt;
            self.cpp = f.cpp;
            self.cur_h1h2 = f.h1h2;
            if voiced {
                self.ring_sum += f.ring;
                self.tilt_sum += f.tilt;
                self.cpp_sum += f.cpp;
                self.active_n += 1;
                // Accumulate the LTAS power sum over voiced columns.
                let pow = self.ltas_pow.get_or_insert_with(|| vec![0.0; col.len()]);
                if pow.len() == col.len() {
                    for (p, &db) in pow.iter_mut().zip(col.iter()) {
                        *p += pow_f64(10.0, f64::from(db) / 10.0);
                    }
                    self.ltas_n += 1;
                }
                // Tally the current register from the (smoothed,
                // sustained-voiced) H1−H2 using the same chest/mix/head
                // thresholds as the take.
                if f.h1h2.is_finite() {
                    let s = register_score(f.h1h2, RegisterOptions::default());
                    if s < 0.4 {
                        self.reg_chest += 1;
                    } else if s < 0.6 {
                        self.reg_mix += 1;
                    } else {
                        self.reg_head += 1;
                    }
                }
            }

            self.tuner_f0 = self.tuner.add(f.f0, f.f0_confidence);
        });
        self.notify();
    }

    fn stop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        self.level = 0.0;
        self.notify();
    }
}

/// [samples] with every non-finite value zeroed before any DSP touches
/// them, as the take paths do (SpectrogramService's reader,
/// MediaAnalysisService). One NaN inside a held note lands in a frame the
/// voiced gate still counts, and from there in the LTAS power sum and the
/// tilt / CPP sums — running totals only `start` resets, so one glitch from
/// the input driver would blank those readouts for the rest of the session.
/// A scrubbed COPY rather than an in-place fix (the buffer is the
/// caller's), paid for only by the rare chunk that needs one.
fn finite_only(samples: &[f32]) -> Cow<'_, [f32]> {
    let Some(i) = samples.iter().position(|s| !s.is_finite()) else {
        return Cow::Borrowed(samples);
    };
    let mut out = samples.to_vec();
    for s in &mut out[i..] {
        if !s.is_finite() {
            *s = 0.0;
        }
    }
    Cow::Owned(out)
}

/// The capture pump's downmix: the engine's interleaved stereo input
/// averaged to the mono [`MonitorService::feed_samples`] takes, as Dart's
/// pumps (the analysis cache's and the mic recording controller's) computed
/// it — `(l + r) * 0.5` in doubles, stored as f32. A stray trailing sample is
/// not a frame and is dropped.
pub fn downmix_interleaved_stereo(frames: &[f32]) -> Vec<f32> {
    frames
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[l, r]| ((f64::from(*l) + f64::from(*r)) * 0.5) as f32)
        .collect()
}

/// Real-time microphone monitor for the live Voiceprint (see the module
/// doc): fed mono PCM, it keeps the rolling live analysis the views read.
pub struct MonitorService {
    state: Mutex<MonitorState>,
}

/// A read-only lock on a [`MonitorService`]'s [`MonitorState`]. Hold it only
/// while reading: the feeder waits for it. The lock is not reentrant, so
/// read what the service's own methods report (`revision`, `active`) off
/// this instead while holding it.
pub struct MonitorRead<'a>(MutexGuard<'a, MonitorState>);

impl Deref for MonitorRead<'_> {
    type Target = MonitorState;

    fn deref(&self) -> &MonitorState {
        &self.0
    }
}

impl Default for MonitorService {
    fn default() -> Self {
        MonitorService::new(DEFAULT_MAX_COLUMNS)
    }
}

impl MonitorService {
    /// A monitor whose rolling window holds [max_columns] columns.
    pub fn new(max_columns: usize) -> Self {
        MonitorService {
            state: Mutex::new(MonitorState::new(max_columns)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, MonitorState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The rolling state, to read (see [`MonitorRead`]).
    pub fn read(&self) -> MonitorRead<'_> {
        MonitorRead(self.lock())
    }

    /// See [`MonitorState::revision`].
    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    pub fn active(&self) -> bool {
        self.lock().active
    }

    /// Begin a live-analysis session (the owner opens the engine capture and
    /// feeds frames via [`Self::feed_samples`]). Resets all rolling state.
    /// A no-op while a session runs.
    pub fn start(&self) {
        self.lock().start();
    }

    /// Feed a chunk of mono samples (range ~[-1, 1], at 48 kHz) captured from
    /// the engine input. Ignored outside a session, and when empty.
    pub fn feed_samples(&self, samples: &[f32]) {
        self.lock().feed_samples(samples);
    }

    /// Ends the session: the level drops to 0, the rolling state stays for
    /// the views until the next [`Self::start`].
    pub fn stop(&self) {
        self.lock().stop();
    }

    /// Asks for the live vocal tract for `body` (or stops estimating it,
    /// `None`): from then on each analysis frame also feeds the inversion
    /// (about half a millisecond a frame), and [`MonitorState::vocal_tract`]
    /// reads its latest state. The body's codebook is built on a worker the
    /// first time a body is asked for; frames meanwhile are not estimated. A
    /// new body starts afresh; the same body again changes nothing.
    pub fn set_vocal_tract(&self, body: Option<BodyMix>) {
        let mut s = self.lock();
        let Some(body) = body else {
            if s.tract.take().is_some() {
                s.notify();
            }
            return;
        };
        if s.tract.as_ref().is_some_and(|t| t.body == body) {
            return;
        }
        let cached = Codebook::cached(body);
        let ready = cached.is_some();
        s.tract = Some(LiveTract {
            body,
            tracker: cached.map(VocalTractTracker::with_codebook),
        });
        s.notify();
        drop(s);
        if !ready {
            let built = std::thread::Builder::new()
                .name("vocal-tract-codebook".into())
                .spawn(move || Codebook::prewarm(body));
            if built.is_err() {
                // no thread to build it on: build it here rather than never
                Codebook::prewarm(body);
            }
        }
    }
}

/// The driving half of the monitor, the seam a view model's owner of the
/// capture pump holds (Dart's tests faked `MonitorService` for its
/// `start`/`stop`).
pub trait LiveMonitor: Send + Sync {
    /// See [`MonitorService::start`].
    fn start(&self);
    /// See [`MonitorService::feed_samples`].
    fn feed_samples(&self, samples: &[f32]);
    /// See [`MonitorService::stop`].
    fn stop(&self);
    /// Whether a session runs.
    fn active(&self) -> bool;
}

impl LiveMonitor for MonitorService {
    fn start(&self) {
        MonitorService::start(self);
    }

    fn feed_samples(&self, samples: &[f32]) {
        MonitorService::feed_samples(self, samples);
    }

    fn stop(&self) {
        MonitorService::stop(self);
    }

    fn active(&self) -> bool {
        MonitorService::active(self)
    }
}

#[cfg(test)]
mod tests {
    //! Ports of monitor_dynamics_test.dart and of the service-level
    //! assertions of voiceprint_tabs_test.dart's "LIVE monitor path" group
    //! (the rest of that group renders the live views: the screens' port),
    //! plus the monitor's own contract: its notify points, its rolling
    //! window and its frames being the live analyzer's.

    use std::f64::consts::PI;

    use super::*;
    use crate::audio_dsp::tests::support::DartRandom;
    use crate::audio_dsp::LiveFrame;

    const SR: i64 = 48000;

    /// voiceprint_tabs_test's `_sine`: [n] samples of a 0.3-amplitude sine.
    fn sine(n: usize, f: f64, sr: i64) -> Vec<f32> {
        (0..n)
            .map(|i| (0.3 * (2.0 * PI * f * i as f64 / sr as f64).sin()) as f32)
            .collect()
    }

    /// voiceprint_tabs_test's `_live`: a started monitor fed [samples] in
    /// 512-sample chunks, the way the engine delivers them.
    fn live(samples: &[f32]) -> MonitorService {
        let monitor = MonitorService::default();
        monitor.start();
        for chunk in samples.chunks(512) {
            monitor.feed_samples(chunk);
        }
        monitor
    }

    // The live averages and dynamics are over voiced frames. The live
    // analyzer holds a phrase "active" a few frames past its last pitched one
    // (so its overlay does not flicker off), and those frames of fading
    // breath were counted as voice: short notes sung at one level read a
    // ~50 dB range.
    #[test]
    fn short_notes_at_one_level_read_a_narrow_dynamic_range_their_tails_of_breath_not_counted() {
        let sr = SR as f64;
        let mut rng = DartRandom::new(7);
        let mut pcm: Vec<f64> = Vec::new();
        for _phrase in 0..6 {
            let mut i = 0;
            while (i as f64) < sr * 0.4 {
                let t = i as f64 / sr;
                pcm.push(
                    0.3 * ((2.0 * PI * 220.0 * t).sin()
                        + 0.25 * (2.0 * PI * 440.0 * t).sin()
                        + 0.1 * (2.0 * PI * 660.0 * t).sin()),
                );
                i += 1;
            }
            let mut i = 0;
            while (i as f64) < sr * 0.5 {
                pcm.push((rng.next_double() * 2.0 - 1.0) * 0.001); // about -60 dBFS
                i += 1;
            }
        }
        let monitor = MonitorService::default();
        monitor.start();
        let all: Vec<f32> = pcm.iter().map(|&v| v as f32).collect();
        for chunk in all.chunks(1024) {
            monitor.feed_samples(chunk);
        }
        let d = monitor.read().live_dynamics();
        assert!(
            d.range_db < 3.0,
            "soft {} loud {} range {}",
            d.soft_db,
            d.loud_db,
            d.range_db
        );
    }

    #[test]
    fn a_nan_inside_a_sustained_note_leaves_the_session_sums_finite() {
        // A NaN landing before the analyzer has seen a held note is never
        // summed. Here one lands mid-note: the voiced gate rides through a
        // few unvoiced frames, so the frame carrying the NaN still counts as
        // voiced and its column, tilt and CPP go into running sums that only
        // start() resets. Unscrubbed, the live LTAS (and the TILT / CPP
        // averages) would read NaN for the rest of the session — and the LTAS
        // tab would fail formatting it.
        let mut data = sine(48000, 220.0, 48000);
        data[30000] = f32::NAN;
        let monitor = live(&data);
        let m = monitor.read();
        let ltas = m.live_ltas().expect("a held note accumulates an LTAS");
        assert!(ltas.alpha_ratio.is_finite());
        assert!(ltas.singers_formant_db.is_finite());
        assert!(ltas.db.iter().all(|d| d.is_finite()));
        assert!(m.tilt_avg().is_finite());
        assert!(m.cpp_avg().is_finite());
        // Scrubbed in a copy: the chunk is the caller's buffer.
        assert!(data[30000].is_nan());
    }

    /// What the live views read off [m]: present values finite, absent ones
    /// absent (the widget tests rendered these without throwing).
    fn assert_readable(m: &MonitorState) {
        assert!(m.level().is_finite() && (0.0..=1.0).contains(&m.level()));
        let n = m.columns().len();
        for h in [
            m.f0_history(),
            m.f1_history(),
            m.f2_history(),
            m.f3_history(),
            m.loudness_history(),
        ] {
            assert_eq!(h.len(), n, "every history is aligned with the columns");
        }
        assert_eq!(m.active_columns().len(), n);
        for col in m.columns() {
            assert_eq!(col.len(), m.bins());
            assert!(
                col.iter().all(|v| v.is_finite()),
                "a column the painter can colour"
            );
        }
        assert!(m.loudness_history().iter().all(|v| v.is_finite()));
        if let Some(ltas) = m.live_ltas() {
            assert!(ltas.db.iter().all(|d| d.is_finite()));
            assert!(ltas.alpha_ratio.is_finite());
        }
        let d = m.live_dynamics();
        assert!(
            (d.soft_db.is_nan() && d.loud_db.is_nan() && d.range_db.is_nan())
                || (d.soft_db.is_finite() && d.loud_db.is_finite() && d.range_db >= 0.0)
        );
        assert_eq!(
            m.register_voiced(),
            m.register_chest() + m.register_mix() + m.register_head()
        );
    }

    #[test]
    fn a_fed_monitor_reads_back_a_live_analysis() {
        let monitor = live(&sine(24000, 220.0, 48000));
        let m = monitor.read();
        assert_readable(&m);
        assert!(m.active());
        assert!(!m.columns().is_empty());
        assert!(m.level() > 0.0);
        let newest = m.columns().last().unwrap();
        assert!(m.latest().is_some_and(|l| Arc::ptr_eq(l, newest)));
    }

    #[test]
    fn a_monitor_fed_silence_reads_back_without_a_voice() {
        let monitor = live(&vec![0.0; 24000]);
        let m = monitor.read();
        assert_readable(&m);
        assert!(m.live_ltas().is_none());
        assert!(m.live_dynamics().range_db.is_nan());
        assert!(m.tuner_f0().is_nan());
        assert!(m.ring_avg().is_nan());
        assert_eq!(m.register_voiced(), 0);
        assert!(m.active_columns().iter().all(|&a| a == 0));
    }

    #[test]
    fn a_monitor_fed_non_finite_samples_never_shows_them() {
        // MonitorService takes raw mic samples straight from the input
        // driver; feed it what a misbehaving driver could deliver.
        let mut data = sine(24000, 220.0, 48000);
        data[100] = f32::NAN;
        data[200] = f32::INFINITY;
        data[300] = f32::NEG_INFINITY;
        let monitor = live(&data);
        assert_readable(&monitor.read());
    }

    #[test]
    fn a_monitor_started_but_never_fed_reads_back_empty() {
        let monitor = MonitorService::default();
        monitor.start();
        let m = monitor.read();
        assert_readable(&m);
        assert!(m.active());
        assert!(m.columns().is_empty());
        assert!(m.latest().is_none());
        assert!(m.live_ltas().is_none());
        assert_eq!(m.level(), 0.0);
        for v in [
            m.current_f0(),
            m.current_f1(),
            m.current_f2(),
            m.current_f3(),
            m.ring(),
            m.tilt(),
            m.cpp(),
            m.current_h1h2(),
            m.tuner_f0(),
            m.ring_avg(),
            m.tilt_avg(),
            m.cpp_avg(),
        ] {
            assert!(v.is_nan());
        }
    }

    #[test]
    fn listeners_hear_a_start_each_fed_chunk_and_a_stop_and_nothing_else() {
        let monitor = MonitorService::default();
        let r0 = monitor.revision();
        // Outside a session a chunk is dropped unheard, as is a stop.
        monitor.feed_samples(&[0.1; 512]);
        monitor.stop();
        assert_eq!(monitor.revision(), r0);
        assert!(monitor.read().columns().is_empty());

        monitor.start();
        assert_eq!(monitor.revision(), r0 + 1);
        monitor.start(); // already running: a no-op
        assert_eq!(monitor.revision(), r0 + 1);
        monitor.feed_samples(&[]); // empty: a no-op
        assert_eq!(monitor.revision(), r0 + 1);
        // A chunk too short for a frame still updates the level, and says so.
        monitor.feed_samples(&[0.1; 100]);
        assert_eq!(monitor.revision(), r0 + 2);
        assert!(monitor.read().columns().is_empty());
        assert!(monitor.read().level() > 0.0);

        monitor.stop();
        assert_eq!(monitor.revision(), r0 + 3);
        assert!(!monitor.active());
        assert_eq!(monitor.read().level(), 0.0);
        monitor.stop();
        assert_eq!(monitor.revision(), r0 + 3);
    }

    #[test]
    fn the_level_maps_rms_dbfs_onto_the_meter() {
        let monitor = MonitorService::default();
        monitor.start();
        // RMS 0.1 is -20 dBFS: (−20 + 50) / 50.
        monitor.feed_samples(&[0.1; 1000]);
        let level = monitor.read().level();
        assert!((level - 0.6).abs() < 1e-6, "{level}");
        // Full scale and above pins at 1; near silence at 0.
        monitor.feed_samples(&[1.5; 1000]);
        assert_eq!(monitor.read().level(), 1.0);
        monitor.feed_samples(&[1e-5; 1000]);
        assert_eq!(monitor.read().level(), 0.0);
    }

    #[test]
    fn a_stop_keeps_the_picture_and_a_new_start_clears_it() {
        let monitor = live(&sine(48000, 220.0, 48000));
        let columns = monitor.read().columns().len();
        assert!(columns > 0);
        assert!(monitor.read().register_voiced() > 0);
        monitor.stop();
        assert_eq!(monitor.read().columns().len(), columns);
        assert!(monitor.read().f0_history().iter().any(|f| f.is_finite()));

        monitor.start();
        let m = monitor.read();
        assert!(m.columns().is_empty());
        assert!(m.f0_history().is_empty());
        assert!(m.loudness_history().is_empty());
        assert!(m.live_ltas().is_none());
        assert_eq!(m.register_voiced(), 0);
        assert!(m.tuner_f0().is_nan());
        assert!(m.ring_avg().is_nan());
        assert!(m.current_f0().is_nan());
    }

    #[test]
    fn the_rolling_window_keeps_the_newest_frames_of_the_live_analyzer() {
        // Every column and metric is the analyzer's frame, the oldest
        // evicted first, all histories in step.
        let data = sine(48000, 220.0, 48000);
        let monitor = MonitorService::new(10);
        monitor.start();
        for chunk in data.chunks(700) {
            monitor.feed_samples(chunk);
        }
        let mut analyzer = LiveVoiceAnalyzer::new(SAMPLE_RATE, AnalysisParams::default());
        let mut frames: Vec<LiveFrame> = Vec::new();
        for chunk in data.chunks(700) {
            analyzer.add_samples(chunk, |f| frames.push(f));
        }
        assert!(frames.len() > 10);
        let tail = &frames[frames.len() - 10..];
        let m = monitor.read();
        assert_eq!(m.columns().len(), 10);
        for (i, f) in tail.iter().enumerate() {
            assert_eq!(&*m.columns()[i], f.column.as_slice());
            assert_eq!(m.f0_history()[i].to_bits(), f.f0.to_bits());
            assert_eq!(m.f1_history()[i].to_bits(), f.f1.to_bits());
            assert_eq!(m.f2_history()[i].to_bits(), f.f2.to_bits());
            assert_eq!(m.f3_history()[i].to_bits(), f.f3.to_bits());
            assert_eq!(m.loudness_history()[i].to_bits(), f.loudness.to_bits());
            assert_eq!(m.active_columns()[i], u8::from(f.active));
        }
        let last = frames.last().unwrap();
        assert_eq!(m.current_f0().to_bits(), last.f0.to_bits());
        assert_eq!(m.current_h1h2().to_bits(), last.h1h2.to_bits());
        assert_eq!(m.ring().to_bits(), last.ring.to_bits());
        // The averages run over the whole session, not the window.
        let voiced: Vec<&LiveFrame> = frames
            .iter()
            .filter(|f| f.active && f.f0_confidence > 0.0)
            .collect();
        assert!(voiced.len() > 10);
        let ring_sum = voiced.iter().fold(0.0, |s, f| s + f.ring);
        assert_eq!(m.ring_avg(), ring_sum / voiced.len() as f64);
        // A held 220 Hz note locks the tuner.
        assert!((m.tuner_f0() - 220.0).abs() < 3.0, "{}", m.tuner_f0());
        assert_eq!(m.bins(), 215);
        assert_eq!(m.max_hz(), 5000.0);
        assert_eq!(m.bin_hz(), 48000.0 / 2048.0);
        assert_eq!(m.frame_rate(), 48000.0 / 512.0);
    }

    #[test]
    fn the_pump_downmix_averages_each_stereo_frame() {
        assert_eq!(
            downmix_interleaved_stereo(&[0.5, -0.25, 1.0, 1.0, 0.1]),
            vec![0.125, 1.0]
        );
        assert!(downmix_interleaved_stereo(&[]).is_empty());
        // In doubles, then narrowed: two maxima do not overflow.
        assert_eq!(
            downmix_interleaved_stereo(&[f32::MAX, f32::MAX]),
            vec![f32::MAX]
        );
    }

    #[test]
    fn the_monitor_is_shared_across_threads_and_driven_through_its_seam() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<MonitorService>();
        let monitor: Arc<MonitorService> = Arc::new(MonitorService::default());
        let driver: Arc<dyn LiveMonitor> = monitor.clone();
        let feeder = std::thread::spawn(move || {
            driver.start();
            driver.feed_samples(&sine(4800, 220.0, 48000));
            driver.active()
        });
        assert!(feeder.join().unwrap());
        assert!(!monitor.read().columns().is_empty());
    }

    /// Not Dart's: the live vocal tract runs only while asked for, on the
    /// monitor's own frames, and starts afresh with a session or a body.
    #[test]
    fn the_live_vocal_tract_runs_while_asked_for() {
        use crate::audio_dsp::tests::support::vowel_at;
        let monitor = MonitorService::default();
        let body = BodyMix::new(0.5, 0.0);
        // as the worker the monitor starts would, before the first frame
        Codebook::prewarm(body);
        let x = vowel_at(700.0, 1200.0, 2600.0, 200.0);
        monitor.start();
        monitor.feed_samples(&x);
        assert!(monitor.read().vocal_tract().is_none());
        let before = monitor.revision();
        monitor.set_vocal_tract(Some(body));
        assert!(monitor.revision() > before);
        assert_eq!(monitor.read().vocal_tract_body(), Some(body));
        monitor.feed_samples(&x);
        {
            let s = monitor.read();
            assert!(!s.vocal_tract_preparing());
            let state = s.vocal_tract().expect("estimated");
            assert!(state.estimate.fitted && state.folds.voiced());
            assert!(state.estimate.residual < 0.1);
        }
        // the same body again keeps the estimate; a new session starts it afresh
        let revision = monitor.revision();
        monitor.set_vocal_tract(Some(body));
        assert_eq!(monitor.revision(), revision);
        monitor.stop();
        monitor.start();
        assert!(!monitor.read().vocal_tract().expect("kept").estimate.fitted);
        monitor.set_vocal_tract(None);
        assert!(monitor.read().vocal_tract().is_none());
        assert_eq!(monitor.read().vocal_tract_body(), None);
    }
}
