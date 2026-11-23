//! YIN fundamental-frequency estimation (de Cheveigné & Kawahara, 2002).

use std::f64::consts::PI;

use vocara_model::util::dart_num::{ceil_to_i64, clamp_f64, floor_to_i64, round_to_i64};

/// [`yin_pitch_with`]'s search band and threshold, at Dart's defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YinParams {
    /// Lowest pitch reported (Hz).
    pub fmin: f64,
    /// Highest pitch reported (Hz).
    pub fmax: f64,
    /// The d′ dip depth that counts as periodic (YIN's absolute threshold).
    pub threshold: f64,
}

impl Default for YinParams {
    fn default() -> Self {
        YinParams {
            fmin: 70.0,
            fmax: 1100.0,
            threshold: 0.15,
        }
    }
}

/// One frame's YIN estimate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YinEstimate {
    /// The pitch in Hz; NaN when no periodic lag exists in the band.
    pub f0: f64,
    /// Periodicity confidence in [0, 1] (1 = perfectly periodic).
    pub periodicity: f64,
}

/// YIN step 1, the squared-difference function d(τ) = Σⱼ (x[j] − x[j+τ])²
/// over the first [w] samples of [frame], for τ in 1..=[tau_max] (d[0] is 0).
///
/// O(w·τmax) — ~245k multiply-adds per 48 kHz frame, the bulk of the take
/// analysis and of the live analyzer's budget. A one-lag loop is bound by
/// its single add chain, so the lags are swept EIGHT at a time: every lag is
/// its own accumulator summed in the same j order with the same IEEE
/// multiply and add (no fused multiply-add), so every d[τ] is the one-lag
/// loop's sum, bit for bit (pinned by test) — the eight chains just overlap.
/// The f64 copy is exact (f32 → f64 is lossless) and spares the conversions
/// per step.
pub(crate) fn yin_difference(frame: &[f32], tau_max: usize, w: usize) -> Vec<f64> {
    let x: Vec<f64> = frame.iter().map(|&v| f64::from(v)).collect();
    let mut d = vec![0.0; tau_max + 1];
    let mut tau = 1;
    while tau + 7 <= tau_max {
        let mut s = [0.0f64; 8];
        let lagged = &x[tau..tau + w + 7];
        for (j, &xj) in x[..w].iter().enumerate() {
            let seg = &lagged[j..j + 8];
            for l in 0..8 {
                let diff = xj - seg[l];
                s[l] += diff * diff;
            }
        }
        d[tau..tau + 8].copy_from_slice(&s);
        tau += 8;
    }
    while tau <= tau_max {
        let mut sum = 0.0;
        for j in 0..w {
            let diff = x[j] - x[j + tau];
            sum += diff * diff;
        }
        d[tau] = sum;
        tau += 1;
    }
    d
}

/// YIN fundamental-frequency estimate for one time-domain [frame] sampled at
/// [sample_rate], over Dart's default band (70–1100 Hz, threshold 0.15). See
/// [`yin_pitch_with`].
pub fn yin_pitch(frame: &[f32], sample_rate: f64) -> YinEstimate {
    yin_pitch_with(frame, sample_rate, YinParams::default())
}

