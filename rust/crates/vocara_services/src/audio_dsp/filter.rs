//! Anti-aliased decimation: the 3-stage Butterworth cascade, batch and
//! streaming.

use std::borrow::Cow;
use std::f64::consts::{PI, SQRT_2};

use super::rolling::RollingF32;

/// One biquad's Direct Form I recursion, its state carried between calls:
/// the batch [`decimate`] runs it once over the whole take, the streaming
/// [`StreamDecimator`] block by block.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

/// A biquad's coefficients (b0, b1, b2, a1, a2), normalized by a0.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl BiquadState {
    /// Filters [x] in place: double state and intermediates, each output
    /// rounded to f32 only where it is stored. Direct Form I stays exact when
    /// writing over the input: each output needs x[i] plus the two PREVIOUS
    /// raw inputs, which live in the x1/x2 scalars — x[i] is captured before
    /// it is overwritten, so nothing downstream reads a filtered value as a
    /// raw one.
    pub(crate) fn run_in_place(&mut self, x: &mut [f32], c: Biquad) {
        let Biquad { b0, b1, b2, a1, a2 } = c;
        let (mut x1, mut x2, mut y1, mut y2) = (self.x1, self.x2, self.y1, self.y2);
        for v in x.iter_mut() {
            let xi = f64::from(*v);
            let yi = b0 * xi + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            *v = yi as f32;
            x2 = x1;
            x1 = xi;
            y2 = y1;
            y1 = yi;
        }
        self.x1 = x1;
        self.x2 = x2;
        self.y1 = y1;
        self.y2 = y2;
    }
}

/// One 2nd-order Butterworth low-pass (RBJ biquad, Q≈0.707) for cutoff [fc]
/// at rate [fs].
pub(crate) fn butterworth_lp(fc: f64, fs: f64) -> Biquad {
    let w0 = 2.0 * PI * fc / fs;
    let (cosw, sinw) = (w0.cos(), w0.sin());
    let alpha = sinw / SQRT_2;
    let a0 = 1.0 + alpha;
    let b0 = (1.0 - cosw) / 2.0 / a0;
    Biquad {
        b0,
        b1: (1.0 - cosw) / a0,
        b2: b0,
        a1: -2.0 * cosw / a0,
        a2: (1.0 - alpha) / a0,
    }
}

/// The anti-alias cutoff for decimating by [factor]: below the new Nyquist.
/// fc = 0.45·newSampleRate, i.e. 0.9·newNyquist, so F3 (≤ ~3.8 kHz) is still
/// well inside the passband.
fn cutoff(fs: f64, factor: usize) -> f64 {
    0.45 * fs / factor as f64
}

/// Anti-aliased decimation by integer [factor] (three cascaded Butterworth
/// LPs). Three cascaded biquads (~6th order) give markedly more stopband
/// attenuation than two, so audible energy above the fold doesn't alias into
/// the formant bands. MEMORY: exactly one working copy of [x] — the three
/// filter stages run in place on it. [x] itself is never mutated (callers
/// keep using the undecimated samples), and a factor of 1 is [x] itself.
pub(crate) fn decimate(x: &[f32], factor: usize, fs: f64) -> Cow<'_, [f32]> {
    if factor <= 1 {
        return Cow::Borrowed(x);
    }
    let c = butterworth_lp(cutoff(fs, factor), fs);
    let mut lp = x.to_vec();
    for _ in 0..3 {
        BiquadState::default().run_in_place(&mut lp, c);
    }
    Cow::Owned(keep_every(&lp, factor))
}

/// Every [factor]-th sample of [lp], from the first, for `len / factor`
/// samples.
fn keep_every(lp: &[f32], factor: usize) -> Vec<f32> {
    (0..lp.len() / factor).map(|i| lp[i * factor]).collect()
}

/// [`decimate`] of [x] by [f1] and by [f2] (both > 1), bit for bit. A
/// biquad's recursion waits on its own last output at every sample, so a
/// cascade stage alone runs at that chain's latency. The two cascades share
/// only their input, so they run side by side in one loop: each chain is the
/// same IEEE operations in the same order as [`BiquadState::run_in_place`],
/// on the same f32-stored stage outputs, and both advance at the cost of
/// one. Pinned to the one-filter cascade by test.
pub(crate) fn decimate_pair(x: &[f32], f1: usize, f2: usize, fs: f64) -> (Vec<f32>, Vec<f32>) {
    let p = butterworth_lp(cutoff(fs, f1), fs);
    let q = butterworth_lp(cutoff(fs, f2), fs);
    let mut lp = x.to_vec();
    let mut lq = x.to_vec();
    for _ in 0..3 {
        let (mut px1, mut px2, mut py1, mut py2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let (mut qx1, mut qx2, mut qy1, mut qy2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for (a, b) in lp.iter_mut().zip(lq.iter_mut()) {
            let pxi = f64::from(*a);
            let qxi = f64::from(*b);
            let pyi = p.b0 * pxi + p.b1 * px1 + p.b2 * px2 - p.a1 * py1 - p.a2 * py2;
            let qyi = q.b0 * qxi + q.b1 * qx1 + q.b2 * qx2 - q.a1 * qy1 - q.a2 * qy2;
            *a = pyi as f32;
            *b = qyi as f32;
            px2 = px1;
            px1 = pxi;
            py2 = py1;
            py1 = pyi;
            qx2 = qx1;
            qx1 = qxi;
            qy2 = qy1;
            qy1 = qyi;
        }
    }
    (keep_every(&lp, f1), keep_every(&lq, f2))
}

/// A 3-stage Butterworth decimator producing [`decimate`]'s output
/// INCREMENTALLY: each block of incoming samples runs through the cascade
/// exactly as the batch runs the whole take — each stage in place over an
/// f32 copy, reading the previous stage's f32-stored output, its state
/// carried from block to block — so the streamed decimated signal is
/// bit-identical to the batch copy.
#[derive(Debug)]
pub(crate) struct StreamDecimator {
    factor: usize,
    stages: [BiquadState; 3],
    coefficients: Biquad,
    /// Position of the next input sample on the decimation grid (0 = kept).
    phase: usize,
    scratch: Vec<f32>,
}

impl StreamDecimator {
    pub(crate) fn new(factor: usize, fs: f64) -> Self {
        debug_assert!(factor >= 1);
        StreamDecimator {
            factor,
            stages: [BiquadState::default(); 3],
            coefficients: butterworth_lp(cutoff(fs, factor), fs),
            phase: 0,
            scratch: Vec::new(),
        }
    }

    /// Feeds the next [block] of raw samples, appending to [out] every
    /// decimated sample that lands on the grid. A block runs each stage's
    /// recursion in a tight loop over the block (one sample at a time cost a
    /// call and the state's loads and stores per stage per sample).
    pub(crate) fn feed_all(&mut self, block: &[f32], out: &mut RollingF32) {
        let factor = self.factor;
        if factor == 1 {
            out.add_all(block);
            return;
        }
        let n = block.len();
        self.scratch.clear();
        self.scratch.extend_from_slice(block);
        for stage in &mut self.stages {
            stage.run_in_place(&mut self.scratch, self.coefficients);
        }
        let mut i = if self.phase == 0 {
            0
        } else {
            factor - self.phase
        };
        while i < n {
            out.add(self.scratch[i]);
            i += factor;
        }
        self.phase = (self.phase + n) % factor;
    }
}
