//! The per-frame voice metrics (ring, tilt, CPP, loudness, F0, F1–F3, H1−H2,
//! input quality): the shared frame pass, the batch functions and the
//! windowed streaming analyzer.

use std::f64::consts::LN_10;

use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::voice_metrics::VoiceMetrics;
use vocara_model::util::dart_num::{
    ceil_to_i64, clamp_i64, compare_f64, floor_to_i64, round_to_i64,
};

use super::fft::{hamming, hanning, RealFft};
use super::filter::{decimate, StreamDecimator};
use super::lpc::{assign_formants, harmonic_peak, lpc_formants_padded, ordered_formants};
use super::rolling::RollingF32;
use super::spectrogram::SpectrogramImage;
use super::yin::yin_pitch;
use super::{
    clamp_int, db20, pad_slice_into, spectrogram_bins, stft_frame_count, AnalysisParams,
    EMIT_BLOCK, VOICED_CONFIDENCE,
};

/// The per-frame metrics computation and its whole-take finish, shared by
/// the batch [`compute_voice_metrics`] and the windowed
/// [`StreamingTakeAnalyzer`] — ONE function body, so the two paths cannot
/// diverge (pinned bit-identical by test). Callers extract the three
/// zero-padded windows (raw fft_size, decimated lpc_win+1 with one
/// pre-emphasis lookback sample, yin yin_win) and stream the raw samples
/// through [`MetricsPass::feed_clip`]; everything else lives here.
struct MetricsPass {
    sample_rate: i64,
    fft_size: usize,
    hop: usize,
    half: usize,
    bin_hz: f64,
    window: Vec<f64>,
    fft: RealFft,
    ds_factor: usize,
    yin_factor: usize,
    ds_rate: f64,
    yin_rate: f64,
    lpc_order: usize,
    lpc_win: usize,
    lpc_window: Vec<f64>,
    yin_win: usize,
    b2k: usize,
    b4k: usize,
    b50: usize,
    b1k: usize,
    b5k: usize,
    q_min: usize,
    q_max: usize,

    // Per-frame outputs, one entry per emitted frame. Dart sized these from
    // the expected frame count and indexed them (a pass fed more frames than
    // it expected threw a RangeError); here they are reserved from it and
    // grow past it.
    ring: Vec<f32>,
    tilt: Vec<f32>,
    cpp: Vec<f32>,
    loudness: Vec<f32>,
    energy: Vec<f64>,
    rf0: Vec<f32>,
    rf1: Vec<f32>,
    rf2: Vec<f32>,
    rf3: Vec<f32>,
    rh1h2: Vec<f32>,
    per: Vec<f32>,

    // Per-frame scratch, reused every frame: the Hann-windowed frame, the dB
    // spectrum mirrored for the cepstrum, the magnitude spectrum (bins
    // 0..=half) and the cepstrum's real parts over the F0 quefrency window
    // q_min..=q_max.
    frame: Vec<f64>,
    logmag: Vec<f64>,
    mags: Vec<f64>,
    cep_re: Vec<f64>,

    max_energy: f64,

    /// When > 0, every frame also keeps the first [spectrogram_bins] bins of
    /// its dB spectrum as a spectrogram column ([image]). The spectrogram's
    /// STFT is this pass's first FFT exactly — same frames, same Hann window,
    /// same transform, same 20·log10(|X| + 1e-9) — so the column IS what a
    /// separate `compute_spectrogram` would compute, bit for bit (pinned by
    /// test), without transforming every frame a third time.
    spectrogram_bins: usize,
    /// The spectrogram image, sized from the pass's frame capacity; `None`
    /// when no spectrogram is kept.
    image: Option<SpectrogramImage>,

    // Clip/peak scan state (streamed; the batch feeds the whole track once).
    len: usize,
    peak: f64,
    clipped: usize,
    run: usize,
    prev_high: f64,
}

const PRE_COEF: f64 = 0.97;

