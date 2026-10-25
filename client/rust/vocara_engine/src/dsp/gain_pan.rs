//! Gain + stereo-balance pan node. The per-channel target gains are smoothed directly (so
//! the pan law is evaluated only on parameter change, not per sample), giving click-free
//! level and pan moves.

use crate::dsp::smooth::Smoothed;
use crate::node::{Node, ProcessIo};
use crate::sample::CHANNELS;

pub mod param {
    pub const GAIN: u16 = 0; // linear
    pub const PAN: u16 = 1; // [-1, 1]
}

/// Attenuate-only stereo balance law. `pan` in [-1, 1]; returns (left_gain, right_gain).
/// Center is UNITY on both channels; panning attenuates ONLY the opposite channel along a
/// cosine taper (a hard side fully mutes it). Unity center is the standard DAW balance
/// contract and what the graph topology requires: every routing hop inserts one pan node
/// (track → master, track → bus → master, ...), so a -3 dB-at-center law would stack one
/// -3 dB per hop — bus-routed tracks 3 dB under direct ones, nested buses 6 dB — and leave
/// the un-panned shared reverb return 3 dB hot relative to its sends. Attenuate-only gains
/// are always <= 1, so this law can never clip.
#[inline]
fn balance(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    let l = (p.max(0.0) * core::f32::consts::FRAC_PI_2).cos();
    let r = ((-p).max(0.0) * core::f32::consts::FRAC_PI_2).cos();
    (l, r)
}

/// `out = inp * (gl, gr)` per frame, the two gains ramping per sample — or, once both have
/// landed (see [`Smoothed::settled`]), applied as the constants every ramp step would return.
#[inline]
fn apply_gains(gl: &mut Smoothed, gr: &mut Smoothed, inp: &[f32], out: &mut [f32]) {
    let frames = out.len() / CHANNELS;
    if let (Some(l), Some(r)) = (gl.settled(frames), gr.settled(frames)) {
        for (o, x) in out
            .chunks_exact_mut(CHANNELS)
            .zip(inp.chunks_exact(CHANNELS))
        {
            o[0] = x[0] * l;
            o[1] = x[1] * r;
        }
        return;
    }
    for i in 0..frames {
        let l = gl.next();
        let r = gr.next();
        out[CHANNELS * i] = inp[CHANNELS * i] * l;
        out[CHANNELS * i + 1] = inp[CHANNELS * i + 1] * r;
    }
}

pub struct GainPanNode {
    gain: f32,
    pan: f32,
    gl: Smoothed,
    gr: Smoothed,
}

impl GainPanNode {
    pub fn new(gain: f32, pan: f32) -> Self {
        let (pl, pr) = balance(pan);
        GainPanNode {
            gain,
            pan,
            gl: Smoothed::new(gain * pl),
            gr: Smoothed::new(gain * pr),
        }
    }

    pub fn unity() -> Self {
        GainPanNode::new(1.0, 0.0)
    }

    fn update_targets(&mut self) {
        let (pl, pr) = balance(self.pan);
        self.gl.set_target(self.gain * pl);
        self.gr.set_target(self.gain * pr);
    }
}

impl Node for GainPanNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.gl.set_time(0.01, sample_rate);
        self.gr.set_time(0.01, sample_rate);
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let frames = io.frames;
        let n = frames * CHANNELS;
        let inp = io.inputs.first().copied();
        let out = &mut *io.outputs[0];
        match inp {
            Some(inp) => apply_gains(&mut self.gl, &mut self.gr, &inp[..n], &mut out[..n]),
            None => {
                for s in &mut out[..n] {
                    *s = 0.0;
                }
            }
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::GAIN => self.gain = value,
            param::PAN => self.pan = value.clamp(-1.0, 1.0),
            _ => return,
        }
        self.update_targets();
    }
}

/// Pan-only node (attenuate-only balance, no gain). Used downstream of a scalar gain so a
/// reverb send can tap the post-gain / pre-pan signal (wet arrives centered, gets its image
/// from the reverb's own width). Center-unity keeps the un-panned reverb return level-matched
/// with the dry path it was sent from.
pub struct PanNode {
    pan: f32,
    gl: Smoothed,
    gr: Smoothed,
}

impl PanNode {
    pub fn new(pan: f32) -> Self {
        let (l, r) = balance(pan);
        PanNode {
            pan,
            gl: Smoothed::new(l),
            gr: Smoothed::new(r),
        }
    }

    pub fn center() -> Self {
        PanNode::new(0.0)
    }
}

impl Node for PanNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.gl.set_time(0.01, sample_rate);
        self.gr.set_time(0.01, sample_rate);
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let frames = io.frames;
        let n = frames * CHANNELS;
        let inp = io.inputs.first().copied();
        let out = &mut *io.outputs[0];
        match inp {
            Some(inp) => apply_gains(&mut self.gl, &mut self.gr, &inp[..n], &mut out[..n]),
            None => {
                for s in &mut out[..n] {
                    *s = 0.0;
                }
            }
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        if param == param::PAN {
            self.pan = value.clamp(-1.0, 1.0);
            let (l, r) = balance(self.pan);
            self.gl.set_target(l);
            self.gr.set_target(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::test_support::assert_matches_reference;

    /// The loop both nodes used before the settled-gains path: both gains stepped per frame.
    fn reference(gl: &mut Smoothed, gr: &mut Smoothed, inp: &[f32], out: &mut [f32]) {
        for i in 0..inp.len() / CHANNELS {
            let l = gl.next();
            let r = gr.next();
            out[CHANNELS * i] = inp[CHANNELS * i] * l;
            out[CHANNELS * i + 1] = inp[CHANNELS * i + 1] * r;
        }
    }

    #[test]
    fn landed_gains_scale_exactly_like_the_per_sample_ramps() {
        let edits = [
            (2, param::PAN, 0.6),
            (8, param::PAN, -1.0),
            (8, param::GAIN, 0.25),
            (25, param::PAN, 0.0),
            (26, param::GAIN, 1.5),
        ];
        let mut fast = PanNode::new(-0.2);
        let mut slow = PanNode::new(-0.2);
        fast.prepare(48_000.0, 1024);
        slow.prepare(48_000.0, 1024);
        let pan_ref =
            |n: &mut PanNode, i: &[f32], o: &mut [f32]| reference(&mut n.gl, &mut n.gr, i, o);
        assert_matches_reference(&mut fast, &mut slow, pan_ref, &edits, 50);

        let mut fast = GainPanNode::new(0.9, 0.3);
        let mut slow = GainPanNode::new(0.9, 0.3);
        fast.prepare(48_000.0, 1024);
        slow.prepare(48_000.0, 1024);
        let gp_ref =
            |n: &mut GainPanNode, i: &[f32], o: &mut [f32]| reference(&mut n.gl, &mut n.gr, i, o);
        assert_matches_reference(&mut fast, &mut slow, gp_ref, &edits, 50);
    }
}
