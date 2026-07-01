//! De-esser: a split-band dynamic high-frequency reducer for sibilance.
//!
//! The signal is split into a high band (a high-pass detector/processing path) and its
//! complementary low band (`x - high`, phase-coherent). When the high band's level exceeds
//! the threshold, only the high band is attenuated (clamped to `range`), so the body of the
//! voice is untouched. No recursive audio path → stable.

use crate::dsp::biquad::{BandKind, BandSpec, Biquad, Coeffs};
use crate::dsp::smooth::Smoothed;
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

pub struct DeEsserNode {
    hp_l: Biquad,
    hp_r: Biquad,
    // Target high-pass filters, valid while `xfading`: seeded from the current filters
    // (copies their history) with the new coefficients, then blended in over XFADE_FRAMES.
    hp_l2: Biquad,
    hp_r2: Biquad,
    xfading: bool,
    xmix: f32,
    xinc: f32,
    freq: f32,
    // Smoothed so a threshold/range knob drag or automation ramps the high-band reduction
    // instead of stepping it per block (zipper/click). Mirrors the compressor's threshold.
    threshold: Smoothed,
    range: Smoothed,
    env_db: f32,
    attack_coef: f32,
    release_coef: f32,
    bypass: bool,
    sr: f64,
}

impl DeEsserNode {
    pub fn new() -> Self {
        let mut d = DeEsserNode {
            hp_l: Biquad::new(Coeffs::PASS),
            hp_r: Biquad::new(Coeffs::PASS),
            hp_l2: Biquad::new(Coeffs::PASS),
            hp_r2: Biquad::new(Coeffs::PASS),
            xfading: false,
            xmix: 0.0,
            xinc: 1.0 / XFADE_FRAMES,
            freq: 6500.0,
            threshold: Smoothed::new(-28.0),
            range: Smoothed::new(12.0),
            env_db: -120.0,
            attack_coef: 1.0,
            release_coef: 1.0,
            bypass: true,
            sr: 48_000.0,
        };
        d.retune_now();
        d.recompute_ballistics();
        d
    }

    fn hp_coeffs(&self) -> Coeffs {
        BandSpec {
            kind: BandKind::HighPass,
            freq: self.freq,
            q: 0.707,
            gain_db: 0.0,
            enabled: true,
        }
        .coeffs(self.sr)
    }

    /// Immediate retune (pre-roll: new/prepare) — no cross-fade.
    fn retune_now(&mut self) {
        let c = self.hp_coeffs();
        self.hp_l.set_coeffs(c);
        self.hp_r.set_coeffs(c);
        self.hp_l2.set_coeffs(c);
        self.hp_r2.set_coeffs(c);
        self.xfading = false;
        self.xmix = 0.0;
    }

    /// Retune with an output cross-fade — used for a runtime FREQ change so the
    /// coefficient swap on a stateful filter does not click.
    fn retune_xfade(&mut self) {
        let c = self.hp_coeffs();
        // Collapse an in-flight fade into the current filters so the new fade starts
        // from a defined state.
        if self.xfading {
            self.hp_l = self.hp_l2;
            self.hp_r = self.hp_r2;
        }
        // Seed the target filters from the current ones (copies history), then retune.
        self.hp_l2 = self.hp_l;
        self.hp_r2 = self.hp_r;
        self.hp_l2.set_coeffs(c);
        self.hp_r2.set_coeffs(c);
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
        self.env_db = -120.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the high-pass biquads' filter tails and the held sibilance envelope
        // carry pre-seek history across the discontinuity (click + wrong gain
        // reduction). Clear filter state (coeffs intact) and the envelope to its
        // idle value — matching Band::reset and the delay/doubler/reverb resets.
        // Commit a pending coefficient cross-fade into the current filters first,
        // otherwise reset would revert to the OLD coeffs and desync from `freq`.
        if self.xfading {
            self.hp_l = self.hp_l2;
            self.hp_r = self.hp_r2;
            self.xfading = false;
        }
        self.hp_l.reset_state();
        self.hp_r.reset_state();
        self.hp_l2.reset_state();
        self.hp_r2.reset_state();
        self.xmix = 0.0;
        self.env_db = -120.0;
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
        if self.bypass {
            return;
        }

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            // Cross-fade the high-pass output on a freq change so a coefficient swap
            // on the stateful biquads doesn't click.
            let (hl, hr) = if self.xfading {
                let m = self.xmix;
                let hl = self.hp_l.tick(l) * (1.0 - m) + self.hp_l2.tick(l) * m;
                let hr = self.hp_r.tick(r) * (1.0 - m) + self.hp_r2.tick(r) * m;
                self.xmix += self.xinc;
                if self.xmix >= 1.0 {
                    self.xmix = 1.0;
                    self.hp_l = self.hp_l2;
                    self.hp_r = self.hp_r2;
                    self.xfading = false;
                }
                (hl, hr)
            } else {
                (self.hp_l.tick(l), self.hp_r.tick(r))
            };
            let low_l = l - hl;
            let low_r = r - hr;

            let peak = hl.abs().max(hr.abs());
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
            out[CHANNELS * i] = low_l + hl * g;
            out[CHANNELS * i + 1] = low_r + hr * g;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::FREQ => {
                self.freq = value.clamp(2000.0, 16000.0);
                self.retune_xfade();
            }
            param::THRESHOLD_DB => self.threshold.set_target(value),
            param::RANGE_DB => self.range.set_target(value.max(0.0)),
            param::BYPASS => self.bypass = value >= 0.5,
            _ => {}
        }
    }
}