impl MetricsPass {
    fn new(
        sample_rate: i64,
        fft_size: usize,
        hop: usize,
        capacity_frames: usize,
        spectrogram_bins: usize,
    ) -> Self {
        let sr = sample_rate as f64;
        let half = fft_size / 2;
        let bin_hz = sr / fft_size as f64;
        let ds_factor = clamp_i64(round_to_i64(sr / 11000.0), 1, 8) as usize;
        let yin_factor = clamp_i64(round_to_i64(sr / 22050.0), 1, 2) as usize;
        let ds_rate = sr / ds_factor as f64;
        let yin_rate = sr / yin_factor as f64;
        let lpc_order = clamp_i64(round_to_i64(2.0 + ds_rate / 1000.0), 8, 20) as usize;
        let lpc_win = fft_size / ds_factor;
        let bin_of =
            |hz: f64| -> usize { round_to_i64(hz / bin_hz).clamp(0, half as i64) as usize };
        let q_min = clamp_int(floor_to_i64(sr / 500.0), 2, half as i64);
        let q_max = clamp_int(ceil_to_i64(sr / 60.0), q_min + 1, half as i64);
        let (q_min, q_max) = (q_min as usize, q_max as usize);
        let cap = |n: usize| Vec::with_capacity(n);
        MetricsPass {
            sample_rate,
            fft_size,
            hop,
            half,
            bin_hz,
            window: hanning(fft_size),
            fft: RealFft::new(fft_size),
            ds_factor,
            yin_factor,
            ds_rate,
            yin_rate,
            lpc_order,
            lpc_win,
            lpc_window: hamming(lpc_win),
            yin_win: round_to_i64(fft_size as f64 / yin_factor as f64) as usize,
            b2k: bin_of(2000.0),
            b4k: bin_of(4000.0),
            b50: bin_of(50.0),
            b1k: bin_of(1000.0),
            b5k: bin_of(5000.0),
            q_min,
            q_max,
            ring: cap(capacity_frames),
            tilt: cap(capacity_frames),
            cpp: cap(capacity_frames),
            loudness: cap(capacity_frames),
            energy: Vec::with_capacity(capacity_frames),
            rf0: cap(capacity_frames),
            rf1: cap(capacity_frames),
            rf2: cap(capacity_frames),
            rf3: cap(capacity_frames),
            rh1h2: cap(capacity_frames),
            per: cap(capacity_frames),
            frame: vec![0.0; fft_size],
            logmag: vec![0.0; fft_size],
            mags: vec![0.0; half + 1],
            cep_re: vec![0.0; q_max - q_min + 1],
            max_energy: 1e-12,
            spectrogram_bins,
            image: (spectrogram_bins > 0)
                .then(|| SpectrogramImage::new(spectrogram_bins, capacity_frames)),
            len: 0,
            peak: 0.0,
            clipped: 0,
            run: 0,
            prev_high: 0.0,
        }
    }

    fn emitted(&self) -> usize {
        self.ring.len()
    }

    /// Streams raw samples through the input-quality scan (peak /
    /// true-clipping run detection). Chunk boundaries are invisible: the run
    /// state carries.
    ///
    /// A clipped run is held at the rail: flat to float rounding, not merely
    /// slow. The engine records float after its input gain, so a clean loud
    /// low note crests above 1.0 with neighbouring samples a few 1e-5 apart;
    /// within the old 3e-4 they read as clipped and the take said to lower
    /// the mic gain over audio that never clipped. (Clipping at the mic
    /// itself is not visible in such a take at all: its high-pass and gain
    /// reshape the plateau. What still reads here is a file clipped where it
    /// was made.)
    fn feed_clip(&mut self, chunk: &[f32]) {
        const CLIP_LEVEL: f64 = 0.997;
        const CLIP_RUN: usize = 4;
        const FLAT_EPS: f64 = 1e-6;
        for &s in chunk {
            let a = f64::from(s).abs();
            if a > self.peak {
                self.peak = a;
            }
            if a >= CLIP_LEVEL && (self.run == 0 || (a - self.prev_high).abs() <= FLAT_EPS) {
                self.run += 1;
                self.prev_high = a;
                if self.run == CLIP_RUN {
                    self.clipped += CLIP_RUN;
                } else if self.run > CLIP_RUN {
                    self.clipped += 1;
                }
            } else {
                self.run = 0;
            }
        }
        self.len += chunk.len();
    }

