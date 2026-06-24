//! Master node: the final summing bus. A single smoothed master gain applied equally to
//! both channels (true identity at unity — no pan law), with an optional gentle soft-clip
//! limiter to tame peaks past 0 dBFS. This is what every track/return edges into.

use crate::dsp::smooth::Smoothed;
use crate::node::{Node, ProcessIo};
use crate::sample::CHANNELS;

pub mod param {
    pub const GAIN: u16 = 0; // linear
}

pub struct MasterNode {
    gain: Smoothed,
    soft_clip: bool,
}

impl MasterNode {
    pub fn new(gain: f32, soft_clip: bool) -> Self {
        MasterNode {
            gain: Smoothed::new(gain),
            soft_clip,
        }
    }

    /// Bit-exact pass-through (gain 1.0, no limiting) — the identity sum bus.
    pub fn unity() -> Self {
        MasterNode::new(1.0, false)
    }

    /// Unity with the soft-clip safety limiter engaged.
    pub fn limited() -> Self {
        MasterNode::new(1.0, true)
    }
}

/// Cubic soft clip: identity for |x| <= 1/3, smoothly saturating toward ±2/3, hard-limited
/// beyond |x| >= 1 (a classic gentle waveshaper). Leaves normal-level material untouched.
#[inline]
fn soft_clip(x: f32) -> f32 {
    if x <= -1.0 {
        -2.0 / 3.0
    } else if x >= 1.0 {
        2.0 / 3.0
    } else {
        x - x * x * x / 3.0
    }
}

impl Node for MasterNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.gain.set_time(0.01, sample_rate);
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
        let clip = self.soft_clip;
        for i in 0..frames {
            let g = self.gain.next();
            let mut l = out[CHANNELS * i] * g;
            let mut r = out[CHANNELS * i + 1] * g;
            if clip {
                l = soft_clip(l);
                r = soft_clip(r);
            }
            out[CHANNELS * i] = l;
            out[CHANNELS * i + 1] = r;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        if param == param::GAIN {
            self.gain.set_target(value);
        }
    }
}
