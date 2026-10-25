//! De-esser: a split-band dynamic high-frequency reducer for sibilance.
//!
//! The signal is split by a Linkwitz-Riley (4th-order) crossover at the detector frequency.
//! When the high band's level exceeds the threshold, only the high band is attenuated
//! (clamped to `range`), so the body of the voice is untouched. LR4's two bands are in phase
//! at every frequency and sum to an allpass, so `low + g·high` stays between g and unity: a
//! reduction never boosts anything. (The old `x - HP2(x)` low band was NOT phase-coherent —
//! it peaked around the crossover, so de-essing lifted 3–7 kHz by up to ~2 dB and barely cut
//! the sibilance itself.) No recursive audio path → stable.

use crate::dsp::biquad::{BandKind, BandSpec, Biquad, Coeffs};
use crate::dsp::smooth::{BypassFade, Smoothed};
use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const FREQ: u16 = 0; // crossover / detector frequency (Hz)
    pub const THRESHOLD_DB: u16 = 1;
    pub const RANGE_DB: u16 = 2; // maximum reduction (dB, >= 0)
    pub const BYPASS: u16 = 3;
}

/// Cross-fade length (frames) when the detector frequency changes, so swapping the
/// high-pass coefficients in place does not produce a zipper/click. Mirrors the EQ
/// `Band` cross-fade in `biquad.rs`.
const XFADE_FRAMES: f32 = 256.0;

/// One channel's crossover. The detector reads the first 2nd-order high-pass stage — the
/// signal the de-esser has always detected on, so thresholds and presets keep their
/// calibration — and the processing bands are the 4th-order (squared Butterworth)
/// completions of the same corner. `Copy` so seeding a cross-fade target from the current
/// filters is a value copy that keeps their history.
#[derive(Clone, Copy)]
struct Split {
    hp1: Biquad,
    hp2: Biquad,
    lp1: Biquad,
    lp2: Biquad,
}

impl Split {
    fn new() -> Self {
        Split {
            hp1: Biquad::new(Coeffs::PASS),
            hp2: Biquad::new(Coeffs::PASS),
            lp1: Biquad::new(Coeffs::PASS),
            lp2: Biquad::new(Coeffs::PASS),
        }
    }

    fn set_coeffs(&mut self, hp: Coeffs, lp: Coeffs) {
        self.hp1.set_coeffs(hp);
        self.hp2.set_coeffs(hp);
        self.lp1.set_coeffs(lp);
        self.lp2.set_coeffs(lp);
    }

    /// (detector, low band, high band) for one input sample.
    #[inline]
    fn tick(&mut self, x: f32) -> (f32, f32, f32) {
        let det = self.hp1.tick(x);
        let high = self.hp2.tick(det);
        let low = self.lp2.tick(self.lp1.tick(x));
        (det, low, high)
    }

    fn reset_state(&mut self) {
        self.hp1.reset_state();
        self.hp2.reset_state();
        self.lp1.reset_state();
        self.lp2.reset_state();
    }
}

pub struct DeEsserNode {
    split_l: Split,
    split_r: Split,
    // Target crossovers, valid while `xfading`: seeded from the current ones (copies their
    // history) with the new coefficients, then blended in over XFADE_FRAMES.
    split_l2: Split,
    split_r2: Split,
    xfading: bool,
    xmix: f32,
    xinc: f32,
    /// Whether the crossover has run on audio. Until then a FREQ change retunes directly:
    /// there is no filter history to cross-fade from.
    heard: bool,
    freq: f32,
    // Smoothed so a threshold/range knob drag or automation ramps the high-band reduction
    // instead of stepping it per block (zipper/click). Mirrors the compressor's threshold.
    threshold: Smoothed,
    range: Smoothed,
    env_db: f32,
    attack_coef: f32,
    release_coef: f32,
    fade: BypassFade,
    sr: f64,
}

impl DeEsserNode {
    pub fn new() -> Self {
        let mut d = DeEsserNode {
            split_l: Split::new(),
            split_r: Split::new(),
            split_l2: Split::new(),
            split_r2: Split::new(),
            xfading: false,
            xmix: 0.0,
            xinc: 1.0 / XFADE_FRAMES,
            heard: false,
            freq: 6500.0,
            threshold: Smoothed::new(-28.0),
            range: Smoothed::new(12.0),
            env_db: -120.0,
            attack_coef: 1.0,
            release_coef: 1.0,
            fade: BypassFade::new(true),
            sr: 48_000.0,
        };
        d.retune_now();
        d.recompute_ballistics();
        d
    }

    /// Butterworth (Q 0.707, the detector's long-standing value) high- and low-pass
    /// coefficients at the crossover; each band cascades two of its stage, which is LR4.
    fn split_coeffs(&self) -> (Coeffs, Coeffs) {
        let stage = |kind| {
            BandSpec {
                kind,
                freq: self.freq,
                q: 0.707,
                gain_db: 0.0,
                enabled: true,
            }
            .coeffs(self.sr)
        };
        (stage(BandKind::HighPass), stage(BandKind::LowPass))
    }

