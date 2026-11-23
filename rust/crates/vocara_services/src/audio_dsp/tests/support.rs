//! What the ported tests share: Dart's `Random` (so every noisy fixture is
//! the Flutter test's own signal), the Dart tests' signal generators, a
//! line-for-line port of fftea's radix-2 FFT and STFT as the equality
//! oracle, and bitwise comparisons.

use std::f64::consts::PI;

use vocara_model::util::dart_num::{clamp_f64, compare_f64, pow_f64, round_to_i64};

/// The Dart tests' rate (`_sr`).
pub const SR: i64 = 44100;

/// The Dart VM's `Random(seed)`: a multiply-with-carry generator over a
/// Thomas Wang-mixed seed, cranked four times (sdk/lib/_internal/vm/lib/
/// math_patch.dart).
pub struct DartRandom {
    state: u64,
}

impl DartRandom {
    pub fn new(seed: i64) -> Self {
        let mut n = seed as u64;
        n = (!n).wrapping_add(n << 21);
        n ^= n >> 24;
        n = n.wrapping_mul(265);
        n ^= n >> 14;
        n = n.wrapping_mul(21);
        n ^= n >> 28;
        n = n.wrapping_add(n << 31);
        if n == 0 {
            n = 0x5a17;
        }
        let mut r = DartRandom { state: n };
        for _ in 0..4 {
            r.next_state();
        }
        r
    }

    fn next_state(&mut self) {
        const A: u64 = 0xffff_da61;
        let lo = self.state & 0xFFFF_FFFF;
        let hi = self.state >> 32;
        self.state = A.wrapping_mul(lo).wrapping_add(hi);
    }

    pub fn next_int(&mut self, max: u32) -> u32 {
        assert!(max > 0);
        if max.is_power_of_two() {
            self.next_state();
            return (self.state & 0xFFFF_FFFF) as u32 & (max - 1);
        }
        loop {
            self.next_state();
            let rnd32 = self.state & 0xFFFF_FFFF;
            let result = rnd32 % u64::from(max);
            if rnd32 - result + u64::from(max) <= 1 << 32 {
                return result as u32;
            }
        }
    }

    pub fn next_double(&mut self) -> f64 {
        let hi = f64::from(self.next_int(1 << 26));
        let lo = f64::from(self.next_int(1 << 27));
        (hi * (1u64 << 27) as f64 + lo) / (1u64 << 53) as f64
    }

    pub fn next_bool(&mut self) -> bool {
        self.next_int(2) == 0
    }
}

/// Bitwise f32 equality — NaN-safe by bits (the streaming/batch contract is
/// BIT-identical output, NaNs included).
pub fn assert_bits_eq(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what} length");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "{what}[{i}] bits ({x} vs {y})");
    }
}

/// Bitwise f64 equality.
pub fn assert_bits_eq64(a: &[f64], b: &[f64], what: &str) {
    assert_eq!(a.len(), b.len(), "{what} length");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "{what}[{i}] bits ({x} vs {y})");
    }
}

/// Dart's `expect(a, b)` on two doubles, where two NaNs also match.
pub fn assert_scalar_eq(a: f64, b: f64, what: &str) {
    if a.is_nan() && b.is_nan() {
        return;
    }
    assert_eq!(a, b, "{what}");
}

/// `closeTo(want, tol)`.
#[track_caller]
pub fn assert_close(got: f64, want: f64, tol: f64, what: &str) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: {got} is not within {tol} of {want}"
    );
}

/// Dart's `List<double>.sort()`.
pub fn sort_dart(v: &mut [f64]) {
    v.sort_by(|a, b| compare_f64(*a, *b));
}

/// The non-NaN values of [xs], sorted.
pub fn finite_sorted(xs: &[f32]) -> Vec<f64> {
    let mut v: Vec<f64> = xs
        .iter()
        .filter(|x| !x.is_nan())
        .map(|&x| f64::from(x))
        .collect();
    sort_dart(&mut v);
    v
}

