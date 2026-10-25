//! A simple stereo feedback delay (echo / throw). Per-channel ring buffer; the delayed signal
//! is fed back (clamped < 1) and mixed wet/dry. Buffers are sized in `prepare` (control thread),
//! never on the audio callback.

use crate::dsp::denormals::sanitize;
use crate::dsp::smooth::{BypassFade, Smoothed};
use crate::node::{Node, ProcessIo};
use crate::sample::{Smp, CHANNELS};

pub mod param {
    pub const TIME_MS: u16 = 0;
    pub const FEEDBACK: u16 = 1; // 0..1
    pub const MIX: u16 = 2; // 0..1 wet
    pub const BYPASS: u16 = 3;
}

const MAX_SECONDS: f64 = 2.0;

/// How long a Time change cross-fades from the old read tap to the new one.
const TAP_FADE_S: f64 = 0.01;

pub struct DelayNode {
    buf_l: Vec<Smp>,
    buf_r: Vec<Smp>,
    pos: usize,
    /// Ring entries still holding pre-bypass audio, counted down while bypassed as process()
    /// progressively overwrites them with silence (see the bypass branch). Re-armed to the
    /// full line length on every active→bypassed transition.
    stale: usize,
    /// Whether the ring may hold non-zero audio: set by the active path's writes, cleared
    /// once a bypass has swept the whole line (or a reset has). A clean line lets reset()
    /// skip its O(line) clear — a seek runs it on the audio thread for every vocal clip's
    /// delay (~0.75 MB each), and most of them were never switched on.
    dirty: bool,
    time_ms: f32,
    /// The read tap (frames behind the write head) the output currently plays, and the one
    /// a Time change is cross-fading to (equal when no fade is running). Jumping the tap
    /// clicked on every tick of a Time drag; one fade runs at a time, and a drag's newer
    /// value starts the next fade once this one lands.
    tap: usize,
    tap_next: usize,
    tap_mix: f32,
    tap_step: f32,
    feedback: f32,
    mix: Smoothed,
    fade: BypassFade,
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
            dirty: false,
            time_ms: 300.0,
            tap: 14_400, // 300 ms at 48 kHz; prepare re-derives it
            tap_next: 14_400,
            tap_mix: 0.0,
            tap_step: 1.0 / (TAP_FADE_S * 48_000.0) as f32,
            feedback: 0.3,
            mix: Smoothed::new(0.0),
            fade: BypassFade::new(true),
            sr: 48_000.0,
        }
    }

    /// The read tap for the current Time setting.
    fn wanted_tap(&self) -> usize {
        let len = self.buf_l.len().max(2);
        ((self.time_ms as f64 * 0.001 * self.sr) as usize).clamp(1, len - 1)
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
        self.dirty = false;
        self.tap = self.wanted_tap();
        self.tap_next = self.tap;
        self.tap_step = 1.0 / (TAP_FADE_S * sample_rate) as f32;
        self.mix.set_time(0.01, sample_rate);
        self.fade.prepare(sample_rate);
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
            if self.stale == 0 {
                self.dirty = false; // the sweep has covered the whole line
            }
            return;
        }

        let len = self.buf_l.len();
        if len == 0 {
            return;
        }
        self.dirty = true;
        let want = self.wanted_tap();
        if self.tap_next == self.tap && want != self.tap {
            self.tap_next = want;
            self.tap_mix = 0.0;
        }
        let fb = self.feedback.clamp(0.0, 0.98);

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            let read = (self.pos + len - self.tap) % len;
            let (mut dl, mut dr) = (self.buf_l[read], self.buf_r[read]);
            if self.tap_next != self.tap {
                let next = (self.pos + len - self.tap_next) % len;
                dl += (self.buf_l[next] - dl) * self.tap_mix;
                dr += (self.buf_r[next] - dr) * self.tap_mix;
                self.tap_mix += self.tap_step;
                if self.tap_mix >= 1.0 {
                    self.tap = self.tap_next;
                }
            }
            // Sanitize the feedback write (like reverb/biquad/compressor/gate): one non-finite
            // input would otherwise re-inject NaN*fb into the ring every `d` samples forever.
            self.buf_l[self.pos] = sanitize(l + dl * fb);
            self.buf_r[self.pos] = sanitize(r + dr * fb);
            // The wet share, blended toward the dry input while the bypass switch fades.
            let wet = self.mix.next() * self.fade.next();
            out[CHANNELS * i] = l + (dl - l) * wet;
            out[CHANNELS * i + 1] = r + (dr - r) * wet;
            self.pos = (self.pos + 1) % len;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::TIME_MS => self.time_ms = value.clamp(1.0, (MAX_SECONDS * 1000.0) as f32),
            param::FEEDBACK => self.feedback = value.clamp(0.0, 0.98),
            param::MIX => self.mix.set_target(value.clamp(0.0, 1.0)),
            param::BYPASS => {
                let bypass = value >= 0.5;
                if bypass && !self.fade.bypassed() {
                    // Entering bypass: schedule the progressive ring clear (see process),
                    // which starts once the switch has faded out.
                    self.stale = self.buf_l.len();
                }
                self.fade.set_bypass(bypass);
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        // Seek clears the echo buffer so pre-jump audio (up to MAX_SECONDS, plus
        // accumulated feedback) doesn't bleed across the discontinuity. A line known to be
        // silent is left alone (see `dirty`).
        if self.dirty {
            for s in self.buf_l.iter_mut() {
                *s = 0.0;
            }
            for s in self.buf_r.iter_mut() {
                *s = 0.0;
            }
        }
        self.pos = 0;
        self.stale = 0;
        self.dirty = false;
        self.tap = self.tap_next; // the line is silent: land any Time fade at once
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_block(d: &mut DelayNode, input: &[Smp]) {
        let mut out = vec![0.0; input.len()];
        let inputs: [&[Smp]; 1] = [input];
        let mut outs: [&mut [Smp]; 1] = [&mut out[..]];
        let mut io = ProcessIo {
            inputs: &inputs,
            outputs: &mut outs[..],
            frames: input.len() / CHANNELS,
            tl: 0,
            sr: 48_000.0,
        };
        d.process(&mut io);
    }

    #[test]
    fn a_seek_does_not_sweep_a_line_nothing_wrote() {
        // reset() runs on the audio thread for every vocal clip's delay on every seek (a
        // ruler drag seeks per pointer move), and a never-enabled delay's ~0.75 MB line is
        // already silent. Plant a sentinel where only a sweep would look: it must survive.
        let mut d = DelayNode::new();
        d.prepare(48_000.0, 256);
        run_block(&mut d, &[0.5; 512]); // bypassed: nothing reaches the line
        d.buf_l[123] = 1.0;
        d.reset();
        assert_eq!(d.buf_l[123], 1.0, "reset swept a line nothing had written");

        // A line the active path wrote IS cleared.
        d.buf_l[123] = 0.0;
        d.set_param(param::BYPASS, 0.0);
        run_block(&mut d, &[0.5; 512]);
        d.reset();
        assert!(
            d.buf_l.iter().chain(d.buf_r.iter()).all(|&s| s == 0.0),
            "reset must clear a line that was written"
        );

        // A bypass that has fully swept the line leaves it clean again (the sweep starts
        // once the switch has faded out).
        d.set_param(param::BYPASS, 0.0);
        run_block(&mut d, &[0.5; 512]);
        d.set_param(param::BYPASS, 1.0);
        for _ in 0..d.buf_l.len() / 256 + 2 {
            run_block(&mut d, &[0.0; 256 * CHANNELS]);
        }
        d.buf_l[123] = 1.0;
        d.reset();
        assert_eq!(
            d.buf_l[123], 1.0,
            "reset swept a line the bypass had cleaned"
        );
    }
}
