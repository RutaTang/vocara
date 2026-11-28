//! Parametric EQ — a cascade of RBJ-cookbook biquads, Direct Form I.
//!
//! The recurrence and sign convention are ported verbatim from the Dart analysis path
//! (`audio_dsp.dart:_biquadLp`): coefficients are pre-divided by `a0` and
//! `y = b0*x + b1*x1 + b2*x2 - a1*y1 - a2*y2`. Each band keeps independent L/R state.
//!
//! A parameter change never lerps coefficients (transiently unstable at high Q): the band
//! holds a second filter seeded from the current one and **cross-fades their outputs** over
//! a few ms.

use crate::dsp::denormals::sanitize;
use crate::node::{Node, ProcessIo};
use crate::sample::{Smp, CHANNELS};

/// Normalized biquad coefficients (already divided by `a0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Coeffs {
    /// Unity pass-through (b0 = 1, everything else 0).
    pub const PASS: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
}

/// Band shapes from the RBJ Audio-EQ cookbook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandKind {
    Peaking,
    LowShelf,
    HighShelf,
    HighPass,
    LowPass,
}

/// A single EQ band specification.
#[derive(Clone, Copy, Debug)]
pub struct BandSpec {
    pub kind: BandKind,
    pub freq: f32,
    pub q: f32,
    pub gain_db: f32,
    pub enabled: bool,
}

impl BandSpec {
    pub fn peaking(freq: f32, q: f32, gain_db: f32) -> Self {
        BandSpec {
            kind: BandKind::Peaking,
            freq,
            q,
            gain_db,
            enabled: true,
        }
    }

    /// RBJ cookbook coefficients for this band at sample rate `sr`, normalized by `a0`.
    pub fn coeffs(&self, sr: f64) -> Coeffs {
        // A peak or shelf at 0 dB is the identity (b = a term for term): pass-through
        // renders it exactly, where the cookbook terms only cancel to within rounding, and
        // lets the band be skipped (see `Band::passes`). Flat is the default strip.
        if self.gain_db == 0.0
            && matches!(
                self.kind,
                BandKind::Peaking | BandKind::LowShelf | BandKind::HighShelf
            )
        {
            return Coeffs::PASS;
        }
        // Guard the frequency away from DC and Nyquist for stability.
        let f0 = (self.freq as f64).clamp(10.0, sr * 0.45);
        let q = (self.q as f64).max(0.05);
        // Guard the gain against corrupt persisted state: the UI clamps to ±15 dB, so ±24 dB
        // never touches legitimate values but keeps 10^(gain/40) sane if a bad gain slips in.
        let gain_db = (self.gain_db as f64).clamp(-24.0, 24.0);
        let a = 10f64.powf(gain_db / 40.0); // amplitude, sqrt of linear gain
        let w0 = 2.0 * std::f64::consts::PI * f0 / sr;
        let cosw = w0.cos();
        let sinw = w0.sin();
        let alpha = sinw / (2.0 * q);

        let (b0, b1, b2, a0, a1, a2) = match self.kind {
            BandKind::Peaking => (
                1.0 + alpha * a,
                -2.0 * cosw,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cosw,
                1.0 - alpha / a,
            ),
            BandKind::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cosw + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cosw),
                    a * ((a + 1.0) - (a - 1.0) * cosw - s),
                    (a + 1.0) + (a - 1.0) * cosw + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cosw),
                    (a + 1.0) + (a - 1.0) * cosw - s,
                )
            }
            BandKind::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cosw + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cosw),
                    a * ((a + 1.0) + (a - 1.0) * cosw - s),
                    (a + 1.0) - (a - 1.0) * cosw + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cosw),
                    (a + 1.0) - (a - 1.0) * cosw - s,
                )
            }
            BandKind::HighPass => (
                (1.0 + cosw) / 2.0,
                -(1.0 + cosw),
                (1.0 + cosw) / 2.0,
                1.0 + alpha,
                -2.0 * cosw,
                1.0 - alpha,
            ),
            BandKind::LowPass => (
                (1.0 - cosw) / 2.0,
                1.0 - cosw,
                (1.0 - cosw) / 2.0,
                1.0 + alpha,
                -2.0 * cosw,
                1.0 - alpha,
            ),
        };

        Coeffs {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }
}

