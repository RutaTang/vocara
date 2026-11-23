//! Live analysis: the mic-rate framer, the live spectrogram STFT, the live
//! voice analyzer and the tuner's smoother.

use std::collections::VecDeque;
use std::f64::consts::{LN_10, LN_2};

use vocara_model::util::dart_num::{
    ceil_to_i64, clamp_i64, compare_f64, floor_to_i64, round_to_i64,
};

use super::fft::{hamming, hanning, RealFft};
use super::filter::{decimate, decimate_pair};
use super::lpc::{assign_formants, lpc_formants, ordered_formants};
use super::metrics::{cepstral_peak_prominence, h1_minus_h2};
use super::rolling::RollingF32;
use super::spectrogram::ColumnStft;
use super::yin::yin_pitch;
use super::{clamp_int, db20, spectrogram_bins, AnalysisParams, EMIT_BLOCK, VOICED_CONFIDENCE};

/// The live analyzers' framing: samples arrive in chunks of any size, and
/// each [fft_size]-sample frame starting at a multiple of [hop] is handed on
/// the moment its last sample is in, through one buffer refilled frame after
/// frame. Fed a block at a time like the take analyzer.
struct LiveFramer {
    fft_size: usize,
    hop: usize,
    /// The current frame's samples, valid during the callback only.
    frame: Vec<f32>,
    raw: RollingF32,
    /// Absolute index of the next frame's first sample.
    next: usize,
}

impl LiveFramer {
    fn new(fft_size: usize, hop: usize) -> Self {
        LiveFramer {
            fft_size,
            hop,
            frame: vec![0.0; fft_size],
            raw: RollingF32::default(),
            next: 0,
        }
    }

    /// Appends [samples], calling [on_frame] with each frame they complete.
    fn add(&mut self, samples: &[f32], mut on_frame: impl FnMut(&[f32])) {
        for block in samples.chunks(EMIT_BLOCK) {
            self.raw.add_all(block);
            while self.raw.len() >= self.next + self.fft_size {
                self.raw
                    .slice_into(&mut self.frame, self.next as i64, self.raw.len() as i64);
                on_frame(&self.frame);
                self.next += self.hop;
                self.raw.drop_below(self.next as i64);
            }
        }
    }

    fn reset(&mut self) {
        self.raw.clear();
        self.next = 0;
    }
}

/// Incremental STFT for live audio: feed samples as they arrive, get one dB
/// column (0..max_hz) per hop samples. Cheap enough for the UI thread (each
/// FFT is sub-millisecond). A column is the spectrogram builders' column:
/// the Hann window, transform and dB conversion fftea had, bit for bit.
pub struct StreamingStft {
    sample_rate: i64,
    fft_size: usize,
    hop: usize,
    max_hz: f64,
    max_bin: usize,
    stft: ColumnStft,
    frames: LiveFramer,
}

impl Default for StreamingStft {
    /// Dart's defaults: 44.1 kHz, 2048/512, up to 5 kHz.
    fn default() -> Self {
        StreamingStft::new(44100, AnalysisParams::default())
    }
}

impl StreamingStft {
    pub fn new(sample_rate: i64, params: AnalysisParams) -> Self {
        let AnalysisParams {
            fft_size,
            hop,
            max_hz,
        } = params;
        let max_bin = spectrogram_bins(max_hz, fft_size, sample_rate);
        StreamingStft {
            sample_rate,
            fft_size,
            hop,
            max_hz,
            max_bin,
            stft: ColumnStft::new(fft_size, max_bin),
            frames: LiveFramer::new(fft_size, hop),
        }
    }