    /// One analysis frame. [raw_padded] is the fft_size raw window
    /// (zero-padded past the end), [ds_padded] the lpc_win+1 decimated window
    /// whose first sample is the pre-emphasis lookback, [yin_padded] the
    /// yin_win decimated YIN window — all extracted by the caller with
    /// `pad_slice` semantics.
    fn emit_frame(&mut self, fr: usize, raw_padded: &[f32], ds_padded: &[f32], yin_padded: &[f32]) {
        debug_assert_eq!(fr, self.emitted(), "frames are emitted in order");
        let fft_size = self.fft_size;
        let half = self.half;

        // --- Plain windowed magnitude spectrum (F18 / F19 / energy / CPP) +
        // the raw-RMS loudness (F9). ---
        let mut raw_sq = 0.0;
        for ((f, &s), &w) in self
            .frame
            .iter_mut()
            .zip(&raw_padded[..fft_size])
            .zip(&self.window)
        {
            let s = f64::from(s);
            raw_sq += s * s;
            *f = s * w;
        }
        self.loudness
            .push((20.0 * (((raw_sq / fft_size as f64).sqrt() + 1e-9).ln() / LN_10)) as f32);
        self.fft.transform(&self.frame);
        self.fft.magnitudes_into(&mut self.mags, half + 1);
        let mags = &self.mags;

        let mut e = 0.0;
        for &m in &mags[1..=half] {
            e += m * m;
        }
        self.energy.push(e);
        if e > self.max_energy {
            self.max_energy = e;
        }

        // F18 — ring / Singing Power Ratio. Skip the DC bin so a decode offset
        // can never be picked as the low-band peak.
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
        self.ring.push((db20(high_pk) - db20(low_pk)) as f32);

        // F19 — spectral tilt / alpha ratio.
        let (mut low_e, mut high_e) = (0.0f64, 0.0f64);
        for &m in &mags[self.b50..self.b1k.max(self.b50)] {
            low_e += m * m;
        }
        for &m in &mags[self.b1k..self.b5k.max(self.b1k)] {
            high_e += m * m;
        }
        self.tilt
            .push((10.0 * (((high_e + 1e-12) / (low_e + 1e-12)).ln() / LN_10)) as f32);

        // F20 — CPP: real cepstrum of the dB spectrum, peak prominence above
        // its least-squares regression over the F0 quefrency window.
        for (l, &m) in self.logmag.iter_mut().zip(&mags[..=half]) {
            *l = db20(m);
        }
        if let Some(image) = &mut self.image {
            let column = image.next_column();
            for (d, &v) in column.iter_mut().zip(&self.logmag[..self.spectrogram_bins]) {
                *d = v as f32;
            }
        }
        for k in half + 1..fft_size {
            self.logmag[k] = self.logmag[fft_size - k];
        }
        self.fft.transform(&self.logmag);
        self.fft
            .real_parts_into(&mut self.cep_re, self.q_min, self.q_max + 1);
        self.cpp
            .push(cepstral_peak_prominence(&self.cep_re, self.q_min, self.q_max, fft_size) as f32);

        // F0 — YIN on the decimated frame centered at this time:
        // octave-robust and time-domain, with a periodicity confidence that
        // drives the voicing gate.
        let yin = yin_pitch(yin_padded, self.yin_rate);
        self.rf0.push(yin.f0 as f32);
        self.per.push(yin.periodicity as f32);

        // H1−H2 register cue: peak dB at F0 vs at its 2nd harmonic (NaN unless
        // a clear in-band F0 whose 2nd harmonic is below Nyquist).
        let f0_hz = yin.f0;
        self.rh1h2.push(
            if !f0_hz.is_nan() && f0_hz > 0.0 && 2.0 * f0_hz < self.sample_rate as f64 / 2.0 {
                h1_minus_h2(&self.mags, f0_hz, self.bin_hz) as f32
            } else {
                f32::NAN
            },
        );

        // F17 — formants via LPC on the decimated frame centered at this
        // time, then assign poles to the F1/F2/F3 bands (stable ordinal
        // labels).
        let cands = lpc_formants_padded(
            ds_padded,
            self.ds_rate,
            self.lpc_order,
            self.lpc_win,
            &self.lpc_window,
            PRE_COEF,
        );
        let (af1, af2, af3) = assign_formants(&cands);
        self.rf1.push(af1 as f32);
        self.rf2.push(af2 as f32);
        self.rf3.push(af3 as f32);
    }

