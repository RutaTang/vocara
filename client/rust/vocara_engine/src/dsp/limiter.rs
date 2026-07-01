//! A simple brick-wall peak limiter: instantaneous gain reduction to hold the output at the
//! ceiling, with a smooth release. No look-ahead (brief inter-sample overs possible), so it's
//! cheap and stable — good as the last insert on a vocal or a bus.

use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const CEILING_DB: u16 = 0; // output ceiling (dB, <= 0)
    pub const RELEASE_MS: u16 = 1;
    pub const BYPASS: u16 = 2;
}

pub struct LimiterNode {
    ceiling: f32, // linear
    release_ms: f32,
    release_coef: f32,
    gain: f32, // current gain (<= 1)
    bypass: bool,
    sr: f64,
}

impl LimiterNode {
    pub fn new() -> Self {
        let mut l = LimiterNode {
            ceiling: db_to_lin(-0.3),
            release_ms: 80.0,
            release_coef: 1.0,
            gain: 1.0,
            bypass: true,
            sr: 48_000.0,
        };
        l.recompute_release();
        l
    }

    fn recompute_release(&mut self) {
        let r = self.release_ms.max(1.0) * 0.001 * self.sr as f32;
        self.release_coef = 1.0 - (-1.0 / r).exp();
    }
}

impl Default for LimiterNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for LimiterNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        self.recompute_release();
        self.gain = 1.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the instantaneously-clamped gain carries across the discontinuity and
        // recovers only over the release time — an audible dropout right after a
        // seek. Matches CompressorNode/DeEsserNode::reset.
        self.gain = 1.0;
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
            // Gain needed so the peak doesn't exceed the ceiling.
            let req = if peak > 1.0e-9 {
                (self.ceiling / peak).min(1.0)
            } else {
                1.0
            };
            // Instant attack (clamp down now), smooth release (recover gradually).
            if req < self.gain {
                self.gain = req;
            } else {
                self.gain += (req - self.gain) * self.release_coef;
            }
            out[CHANNELS * i] = l * self.gain;
            out[CHANNELS * i + 1] = r * self.gain;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::CEILING_DB => self.ceiling = db_to_lin(value.min(0.0)),
            param::RELEASE_MS => {
                self.release_ms = value;
                self.recompute_release();
            }
            param::BYPASS => self.bypass = value >= 0.5,
            _ => {}
        }
    }
}
