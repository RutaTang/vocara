//! F17 formant estimation (LPC) and the H1−H2 harmonic peak reader.

use std::f64::consts::PI;

use vocara_model::util::dart_num::{compare_f64, round_to_i64};

use super::pad_slice;

/// Levinson-Durbin recursion: LPC coefficients `[1, a1..ap]` for the
/// all-pole model A(z)=1+Σ a_k·z⁻ᵏ, or `None` if the frame is degenerate.
fn levinson(r: &[f64], order: usize) -> Option<Vec<f64>> {
    let mut a = vec![0.0; order + 1];
    let mut prev = vec![0.0; order + 1];
    a[0] = 1.0;
    let mut err = r[0];
    if err <= 0.0 {
        return None;
    }
    for i in 1..=order {
        let mut acc = r[i];
        for j in 1..i {
            acc += a[j] * r[i - j];
        }
        let k = -acc / err;
        prev[..i].copy_from_slice(&a[..i]); // the update below reads prev[1, i)
        for j in 1..i {
            a[j] = prev[j] + k * prev[i - j];
        }
        a[i] = k;
        err *= 1.0 - k * k;
        if err <= 0.0 {
            return None;
        }
    }
    Some(a)
}

/// Durand-Kerner: all complex roots of the monic polynomial with descending
/// coefficients [coeffs] (`coeffs[0] == 1`). Returns (re, im).
fn durand_kerner(coeffs: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let p = coeffs.len() - 1;
    let mut re = vec![0.0; p];
    let mut im = vec![0.0; p];
    // Initial guesses equally spaced on a circle (offset off the real axis) —
    // spreads the roots so coincident-guess stalls are unlikely.
    for k in 0..p {
        let ang = 2.0 * PI * (k as f64 + 0.5) / p as f64;
        re[k] = 0.9 * ang.cos();
        im[k] = 0.9 * ang.sin();
    }
    for _ in 0..60 {
        let mut max_step = 0.0f64;
        for k in 0..p {
            let (zr, zi) = (re[k], im[k]);
            // P(z_k) via Horner, and denom = Π_{j≠k}(z_k - z_j), in one loop;
            // each still runs its steps in its own order.
            let (mut pr, mut pi) = (coeffs[0], 0.0f64);
            let (mut dr, mut di) = (1.0f64, 0.0f64);
            for t in 0..p {
                let nr = pr * zr - pi * zi + coeffs[t + 1];
                let ni = pr * zi + pi * zr;
                pr = nr;
                pi = ni;
                if t != k {
                    let (xr, xi) = (zr - re[t], zi - im[t]);
                    let mr = dr * xr - di * xi;
                    let mi = dr * xi + di * xr;
                    dr = mr;
                    di = mi;
                }
            }
            let dd = dr * dr + di * di;
            if dd < 1e-30 {
                // Near-coincident roots stall this update — nudge and keep
                // iterating so a stuck root isn't mistaken for convergence.
                re[k] = zr + 1e-3 * (k + 1) as f64;
                im[k] = zi + 1e-3;
                max_step = 1.0;
                continue;
            }
            let qr = (pr * dr + pi * di) / dd;
            let qi = (pi * dr - pr * di) / dd;
            re[k] = zr - qr;
            im[k] = zi - qi;
            let step = qr * qr + qi * qi;
            if step > max_step {
                max_step = step;
            }
        }
        if max_step < 1e-18 {
            break;
        }
    }
    (re, im)
}

/// LPC formant candidates (sorted Hz) for the frame of [ds] (rate [ds_rate])
/// centered at sample [center]: pre-emphasis → Hamming → autocorrelation →
/// Levinson → polynomial roots → keep narrow-bandwidth in-range poles.
pub(crate) fn lpc_formants(
    ds: &[f32],
    ds_rate: f64,
    order: usize,
    win: usize,
    window: &[f64],
    pre_coef: f64,
    center: i64,
) -> Vec<f64> {
    if ds.len() < 2 {
        return Vec::new();
    }
    let start = center - (win / 2) as i64;
    lpc_formants_padded(
        &pad_slice(ds, start - 1, win + 1, ds.len()),
        ds_rate,
        order,
        win,
        window,
        pre_coef,
    )
}