/// The median of the non-NaN values (NaN when there are none).
pub fn median(xs: &[f32]) -> f64 {
    let v = finite_sorted(xs);
    if v.is_empty() {
        f64::NAN
    } else {
        v[v.len() / 2]
    }
}

/// Synthesizes a vowel at [SR]: an impulse train at [f0] driven through
/// three two-pole resonators at the given formant frequencies — a textbook
/// source-filter vowel whose spectral envelope peaks at F1/F2/F3.
pub fn vowel(f1: f64, f2: f64, f3: f64, f0: f64, sec: f64) -> Vec<f32> {
    let sr = SR as f64;
    let n = round_to_i64(sr * sec) as usize;
    let mut src = vec![0.0f32; n];
    let period = round_to_i64(sr / f0) as usize;
    let mut i = 0;
    while i < n {
        src[i] = 1.0;
        i += period;
    }
    // Approximate a glottal source roll-off (~ -12 dB/oct) so the
    // pre-emphasis in the formant estimator behaves as it does on real voiced
    // audio.
    let (mut g1, mut g2) = (0.0f64, 0.0f64);
    for s in &mut src {
        g1 = f64::from(*s) + 0.99 * g1;
        g2 = g1 + 0.99 * g2;
        *s = (g2 * 1e-3) as f32;
    }
    let reson = |fc: f64, bw: f64, gain: f64| -> Vec<f32> {
        let r = (-PI * bw / sr).exp();
        let theta = 2.0 * PI * fc / sr;
        let a1 = -2.0 * r * theta.cos();
        let a2 = r * r;
        let mut out = vec![0.0f32; n];
        for i in 0..n {
            let y1 = if i >= 1 { f64::from(out[i - 1]) } else { 0.0 };
            let y2 = if i >= 2 { f64::from(out[i - 2]) } else { 0.0 };
            out[i] = (gain * f64::from(src[i]) - a1 * y1 - a2 * y2) as f32;
        }
        out
    };
    let o1 = reson(f1, 70.0, 1.0);
    let o2 = reson(f2, 90.0, 0.6);
    let o3 = reson(f3, 120.0, 0.4);
    let mut out = vec![0.0f32; n];
    let mut peak = 1e-9f64;
    for i in 0..n {
        out[i] = (f64::from(o1[i]) + f64::from(o2[i]) + f64::from(o3[i])) as f32;
        if f64::from(out[i]).abs() > peak {
            peak = f64::from(out[i]).abs();
        }
    }
    normalize(&mut out, peak);
    out
}

/// `out[i] /= peak * 1.1`, through f64 as Dart's `Float32List` did.
fn normalize(out: &mut [f32], peak: f64) {
    for v in out {
        *v = (f64::from(*v) / (peak * 1.1)) as f32;
    }
}

/// [`vowel`] at Dart's defaults (130 Hz, one second).
pub fn vowel_at(f1: f64, f2: f64, f3: f64, f0: f64) -> Vec<f32> {
    vowel(f1, f2, f3, f0, 1.0)
}

/// Sum of equal-amplitude sine tones — a controlled way to put energy in
/// specific bands for the ring/tilt metrics.
pub fn tones(freqs: &[f64], sec: f64) -> Vec<f32> {
    let n = round_to_i64(SR as f64 * sec) as usize;
    let mut out = vec![0.0f32; n];
    for &f in freqs {
        for (i, v) in out.iter_mut().enumerate() {
            *v = (f64::from(*v) + (2.0 * PI * f * i as f64 / SR as f64).sin()) as f32;
        }
    }
    let mut peak = 1e-9f64;
    for &v in &out {
        if f64::from(v).abs() > peak {
            peak = f64::from(v).abs();
        }
    }
    normalize(&mut out, peak);
    out
}

/// A pure sine at [hz] sampled at [sr], length [n].
pub fn sine(hz: f64, sr: f64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (2.0 * PI * hz * i as f64 / sr).sin() as f32)
        .collect()
}