/// YIN fundamental-frequency estimate (de Cheveigné & Kawahara, 2002) for
/// one time-domain [frame] sampled at [sample_rate]. Returns the pitch in Hz
/// plus a periodicity confidence in [0, 1] (1 = perfectly periodic); the
/// pitch is NaN when no periodic lag exists in `fmin..fmax`.
///
/// Unlike the cepstrum/autocorrelation the live path used before, YIN works
/// in the time domain and the cumulative-mean normalization (step 2)
/// suppresses the zero-lag dip that makes those methods jump octaves — so it
/// is both more accurate and far less prone to octave errors. Pure.
pub fn yin_pitch_with(frame: &[f32], sample_rate: f64, params: YinParams) -> YinEstimate {
    let YinParams {
        fmin,
        fmax,
        threshold,
    } = params;
    let unvoiced = YinEstimate {
        f0: f64::NAN,
        periodicity: 0.0,
    };
    let n = frame.len() as i64;
    if n < 4 {
        return unvoiced;
    }
    // Search a little past the [fmin, fmax] band so a tone sitting on a
    // boundary produces an interior dip (with neighbours for parabolic
    // interpolation) instead of a half-period correlation pinned to the
    // search edge — which would otherwise report a false ~fmin pitch for
    // sub-fmin tones and drop a tone just under fmax. The final range gate
    // still rejects out-of-band pitch.
    let mut tau_max = ceil_to_i64(sample_rate / (fmin * 0.9));
    let tau_min = floor_to_i64(sample_rate / (fmax * 1.1)).clamp(2, n - 1);
    if tau_max > n - 1 {
        tau_max = n - 1;
    }
    let w = n - tau_max; // integration window length
    if w < 2 || tau_max <= tau_min {
        return unvoiced;
    }
    let (tau_max, tau_min) = (tau_max as usize, tau_min as usize);

    // 1) Squared-difference function d(τ) = Σⱼ (x[j] − x[j+τ])².
    let d = yin_difference(frame, tau_max, w as usize);

    // 2) Cumulative mean normalized difference d′(τ) — d(τ) divided by the
    // running mean of d up to τ. d′(0)=1; this is what flattens the spurious
    // low-τ dip.
    let mut dn = vec![0.0; tau_max + 1];
    dn[0] = 1.0;
    let mut running = 0.0;
    for t in 1..=tau_max {
        running += d[t];
        dn[t] = if running > 0.0 {
            d[t] * t as f64 / running
        } else {
            1.0
        };
    }

    // 3) Absolute threshold: the first lag whose dip falls below [threshold]
    // (descending to that dip's local minimum); if none does, fall back to
    // the in-band global minimum so periodicity still reflects the best lag.
    // The threshold scan starts below the band (d′ is defined from τ=1):
    // starting it at tauMin skipped an above-fmax tone's true period T and
    // landed on its 2T dip, which the range gate then accepted as f0/2 — an
    // octave-low pitch at full confidence. Found at T, such a tone fails the
    // range gate → NaN.
    let mut tau: Option<usize> = None;
    let mut t = 2;
    while t <= tau_max {
        if dn[t] < threshold {
            while t < tau_max && dn[t + 1] < dn[t] {
                t += 1;
            }
            tau = Some(t);
            break;
        }
        t += 1;
    }
    let tau = match tau {
        None => {
            let mut best = tau_min;
            for t in tau_min + 1..=tau_max {
                if dn[t] < dn[best] {
                    best = t;
                }
            }
            // The shortest period nearly as strong as the deepest (YIN's own
            // preference, which the threshold scan applies above): a breathy
            // or noisy voiced frame dips almost equally at T, 2T and 3T, and
            // the deepest of them was an octave or a twelfth low about half
            // the time — at a confidence the voicing gate passes.
            let mut pick = best;
            for k in 2..=4 {
                let c = round_to_i64(best as f64 / k as f64);
                if c - 1 < tau_min as i64 {
                    break;
                }
                let c = c as usize;
                let mut m = c;
                if dn[c - 1] < dn[m] {
                    m = c - 1;
                }
                if dn[c + 1] < dn[m] {
                    m = c + 1;
                }
                if dn[m] <= dn[best] + 0.1 && m < pick {
                    pick = m;
                }
            }
            pick
        }
        // A note whose 2nd harmonic dominates — a bright vowel or a belt with
        // F1 on H2, 16 dB over H1/H3 — repeats almost exactly at T/2, so its
        // first dip sits there. Above the band it read NaN (every such note
        // above ~605 Hz once the scan began below the band); in it, below
        // ~567 Hz, an octave high. It is only NEARLY periodic at T/2 (H1/H3
        // don't cancel there) and truly periodic at T: an in-band double
        // whose dip is clearly deeper, and whose fundamental is really in
        // the frame, is the period. An ordinary note has nothing at f/2, and
        // an above-band tone is periodic at every multiple, so it still
        // reads NaN.
        Some(tau) => deeper_in_band_multiple(frame, &dn, tau, tau_min, tau_max).unwrap_or(tau),
    };

    // 4) Parabolic interpolation around the chosen lag for sub-sample
    // precision, and the depth of d′ AT that interpolated vertex (a touch
    // below dn[tau]) — which is the more faithful periodicity/voicing
    // confidence.
    let vertex = yin_vertex(&dn, tau, tau_max);
    let better_tau = vertex.lag;
    let d_min = vertex.depth;

    let periodicity = clamp_f64(1.0 - d_min, 0.0, 1.0);
    let f0 = if better_tau > 0.0 {
        sample_rate / better_tau
    } else {
        f64::NAN
    };
    // A small tolerance keeps a tone sitting exactly on a band edge (whose
    // interpolated lag can land a hair outside) from being dropped, while a
    // genuinely out-of-band pitch (e.g. a sub-fmin bass note) still returns
    // NaN.
    if f0 < fmin * 0.97 || f0 > fmax * 1.03 {
        return YinEstimate {
            f0: f64::NAN,
            periodicity,
        };
    }
    YinEstimate { f0, periodicity }
}