    /// Whole-take post-processing (voicing gate, median smoothing, quality
    /// stats) — verbatim the batch tail, over the emitted frames — and the
    /// spectrogram the pass captured, if it captured one.
    fn finish(self, max_hz: f64) -> (VoiceMetrics, Option<Spectrogram>) {
        let MetricsPass {
            sample_rate,
            fft_size,
            hop,
            ring,
            tilt,
            cpp,
            loudness,
            energy,
            mut rf0,
            mut rf1,
            mut rf2,
            mut rf3,
            mut rh1h2,
            per,
            max_energy,
            image,
            len,
            peak,
            clipped,
            ..
        } = self;
        let spectrogram = image.map(|image| image.finish(max_hz, sample_rate, fft_size, hop));
        let n_frames = ring.len();
        let mut active = vec![0u8; n_frames];
        let mut voiced_mask = vec![0u8; n_frames];

        // Voicing gate: a frame is voiced only when it is both audible
        // (within 30 dB of peak energy) AND periodic (YIN periodicity ≥
        // VOICED_CONFIDENCE). Averages use audible frames; F0/formants are
        // nulled on every non-voiced frame so the overlay never draws
        // pitch/formant dots on noise or breath.
        let gate = max_energy * 1e-3;
        let (mut r_sum, mut t_sum, mut c_sum, mut n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
        for fr in 0..n_frames {
            let audible = energy[fr] >= gate;
            let voiced = audible && f64::from(per[fr]) >= VOICED_CONFIDENCE;
            // [active] tracks audible frames (LTAS), but the ring/tilt/CPP
            // timbre averages use only VOICED frames — averaging over
            // breath/consonant noise dilutes them and mismatched the live
            // readout's voiced-only average.
            if audible {
                active[fr] = 1;
            }
            if voiced {
                voiced_mask[fr] = 1;
                r_sum += f64::from(ring[fr]);
                t_sum += f64::from(tilt[fr]);
                c_sum += f64::from(cpp[fr]);
                n += 1;
            } else {
                rf0[fr] = f32::NAN;
                rf1[fr] = f32::NAN;
                rf2[fr] = f32::NAN;
                rf3[fr] = f32::NAN;
                rh1h2[fr] = f32::NAN;
            }
        }
        // Temporal median smoothing (±2 frames) turns the raw F0/formants
        // into steady tracks, removing the per-frame jitter spectral
        // peak-picking suffered from.
        let mut f0 = median_of(&rf0, 2);
        let mut f1 = median_of(&rf1, 2);
        let mut f2 = median_of(&rf2, 2);
        let mut f3 = median_of(&rf3, 2);
        let mut h1h2 = median_of(&rh1h2, 2);
        // Re-apply the voicing gate AFTER smoothing. The two NaN meanings
        // differ: the median bridges measurement dropouts (LPC/H1−H2 misses
        // on frames the gate declared VOICED) from valid neighbours, while
        // this mask keeps every gate-NaN frame NaN so smoothing never paints
        // F0/formants onto breath or silence. Mirrors the live path's
        // smooth_gated, which separates "unvoiced" from "voiced but
        // unmeasured" the same way.
        for fr in 0..n_frames {
            if voiced_mask[fr] == 0 {
                f0[fr] = f32::NAN;
                f1[fr] = f32::NAN;
                f2[fr] = f32::NAN;
                f3[fr] = f32::NAN;
                h1h2[fr] = f32::NAN;
            } else {
                let (a, b, c) =
                    ordered_formants(f64::from(f1[fr]), f64::from(f2[fr]), f64::from(f3[fr]));
                f1[fr] = a as f32;
                f2[fr] = b as f32;
                f3[fr] = c as f32;
            }
        }
        // No voiced frame means no timbre average: NaN, the readouts' "—"
        // (and what the live monitor's averages answer). A 0.0 read as a
        // measurement (CPP "0.0", a technique gap of several dB to the
        // reference).
        let inv = if n == 0 { f64::NAN } else { 1.0 / n as f64 };

        // --- Input quality: peak level, clipping, SNR ---------------------
        let peak_dbfs = if len == 0 {
            f64::NAN
        } else {
            20.0 * ((peak + 1e-9).ln() / LN_10)
        };
        let clipped_fraction = if len == 0 {
            0.0
        } else {
            clipped as f64 / len as f64
        };
        // SNR: median loudness of VOICED frames over the noise floor (the
        // 10th-pct of all frame loudness ≈ background between phrases).
        let mut all_loud = Vec::new();
        let mut voiced_loud = Vec::new();
        for fr in 0..n_frames {
            let v = f64::from(loudness[fr]);
            if !v.is_finite() {
                continue;
            }
            all_loud.push(v);
            if active[fr] == 1 && f64::from(per[fr]) >= VOICED_CONFIDENCE {
                voiced_loud.push(v);
            }
        }
        let mut snr_db = f64::NAN;
        if all_loud.len() >= 8 && !voiced_loud.is_empty() {
            all_loud.sort_by(|a, b| compare_f64(*a, *b));
            voiced_loud.sort_by(|a, b| compare_f64(*a, *b));
            let noise = all_loud[floor_to_i64(all_loud.len() as f64 * 0.1) as usize];
            let signal = voiced_loud[voiced_loud.len() / 2];
            snr_db = signal - noise;
        }

        let metrics = VoiceMetrics {
            frames: n_frames,
            hop: hop as i64,
            fft_size: fft_size as i64,
            sample_rate,
            ring,
            tilt,
            cpp,
            loudness,
            f0,
            f1,
            f2,
            f3,
            h1h2,
            active,
            ring_avg: r_sum * inv,
            tilt_avg: t_sum * inv,
            cpp_avg: c_sum * inv,
            peak_dbfs,
            clipped_fraction,
            snr_db,
        };
        (metrics, spectrogram)
    }
}

/// CPP from the cepstrum's real parts [cep] over quefrencies
/// [q_min]..=[q_max] (scaled by 1/[fft_size]): locate the rahmonic peak,
/// then fit the cepstral-baseline regression over the window EXCLUDING a
/// small neighborhood of the peak — so the line reflects the spectral trend,
/// not the peak it's measured against (including the peak biases
/// prominence, and thus CPP, downward).
pub(crate) fn cepstral_peak_prominence(
    cep: &[f64],
    q_min: usize,
    q_max: usize,
    fft_size: usize,
) -> f64 {
    let fft_size = fft_size as f64;
    let mut peak_q = q_min;
    let mut peak_v = -1e30;
    for q in q_min..=q_max {
        let v = cep[q - q_min] / fft_size;
        if v > peak_v {
            peak_v = v;
            peak_q = q;
        }
    }
    const CPP_GUARD: usize = 3; // bins around the peak left out of the baseline fit
    let (mut sx, mut sy, mut sxx, mut sxy, mut cnt) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0usize);
    for q in q_min..=q_max {
        if q.abs_diff(peak_q) <= CPP_GUARD {
            continue;
        }
        let v = cep[q - q_min] / fft_size;
        let qd = q as f64;
        sx += qd;
        sy += v;
        sxx += qd * qd;
        sxy += qd * v;
        cnt += 1;
    }
    let cnt_d = cnt as f64;
    let denom = cnt_d * sxx - sx * sx;
    let slope = if denom.abs() < 1e-12 {
        0.0
    } else {
        (cnt_d * sxy - sx * sy) / denom
    };
    let intercept = if cnt == 0 {
        0.0
    } else {
        (sy - slope * sx) / cnt_d
    };
    peak_v - (slope * peak_q as f64 + intercept)
}