    pub fn sample_rate(&self) -> i64 {
        self.sample_rate
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    pub fn max_hz(&self) -> f64 {
        self.max_hz
    }

    /// Bins per column (DC up).
    pub fn max_bin(&self) -> usize {
        self.max_bin
    }

    pub fn bin_hz(&self) -> f64 {
        self.sample_rate as f64 / self.fft_size as f64
    }

    /// Appends [samples] and emits a dB column via [on_column] for every
    /// hop. Each column is a new vector, the caller's to keep.
    pub fn add_samples(&mut self, samples: &[f32], mut on_column: impl FnMut(Vec<f32>)) {
        let (stft, max_bin) = (&mut self.stft, self.max_bin);
        self.frames.add(samples, |frame| {
            let mut col = vec![0.0f32; max_bin];
            stft.column_into(frame, &mut col);
            on_column(col);
        });
    }

    pub fn reset(&mut self) {
        self.frames.reset();
    }
}

/// One live-analysis frame: the dB spectrogram column plus the per-frame
/// voice metrics (already trailing-median smoothed). Formant/F0 are NaN when
/// unvoiced, and [active] reflects the periodicity-based voicing decision
/// (not loudness).
#[derive(Clone, Debug, PartialEq)]
pub struct LiveFrame {
    pub column: Vec<f32>,
    pub ring: f64,
    pub tilt: f64,
    pub cpp: f64,
    pub f0: f64,
    pub f1: f64,
    pub f2: f64,
    pub f3: f64,
    /// Whether the voice is sustained (voiced for the overlay attack).
    pub active: bool,
    /// YIN periodicity for this frame (0 when unvoiced) — drives the tuner
    /// lock.
    pub f0_confidence: f64,
    /// H1−H2 (dB) for this frame, or NaN when unvoiced — drives the live
    /// register.
    pub h1h2: f64,
    /// Frame RMS level (dBFS) — drives the live loudness / dynamics readout.
    pub loudness: f64,
}

/// Real-time version of `compute_voice_metrics`: feed mic samples, get one
/// [`LiveFrame`] per hop (dB column + ring/tilt/CPP/F0/formants). Same DSP
/// as the offline path, computed incrementally (sub-ms/frame), with
/// trailing-median smoothing (no future frames available live).
pub struct LiveVoiceAnalyzer {
    frames: LiveFramer,
    core: LiveCore,
}

/// Everything of [`LiveVoiceAnalyzer`] but its framer (so a frame callback
/// can borrow it while the framer runs).
struct LiveCore {
    fft_size: usize,
    hop: usize,
    sample_rate: i64,
    max_hz: f64,
    max_bin: usize,
    window: Vec<f64>,
    fft: RealFft,
    half: usize,
    bin_hz: f64,
    ds_factor: usize,
    ds_rate: f64,
    yin_factor: usize,
    yin_rate: f64,
    lpc_order: usize,
    lpc_win: usize,
    lpc_window: Vec<f64>,
    b2k: usize,
    b4k: usize,
    b50: usize,
    b1k: usize,
    b5k: usize,
    q_min: usize,
    q_max: usize,

    // Per-frame scratch, reused frame after frame: the Hann-windowed frame,
    // the dB spectrum mirrored for the cepstrum, the magnitude spectrum (bins
    // 0..=half) and the cepstrum's real parts over q_min..=q_max.
    frame: Vec<f64>,
    logmag: Vec<f64>,
    mags: Vec<f64>,
    cep: Vec<f64>,

