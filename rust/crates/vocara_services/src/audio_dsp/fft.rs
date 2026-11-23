//! The FFT of a real frame and the analysis windows, as the Dart code took
//! them through fftea.

use std::f64::consts::{FRAC_1_SQRT_2, PI};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// The FFT of a real frame of one fixed size, as every analysis here takes
/// it: magnitudes for the spectrum, real parts for the cepstrum. A power of
/// two of 16 or more (every production size) is [`Radix2RealFft`], fftea's
/// own radix-2 arithmetic; any other size is `rustfft`, where Dart used
/// fftea's general FFT. Both are the unnormalized forward transform.
pub(crate) enum RealFft {
    Radix2(Radix2RealFft),
    General(GeneralRealFft),
}

impl RealFft {
    pub(crate) fn new(size: usize) -> Self {
        if size >= 16 && size.is_power_of_two() {
            RealFft::Radix2(Radix2RealFft::new(size))
        } else {
            RealFft::General(GeneralRealFft::new(size))
        }
    }

    /// Transforms [x] (the FFT's size long), replacing the previous result.
    pub(crate) fn transform(&mut self, x: &[f64]) {
        match self {
            RealFft::Radix2(f) => f.transform(x),
            RealFft::General(f) => f.transform(x),
        }
    }

    /// |X[k]| for k in `[0, count)` into [out] — fftea's `magnitudes()`.
    pub(crate) fn magnitudes_into(&self, out: &mut [f64], count: usize) {
        let (re, im) = self.result();
        for k in 0..count {
            // fftea: sqrt(x·x + y·y) per value.
            out[k] = (re[k] * re[k] + im[k] * im[k]).sqrt();
        }
    }

    /// Re X[k] for k in `[from, to)` into [out] at `k - from`.
    pub(crate) fn real_parts_into(&self, out: &mut [f64], from: usize, to: usize) {
        let (re, _) = self.result();
        out[..to - from].copy_from_slice(&re[from..to]);
    }

    fn result(&self) -> (&[f64], &[f64]) {
        match self {
            RealFft::Radix2(f) => (&f.re, &f.im),
            RealFft::General(f) => (&f.re, &f.im),
        }
    }
}

/// fftea's `FFT(size).realFft` for a power-of-two size ≥ 16 (its
/// `Radix2FFT`), bit for bit.
///
/// fftea keeps each value as an interleaved (re, im) vector and multiplies
/// by a twiddle by swapping and negating lanes; this keeps the real and the
/// imaginary parts apart and runs the SAME multiplies and adds in the same
/// order: q = o·w̄ as (o.re·w.re + o.im·w.im, o.im·w.re − o.re·w.im), where
/// fftea adds the product with the negated o.re (IEEE subtraction is exactly
/// that), then p ± q, over fftea's twiddles built fftea's way. The Dart port
/// also scheduled two stages per pass over Float64x2 pairs for speed; that
/// reorders only which butterfly runs when, never an operation within one,
/// so the plain stage loop here gives the same bits. Pinned equal to a
/// line-for-line port of fftea by test.
pub(crate) struct Radix2RealFft {
    size: usize,
    /// The input sample fftea's bit-reversal permutation moves to position k.
    rev: Vec<u32>,
    /// fftea's twiddle for the j-th butterfly of the stage of half-span m,
    /// laid out flat at `[m, 2m)` so a stage's twiddles are contiguous.
    tw_re: Vec<f64>,
    tw_im: Vec<f64>,
    /// The result: Re/Im of X[k].
    re: Vec<f64>,
    im: Vec<f64>,
}

impl Radix2RealFft {
    pub(crate) fn new(size: usize) -> Self {
        assert!(
            size >= 16 && size.is_power_of_two(),
            "size {size} is not a power of two ≥ 16"
        );
        let bits = size.trailing_zeros();
        let rev = (0..size as u32)
            .map(|k| k.reverse_bits() >> (32 - bits))
            .collect();
        let half = size >> 1;
        let (tw_x, tw_y) = fftea_twiddles(size);
        // fftea's stage of half-span m reads tw[j·(size/2)/m] for its j-th
        // butterfly.
        let mut tw_re = vec![0.0; size];
        let mut tw_im = vec![0.0; size];
        let mut m = 1;
        while m < size {
            let stride = half / m;
            for j in 0..m {
                tw_re[m + j] = tw_x[j * stride];
                tw_im[m + j] = tw_y[j * stride];
            }
            m <<= 1;
        }
        Radix2RealFft {
            size,
            rev,
            tw_re,
            tw_im,
            re: vec![0.0; size],
            im: vec![0.0; size],
        }
    }