/// H1−H2 (dB) for a frame whose pitch is [f0_hz]: the harmonic peaks at F0
/// and 2·F0 in [mags] (bins 0..=half, [bin_hz] apart). The ±-bin search
/// shrinks at low F0 so the F0 and 2·F0 windows can never overlap (overlap
/// lets H1's leakage inflate H2 and biases H1−H2 down).
pub(crate) fn h1_minus_h2(mags: &[f64], f0_hz: f64, bin_hz: f64) -> f64 {
    let hw = clamp_i64(floor_to_i64((f0_hz / bin_hz - 1.0) / 2.0), 0, 2);
    db20(harmonic_peak(mags, f0_hz / bin_hz, hw))
        - db20(harmonic_peak(mags, 2.0 * f0_hz / bin_hz, hw))
}

/// The running ±[half]-frame median of [src] (ignoring NaN). A NaN center is
/// bridged from its valid neighbours on purpose: the raw tracks carry NaN on
/// VOICED frames too (LPC / H1−H2 measurement dropouts), and those one-frame
/// holes should be filled. Keeping unvoiced frames NaN is the CALLER's job —
/// it re-applies the voicing mask after smoothing.
fn median_of(src: &[f32], half: usize) -> Vec<f32> {
    let n = src.len();
    let mut dst = vec![0.0f32; n];
    let mut buf: Vec<f64> = Vec::with_capacity(2 * half + 1);
    for (i, d) in dst.iter_mut().enumerate() {
        buf.clear();
        let lo = i.saturating_sub(half);
        let hi = (i + half).min(n - 1);
        for &v in &src[lo..=hi] {
            if !v.is_nan() {
                buf.push(f64::from(v));
            }
        }
        *d = if buf.is_empty() {
            f32::NAN
        } else {
            buf.sort_by(|a, b| compare_f64(*a, *b));
            buf[buf.len() / 2] as f32
        };
    }
    dst
}