    f0_h: VecDeque<f64>,
    f1_h: VecDeque<f64>,
    f2_h: VecDeque<f64>,
    f3_h: VecDeque<f64>,
    h1h2_h: VecDeque<f64>,
    voiced_run: u32,
    overlay_silent: u32,
}

/// Trailing-median length of the live tracks.
const SMOOTH_N: usize = 5;
/// Absolute RMS floor (dBFS) below which a frame is treated as
/// silence/noise — room noise is typically < −50 dBFS, sung/spoken voice
/// > −35.
const VAD_FLOOR_DB: f64 = -47.0;
/// Overlay sustain gate: F0/formant dots (and the spectrogram brightness)
/// only appear once the voice has been continuously voiced for
/// [OVERLAY_ATTACK] frames, and survive up to [OVERLAY_RELEASE] unvoiced
/// frames before resetting — so brief/scattered voiced blips (speech,
/// breath, taps) never draw dots, only sustained singing does (mirrors how
/// the tuner waits for a held note).
const OVERLAY_ATTACK: u32 = 4;
const OVERLAY_RELEASE: u32 = 4;

impl Default for LiveVoiceAnalyzer {
    /// Dart's defaults: 44.1 kHz, 2048/512, up to 5 kHz.
    fn default() -> Self {
        LiveVoiceAnalyzer::new(44100, AnalysisParams::default())
    }
}

impl LiveVoiceAnalyzer {
    pub fn new(sample_rate: i64, params: AnalysisParams) -> Self {
        let AnalysisParams {
            fft_size,
            hop,
            max_hz,
        } = params;
        let sr = sample_rate as f64;
        let half = fft_size / 2;
        let bin_hz = sr / fft_size as f64;
        let ds_factor = clamp_i64(round_to_i64(sr / 11000.0), 1, 8) as usize;
        let ds_rate = sr / ds_factor as f64;
        let lpc_order = clamp_i64(round_to_i64(2.0 + ds_rate / 1000.0), 8, 20) as usize;
        let lpc_win = fft_size / ds_factor;
        // F0 runs on a separate, only-lightly-decimated (~22 kHz) copy so the
        // lag resolution stays octave-robust at high pitch (see
        // compute_voice_metrics).
        let yin_factor = clamp_i64(round_to_i64(sr / 22050.0), 1, 2) as usize;
        let yin_rate = sr / yin_factor as f64;
        let bin_of =
            |hz: f64| -> usize { round_to_i64(hz / bin_hz).clamp(0, half as i64) as usize };
        let q_min = clamp_int(floor_to_i64(sr / 500.0), 2, half as i64);
        // Clamp to half: only quefrencies in [0, half] are independent in the
        // symmetric cepstrum (mirrored above Nyquist). Defensive — the live
        // analyzer runs at 48 kHz where q_max < half anyway, but keeps the two
        // analysis paths consistent.
        let q_max = clamp_int(ceil_to_i64(sr / 60.0), q_min + 1, half as i64);
        let (q_min, q_max) = (q_min as usize, q_max as usize);
        LiveVoiceAnalyzer {
            frames: LiveFramer::new(fft_size, hop),
            core: LiveCore {
                fft_size,
                hop,
                sample_rate,
                max_hz,
                max_bin: spectrogram_bins(max_hz, fft_size, sample_rate),
                window: hanning(fft_size),
                fft: RealFft::new(fft_size),
                half,
                bin_hz,
                ds_factor,
                ds_rate,
                yin_factor,
                yin_rate,
                lpc_order,
                lpc_win,
                lpc_window: hamming(lpc_win),
                b2k: bin_of(2000.0),
                b4k: bin_of(4000.0),
                b50: bin_of(50.0),
                b1k: bin_of(1000.0),
                b5k: bin_of(5000.0),
                q_min,
                q_max,
                frame: vec![0.0; fft_size],
                logmag: vec![0.0; fft_size],
                mags: vec![0.0; half + 1],
                cep: vec![0.0; q_max - q_min + 1],
                f0_h: VecDeque::with_capacity(SMOOTH_N + 1),
                f1_h: VecDeque::with_capacity(SMOOTH_N + 1),
                f2_h: VecDeque::with_capacity(SMOOTH_N + 1),
                f3_h: VecDeque::with_capacity(SMOOTH_N + 1),
                h1h2_h: VecDeque::with_capacity(SMOOTH_N + 1),
                voiced_run: 0,
                overlay_silent: 0,
            },
        }
    }

    pub fn sample_rate(&self) -> i64 {
        self.core.sample_rate
    }

    pub fn fft_size(&self) -> usize {
        self.core.fft_size
    }

    pub fn hop(&self) -> usize {
        self.core.hop
    }

