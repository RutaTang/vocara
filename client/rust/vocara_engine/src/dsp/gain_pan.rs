//! Gain + equal-power pan node. The per-channel target gains are smoothed directly (so the
//! cos/sin pan law is evaluated only on parameter change, not per sample), giving
//! click-free level and pan moves.

use crate::dsp::smooth::Smoothed;
use crate::node::{Node, ProcessIo};
use crate::sample::{equal_power_pan, CHANNELS};

pub mod param {
    pub const GAIN: u16 = 0; // linear
    pub const PAN: u16 = 1; // [-1, 1]
}

pub struct GainPanNode {
    gain: f32,
    pan: f32,
    gl: Smoothed,
    gr: Smoothed,
}

impl GainPanNode {
    pub fn new(gain: f32, pan: f32) -> Self {
        let (pl, pr) = equal_power_pan(pan);
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
        let (pl, pr) = equal_power_pan(self.pan);
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
            Some(inp) => {
                for i in 0..frames {
                    let gl = self.gl.next();
                    let gr = self.gr.next();
                    out[CHANNELS * i] = inp[CHANNELS * i] * gl;
                    out[CHANNELS * i + 1] = inp[CHANNELS * i + 1] * gr;
                }
            }
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

/// Pan-only node (equal-power, no gain). Used downstream of a scalar gain so a reverb send
/// can tap the post-gain / pre-pan signal (wet arrives centered, gets its image from the
/// reverb's own width).
pub struct PanNode {
    pan: f32,
    gl: Smoothed,
    gr: Smoothed,
}

impl PanNode {
    pub fn new(pan: f32) -> Self {
        let (l, r) = equal_power_pan(pan);
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
            Some(inp) => {
                for i in 0..frames {
                    let gl = self.gl.next();
                    let gr = self.gr.next();
                    out[CHANNELS * i] = inp[CHANNELS * i] * gl;
                    out[CHANNELS * i + 1] = inp[CHANNELS * i + 1] * gr;
                }
            }
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
            let (l, r) = equal_power_pan(self.pan);
            self.gl.set_target(l);
            self.gr.set_target(r);
        }
    }
}