/// [`compute_voice_metrics_and_spectrogram`]'s result.
#[derive(Clone, Debug)]
pub struct MetricsAndSpectrogram {
    pub metrics: VoiceMetrics,
    pub spectrogram: Spectrogram,
}

/// [`StreamingTakeAnalyzer::finish`]'s result: the metrics, and the
/// spectrogram when the analyzer was asked for one.
#[derive(Clone, Debug)]
pub struct StreamedTakeAnalysis {
    pub metrics: VoiceMetrics,
    pub spectrogram: Option<Spectrogram>,
}

/// Computes per-frame voice-timbre metrics from mono [samples] —
/// ring/SPR (F18), spectral tilt / alpha ratio (F19), cepstral peak
/// prominence (F20), the first three formants F1/F2/F3 (F17), F0, H1−H2,
/// loudness and the input-quality stats. Same framing as
/// `compute_spectrogram` ([`AnalysisParams::max_hz`] is not used). Pure. See
/// [`VoiceMetrics`] for the meaning of each curve. The per-frame math is
/// shared verbatim with the windowed [`StreamingTakeAnalyzer`] (this batch
/// path doubles as its equality oracle).
pub fn compute_voice_metrics(
    samples: &[f32],
    sample_rate: i64,
    params: AnalysisParams,
) -> VoiceMetrics {
    batch_pass(samples, sample_rate, params.fft_size, params.hop, 0)
        .finish(params.max_hz)
        .0
}

/// [`compute_voice_metrics`] and `compute_spectrogram` over the same
/// [samples] in ONE pass: the spectrogram columns come out of the metrics
/// pass's own FFT, bit-identical to the separate call, which transformed
/// every frame a third time.
pub fn compute_voice_metrics_and_spectrogram(
    samples: &[f32],
    sample_rate: i64,
    params: AnalysisParams,
) -> MetricsAndSpectrogram {
    let bins = spectrogram_bins(params.max_hz, params.fft_size, sample_rate);
    let (metrics, spectrogram) =
        batch_pass(samples, sample_rate, params.fft_size, params.hop, bins).finish(params.max_hz);
    MetricsAndSpectrogram {
        metrics,
        spectrogram: spectrogram.unwrap_or_else(|| empty_spectrogram(params, sample_rate)),
    }
}

/// The spectrogram of a pass asked for none (only a zero-bin request: a
/// maxHz at or below 0), assembled as the image would assemble it.
fn empty_spectrogram(params: AnalysisParams, sample_rate: i64) -> Spectrogram {
    SpectrogramImage::new(0, 0).finish(params.max_hz, sample_rate, params.fft_size, params.hop)
}

