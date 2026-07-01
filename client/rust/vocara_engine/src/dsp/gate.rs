//! Noise gate / downward expander: attenuates the signal when its level falls below the
//! threshold (cleans breaths / room tone between phrases). Stereo-linked peak detector with
//! attack/release ballistics on the gain-reduction envelope. No recursive audio path.

use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const THRESHOLD_DB: u16 = 0;
    pub const RANGE_DB: u16 = 1; // attenuation when fully closed (dB, >= 0)
    pub const ATTACK_MS: u16 = 2;
    pub const RELEASE_MS: u16 = 3;
    pub const BYPASS: u16 = 4;
}

const RATIO: f32 = 3.0; // expander slope below the threshold

pub struct GateNode {
    threshold_db: f32,
    range_db: f32,
    attack_ms: f32,
    release_ms: f32,
    attack_coef: f32,
    release_coef: f32,
    env_db: f32, // current gain reduction (dB, <= 0)
    bypass: bool,
    sr: f64,
}

impl GateNode {
    pub fn new() -> Self {
        let mut g = GateNode {
            threshold_db: -45.0,
            range_db: 60.0,
            attack_ms: 2.0,
            release_ms: 120.0,
            attack_coef: 1.0,
            release_coef: 1.0,
            env_db: 0.0,
            bypass: true,
            sr: 48_000.0,
        };
        g.recompute_ballistics();
        g
    }

    fn recompute_ballistics(&mut self) {
        let a = self.attack_ms.max(0.01) * 0.001 * self.sr as f32;
        let r = self.release_ms.max(0.01) * 0.001 * self.sr as f32;
        self.attack_coef = 1.0 - (-1.0 / a).exp();
        self.release_coef = 1.0 - (-1.0 / r).exp();
    }
}

impl Default for GateNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for GateNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        self.recompute_ballistics();
        self.env_db = 0.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the held gain-reduction envelope carries across the discontinuity and
        // recovers only over the attack/release time — an audible momentary wrong
        // level right after a seek. Matches CompressorNode/DeEsserNode::reset.
        self.env_db = 0.0;
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
            let peak = l.abs().max(r.abs());
            let x_db = 20.0 * (peak + 1.0e-12).log10();

            // Below threshold → expand downward; clamp to -range.
            let target_db = if x_db >= self.threshold_db {
                0.0
            } else {
                ((x_db - self.threshold_db) * RATIO).max(-self.range_db)
            };
            // Opening (less reduction) is the attack direction.
            let coef = if target_db > self.env_db {
                self.attack_coef
            } else {
                self.release_coef
            };
            self.env_db += (target_db - self.env_db) * coef;

            let g = db_to_lin(self.env_db);
            out[CHANNELS * i] = l * g;
            out[CHANNELS * i + 1] = r * g;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::THRESHOLD_DB => self.threshold_db = value,
            param::RANGE_DB => self.range_db = value.max(0.0),
            param::ATTACK_MS => {
                self.attack_ms = value;
                self.recompute_ballistics();
            }
            param::RELEASE_MS => {
                self.release_ms = value;
                self.recompute_ballistics();
            }
            param::BYPASS => self.bypass = value >= 0.5,
            _ => {}
        }
    }
}