/// A harmonic tone at [f0]: H1..H[harmonics] with amplitude 1/hᵖ (p =
/// [rolloff]) and fixed phases — or, with [h2_over_db], a note whose 2nd
/// harmonic sits that many dB over H1 and H3 (a bright high vowel).
pub fn harmonic(
    f0: f64,
    sr: f64,
    n: usize,
    harmonics: u32,
    rolloff: f64,
    h2_over_db: Option<f64>,
) -> Vec<f32> {
    let mut out = vec![0.0f32; n];
    let side = h2_over_db.map_or(0.0, |db| pow_f64(10.0, -db / 20.0));
    let top = if h2_over_db.is_none() { harmonics } else { 3 };
    for h in 1..=top {
        let hf = f64::from(h);
        if hf * f0 >= sr / 2.0 {
            break;
        }
        let a = match h2_over_db {
            None => 1.0 / pow_f64(hf, rolloff),
            Some(_) => {
                if h == 2 {
                    1.0
                } else {
                    side
                }
            }
        };
        for (i, v) in out.iter_mut().enumerate() {
            let s = (2.0 * PI * hf * f0 * i as f64 / sr + 0.7 * hf).sin();
            *v = (f64::from(*v) + 0.4 * a * s) as f32;
        }
    }
    out
}

/// A note whose 2nd harmonic sits [db] over H1 and H3.
pub fn h2_bright(f0: f64, sr: f64, n: usize, db: f64) -> Vec<f32> {
    harmonic(f0, sr, n, 6, 1.0, Some(db))
}

/// White noise in ±0.5 from Dart's `Random(seed)`.
pub fn noise(sec: f64, seed: i64) -> Vec<f32> {
    let n = round_to_i64(SR as f64 * sec) as usize;
    let mut rnd = DartRandom::new(seed);
    (0..n)
        .map(|_| ((rnd.next_double() * 2.0 - 1.0) * 0.5) as f32)
        .collect()
}

/// A sustained tone (with harmonics) whose pitch oscillates sinusoidally — a
/// synthetic vibrato at [rate_hz] / ±[extent_cents] around [f0].
pub fn vibrato_tone(f0: f64, rate_hz: f64, extent_cents: f64, sec: f64, sr: i64) -> Vec<f32> {
    let srf = sr as f64;
    let n = round_to_i64(srf * sec) as usize;
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / srf;
            let ratio = pow_f64(
                2.0,
                (extent_cents / 1200.0) * (2.0 * PI * rate_hz * t).sin(),
            );
            phase += 2.0 * PI * f0 * ratio / srf;
            (0.6 * phase.sin() + 0.3 * (2.0 * phase).sin() + 0.15 * (3.0 * phase).sin()) as f32
        })
        .collect()
}

/// A sung vowel at [f0]: a band-limited harmonic source at -12 dB/octave
/// through one resonator per formant (80 Hz bandwidth), normalized to [amp]
/// peak.
pub fn sung_vowel(f0: f64, formants: &[f64], sec: f64, amp: f64) -> Vec<f32> {
    let sr = SR as f64;
    let n = round_to_i64(sr * sec) as usize;
    let mut x = vec![0.0f64; n];
    let mut phase = 0.0f64;
    for v in &mut x {
        phase += 2.0 * PI * f0 / sr;
        let mut s = 0.0;
        let mut k = 1i64;
        while k as f64 * f0 < sr / 2.0 * 0.95 {
            s += (k as f64 * phase).sin() / (k * k) as f64;
            k += 1;
        }
        *v = s;
    }
    for &fc in formants {
        let r = (-PI * 80.0 / sr).exp();
        let (a1, a2) = (-2.0 * r * (2.0 * PI * fc / sr).cos(), r * r);
        let mut out = vec![0.0f64; n];
        for i in 0..n {
            out[i] = x[i]
                - a1 * if i >= 1 { out[i - 1] } else { 0.0 }
                - a2 * if i >= 2 { out[i - 2] } else { 0.0 };
        }
        x = out;
    }
    let mut peak = 0.0f64;
    for &v in &x {
        peak = peak.max(v.abs());
    }
    x.iter().map(|&v| (v / peak * amp) as f32).collect()
}