/// The batch frame loop shared by [`compute_voice_metrics`] and
/// [`compute_voice_metrics_and_spectrogram`]; the caller finishes the pass.
fn batch_pass(
    samples: &[f32],
    sample_rate: i64,
    fft_size: usize,
    hop: usize,
    spectrogram_bins: usize,
) -> MetricsPass {
    // Match the spectrogram's STFT framing exactly (stft_frame_count: a
    // zero-padded trailing partial frame is emitted) so VoiceMetrics.frames
    // == Spectrogram.frames and the per-frame overlay/playhead stay aligned.
    // The final frame is zero-padded past the end of [samples].
    let len = samples.len();
    let n_frames = stft_frame_count(len, fft_size, hop);
    let mut pass = MetricsPass::new(sample_rate, fft_size, hop, n_frames, spectrogram_bins);
    pass.feed_clip(samples);

    // Whole-track decimated copies (the streaming path produces these
    // incrementally with identical filter math; see StreamingTakeAnalyzer).
    let ds = decimate(samples, pass.ds_factor, sample_rate as f64);
    let yin_sig = if pass.yin_factor == 1 {
        std::borrow::Cow::Borrowed(samples)
    } else {
        decimate(samples, pass.yin_factor, sample_rate as f64)
    };

    let mut raw_window = vec![0.0f32; fft_size];
    let mut ds_window = vec![0.0f32; pass.lpc_win + 1];
    let mut yin_window = vec![0.0f32; pass.yin_win];
    for fr in 0..n_frames {
        let start = fr * hop;
        let (ds_center, yin_center) = pass.centers(fr);
        pad_slice_into(&mut raw_window, samples, start as i64, len);
        pad_slice_into(
            &mut ds_window,
            &ds,
            pass.ds_window_start(ds_center),
            ds.len(),
        );
        pad_slice_into(
            &mut yin_window,
            &yin_sig,
            pass.yin_window_start(yin_center),
            yin_sig.len(),
        );
        pass.emit_frame(fr, &raw_window, &ds_window, &yin_window);
    }
    pass
}

impl MetricsPass {
    /// The decimated-domain centres of frame [fr]'s window (LPC, YIN).
    fn centers(&self, fr: usize) -> (i64, i64) {
        let centre = (fr * self.hop) as f64 + self.fft_size as f64 / 2.0;
        (
            round_to_i64(centre / self.ds_factor as f64),
            round_to_i64(centre / self.yin_factor as f64),
        )
    }

    /// Where the LPC window (with its one lookback sample) starts for a
    /// decimated centre.
    fn ds_window_start(&self, ds_center: i64) -> i64 {
        ds_center - (self.lpc_win / 2) as i64 - 1
    }

    /// Where the YIN window starts for a decimated centre.
    fn yin_window_start(&self, yin_center: i64) -> i64 {
        yin_center - (self.yin_win / 2) as i64
    }
}

/// WINDOWED streaming take analysis: feeds mono chunks, computes the
/// spectrogram (optional) and the full [`VoiceMetrics`] without the
/// whole-track buffer or the full decimated copies ever existing. The
/// per-frame math IS the pass the batch [`compute_voice_metrics`] runs, and
/// the decimators reproduce the batch decimation bit-exactly, so the result
/// is pinned bit-identical to the batch path by test (batch = the equality
/// oracle). The spectrogram is the pass's own first FFT per frame, equal to
/// `compute_spectrogram` bit for bit.
///
/// Rolling lookback: raw needs `[fr*hop, fr*hop + fft_size)`, the decimated
/// signals a centered window each — a frame is emitted the moment all three
/// are available, and consumed head is dropped, so the resident set is a few
/// windows regardless of take length.
pub struct StreamingTakeAnalyzer {
    max_hz: f64,
    pass: MetricsPass,
    ds_dec: StreamDecimator,
    yin_dec: StreamDecimator,
    raw: RollingF32,
    ds: RollingF32,
    yin: RollingF32,
    /// Each frame's three windows, refilled frame after frame (the pass
    /// keeps nothing of them).
    raw_window: Vec<f32>,
    ds_window: Vec<f32>,
    yin_window: Vec<f32>,
    next_frame: usize,
}