/// One Direct-Form-I biquad with its own state. `Copy` so seeding a target filter from the
/// current one is a value copy that preserves history (no transient).
#[derive(Clone, Copy)]
pub struct Biquad {
    c: Coeffs,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn new(c: Coeffs) -> Self {
        Biquad {
            c,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Replace coefficients, keeping the filter state (history).
    #[inline]
    pub fn set_coeffs(&mut self, c: Coeffs) {
        self.c = c;
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        // EXACT DF-I recurrence ported from audio_dsp.dart:_biquadLp.
        let y = self.c.b0 * x + self.c.b1 * self.x1 + self.c.b2 * self.x2
            - self.c.a1 * self.y1
            - self.c.a2 * self.y2;
        let y = sanitize(y);
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// The history a [`tick`](Self::tick) on [`Coeffs::PASS`] leaves for a finite `x`
    /// (its output is `x`).
    #[inline]
    fn shift(&mut self, x: f32) {
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = x;
    }

    /// Zero the filter state (history), keeping coefficients. Used on seek.
    #[inline]
    pub fn reset_state(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline(always)]
    fn history(&self) -> History {
        History {
            x1: self.x1,
            x2: self.x2,
            y1: self.y1,
            y2: self.y2,
        }
    }

    #[inline(always)]
    fn set_history(&mut self, h: History) {
        (self.x1, self.x2, self.y1, self.y2) = (h.x1, h.x2, h.y1, h.y2);
    }
}

/// A [`Biquad`]'s history, held in registers by [`Band::process_pair`].
#[derive(Clone, Copy)]
struct History {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl History {
    /// [`Biquad::tick`] with the coefficients passed in: the same expression, term for term.
    #[inline(always)]
    fn tick(&mut self, c: &Coeffs, x: f32) -> f32 {
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        let y = sanitize(y);
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Cross-fade length when a band's coefficients change (avoids zipper/instability).
const XFADE_FRAMES: f32 = 256.0;

/// One EQ band: independent L/R biquads, plus a second L/R pair for a coefficient change
/// that is cross-faded in.
struct Band {
    spec: BandSpec,
    // Current filters.
    l: Biquad,
    r: Biquad,
    // Target filters (valid while `xfading`).
    ln: Biquad,
    rn: Biquad,
    xfading: bool,
    mix: f32,
    xinc: f32,
    sr: f64,
}

impl Band {
    fn new(spec: BandSpec, sr: f64) -> Self {
        let c = if spec.enabled {
            spec.coeffs(sr)
        } else {
            Coeffs::PASS
        };
        Band {
            spec,
            l: Biquad::new(c),
            r: Biquad::new(c),
            ln: Biquad::new(c),
            rn: Biquad::new(c),
            xfading: false,
            mix: 0.0,
            xinc: 1.0 / XFADE_FRAMES,
            sr,
        }
    }

    /// Reconfigure this band; cross-fades from the old response to the new one.
    fn set_spec(&mut self, spec: BandSpec) {
        self.spec = spec;
        let c = if spec.enabled {
            spec.coeffs(self.sr)
        } else {
            Coeffs::PASS
        };
        // No audio has run since the fade began: this is a later field of the same
        // multi-command edit (ve_set_eq_band sends FREQ, Q, GAIN_DB and ENABLED back to
        // back, all applied before the block renders). Retarget the pending fade —
        // the target filters' history still equals the current ones' — instead of
        // committing it below, which would swap the old response out instantly.
        if self.xfading && self.mix == 0.0 {
            self.ln.set_coeffs(c);
            self.rn.set_coeffs(c);
            return;
        }
        // If already cross-fading, collapse the in-flight blend into the current filter
        // first so the new fade starts from a defined state.
        if self.xfading {
            self.l = self.ln;
            self.r = self.rn;
        }
        // Seed the target filters from the current ones (copies history) then set coeffs.
        self.ln = self.l;
        self.rn = self.r;
        self.ln.set_coeffs(c);
        self.rn.set_coeffs(c);
        self.xfading = true;
        self.mix = 0.0;
    }

    #[inline]
    fn process(&mut self, buf: &mut [Smp], frames: usize) {
        if self.passes() {
            self.pass(buf, frames);
        } else if self.xfading {
            let mut m = self.mix;
            for i in 0..frames {
                let il = buf[CHANNELS * i];
                let ir = buf[CHANNELS * i + 1];
                let al = self.l.tick(il);
                let bl = self.ln.tick(il);
                let ar = self.r.tick(ir);
                let br = self.rn.tick(ir);
                buf[CHANNELS * i] = al * (1.0 - m) + bl * m;
                buf[CHANNELS * i + 1] = ar * (1.0 - m) + br * m;
                m += self.xinc;
                if m > 1.0 {
                    m = 1.0;
                }
            }
            self.mix = m;
            if self.mix >= 1.0 {
                self.l = self.ln;
                self.r = self.rn;
                self.xfading = false;
            }
        } else {
            self.run(buf, frames);
        }
    }

    /// Every frame through the current filters.
    #[inline]
    fn run(&mut self, buf: &mut [Smp], frames: usize) {
        for i in 0..frames {
            buf[CHANNELS * i] = self.l.tick(buf[CHANNELS * i]);
            buf[CHANNELS * i + 1] = self.r.tick(buf[CHANNELS * i + 1]);
        }
    }

    /// Whether the band is pass-through with no cross-fade in flight — disabled, or a flat
    /// peak or shelf — so [`pass`](Self::pass) renders it.
    #[inline]
    fn passes(&self) -> bool {
        !self.xfading && self.l.c == Coeffs::PASS && self.r.c == Coeffs::PASS
    }

    /// [`run`](Self::run) on pass-through coefficients without the per-frame filter: what
    /// ticking renders (`y = x`, a non-finite sample as 0), up to the sign of a zero, with the
    /// history it would leave, so a later edit cross-fades in from the same state. The default
    /// strip is all flat, so this is most bands on most tracks.
    fn pass(&mut self, buf: &mut [Smp], frames: usize) {
        let block = &mut buf[..frames * CHANNELS];
        for s in block.iter_mut() {
            *s = sanitize(*s);
        }
        let held = block.as_chunks::<CHANNELS>().0;
        for &[l, r] in &held[held.len().saturating_sub(2)..] {
            self.l.shift(l);
            self.r.shift(r);
        }
    }

    /// Whether [`process_pair`](Self::process_pair) may run this band: no cross-fade in flight,
    /// and both channels on the same coefficients (they are only ever set together, so the
    /// pair loop reads one set for both).
    #[inline]
    fn pairable(&self) -> bool {
        !self.xfading && self.l.c == self.r.c
    }

    /// `a` then `b` over the block — exactly [`process`](Self::process) on `a`, then on `b` —
    /// with the two bands interleaved frame by frame. Each filter sees the same inputs in the
    /// same order (band `b`'s input at a frame is band `a`'s output at that frame either way),
    /// but four independent recursions are in flight instead of two, which is what bounds a
    /// cascade: every biquad output waits on the previous one through `a1 * y1`. Both bands must
    /// be [`pairable`](Self::pairable).
    fn process_pair(a: &mut Band, b: &mut Band, buf: &mut [Smp], frames: usize) {
        let (ca, cb) = (a.l.c, b.l.c);
        let (mut al, mut ar) = (a.l.history(), a.r.history());
        let (mut bl, mut br) = (b.l.history(), b.r.history());
        for f in buf[..frames * CHANNELS].as_chunks_mut::<CHANNELS>().0 {
            let l = al.tick(&ca, f[0]);
            let r = ar.tick(&ca, f[1]);
            f[0] = bl.tick(&cb, l);
            f[1] = br.tick(&cb, r);
        }
        a.l.set_history(al);
        a.r.set_history(ar);
        b.l.set_history(bl);
        b.r.set_history(br);
    }

    /// Clear all filter history and end any in-flight cross-fade so a seek does
    /// not bleed filter tails across the discontinuity.
    fn reset(&mut self) {
        // Commit a pending coefficient cross-fade into the current filters first
        // (mirroring the xfade-complete path). Otherwise reset would discard the NEW
        // coeffs that only live in ln/rn and revert the band to its OLD response,
        // desyncing it from band.spec / the UI until the band is edited again.
        if self.xfading {
            self.l = self.ln;
            self.r = self.rn;
            self.xfading = false;
        }
        self.l.reset_state();
        self.r.reset_state();
        self.ln.reset_state();
        self.rn.reset_state();
        self.mix = 0.0;
    }
}

/// Field selectors for `set_param` (param id = band_index << 3 | field).
pub mod param {
    pub const FIELD_BITS: u16 = 3;
    pub const FREQ: u16 = 0;
    pub const Q: u16 = 1;
    pub const GAIN_DB: u16 = 2;
    pub const ENABLED: u16 = 3;
}

/// Parametric EQ node: an ordered cascade of [`Band`]s applied to interleaved stereo.
pub struct EqNode {
    bands: Vec<Band>,
    sr: f64,
    /// Whether any audio has run through the bands. Until then a band edit applies directly:
    /// a rebuilt graph's re-pushed EQ must not cross-fade in from the flat build default.
    heard: bool,
}

impl EqNode {
    /// Build from band specs. `prepare` will rebuild at the real sample rate.
    pub fn new(specs: &[BandSpec]) -> Self {
        let sr = 48_000.0;
        EqNode {
            bands: specs.iter().map(|s| Band::new(*s, sr)).collect(),
            sr,
            heard: false,
        }
    }

    /// A sensible default 6-band channel strip: low-shelf, four peaks, high-shelf,
    /// all flat. Frequencies match the Dart `TrackFx` default so the UI and engine
    /// agree band-for-band.
    pub fn default_strip() -> Self {
        EqNode::new(&[
            BandSpec {
                kind: BandKind::LowShelf,
                freq: 100.0,
                q: 0.7,
                gain_db: 0.0,
                enabled: true,
            },
            BandSpec::peaking(250.0, 1.0, 0.0),
            BandSpec::peaking(700.0, 1.0, 0.0),
            BandSpec::peaking(2000.0, 1.0, 0.0),
            BandSpec::peaking(6000.0, 1.0, 0.0),
            BandSpec {
                kind: BandKind::HighShelf,
                freq: 12000.0,
                q: 0.7,
                gain_db: 0.0,
                enabled: true,
            },
        ])
    }

    /// Reconfigure a band (used by tests and the control plane).
    pub fn set_band(&mut self, index: usize, spec: BandSpec) {
        let (heard, sr) = (self.heard, self.sr);
        if let Some(b) = self.bands.get_mut(index) {
            if heard {
                b.set_spec(spec);
            } else {
                *b = Band::new(spec, sr);
            }
        }
    }
}

impl Node for EqNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        // Re-seed every band at the real rate (no cross-fade — this is pre-roll).
        let specs: Vec<BandSpec> = self.bands.iter().map(|b| b.spec).collect();
        self.bands = specs.iter().map(|s| Band::new(*s, sample_rate)).collect();
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let frames = io.frames;
        let n = frames * CHANNELS;
        // Access fields directly so the input (io.inputs) and output (io.outputs) borrows
        // stay disjoint.
        let inp = io.inputs.first().copied();
        let out = &mut *io.outputs[0];
        match inp {
            Some(inp) => out[..n].copy_from_slice(&inp[..n]),
            None => {
                for s in &mut out[..n] {
                    *s = 0.0;
                }
            }
        }
        self.heard = true;
        // The cascade in order, two bands per pass where both allow it (see `process_pair`).
        let count = self.bands.len();
        let mut i = 0;
        while i < count {
            if i + 1 < count
                && self.bands[i].pairable()
                && self.bands[i + 1].pairable()
                && !self.bands[i].passes()
                && !self.bands[i + 1].passes()
            {
                let (done, rest) = self.bands.split_at_mut(i + 1);
                Band::process_pair(&mut done[i], &mut rest[0], out, frames);
                i += 2;
            } else {
                self.bands[i].process(out, frames);
                i += 1;
            }
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        let idx = (param >> param::FIELD_BITS) as usize;
        let field = param & ((1 << param::FIELD_BITS) - 1);
        if let Some(b) = self.bands.get(idx) {
            let mut spec = b.spec;
            match field {
                param::FREQ => spec.freq = value,
                param::Q => spec.q = value,
                param::GAIN_DB => spec.gain_db = value,
                param::ENABLED => spec.enabled = value >= 0.5,
                _ => return,
            }
            self.set_band(idx, spec);
        }
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.reset();
        }
    }

    /// Each band's filter history carries (a fresh one restarted every EQ'd track from zero
    /// state, a click); the coefficients stay this node's. A band mid cross-fade hands over
    /// the history of the filter it was fading to, the one that carries its setting.
    fn carry_state(&mut self, old: &mut dyn Node) {
        let Some(old) = crate::node::same_kind::<EqNode>(old) else {
            return;
        };
        if old.sr != self.sr || old.bands.len() != self.bands.len() {
            return;
        }
        for (b, o) in self.bands.iter_mut().zip(&old.bands) {
            let (l, r) = if o.xfading {
                (&o.ln, &o.rn)
            } else {
                (&o.l, &o.r)
            };
            b.l.set_history(l.history());
            b.r.set_history(r.history());
            b.ln.set_history(l.history());
            b.rn.set_history(r.history());
        }
        self.heard = old.heard;
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::test_support::{assert_matches_reference, material};

    /// The cascade as it ran before band pairing: each band over the whole block in turn.
    fn reference(eq: &mut EqNode, inp: &[f32], out: &mut [f32]) {
        out.copy_from_slice(inp);
        eq.heard = true;
        for band in &mut eq.bands {
            band.process(out, inp.len() / CHANNELS);
        }
    }

    fn id(band: u16, field: u16) -> u16 {
        (band << param::FIELD_BITS) | field
    }

    /// Two bands per pass render exactly what one band per pass renders: the flat default
    /// strip, boosts and cuts, a band switched off, cross-fades on some bands while their
    /// neighbours pair (and a second edit landing mid-fade), and an odd band count.
    #[test]
    fn paired_bands_match_the_band_by_band_cascade_bit_for_bit() {
        let edits = [
            (0, id(2, param::GAIN_DB), 4.0), // before anything is heard: applies directly
            (4, id(0, param::GAIN_DB), -6.0),
            (4, id(3, param::GAIN_DB), 9.0),
            (5, id(3, param::Q), 4.0), // mid-fade on band 3
            (12, id(4, param::ENABLED), 0.0),
            (13, id(1, param::FREQ), 180.0),
            (30, id(5, param::GAIN_DB), -12.0),
            (31, id(2, param::GAIN_DB), 0.0),
        ];
        let five = || {
            EqNode::new(&[
                BandSpec {
                    kind: BandKind::HighPass,
                    freq: 80.0,
                    q: 0.7,
                    gain_db: 0.0,
                    enabled: true,
                },
                BandSpec::peaking(400.0, 2.0, -3.0),
                BandSpec::peaking(1_500.0, 0.8, 2.0),
                BandSpec::peaking(5_000.0, 1.0, 0.0),
                BandSpec {
                    kind: BandKind::LowPass,
                    freq: 15_000.0,
                    q: 0.7,
                    gain_db: 0.0,
                    enabled: true,
                },
            ])
        };
        for fresh in [EqNode::default_strip as fn() -> EqNode, five] {
            let (mut fast, mut slow) = (fresh(), fresh());
            fast.prepare(48_000.0, 1024);
            slow.prepare(48_000.0, 1024);
            assert_matches_reference(&mut fast, &mut slow, reference, &edits, 60);
        }
    }

    /// Runs `eq` over `blocks` blocks of the shared material, applying `edits` like
    /// `assert_matches_reference`, and returns every output sample. `tick_all` ticks every
    /// band that is not fading through its filters instead of letting pass-through bands skip.
    fn render(
        eq: &mut EqNode,
        edits: &[(usize, u16, f32)],
        blocks: usize,
        tick_all: bool,
    ) -> Vec<f32> {
        let sizes = [512, 1, 37, 1024, 256, 129, 700, 2];
        let total: usize = (0..blocks).map(|b| sizes[b % sizes.len()]).sum();
        let input = material(total);
        let mut out = input.clone();
        let mut at = 0;
        for b in 0..blocks {
            for &(when, param, value) in edits {
                if when == b {
                    eq.set_param(param, value);
                }
            }
            let frames = sizes[b % sizes.len()];
            let block = &mut out[at * CHANNELS..(at + frames) * CHANNELS];
            if tick_all {
                eq.heard = true;
                for band in &mut eq.bands {
                    if band.xfading {
                        band.process(block, frames);
                    } else {
                        band.run(block, frames);
                    }
                }
            } else {
                let inputs: [&[Smp]; 1] = [&input[at * CHANNELS..(at + frames) * CHANNELS]];
                let mut outs: [&mut [Smp]; 1] = [block];
                eq.process(&mut ProcessIo {
                    inputs: &inputs,
                    outputs: &mut outs[..],
                    frames,
                    tl: at as i64,
                    sr: 48_000.0,
                });
            }
            at += frames;
        }
        out
    }

    /// A pass-through band (flat or switched off) skips its filter but renders what ticking it
    /// renders, and leaves the history ticking would: bands fade in from it and back to it, on
    /// the default strip and on one mixing filters with flat and disabled bands, through block
    /// sizes of 1 and 2 frames. Equal as numbers; only the sign of a zero may differ.
    #[test]
    fn skipped_pass_through_bands_render_what_ticking_them_renders() {
        let edits = [
            (3, id(2, param::GAIN_DB), 6.0), // a skipped flat band fades in
            (5, id(0, param::ENABLED), 0.0),
            (7, id(2, param::GAIN_DB), 0.0), // and back out to flat
            (8, id(4, param::GAIN_DB), -4.0),
            (8, id(4, param::GAIN_DB), 0.0), // retargeted to flat before any audio
            (11, id(1, param::GAIN_DB), 3.0),
            (12, id(1, param::ENABLED), 0.0), // a fade in flight switched off
            (16, id(0, param::ENABLED), 1.0),
            (17, id(5, param::Q), 3.0), // a flat band's shape changes: still pass-through
            (19, id(3, param::GAIN_DB), -9.0),
        ];
        let mixed = || {
            EqNode::new(&[
                BandSpec {
                    kind: BandKind::HighPass,
                    freq: 90.0,
                    q: 0.7,
                    gain_db: 0.0,
                    enabled: true,
                },
                BandSpec::peaking(300.0, 1.0, 0.0),
                BandSpec::peaking(900.0, 2.0, 5.0),
                BandSpec {
                    kind: BandKind::LowShelf,
                    freq: 120.0,
                    q: 0.7,
                    gain_db: 0.0,
                    enabled: false,
                },
                BandSpec::peaking(3_000.0, 1.0, 0.0),
                BandSpec {
                    kind: BandKind::HighShelf,
                    freq: 10_000.0,
                    q: 0.7,
                    gain_db: 0.0,
                    enabled: true,
                },
            ])
        };
        for fresh in [EqNode::default_strip as fn() -> EqNode, mixed] {
            let (mut fast, mut slow) = (fresh(), fresh());
            fast.prepare(48_000.0, 1024);
            slow.prepare(48_000.0, 1024);
            let got = render(&mut fast, &edits, 40, false);
            let want = render(&mut slow, &edits, 40, true);
            for (k, (g, w)) in got.iter().zip(&want).enumerate() {
                assert!(g == w, "sample {k}: {g} vs {w} ticking every band");
            }
        }
    }

    /// The flat default strip hands its input through untouched, bit for bit, where its
    /// cookbook filters (b = a) only reproduced it to within rounding.
    #[test]
    fn the_flat_default_strip_is_an_exact_pass_through() {
        let mut eq = EqNode::default_strip();
        eq.prepare(48_000.0, 1024);
        let got = render(&mut eq, &[], 12, false);
        let sizes = [512, 1, 37, 1024, 256, 129, 700, 2];
        let input = material((0..12).map(|b| sizes[b % sizes.len()]).sum());
        assert!(got
            .iter()
            .zip(&input)
            .all(|(g, x)| g.to_bits() == x.to_bits()));
    }
}