    pub(crate) fn transform(&mut self, x: &[f64]) {
        let n = self.size;
        let (re, im) = (&mut self.re, &mut self.im);
        // fftea's realFft input is (x, +0), permuted by bit reversal.
        for k in 0..n {
            re[k] = x[self.rev[k] as usize];
            im[k] = 0.0;
        }
        let (tw_re, tw_im) = (&self.tw_re, &self.tw_im);
        let mut m = 1;
        while m < n {
            let mut k0 = 0;
            while k0 < n {
                for j in 0..m {
                    let k = k0 + j;
                    let km = k + m;
                    let (wr, wi) = (tw_re[m + j], tw_im[m + j]);
                    let (pr, pi, or, oi) = (re[k], im[k], re[km], im[km]);
                    let qr = or * wr + oi * wi;
                    let qi = oi * wr - or * wi;
                    re[k] = pr + qr;
                    im[k] = pi + qi;
                    re[km] = pr - qr;
                    im[km] = pi - qi;
                }
                k0 += 2 * m;
            }
            m <<= 1;
        }
    }

    /// The spectrum of the last transform, interleaved like fftea's
    /// `realFft` result (re, im per bin).
    #[cfg(test)]
    pub(crate) fn spectrum(&self) -> Vec<(f64, f64)> {
        self.re
            .iter()
            .copied()
            .zip(self.im.iter().copied())
            .collect()
    }
}

/// fftea 1.5's Radix2FFT twiddles (cos θ, sin θ for θ = 2πi/size, i below
/// size/2), computed exactly as it computes them: by symmetry from the first
/// eighth, with the eighth and quarter points exact.
pub(crate) fn fftea_twiddles(size: usize) -> (Vec<f64>, Vec<f64>) {
    let half = size >> 1;
    let mut x = vec![0.0; half];
    let mut y = vec![0.0; half];
    x[0] = 1.0;
    y[0] = 0.0;
    let step = 2.0 * PI / size as f64;
    let quarter = half >> 1;
    let eighth = quarter >> 1;
    for i in 1..eighth {
        let theta = step * i as f64;
        x[i] = theta.cos();
        y[i] = theta.sin();
    }
    x[eighth] = FRAC_1_SQRT_2;
    y[eighth] = FRAC_1_SQRT_2;
    for i in 1..eighth {
        // z = -tw[eighth - i]; tw[eighth + i] = (-z.y, -z.x).
        let (zx, zy) = (-x[eighth - i], -y[eighth - i]);
        x[eighth + i] = -zy;
        y[eighth + i] = -zx;
    }
    x[quarter] = 0.0;
    y[quarter] = 1.0;
    for i in 1..quarter {
        let (zx, zy) = (x[quarter - i], y[quarter - i]);
        x[quarter + i] = -zx;
        y[quarter + i] = zy;
    }
    (x, y)
}

/// [`RealFft`] by `rustfft`, for the sizes [`Radix2RealFft`] does not take
/// (Dart: fftea's `FFT(size).realFft`). Forward and unnormalized, like
/// fftea; the last bits may differ from fftea's mixed-radix arithmetic, a
/// path no production size takes.
pub(crate) struct GeneralRealFft {
    fft: Arc<dyn Fft<f64>>,
    buf: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    re: Vec<f64>,
    im: Vec<f64>,
}

impl GeneralRealFft {
    fn new(size: usize) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(size);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        GeneralRealFft {
            fft,
            buf: vec![Complex::new(0.0, 0.0); size],
            scratch,
            re: vec![0.0; size],
            im: vec![0.0; size],
        }
    }

    fn transform(&mut self, x: &[f64]) {
        for (c, &v) in self.buf.iter_mut().zip(x) {
            *c = Complex::new(v, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        for (k, c) in self.buf.iter().enumerate() {
            self.re[k] = c.re;
            self.im[k] = c.im;
        }
    }
}

/// fftea's `Window.hanning(size)`: `0.5 − 0.5·cos(2πi / (size − 1))`,
/// computed for the first half and mirrored onto the second (so the window
/// is exactly symmetric), `[1]` for a single sample.
pub(crate) fn hanning(size: usize) -> Vec<f64> {
    let mut a = vec![0.0; size];
    if size == 0 {
        return a;
    }
    if size == 1 {
        a[0] = 1.0;
        return a;
    }
    // fftea's `cosine(size, 0.5)`.
    let amplitude = 0.5;
    let offset = 1.0 - amplitude;
    let scale = 2.0 * PI / (size - 1) as f64;
    let half = size >> 1;
    let n = size - 1;
    for (i, w) in a.iter_mut().enumerate().take(half + 1) {
        *w = offset - amplitude * (scale * i as f64).cos();
    }
    for i in 0..half {
        a[n - i] = a[i];
    }
    a
}

/// A Hamming window of length [n] (Dart's own, not fftea's: every sample is
/// computed, none mirrored).
pub(crate) fn hamming(n: usize) -> Vec<f64> {
    let mut w = vec![0.0; n];
    if n <= 1 {
        if n == 1 {
            w[0] = 1.0;
        }
        return w;
    }
    for (i, v) in w.iter_mut().enumerate() {
        *v = 0.54 - 0.46 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
    }
    w
}