impl StreamingTakeAnalyzer {
    /// An analyzer for a take of [expected_frames] frames (its
    /// [`stft_frame_count`], which sizes the outputs once). With
    /// [spectrogram] it also builds the take's spectrogram up to
    /// [`AnalysisParams::max_hz`].
    pub fn new(
        sample_rate: i64,
        params: AnalysisParams,
        expected_frames: usize,
        spectrogram: bool,
    ) -> Self {
        let AnalysisParams {
            fft_size,
            hop,
            max_hz,
        } = params;
        let bins = if spectrogram {
            spectrogram_bins(max_hz, fft_size, sample_rate)
        } else {
            0
        };
        let pass = MetricsPass::new(sample_rate, fft_size, hop, expected_frames, bins);
        let fs = sample_rate as f64;
        StreamingTakeAnalyzer {
            max_hz,
            ds_dec: StreamDecimator::new(pass.ds_factor, fs),
            yin_dec: StreamDecimator::new(pass.yin_factor, fs),
            raw: RollingF32::default(),
            ds: RollingF32::default(),
            yin: RollingF32::default(),
            raw_window: vec![0.0; fft_size],
            ds_window: vec![0.0; pass.lpc_win + 1],
            yin_window: vec![0.0; pass.yin_win],
            next_frame: 0,
            pass,
        }
    }

    /// Feeds the next chunk of mono samples. [loudest] is each of its frames'
    /// loudest channel when the source had more than one: the peak and
    /// clipping scan reads it, since averaged into [chunk] a full-scale
    /// channel beside a silent one read 6 dB low and never clipped.
    pub fn add(&mut self, chunk: &[f32], loudest: Option<&[f32]>) {
        self.pass.feed_clip(loudest.unwrap_or(chunk));
        for block in chunk.chunks(EMIT_BLOCK) {
            self.raw.add_all(block);
            self.ds_dec.feed_all(block, &mut self.ds);
            self.yin_dec.feed_all(block, &mut self.yin);
            self.emit_ready(false);
        }
    }

    fn emit_ready(&mut self, flush: bool) {
        let len = self.raw.len();
        let (fft_size, hop) = (self.pass.fft_size, self.pass.hop);
        // The frame count is fixed by the FINAL length; during feeding only
        // frames whose inputs are complete may emit (they exist for any
        // longer final length too).
        let n_frames = stft_frame_count(len, fft_size, hop);
        while self.next_frame < n_frames {
            let fr = self.next_frame;
            let start = fr * hop;
            let (ds_center, yin_center) = self.pass.centers(fr);
            // The BATCH decimated length is len over the factor (floored); the
            // incremental decimator can be one provisional sample ahead when
            // len is not a multiple of the factor. Readiness AND extraction
            // cap at the batch-equivalent length, so a tail frame never reads
            // a sample the batch would have zero-padded (bit-exactness at the
            // edges).
            let ds_max = (len / self.pass.ds_factor) as i64;
            let yin_max = if self.pass.yin_factor == 1 {
                len as i64
            } else {
                (len / self.pass.yin_factor) as i64
            };
            let ds_start = self.pass.ds_window_start(ds_center);
            let yin_start = self.pass.yin_window_start(yin_center);
            if !flush {
                let raw_ready = start + fft_size <= len;
                // Dart: start + lpcWin + 1 <= dsMax (the window's last sample).
                let ds_ready = ds_start + (self.pass.lpc_win as i64) < ds_max;
                let yin_ready = yin_start + self.pass.yin_win as i64 <= yin_max;
                if !raw_ready || !ds_ready || !yin_ready {
                    return;
                }
            }
            self.raw
                .slice_into(&mut self.raw_window, start as i64, len as i64);
            self.ds.slice_into(&mut self.ds_window, ds_start, ds_max);
            self.yin
                .slice_into(&mut self.yin_window, yin_start, yin_max);
            self.pass
                .emit_frame(fr, &self.raw_window, &self.ds_window, &self.yin_window);
            self.next_frame = fr + 1;
            // Retain only what future frames can still reach.
            let (next_ds, next_yin) = self.pass.centers(self.next_frame);
            self.raw.drop_below((self.next_frame * hop) as i64);
            self.ds.drop_below(self.pass.ds_window_start(next_ds));
            self.yin.drop_below(self.pass.yin_window_start(next_yin));
        }
    }

    /// Flushes the tail (frames whose windows extend past the end,
    /// zero-padded exactly like the batch) and assembles the results.
    pub fn finish(mut self) -> StreamedTakeAnalysis {
        self.emit_ready(true);
        let (metrics, spectrogram) = self.pass.finish(self.max_hz);
        StreamedTakeAnalysis {
            metrics,
            spectrogram,
        }
    }
}
