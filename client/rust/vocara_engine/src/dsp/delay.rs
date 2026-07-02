//! A simple stereo feedback delay (echo / throw). Per-channel ring buffer; the delayed signal
//! is fed back (clamped < 1) and mixed wet/dry. Buffers are sized in `prepare` (control thread),
//! never on the audio callback.

use crate::node::{Node, ProcessIo};
use crate::sample::{Smp, CHANNELS};

pub mod param {
    pub const TIME_MS: u16 = 0;
    pub const FEEDBACK: u16 = 1; // 0..1
    pub const MIX: u16 = 2; // 0..1 wet
    pub const BYPASS: u16 = 3;
}

const MAX_SECONDS: f64 = 2.0;

pub struct DelayNode {
    buf_l: Vec<Smp>,
    buf_r: Vec<Smp>,
    pos: usize,
    /// Ring entries still holding pre-bypass audio, counted down while bypassed as process()
    /// progressively overwrites them with silence (see the bypass branch). Re-armed to the
    /// full line length on every active→bypassed transition.
    stale: usize,
    time_ms: f32,
    feedback: f32,
    mix: f32,
    bypass: bool,
    sr: f64,
}

impl DelayNode {
    pub fn new() -> Self {
        let len = (48_000.0 * MAX_SECONDS) as usize;
        DelayNode {
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            pos: 0,
            stale: 0,
            time_ms: 300.0,
            feedback: 0.3,
            mix: 0.0,
            bypass: true,
            sr: 48_000.0,
        }
    }
}

impl Default for DelayNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for DelayNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        let len = (sample_rate * MAX_SECONDS) as usize;
        self.buf_l = vec![0.0; len];
        self.buf_r = vec![0.0; len];
        self.pos = 0;
        self.stale = 0; // freshly zeroed line
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
            // Progressively overwrite the ring with silence while bypassed (bounded per-block
            // work — set_param runs on the audio thread, so an unbounded clear there would
            // violate RT safety). After one full line length of bypassed frames the ring is
            // clean, so re-activating never replays pre-bypass audio as a one-shot ghost
            // echo; a shorter toggle keeps natural tail continuity (the un-cleared region is
            // exactly what a never-bypassed line would still be reading).
            let len = self.buf_l.len();
            if len == 0 || self.stale == 0 {
                return;
            }
            let clear = frames.min(self.stale);
            for _ in 0..clear {
                self.buf_l[self.pos] = 0.0;
                self.buf_r[self.pos] = 0.0;
                self.pos = (self.pos + 1) % len;
            }
            self.stale -= clear;
            return;
        }

        let len = self.buf_l.len();
        if len == 0 {
            return;
        }
        let d = ((self.time_ms as f64 * 0.001 * self.sr) as usize).clamp(1, len - 1);
        let fb = self.feedback.clamp(0.0, 0.98);
        let mix = self.mix.clamp(0.0, 1.0);

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            let read = (self.pos + len - d) % len;
            let dl = self.buf_l[read];
            let dr = self.buf_r[read];
            self.buf_l[self.pos] = l + dl * fb;
            self.buf_r[self.pos] = r + dr * fb;
            out[CHANNELS * i] = l * (1.0 - mix) + dl * mix;
            out[CHANNELS * i + 1] = r * (1.0 - mix) + dr * mix;
            self.pos = (self.pos + 1) % len;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::TIME_MS => self.time_ms = value.clamp(1.0, (MAX_SECONDS * 1000.0) as f32),
            param::FEEDBACK => self.feedback = value.clamp(0.0, 0.98),
            param::MIX => self.mix = value.clamp(0.0, 1.0),
            param::BYPASS => {
                let bypass = value >= 0.5;
                if bypass && !self.bypass {
                    // Entering bypass: schedule the progressive ring clear (see process).
                    self.stale = self.buf_l.len();
                }
                self.bypass = bypass;
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        // Seek clears the echo buffer so pre-jump audio (up to MAX_SECONDS, plus
        // accumulated feedback) doesn't bleed across the discontinuity.
        for s in self.buf_l.iter_mut() {
            *s = 0.0;
        }
        for s in self.buf_r.iter_mut() {
            *s = 0.0;
        }
        self.pos = 0;
        self.stale = 0;
    }
}
