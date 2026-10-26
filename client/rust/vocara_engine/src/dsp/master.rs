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
pub(crate) fn soft_clip(x: f32) -> f32 {
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

/// `out + add` with `add` bent into the headroom `out` leaves toward the rail it pushes at, so
/// the sum never passes full scale: the live monitor summed onto a mix the master already
/// clipped. Exactly `out` when `add` is zero — running the sum through [`soft_clip`] again
/// clipped the backing a second time (a track mastered to -0.1 dBFS lost another 0.3 dB of
/// peak, ~4% THD instead of ~2.5%) whenever the monitor was armed, sung into or not. Linear
/// while `add` stays within the knee's share of that headroom, then the same tanh curve the
/// master uses, scaled to it; with a silent `out` it IS [`soft_clip`] of `add`.
#[inline]
pub(crate) fn add_within_full_scale(out: f32, add: f32) -> f32 {
    if add == 0.0 || add.is_nan() {
        return out + add;
    }
    let a = add.abs();
    // Distance to the rail `add` pushes toward. None left (a mix over full scale, which only
    // an unlimited master leaves): the monitor adds nothing that way.
    let room = 1.0 - out * add.signum();
    if room <= 0.0 {
        return out;
    }
    let knee = CLIP_KNEE * room;
    let bent = if a <= knee {
        a
    } else {
        let span = room - knee;
        (knee + span * ((a - knee) / span).tanh()).min(room)
    };
    out + bent.copysign(add)
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
        let clip = self.soft_clip;
        // A landed gain (the steady state of every fader and send) is one value for the whole
        // block: scale straight from the input, the same products the ramp loop below forms.
        if let (Some(inp), Some(g)) = (inp, self.gain.settled(frames)) {
            if clip {
                for (o, &x) in out[..n].iter_mut().zip(&inp[..n]) {
                    *o = soft_clip(x * g);
                }
            } else {
                for (o, &x) in out[..n].iter_mut().zip(&inp[..n]) {
                    *o = x * g;
                }
            }
            return;
        }
        match inp {
            Some(inp) => out[..n].copy_from_slice(&inp[..n]),
            None => {
                for s in &mut out[..n] {
                    *s = 0.0;
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::test_support::assert_matches_reference;

    /// The loop `process` used before the settled-gain path: copy, then step the gain per
    /// sample.
    fn reference(m: &mut MasterNode, inp: &[f32], out: &mut [f32]) {
        out.copy_from_slice(inp);
        for i in 0..inp.len() / CHANNELS {
            let g = m.gain.next();
            let mut l = out[CHANNELS * i] * g;
            let mut r = out[CHANNELS * i + 1] * g;
            if m.soft_clip {
                l = soft_clip(l);
                r = soft_clip(r);
            }
            out[CHANNELS * i] = l;
            out[CHANNELS * i + 1] = r;
        }
    }

    #[test]
    fn a_landed_gain_scales_exactly_like_the_per_sample_ramp() {
        for clip in [false, true] {
            let mut fast = MasterNode::new(0.8, clip);
            let mut slow = MasterNode::new(0.8, clip);
            fast.prepare(48_000.0, 1024);
            slow.prepare(48_000.0, 1024);
            let edits = [
                (3, param::GAIN, 1.7),
                (9, param::GAIN, 0.0),
                (20, param::GAIN, 0.31),
                (21, param::GAIN, -0.5),
                (40, param::GAIN, 1.0),
            ];
            assert_matches_reference(&mut fast, &mut slow, reference, &edits, 60);
        }
    }

    #[test]
    fn the_monitor_sum_leaves_a_mix_it_adds_nothing_to_alone() {
        for i in -100..=100 {
            let out = i as f32 / 100.0;
            assert_eq!(add_within_full_scale(out, 0.0).to_bits(), out.to_bits());
            assert_eq!(add_within_full_scale(out, -0.0), out);
        }
    }

    #[test]
    fn the_monitor_sum_never_passes_full_scale_and_keeps_its_direction() {
        let mut worst = 0.0f32;
        for i in -100..=100 {
            let out = soft_clip(i as f32 / 50.0);
            for j in -400..=400 {
                let add = j as f32 / 100.0;
                let y = add_within_full_scale(out, add);
                worst = worst.max(y.abs());
                // Never against the direction it was pushed, never past where it was pushed.
                if add > 0.0 {
                    assert!(y >= out && y <= out + add, "{out} + {add} gave {y}");
                } else if add < 0.0 {
                    assert!(y <= out && y >= out + add, "{out} + {add} gave {y}");
                }
            }
        }
        assert!(worst <= 1.0, "peaked at {worst}");
    }

    #[test]
    fn the_monitor_sum_is_the_master_clip_over_silence_and_linear_below_it() {
        for j in -400..=400 {
            let add = j as f32 / 100.0;
            assert!((add_within_full_scale(0.0, add) - soft_clip(add)).abs() < 1e-6);
        }
        // Within the knee's share of the headroom, an exact sum.
        assert_eq!(add_within_full_scale(0.5, 0.25), 0.75);
        assert_eq!(add_within_full_scale(-0.5, -0.25), -0.75);
        // Continuous through the knee.
        let knee = CLIP_KNEE * 0.5;
        let below = add_within_full_scale(0.5, knee - 1e-4);
        let above = add_within_full_scale(0.5, knee + 1e-4);
        assert!((above - below).abs() < 3e-4);
    }
}