    /// Immediate retune (pre-roll: new/prepare) — no cross-fade.
    fn retune_now(&mut self) {
        let (hp, lp) = self.split_coeffs();
        self.split_l.set_coeffs(hp, lp);
        self.split_r.set_coeffs(hp, lp);
        self.split_l2.set_coeffs(hp, lp);
        self.split_r2.set_coeffs(hp, lp);
        self.xfading = false;
        self.xmix = 0.0;
    }

    /// Retune with an output cross-fade — used for a runtime FREQ change so the
    /// coefficient swap on a stateful filter does not click.
    fn retune_xfade(&mut self) {
        let (hp, lp) = self.split_coeffs();
        // Collapse an in-flight fade into the current filters so the new fade starts
        // from a defined state.
        if self.xfading {
            self.split_l = self.split_l2;
            self.split_r = self.split_r2;
        }
        // Seed the target filters from the current ones (copies history), then retune.
        self.split_l2 = self.split_l;
        self.split_r2 = self.split_r;
        self.split_l2.set_coeffs(hp, lp);
        self.split_r2.set_coeffs(hp, lp);
        self.xfading = true;
        self.xmix = 0.0;
    }

    fn recompute_ballistics(&mut self) {
        let a = 0.001 * self.sr as f32; // ~1 ms attack
        let r = 0.060 * self.sr as f32; // ~60 ms release
        self.attack_coef = 1.0 - (-1.0 / a).exp();
        self.release_coef = 1.0 - (-1.0 / r).exp();
    }
}

impl Default for DeEsserNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for DeEsserNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        self.retune_now();
        self.recompute_ballistics();
        let tau = 0.01;
        self.threshold.set_time(tau, sample_rate);
        self.range.set_time(tau, sample_rate);
        self.fade.prepare(sample_rate);
        self.env_db = -120.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the crossover biquads' filter tails and the held sibilance envelope
        // carry pre-seek history across the discontinuity (click + wrong gain
        // reduction). Clear filter state (coeffs intact) and the envelope to its
        // idle value — matching Band::reset and the delay/doubler/reverb resets.
        // Commit a pending coefficient cross-fade into the current filters first,
        // otherwise reset would revert to the OLD coeffs and desync from `freq`.
        if self.xfading {
            self.split_l = self.split_l2;
            self.split_r = self.split_r2;
            self.xfading = false;
        }
        self.split_l.reset_state();
        self.split_r.reset_state();
        self.split_l2.reset_state();
        self.split_r2.reset_state();
        self.xmix = 0.0;
        self.env_db = -120.0;
        self.fade.settle();
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let frames = io.frames;
        let n = frames * CHANNELS;
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
        if self.fade.begin_block() {
            return;
        }
        self.heard = true;

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            // Cross-fade the crossover outputs on a freq change so a coefficient swap on
            // the stateful biquads doesn't click.
            let ((dl, low_l, hl), (dr, low_r, hr)) = if self.xfading {
                let m = self.xmix;
                let blend = |a: (f32, f32, f32), b: (f32, f32, f32)| {
                    (
                        a.0 * (1.0 - m) + b.0 * m,
                        a.1 * (1.0 - m) + b.1 * m,
                        a.2 * (1.0 - m) + b.2 * m,
                    )
                };
                let sl = blend(self.split_l.tick(l), self.split_l2.tick(l));
                let sr = blend(self.split_r.tick(r), self.split_r2.tick(r));
                self.xmix += self.xinc;
                if self.xmix >= 1.0 {
                    self.xmix = 1.0;
                    self.split_l = self.split_l2;
                    self.split_r = self.split_r2;
                    self.xfading = false;
                }
                (sl, sr)
            } else {
                (self.split_l.tick(l), self.split_r.tick(r))
            };

            let peak = dl.abs().max(dr.abs());
            let x_db = 20.0 * (peak + 1.0e-12).log10();
            let coef = if x_db > self.env_db {
                self.attack_coef
            } else {
                self.release_coef
            };
            self.env_db += (x_db - self.env_db) * coef;

            let threshold_db = self.threshold.next();
            let range_db = self.range.next();
            let over = self.env_db - threshold_db;
            let red_db = if over > 0.0 {
                (-over).max(-range_db)
            } else {
                0.0
            };
            let g = db_to_lin(red_db);
            // Blended toward the dry input while the bypass switch fades.
            let f = self.fade.next();
            out[CHANNELS * i] = l + (low_l + hl * g - l) * f;
            out[CHANNELS * i + 1] = r + (low_r + hr * g - r) * f;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::FREQ => {
                self.freq = value.clamp(2000.0, 16000.0);
                if self.heard {
                    self.retune_xfade();
                } else {
                    self.retune_now();
                }
            }
            param::THRESHOLD_DB => self.threshold.set_target(value),
            param::RANGE_DB => self.range.set_target(value.max(0.0)),
            param::BYPASS => self.fade.set_bypass(value >= 0.5),
            _ => {}
        }
    }
}