    pub fn max_hz(&self) -> f64 {
        self.core.max_hz
    }

    pub fn bin_hz(&self) -> f64 {
        self.core.bin_hz
    }

    /// Bins per column (DC up).
    pub fn bins(&self) -> usize {
        self.core.max_bin
    }

    /// Frames per second (one per hop).
    pub fn frame_rate(&self) -> f64 {
        self.core.sample_rate as f64 / self.core.hop as f64
    }

    /// Appends [samples] and emits a [`LiveFrame`] via [on_frame] for every
    /// hop.
    pub fn add_samples(&mut self, samples: &[f32], mut on_frame: impl FnMut(LiveFrame)) {
        let core = &mut self.core;
        self.frames.add(samples, |raw| on_frame(core.analyze(raw)));
    }

    /// Starts a fresh session: no framing backlog, no smoothing history, no
    /// sustain state.
    pub fn reset(&mut self) {
        self.frames.reset();
        let c = &mut self.core;
        c.f0_h.clear();
        c.f1_h.clear();
        c.f2_h.clear();
        c.f3_h.clear();
        c.h1h2_h.clear();
        c.voiced_run = 0;
        c.overlay_silent = 0;
    }
}

impl LiveCore {
    fn analyze(&mut self, raw: &[f32]) -> LiveFrame {
        let fft_size = self.fft_size;
        let half = self.half;
        let mut raw_sq = 0.0;
        for ((f, &s), &w) in self
            .frame
            .iter_mut()
            .zip(&raw[..fft_size])
            .zip(&self.window)
        {
            let s = f64::from(s);
            raw_sq += s * s;
            *f = s * w;
        }
        self.fft.transform(&self.frame);
        self.fft.magnitudes_into(&mut self.mags, half + 1);
        let mags = &self.mags;

        // The dB spectrum: the column's bins, and the cepstrum's input below.
        for (l, &m) in self.logmag.iter_mut().zip(&mags[..=half]) {
            *l = db20(m);
        }
        let col: Vec<f32> = self.logmag[..self.max_bin]
            .iter()
            .map(|&v| v as f32)
            .collect();
        // Voice-activity floor: reject anything below an absolute RMS level
        // so the analyzer never reacts to room noise; the YIN periodicity
        // gate (below) then separates a real voice from non-periodic noise
        // above the floor.
        let rms_db = db20((raw_sq / fft_size as f64).sqrt());
        let audible = rms_db > VAD_FLOOR_DB;

        // Ring + tilt. Skip the DC bin in the low-band peak search.
        let (mut low_pk, mut high_pk) = (1e-12f64, 1e-12f64);
        for &m in &mags[1.min(self.b2k)..self.b2k] {
            if m > low_pk {
                low_pk = m;
            }
        }
        for &m in &mags[self.b2k..self.b4k.max(self.b2k)] {
            if m > high_pk {
                high_pk = m;
            }
        }
        let ring = db20(high_pk) - db20(low_pk);
        let (mut low_e, mut high_e) = (0.0f64, 0.0f64);
        for &m in &mags[self.b50..self.b1k.max(self.b50)] {
            low_e += m * m;
        }
        for &m in &mags[self.b1k..self.b5k.max(self.b1k)] {
            high_e += m * m;
        }
        let tilt = 10.0 * (((high_e + 1e-12) / (low_e + 1e-12)).ln() / LN_10);

        // Cepstrum → CPP. Baseline regression excludes a small neighborhood
        // of the peak (see compute_voice_metrics) so the prominence isn't
        // biased down.
        for k in half + 1..fft_size {
            self.logmag[k] = self.logmag[fft_size - k];
        }
        self.fft.transform(&self.logmag);
        self.fft
            .real_parts_into(&mut self.cep, self.q_min, self.q_max + 1);
        let cpp = cepstral_peak_prominence(&self.cep, self.q_min, self.q_max, fft_size);

        // F0 + voicing via YIN on a decimated copy of this window — but only
        // when the frame is audible, so a silent room skips the decimate +
        // YIN cost every hop (voiced requires audible anyway, so the result
        // is unchanged).
        let mut ds: Option<Vec<f32>> = None;
        let (mut yin_f0, mut periodicity) = (f64::NAN, 0.0);
        if audible {
            let sr = self.sample_rate as f64;
            // YIN on the lighter ~22 kHz decimation; formants stay on the
            // 11 kHz ds.
            let yin = if self.yin_factor == 1 {
                ds = Some(decimate(raw, self.ds_factor, sr).into_owned());
                yin_pitch(raw, self.yin_rate)
            } else {
                let (d, yin_sig) = decimate_pair(raw, self.ds_factor, self.yin_factor, sr);
                ds = Some(d);
                yin_pitch(&yin_sig, self.yin_rate)
            };
            yin_f0 = yin.f0;
            periodicity = yin.periodicity;
        }
        let voiced = audible && periodicity >= VOICED_CONFIDENCE;
        // A frame can be voiced (periodic) yet have no in-band F0 (pitch
        // outside 70–1100 Hz → NaN). Gate the F0 channel separately so the
        // tuner never locks a stale value and no stale pitch dot is drawn for
        // that frame.
        let f0_voiced = voiced && !yin_f0.is_nan();

        // Sustain gate: count consecutive voiced frames (tolerating short
        // unvoiced dropouts) and only treat the frame as 'sustained' once the
        // run clears the attack — so only held notes drive the
        // overlay/spectrogram, not the brief voiced blips of
        // speech/breath/background.
        if voiced {
            self.voiced_run = self.voiced_run.saturating_add(1);
            self.overlay_silent = 0;
        } else {
            self.overlay_silent = self.overlay_silent.saturating_add(1);
            if self.overlay_silent > OVERLAY_RELEASE {
                self.voiced_run = 0;
            }
        }
        let sustained = self.voiced_run >= OVERLAY_ATTACK;

        // Formants (LPC on the same decimated window) only on voiced frames —
        // formants are a property of phonation, not of noise.
        let (mut rf1, mut rf2, mut rf3) = (f64::NAN, f64::NAN, f64::NAN);
        if voiced {
            if let Some(ds) = &ds {
                let cands = lpc_formants(
                    ds,
                    self.ds_rate,
                    self.lpc_order,
                    self.lpc_win,
                    &self.lpc_window,
                    0.97,
                    (ds.len() / 2) as i64,
                );
                (rf1, rf2, rf3) = assign_formants(&cands);
            }
        }

        // H1−H2 register cue (only on a sustained, in-band voiced frame),
        // median-smoothed like the other live channels so the register
        // readout is steady (raw H1−H2 is noisy frame-to-frame and would
        // jitter across the chest/mix/head score thresholds).
        let mut raw_h1h2 = f64::NAN;
        let h1h2_voiced =
            sustained && f0_voiced && yin_f0 > 0.0 && 2.0 * yin_f0 < self.sample_rate as f64 / 2.0;
        if h1h2_voiced {
            raw_h1h2 = h1_minus_h2(&self.mags, yin_f0, self.bin_hz);
        }
        let h1h2 = smooth_gated(&mut self.h1h2_h, raw_h1h2, h1h2_voiced);
        // Smoothed one track at a time, like the take's (see
        // ordered_formants).
        let gate = sustained && voiced;
        let s1 = smooth_gated(&mut self.f1_h, rf1, gate);
        let s2 = smooth_gated(&mut self.f2_h, rf2, gate);
        let s3 = smooth_gated(&mut self.f3_h, rf3, gate);
        let (f1, f2, f3) = ordered_formants(s1, s2, s3);

        LiveFrame {
            column: col,
            ring,
            tilt,
            cpp,
            f0: smooth_gated(&mut self.f0_h, yin_f0, sustained && f0_voiced),
            f1,
            f2,
            f3,
            active: sustained,
            f0_confidence: if f0_voiced { periodicity } else { 0.0 },
            h1h2,
            loudness: rms_db,
        }
    }
}

/// Trailing median (last [SMOOTH_N] frames), ignoring NaN — but only emits a
/// value on voiced frames. On unvoiced frames it records the gap (NaN) and
/// returns NaN, so a recent voiced value is never smeared onto
/// silence/noise.
fn smooth_gated(hist: &mut VecDeque<f64>, v: f64, voiced: bool) -> f64 {
    hist.push_back(if voiced { v } else { f64::NAN });
    if hist.len() > SMOOTH_N {
        hist.pop_front();
    }
    if !voiced {
        return f64::NAN;
    }
    let mut valid: Vec<f64> = hist.iter().copied().filter(|x| !x.is_nan()).collect();
    valid.sort_by(|a, b| compare_f64(*a, *b));
    if valid.is_empty() {
        f64::NAN
    } else {
        valid[valid.len() / 2]
    }
}

/// [`TunerTracker`]'s knobs, at Dart's defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TunerOptions {
    /// The periodicity a frame needs to count as voiced.
    pub min_confidence: f64,
    /// Consecutive voiced frames within ~1.5 semitones before a note shows.
    pub attack: u32,
    /// Unvoiced frames before the reading releases.
    pub release: u32,
    /// The EMA glide weight of each new frame.
    pub alpha: f64,
}

