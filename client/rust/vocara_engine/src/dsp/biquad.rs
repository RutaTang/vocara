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

    /// Zero the filter state (history), keeping coefficients. Used on seek.
    #[inline]
    pub fn reset_state(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
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
        if self.xfading {
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
            for i in 0..frames {
                buf[CHANNELS * i] = self.l.tick(buf[CHANNELS * i]);
                buf[CHANNELS * i + 1] = self.r.tick(buf[CHANNELS * i + 1]);
            }
        }
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
        for band in &mut self.bands {
            band.process(out, frames);
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
}
