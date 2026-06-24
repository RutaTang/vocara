//! Vocal doubler / chorus: two short LFO-modulated delay lines (L/R out of phase) that
//! thicken and widen the voice. Fractional read with linear interpolation; no feedback.
//! Buffers sized in `prepare`.

use crate::node::{Node, ProcessIo};
use crate::sample::{Smp, CHANNELS};

pub mod param {
    pub const DELAY_MS: u16 = 0; // base delay
    pub const DEPTH_MS: u16 = 1; // modulation depth
    pub const RATE_HZ: u16 = 2; // LFO rate
    pub const MIX: u16 = 3; // 0..1 wet
    pub const BYPASS: u16 = 4;
}

const MAX_SECONDS: f64 = 0.1;
const TWO_PI: f32 = std::f32::consts::PI * 2.0;

pub struct DoublerNode {
    buf_l: Vec<Smp>,
    buf_r: Vec<Smp>,
    pos: usize,
    phase_l: f32,
    phase_r: f32,
    delay_ms: f32,
    depth_ms: f32,
    rate_hz: f32,
    mix: f32,
    bypass: bool,
    sr: f64,
}

impl DoublerNode {
    pub fn new() -> Self {
        let len = (48_000.0 * MAX_SECONDS) as usize;
        DoublerNode {
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            pos: 0,
            phase_l: 0.0,
            phase_r: std::f32::consts::PI, // L/R out of phase for width
            delay_ms: 18.0,
            depth_ms: 4.0,
            rate_hz: 0.6,
            mix: 0.0,
            bypass: true,
            sr: 48_000.0,
        }
    }

    #[inline]
    fn read(buf: &[Smp], pos: usize, delay_samples: f32) -> Smp {
        let len = buf.len();
        let d = delay_samples.clamp(1.0, (len - 2) as f32);
        let base = d.floor();
        let frac = d - base;
        let i0 = (pos + len - base as usize) % len;
        let i1 = (i0 + len - 1) % len;
        buf[i0] * (1.0 - frac) + buf[i1] * frac
    }
}

impl Default for DoublerNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for DoublerNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        let len = (sample_rate * MAX_SECONDS) as usize;
        self.buf_l = vec![0.0; len];
        self.buf_r = vec![0.0; len];
        self.pos = 0;
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
        let len = self.buf_l.len();
        if len < 4 {
            return;
        }
        let mix = self.mix.clamp(0.0, 1.0);
        let base = (self.delay_ms as f64 * 0.001 * self.sr) as f32;
        let depth = (self.depth_ms as f64 * 0.001 * self.sr) as f32;
        let inc = TWO_PI * self.rate_hz / self.sr as f32;

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            let dl = base + depth * self.phase_l.sin();
            let dr = base + depth * self.phase_r.sin();
            let wl = Self::read(&self.buf_l, self.pos, dl);
            let wr = Self::read(&self.buf_r, self.pos, dr);
            self.buf_l[self.pos] = l;
            self.buf_r[self.pos] = r;
            // Keep the dry mostly intact; the wet copy thickens / widens.
            out[CHANNELS * i] = l * (1.0 - 0.5 * mix) + wl * mix;
            out[CHANNELS * i + 1] = r * (1.0 - 0.5 * mix) + wr * mix;
            self.pos = (self.pos + 1) % len;
            self.phase_l += inc;
            self.phase_r += inc;
            if self.phase_l >= TWO_PI {
                self.phase_l -= TWO_PI;
            }
            if self.phase_r >= TWO_PI {
                self.phase_r -= TWO_PI;
            }
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::DELAY_MS => self.delay_ms = value.clamp(1.0, 50.0),
            param::DEPTH_MS => self.depth_ms = value.clamp(0.0, 20.0),
            param::RATE_HZ => self.rate_hz = value.clamp(0.05, 8.0),
            param::MIX => self.mix = value.clamp(0.0, 1.0),
            param::BYPASS => self.bypass = value >= 0.5,
            _ => {}
        }
    }

    fn reset(&mut self) {
        // Seek clears the modulated delay lines and resets the LFO phase so the
        // doubler doesn't smear pre-jump audio across the discontinuity.
        for s in self.buf_l.iter_mut() {
            *s = 0.0;
        }
        for s in self.buf_r.iter_mut() {
            *s = 0.0;
        }
        self.pos = 0;
        self.phase_l = 0.0;
        self.phase_r = std::f32::consts::PI;
    }
}