impl Default for TunerOptions {
    fn default() -> Self {
        TunerOptions {
            min_confidence: 0.45,
            attack: 5,
            release: 6,
            alpha: 0.13,
        }
    }
}

/// Stabilizes a noisy live F0 stream into a steady tuner reading. A note
/// must be clearly voiced (periodicity ≥ `min_confidence`) and hold for
/// `attack` consecutive frames within ~1.5 semitones before it shows — which
/// suppresses the silence/onset jitter — then it EMA-glides; it releases
/// back to NaN after `release` unvoiced frames. Pure + deterministic.
#[derive(Clone, Debug)]
pub struct TunerTracker {
    opts: TunerOptions,
    ema: f64,
    hold: u32,
    silent: u32,
}

impl Default for TunerTracker {
    fn default() -> Self {
        TunerTracker::new(TunerOptions::default())
    }
}

impl TunerTracker {
    pub fn new(opts: TunerOptions) -> Self {
        TunerTracker {
            opts,
            ema: f64::NAN,
            hold: 0,
            silent: 0,
        }
    }

    pub fn options(&self) -> TunerOptions {
        self.opts
    }

    /// Feeds one frame's [f0] + its YIN periodicity [confidence]; returns the
    /// stable tuner pitch (Hz) or NaN when nothing is locked.
    pub fn add(&mut self, f0: f64, confidence: f64) -> f64 {
        let TunerOptions {
            min_confidence,
            attack,
            release,
            alpha,
        } = self.opts;
        let voiced = !f0.is_nan() && f0 > 0.0 && confidence >= min_confidence;
        if voiced {
            if self.ema.is_nan() {
                self.ema = f0;
                self.hold = 1;
            } else {
                let semis = (12.0 * ((f0 / self.ema).ln() / LN_2)).abs();
                if semis < 1.5 {
                    self.ema = self.ema * (1.0 - alpha) + f0 * alpha;
                    self.hold = self.hold.saturating_add(1);
                } else {
                    self.ema = f0; // jumped to a new note — re-lock (re-attack)
                    self.hold = 1;
                }
            }
            self.silent = 0;
        } else {
            self.silent = self.silent.saturating_add(1);
            if self.silent > release {
                self.ema = f64::NAN;
                self.hold = 0;
            }
        }
        if self.hold >= attack {
            self.ema
        } else {
            f64::NAN
        }
    }

    pub fn reset(&mut self) {
        self.ema = f64::NAN;
        self.hold = 0;
        self.silent = 0;
    }
}