/// [`lpc_formants`] on a pre-extracted window: [padded] holds `win + 1`
/// zero-padded samples whose FIRST value is the pre-emphasis lookback
/// (`ds[start - 1]`). Shared by the batch and streaming metrics paths.
pub(crate) fn lpc_formants_padded(
    padded: &[f32],
    ds_rate: f64,
    order: usize,
    win: usize,
    window: &[f64],
    pre_coef: f64,
) -> Vec<f64> {
    let mut frame = vec![0.0; win];
    let mut prev = f64::from(padded[0]);
    let mut energy = 0.0;
    for i in 0..win {
        let s = f64::from(padded[1 + i]);
        frame[i] = (s - pre_coef * prev) * window[i];
        prev = s;
        energy += frame[i] * frame[i];
    }
    if energy <= 1e-12 {
        return Vec::new();
    }

    // Autocorrelation r[lag] = Σ_{i ≥ lag} frame[i]·frame[i − lag], each lag
    // summed in i order (Dart ran four lags per sweep, each its own
    // accumulator in that same order: the same value, bit for bit).
    let mut r = vec![0.0; order + 1];
    for (lag, r) in r.iter_mut().enumerate() {
        let mut acc = 0.0;
        for i in lag..win {
            acc += frame[i] * frame[i - lag];
        }
        *r = acc;
    }
    if r[0] <= 0.0 {
        return Vec::new();
    }
    r[0] *= 1.0 + 1e-6; // ridge for numerical stability

    let Some(a) = levinson(&r, order) else {
        return Vec::new();
    };

    let (re, im) = durand_kerner(&a);
    let nyq = ds_rate / 2.0;
    let mut out = Vec::new();
    for k in 0..re.len() {
        if im[k] <= 0.0 {
            continue; // upper half-plane only (skip conjugates)
        }
        let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
        // Allow magnitudes a hair over 1 so sharp formants whose pole rounds
        // onto the unit circle aren't dropped; clearly-unstable poles are
        // still rejected.
        if mag <= 0.0 || mag > 1.001 {
            continue;
        }
        let freq = im[k].atan2(re[k]) * ds_rate / (2.0 * PI);
        if freq < 90.0 || freq > nyq - 100.0 {
            continue;
        }
        let bw = -ds_rate / PI * mag.ln();
        if bw > 400.0 {
            continue; // wide => not a formant
        }
        out.push(freq);
    }
    out.sort_by(|a, b| compare_f64(*a, *b));
    out
}

/// Peak magnitude within ±[half_width] bins of [center_bin] (finds the
/// actual harmonic peak despite coarse FFT resolution). A genuine harmonic's
/// energy is spread across neighbouring bins by the Hann window, so once the
/// peak bin is located its true inter-bin amplitude is recovered by
/// parabolic interpolation of the log-magnitude — otherwise H1/H2 are read
/// low and biased. Returns 0 if out of range.
pub(crate) fn harmonic_peak(mags: &[f64], center_bin: f64, half_width: i64) -> f64 {
    let c = round_to_i64(center_bin);
    let len = mags.len() as i64;
    let mut peak = 0.0;
    let mut peak_bin: i64 = -1;
    for b in c - half_width..=c + half_width {
        if b >= 0 && b < len && mags[b as usize] > peak {
            peak = mags[b as usize];
            peak_bin = b;
        }
    }
    if peak_bin <= 0 || peak_bin >= len - 1 {
        return peak;
    }
    let pb = peak_bin as usize;
    const EPS: f64 = 1e-12;
    let l = (mags[pb - 1] + EPS).ln();
    let m = (mags[pb] + EPS).ln();
    let r = (mags[pb + 1] + EPS).ln();
    let denom = l - 2.0 * m + r;
    if denom >= 0.0 {
        return peak; // not a convex peak — keep the raw bin value
    }
    let delta = 0.5 * (l - r) / denom;
    (m - 0.25 * (l - r) * delta).exp()
}

