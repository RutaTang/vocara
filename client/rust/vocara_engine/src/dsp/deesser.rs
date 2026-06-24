//! De-esser: a split-band dynamic high-frequency reducer for sibilance.
//!
//! The signal is split into a high band (a high-pass detector/processing path) and its
//! complementary low band (`x - high`, phase-coherent). When the high band's level exceeds
//! the threshold, only the high band is attenuated (clamped to `range`), so the body of the
//! voice is untouched. No recursive audio path → stable.

use crate::dsp::biquad::{BandKind, BandSpec, Biquad};
use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const FREQ: u16 = 0; // crossover / detector frequency (Hz)
    pub const THRESHOLD_DB: u16 = 1;
    pub const RANGE_DB: u16 = 2; // maximum reduction (dB, >= 0)
    pub const BYPASS: u16 = 3;
}

pub struct DeEsserNode {
    hp_l: Biquad,
    hp_r: Biquad,
    freq: f32,
    threshold_db: f32,
    range_db: f32,
    env_db: f32,
    attack_coef: f32,
    release_coef: f32,
    bypass: bool,
    sr: f64,
}

impl DeEsserNode {
    pub fn new() -> Self {
        let mut d = DeEsserNode {
            hp_l: Biquad::new(crate::dsp::biquad::Coeffs::PASS),
            hp_r: Biquad::new(crate::dsp::biquad::Coeffs::PASS),
            freq: 6500.0,
            threshold_db: -28.0,
            range_db: 12.0,
            env_db: -120.0,
            attack_coef: 1.0,
            release_coef: 1.0,
            bypass: true,
            sr: 48_000.0,
        };
        d.retune();
        d.recompute_ballistics();
        d
    }

    fn retune(&mut self) {
        let c = BandSpec {
            kind: BandKind::HighPass,
            freq: self.freq,
            q: 0.707,
            gain_db: 0.0,
            enabled: true,
        }
        .coeffs(self.sr);
        self.hp_l.set_coeffs(c);
        self.hp_r.set_coeffs(c);
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
        self.retune();
        self.recompute_ballistics();
        self.env_db = -120.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the high-pass biquads' filter tails and the held sibilance envelope
        // carry pre-seek history across the discontinuity (click + wrong gain
        // reduction). Clear filter state (coeffs intact) and the envelope to its
        // idle value — matching Band::reset and the delay/doubler/reverb resets.
        self.hp_l.reset_state();
        self.hp_r.reset_state();
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
            let hl = self.hp_l.tick(l);
            let hr = self.hp_r.tick(r);
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

            let over = self.env_db - self.threshold_db;
            let red_db = if over > 0.0 {
                (-over).max(-self.range_db)
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
                self.retune();
            }
            param::THRESHOLD_DB => self.threshold_db = value,
            param::RANGE_DB => self.range_db = value.max(0.0),
            param::BYPASS => self.bypass = value >= 0.5,
            _ => {}
        }
    }
}
