//! A simple brick-wall peak limiter: instantaneous gain reduction to hold the output at the
//! ceiling, with a smooth release. No look-ahead (brief inter-sample overs possible), so it's
//! cheap and stable — good as the last insert on a vocal or a bus.

use crate::dsp::smooth::{BypassFade, Smoothed};
use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const CEILING_DB: u16 = 0; // output ceiling (dB, <= 0)
    pub const RELEASE_MS: u16 = 1;
    pub const BYPASS: u16 = 2;
}

pub struct LimiterNode {
    /// Linear, ramped: with the instant attack, a ceiling stepped down while the signal sat
    /// above it cut the gain in one sample (0.9 to 0.25 for a -12 dB drop at a peak).
    ceiling: Smoothed,
    release_ms: f32,
    release_coef: f32,
    gain: f32, // current gain (<= 1)
    fade: BypassFade,
    sr: f64,
}

impl LimiterNode {
    pub fn new() -> Self {
        let mut l = LimiterNode {
            ceiling: Smoothed::new(db_to_lin(-0.3)),
            release_ms: 80.0,
            release_coef: 1.0,
            gain: 1.0,
            fade: BypassFade::new(true),
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
        self.ceiling.set_time(0.01, sample_rate);
        self.fade.prepare(sample_rate);
        self.gain = 1.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the instantaneously-clamped gain carries across the discontinuity and
        // recovers only over the release time — an audible dropout right after a
        // seek. Matches CompressorNode/DeEsserNode::reset.
        self.gain = 1.0;
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

        let ceiling_held = self.ceiling.settled(frames);
        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            let peak = l.abs().max(r.abs());
            let ceiling = ceiling_held.unwrap_or_else(|| self.ceiling.next());
            // Gain needed so the peak doesn't exceed the ceiling.
            let req = if peak > 1.0e-9 {
                (ceiling / peak).min(1.0)
            } else {
                1.0
            };
            // Instant attack (clamp down now), smooth release (recover gradually).
            if req < self.gain {
                self.gain = req;
            } else {
                self.gain += (req - self.gain) * self.release_coef;
            }
            // Blended toward unity while the bypass switch fades.
            let g = 1.0 + (self.gain - 1.0) * self.fade.next();
            out[CHANNELS * i] = l * g;
            out[CHANNELS * i + 1] = r * g;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::CEILING_DB => self.ceiling.set_target(db_to_lin(value.min(0.0))),
            param::RELEASE_MS => {
                self.release_ms = value;
                self.recompute_release();
            }
            param::BYPASS => {
                let bypass = value >= 0.5;
                // The gain reduction froze with whatever the material was doing when the
                // node went fully bypassed; switched back on over a quieter passage it
                // ducked that by the stale state for a release time. Start idle, as
                // after a seek, like the compressor. (Switched back on mid-fade-out,
                // the state is still live and is kept.)
                if !bypass && self.fade.idle() {
                    self.reset();
                }
                self.fade.set_bypass(bypass);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowering_the_ceiling_at_a_peak_does_not_step_the_output() {
        // With the instant attack, a ceiling stepped down while the signal sat above it cut
        // the gain in one sample: 0.9 to 0.25 for a -12 dB drop at a peak.
        let (sr, block) = (48_000.0, 128usize);
        let mut l = LimiterNode::new();
        l.prepare(sr, block);
        l.set_param(param::BYPASS, 0.0);
        l.set_param(param::CEILING_DB, 0.0);
        let total = 48_000usize;
        let input: Vec<f32> = (0..total)
            .flat_map(|i| {
                let s = 0.9 * (i as f32 * 220.0 * std::f32::consts::TAU / sr as f32).sin();
                [s, s]
            })
            .collect();
        // The block whose first frame sits on a peak (220 Hz: a peak every ~218 frames).
        let edit = (24_000 / block * block..total - block)
            .step_by(block)
            .max_by(|&a, &b| input[2 * a].abs().total_cmp(&input[2 * b].abs()))
            .unwrap();
        let mut out = vec![0.0; total * CHANNELS];
        let mut at = 0;
        while at < total {
            if at == edit {
                l.set_param(param::CEILING_DB, -12.0);
            }
            let n = block.min(total - at);
            let inputs: [&[f32]; 1] = [&input[at * CHANNELS..(at + n) * CHANNELS]];
            let mut outs: [&mut [f32]; 1] = [&mut out[at * CHANNELS..(at + n) * CHANNELS]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut outs[..],
                frames: n,
                tl: at as i64,
                sr,
            };
            l.process(&mut io);
            at += n;
        }
        let step = |v: &[f32], i: usize| (v[2 * i] - v[2 * (i - 1)]).abs();
        let material = (1..total).map(|i| step(&input, i)).fold(0.0, f32::max);
        let around = (edit.saturating_sub(4)..edit + 4 * block)
            .map(|i| step(&out, i))
            .fold(0.0, f32::max);
        assert!(
            around < material * 1.5,
            "a step of {around} at the edit; the material's own largest is {material}"
        );
        // And it does come down to the new ceiling.
        let ceiling = db_to_lin(-12.0);
        let tail = out[(total - 4_000) * 2..]
            .iter()
            .fold(0.0f32, |m, &x| m.max(x.abs()));
        assert!(
            tail <= ceiling * 1.0001,
            "settled at {tail}, ceiling {ceiling}"
        );
    }
}