/// Assigns sorted formant candidates [cands] (Hz) jointly to (F1, F2, F3):
/// the lexicographically-smallest in-band, ordered, ≥150 Hz-separated
/// triple. Unlike the old greedy lowest-in-band pick, a candidate is only
/// labeled F1 if a valid F2/F3 exists above it — so a missing/spurious low
/// pole can't cascade into a mislabeled F2/F3. Falls back to the best F1+F2
/// pair, then F1 alone.
// The band tests stay spelled out: `c < lo || c > hi` lets a NaN candidate
// through where `!(lo..=hi).contains(&c)` would not, and Dart's did.
#[allow(clippy::manual_range_contains)]
pub(crate) fn assign_formants(cands: &[f64]) -> (f64, f64, f64) {
    const F1_LO: f64 = 200.0;
    const F1_HI: f64 = 1100.0;
    const F2_LO: f64 = 700.0;
    const F2_HI: f64 = 2700.0;
    const F3_LO: f64 = 1600.0;
    const F3_HI: f64 = 3800.0;
    const SEP: f64 = 150.0;
    let n = cands.len();
    // First full triple (cands sorted ascending ⇒ lexicographically smallest).
    for i in 0..n {
        let c1 = cands[i];
        if c1 < F1_LO || c1 > F1_HI {
            continue;
        }
        for j in i + 1..n {
            let c2 = cands[j];
            if c2 < F2_LO || c2 > F2_HI || c2 < c1 + SEP {
                continue;
            }
            for &c3 in &cands[j + 1..] {
                if c3 >= F3_LO && c3 <= F3_HI && c3 >= c2 + SEP {
                    return (c1, c2, c3);
                }
            }
        }
    }
    // Best F1+F2 pair.
    for i in 0..n {
        let c1 = cands[i];
        if c1 < F1_LO || c1 > F1_HI {
            continue;
        }
        for &c2 in &cands[i + 1..] {
            if c2 >= F2_LO && c2 <= F2_HI && c2 >= c1 + SEP {
                return (c1, c2, f64::NAN);
            }
        }
    }
    // F1 alone (returns on the first in-band F1 ⇒ only reached when an F1
    // exists).
    for &c1 in cands {
        if (F1_LO..=F1_HI).contains(&c1) {
            return (c1, f64::NAN, f64::NAN);
        }
    }
    // No in-band F1 at all: still report F2/F3 (a high voice, or a missed F1,
    // must not drop the upper formants — this matches the old greedy
    // behavior).
    for j in 0..n {
        let c2 = cands[j];
        if c2 < F2_LO || c2 > F2_HI {
            continue;
        }
        for &c3 in &cands[j + 1..] {
            if c3 >= F3_LO && c3 <= F3_HI && c3 >= c2 + SEP {
                return (f64::NAN, c2, c3);
            }
        }
    }
    for &c2 in cands {
        if (F2_LO..=F2_HI).contains(&c2) {
            return (f64::NAN, c2, f64::NAN);
        }
    }
    (f64::NAN, f64::NAN, f64::NAN)
}

/// A frame's formants back in order (F1 < F2 < F3), each missing one left
/// missing and the measured ones sorted into the measured slots. Each track
/// is median-smoothed on its own and their gaps differ, so at a vowel change
/// F2 could take a later frame's value and F3 an earlier one: F2 above F3,
/// the labels swapped on the overlay and on the spectrum's markers.
pub(crate) fn ordered_formants(f1: f64, f2: f64, f3: f64) -> (f64, f64, f64) {
    let mut v = [f1, f2, f3];
    let mut measured: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    measured.sort_by(|a, b| compare_f64(*a, *b));
    let mut k = 0;
    for x in &mut v {
        if x.is_finite() {
            *x = measured[k];
            k += 1;
        }
    }
    (v[0], v[1], v[2])
}