/// yin_octave_test's source-filter vowel: glottal pulse train (with cycle
/// jitter and vibrato) through F1/F2/F3 resonators, plus aspiration noise
/// that is high-passed (breath) at [hnr_db] below the voiced part.
pub fn breathy_vowel(sr: i64, hnr_db: f64, high_pass_noise: bool, seed: i64) -> Vec<f32> {
    let (f0, sec, jitter, vib_cents, vib_hz) = (220.0, 1.5, 0.01, 40.0, 5.5);
    let srf = sr as f64;
    let mut rng = DartRandom::new(seed);
    let n = round_to_i64(srf * sec) as usize;
    let mut src = vec![0.0f32; n];
    let mut t = 0.0f64;
    while t < n as f64 {
        src[t.floor() as usize] = 1.0;
        let inst = f0
            * pow_f64(
                2.0,
                vib_cents / 1200.0 * (2.0 * PI * vib_hz * t / srf).sin(),
            );
        t += srf / inst * (1.0 + jitter * (rng.next_double() * 2.0 - 1.0));
    }
    let (mut g1, mut g2) = (0.0f64, 0.0f64);
    for s in &mut src {
        g1 = f64::from(*s) + 0.99 * g1;
        g2 = g1 + 0.99 * g2;
        *s = (g2 * 1e-3) as f32;
    }
    let reson = |x: &[f32], fc: f64, bw: f64, gain: f64| -> Vec<f32> {
        let r = (-PI * bw / srf).exp();
        let th = 2.0 * PI * fc / srf;
        let (a1, a2) = (-2.0 * r * th.cos(), r * r);
        let mut out = vec![0.0f32; n];
        for i in 0..n {
            let y1 = if i >= 1 { f64::from(out[i - 1]) } else { 0.0 };
            let y2 = if i >= 2 { f64::from(out[i - 2]) } else { 0.0 };
            out[i] = (gain * f64::from(x[i]) - a1 * y1 - a2 * y2) as f32;
        }
        out
    };
    let o1 = reson(&src, 700.0, 70.0, 1.0);
    let o2 = reson(&src, 1200.0, 90.0, 0.6);
    let o3 = reson(&src, 2600.0, 120.0, 0.4);
    let v: Vec<f32> = (0..n)
        .map(|i| (f64::from(o1[i]) + f64::from(o2[i]) + f64::from(o3[i])) as f32)
        .collect();
    let mut pv = 0.0f64;
    for &x in &v {
        pv += f64::from(x) * f64::from(x);
    }
    pv /= n as f64;
    let mut nz = vec![0.0f32; n];
    let mut prev = 0.0f64;
    for z in &mut nz {
        let u1 = clamp_f64(rng.next_double(), 1e-12, 1.0);
        let u2 = rng.next_double();
        let g = (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos();
        *z = (if high_pass_noise { g - prev } else { g }) as f32;
        prev = g;
    }
    let mut pn = 0.0f64;
    for &x in &nz {
        pn += f64::from(x) * f64::from(x);
    }
    pn /= n as f64;
    let k = (pv / pn / pow_f64(10.0, hnr_db / 10.0)).sqrt();
    let mut out = vec![0.0f32; n];
    let mut peak = 1e-9f64;
    for i in 0..n {
        out[i] = (f64::from(v[i]) + k * f64::from(nz[i])) as f32;
        peak = peak.max(f64::from(out[i]).abs());
    }
    for o in &mut out {
        *o = (f64::from(*o) / peak * 0.5) as f32;
    }
    out
}

/// yin_octave_test's `_h2Dominant`: H2 [db] over H1 and H3, no Nyquist
/// guard.
pub fn h2_dominant(f0: f64, sr: f64, n: usize, db: f64) -> Vec<f32> {
    let mut out = vec![0.0f32; n];
    let side = pow_f64(10.0, -db / 20.0);
    for h in 1..=3u32 {
        let hf = f64::from(h);
        let a = if h == 2 { 1.0 } else { side };
        for (i, v) in out.iter_mut().enumerate() {
            let s = (2.0 * PI * hf * f0 * i as f64 / sr + 0.7 * hf).sin();
            *v = (f64::from(*v) + 0.4 * a * s) as f32;
        }
    }
    out
}

/// fftea 1.5's `Radix2FFT` as written (twiddles, 32-bit bit reversal by
/// swaps, the main loop with its interleaved (re, im) arithmetic), applied
/// to `realFft`'s (x, 0) input: the equality oracle for the analysis FFT.
pub fn fftea_real_fft(x: &[f64]) -> Vec<(f64, f64)> {
    let n = x.len();
    assert!(n.is_power_of_two() && n >= 8);
    let bits = n.trailing_zeros();
    // _calculateTwiddles.
    let half = n >> 1;
    let mut tw = vec![(0.0f64, 0.0f64); half];
    tw[0] = (1.0, 0.0);
    let step = 2.0 * PI / n as f64;
    let quarter = half >> 1;
    let eighth = quarter >> 1;
    for (i, w) in tw.iter_mut().enumerate().take(eighth).skip(1) {
        let theta = step * i as f64;
        *w = (theta.cos(), theta.sin());
    }
    tw[eighth] = (
        std::f64::consts::FRAC_1_SQRT_2,
        std::f64::consts::FRAC_1_SQRT_2,
    );
    for i in 1..eighth {
        let z = (-tw[eighth - i].0, -tw[eighth - i].1);
        tw[eighth + i] = (-z.1, -z.0);
    }
    tw[quarter] = (0.0, 1.0);
    for i in 1..quarter {
        let z = tw[quarter - i];
        tw[quarter + i] = (-z.0, z.1);
    }
    // realFft: (x, 0).
    let mut c: Vec<(f64, f64)> = x.iter().map(|&v| (v, 0.0)).collect();
    // Bit reverse permutation.
    let shift = 32 - bits;
    for i in 0..n {
        let mut j = i as u32;
        j = ((j >> 16) & 0x0000_FFFF) | ((j & 0x0000_FFFF) << 16);
        j = ((j >> 8) & 0x00FF_00FF) | ((j & 0x00FF_00FF) << 8);
        j = ((j >> 4) & 0x0F0F_0F0F) | ((j & 0x0F0F_0F0F) << 4);
        j = ((j >> 2) & 0x3333_3333) | ((j & 0x3333_3333) << 2);
        j = ((j >> 1) & 0x5555_5555) | ((j & 0x5555_5555) << 1);
        let j = (j >> shift) as usize;
        if j < i {
            c.swap(i, j);
        }
    }
    // FFT main loop.
    let n2 = n >> 1;
    for ms in 0..bits {
        let m = 1usize << ms;
        let nm = n2 >> ms;
        let (mut k, mut t) = (0usize, 0usize);
        while k < n {
            let km = k + m;
            let p = c[k];
            let o = c[km];
            let w = tw[t];
            // q = o.scale(w.x) + Float64x2(o.y, -o.x).scale(w.y)
            let q = (o.0 * w.0 + o.1 * w.1, o.1 * w.0 + (-o.0) * w.1);
            c[k] = (p.0 + q.0, p.1 + q.1);
            c[km] = (p.0 - q.0, p.1 - q.1);
            k += 1;
            t += nm;
            if t >= n2 {
                k += m;
                t = 0;
            }
        }
    }
    c
}

/// fftea's `Window.hanning`, as written.
pub fn fftea_hanning(size: usize) -> Vec<f64> {
    let mut a = vec![0.0; size];
    if size == 1 {
        a[0] = 1.0;
        return a;
    }
    let scale = 2.0 * PI / (size - 1) as f64;
    let half = size >> 1;
    let n = size - 1;
    for (i, w) in a.iter_mut().enumerate().take(half + 1) {
        *w = 0.5 - 0.5 * (scale * i as f64).cos();
    }
    for i in 0..half {
        a[n - i] = a[i];
    }
    a
}

/// |X[k]| of a real frame: fftea's radix-2 for powers of two, a plain
/// `rustfft` transform for any other size (where Dart's oracle was fftea's
/// own mixed-radix FFT).
pub fn reference_magnitudes(frame: &[f64]) -> Vec<f64> {
    let n = frame.len();
    if n.is_power_of_two() && n >= 8 {
        return fftea_real_fft(frame)
            .iter()
            .map(|&(re, im)| (re * re + im * im).sqrt())
            .collect();
    }
    use rustfft::num_complex::Complex;
    let mut buf: Vec<Complex<f64>> = frame.iter().map(|&v| Complex::new(v, 0.0)).collect();
    rustfft::FftPlanner::new()
        .plan_fft_forward(n)
        .process(&mut buf);
    buf.iter()
        .map(|c| (c.re * c.re + c.im * c.im).sqrt())
        .collect()
}

/// What `computeSpectrogram` was: fftea's STFT (`run`) over the samples —
/// chunks every [hop], the last zero-padded, none for empty input — Hann
/// windowed, the first max_bin magnitudes of each frame in dB.
pub fn fftea_stft_columns(
    samples: &[f32],
    sr: i64,
    max_hz: f64,
    fft_size: usize,
    hop: usize,
) -> (Vec<f32>, usize, usize) {
    let max_bin =
        (fft_size / 2 + 1).min((max_hz * fft_size as f64 / sr as f64).ceil() as usize + 1);
    let window = fftea_hanning(fft_size);
    let mut out = Vec::new();
    let mut frames = 0;
    if samples.is_empty() {
        return (out, 0, max_bin);
    }
    // STFT._run with chunkStride = hop.
    let n = fft_size;
    let overlap = n - hop;
    let mut chunk = vec![0.0f64; n];
    let mut i = 0usize;
    loop {
        let mut ci = 0usize;
        if i > 0 {
            let offset = i - overlap;
            for (c, &s) in chunk.iter_mut().zip(&samples[offset..offset + overlap]) {
                *c = f64::from(s);
            }
            ci = overlap;
        }
        let istop = (i + n - ci).min(samples.len());
        while i < istop {
            chunk[ci] = f64::from(samples[i]);
            i += 1;
            ci += 1;
        }
        chunk[ci..].fill(0.0);
        let frame: Vec<f64> = chunk.iter().zip(&window).map(|(&x, &w)| x * w).collect();
        let mags = reference_magnitudes(&frame);
        for &m in &mags[..max_bin] {
            out.push((20.0 * ((m + 1e-9).ln() / std::f64::consts::LN_10)) as f32);
        }
        frames += 1;
        if i >= samples.len() {
            break;
        }
    }
    (out, frames, max_bin)
}

#[test]
fn dart_random_matches_the_dart_vm() {
    // Printed by the Dart VM (3.10) for these seeds.
    let mut r = DartRandom::new(7);
    assert_eq!(r.next_double(), 0.11040538936331912);
    assert_eq!(r.next_double(), 0.7314634084349075);
    assert_eq!(r.next_double(), 0.4925330073374814);
    let mut q = DartRandom::new(31);
    let ints: Vec<u32> = (0..6).map(|_| q.next_int(10)).collect();
    assert_eq!(ints, [6, 9, 6, 3, 8, 5]);
    assert!(!q.next_bool());
    assert!(!q.next_bool());
    assert!(!q.next_bool());
    assert_eq!(q.next_int(1000), 645);
    assert_eq!(q.next_int(2046), 1676);
    assert_eq!(q.next_int(1 << 20), 60847);
    let mut z = DartRandom::new(0);
    assert_eq!(z.next_double(), 0.8255140718871702);
    assert_eq!(z.next_int(7), 6);
    let mut neg = DartRandom::new(-5);
    assert_eq!(neg.next_double(), 0.7235720321336252);
    assert_eq!(neg.next_int(100), 92);
}
