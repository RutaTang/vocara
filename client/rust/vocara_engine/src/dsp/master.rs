//! Master node: the final summing bus. A single smoothed master gain applied equally to
//! both channels (true identity at unity — no pan law), with an optional soft-clip safety
//! ceiling that leaves everything below -3 dBFS untouched and tames peaks toward 0 dBFS.
//! This is what every track/return edges into.

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

/// Where the safety clip starts bending: -3 dBFS. Everything below passes bit-exact, so a
/// mix (or a mastered backing track) that never reaches the ceiling is never coloured.
const CLIP_KNEE: f32 = 0.707_945_8; // 10^(-3/20)

/// Soft clip with a 0 dBFS ceiling: exactly linear up to [`CLIP_KNEE`], then
/// `knee + (1 - knee) * tanh((|x| - knee) / (1 - knee))` above it. Value and slope match the
/// linear segment at the knee (C¹, no corner), and the curve approaches but never exceeds
/// ±1.0. The cubic `K * (u - u^3/3)` this replaced had no linear region at all — ~1% THD on
/// a -6 dBFS mix and ~4% on a track mastered to -0.1 dBFS, on every playback.
#[inline]
fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    // A NaN passes through unchanged, as it always has (it is not this stage's job to
    // invent a full-scale sample from it).
    if a <= CLIP_KNEE || a.is_nan() {
        return x;
    }
    let span = 1.0 - CLIP_KNEE;
    let y = CLIP_KNEE + span * ((a - CLIP_KNEE) / span).tanh();
    y.min(1.0).copysign(x)
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