/// The parabolic vertex of d′ around an integer lag: the sub-sample lag and
/// the interpolated depth there.
#[derive(Clone, Copy, Debug)]
struct Vertex {
    lag: f64,
    depth: f64,
}

/// The parabolic vertex of d′ around integer lag [tau]. Only a genuine
/// (convex) minimum is interpolated; otherwise the integer lag and its own
/// depth.
fn yin_vertex(dn: &[f64], tau: usize, tau_max: usize) -> Vertex {
    if tau > 1 && tau < tau_max {
        let (s0, s1, s2) = (dn[tau - 1], dn[tau], dn[tau + 1]);
        let denom = s0 - 2.0 * s1 + s2;
        if denom > 1e-12 {
            return Vertex {
                lag: tau as f64 + 0.5 * (s0 - s2) / denom,
                depth: s1 - (s0 - s2) * (s0 - s2) / (8.0 * denom),
            };
        }
    }
    Vertex {
        lag: tau as f64,
        depth: dn[tau],
    }
}

/// How much deeper (absolute d′) the in-band multiple's dip must be than an
/// above-band first dip to be taken as the true period. A note whose 2nd
/// harmonic sits 16 dB over H1/H3 dips ~0.1 at T/2 and ~0 at T.
const MULTIPLE_MARGIN: f64 = 0.02;

/// The weakest fundamental, relative to the frame's overall level, that a
/// rescued period may claim (−35 dB). A note whose H2 dominates still has a
/// real H1 at that pitch (−16 dB in the case this exists for); an above-band
/// tone has nothing there but noise and window leakage (< −60 dB) — the
/// check that keeps a bright high tone, whose integer-lag misalignment can
/// leave its 2T dip deeper than its T dip, from reading an octave low.
const MIN_FUNDAMENTAL_RATIO: f64 = 0.0178;

/// The integer lag of the in-band double 2τ of the above-band dip at [tau]
/// when that is the period (see [`yin_pitch_with`] step 3), or `None`. Only
/// the double is considered: a 3rd-harmonic-dominated note stays NaN, as
/// before, rather than widen what a misaligned high tone can be mistaken
/// for.
fn deeper_in_band_multiple(
    frame: &[f32],
    dn: &[f64],
    tau: usize,
    tau_min: usize,
    tau_max: usize,
) -> Option<usize> {
    let first = yin_vertex(dn, tau, tau_max);
    let target = round_to_i64(first.lag * 2.0);
    if target < tau_min as i64 || target >= tau_max as i64 {
        return None;
    }
    let target = target as usize;
    // The double's own local minimum, within a lag of the target.
    let mut m = target;
    if dn[target - 1] < dn[m] {
        m = target - 1;
    }
    if dn[target + 1] < dn[m] {
        m = target + 1;
    }
    if m < tau_min {
        return None;
    }
    let twice = yin_vertex(dn, m, tau_max);
    if twice.depth >= 0.5 * first.depth || first.depth - twice.depth < MULTIPLE_MARGIN {
        return None;
    }
    (fundamental_ratio(frame, 1.0 / twice.lag) >= MIN_FUNDAMENTAL_RATIO).then_some(m)
}

/// The amplitude of the sinusoid at normalized frequency [cycles] (cycles
/// per sample) in [frame], relative to the frame's overall level — both
/// Hann-windowed, so a component of amplitude a among components a₁…aₖ
/// reads ≈ a / √(Σaᵢ²). One direct DFT term: O(n), on the rare rescue path
/// only.
fn fundamental_ratio(frame: &[f32], cycles: f64) -> f64 {
    let n = frame.len();
    let (mut re, mut im, mut energy, mut w_sum) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (i, &v) in frame.iter().enumerate() {
        let s = f64::from(v);
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
        let x = s * w;
        let ph = 2.0 * PI * cycles * i as f64;
        re += x * ph.cos();
        im += x * ph.sin();
        energy += s * s * w;
        w_sum += w;
    }
    if energy <= 0.0 || w_sum <= 0.0 {
        return 0.0;
    }
    let amplitude = 2.0 * (re * re + im * im).sqrt() / w_sum;
    amplitude / (2.0 * energy / w_sum).sqrt()
}
