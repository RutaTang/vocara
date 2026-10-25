//! Vocal doubler / chorus: two short LFO-modulated delay lines (L/R out of phase) that
//! thicken and widen the voice. Fractional read with linear interpolation; no feedback.
//! Buffers sized in `prepare`.

use crate::dsp::smooth::{BypassFade, Smoothed};
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

/// How long a Delay change cross-fades from the old base tap to the new one.
const BASE_FADE_S: f64 = 0.01;
/// The fastest the modulation depth may move (frames per frame). A read position moving at
/// this rate shifts the doubled copy's pitch by this fraction: 1.5%, a quarter of a semitone.
const DEPTH_SLEW: f32 = 0.015;

pub struct DoublerNode {
    buf_l: Vec<Smp>,
    buf_r: Vec<Smp>,
    pos: usize,
    /// Ring entries still holding pre-bypass audio, counted down while bypassed as process()
    /// progressively overwrites them with silence (mirrors DelayNode's ghost-echo guard).
    stale: usize,
    /// Whether the lines may hold non-zero audio (mirrors DelayNode's): lets reset() skip
    /// clearing lines that are already silent.
    dirty: bool,
    phase_l: f32,
    phase_r: f32,
    /// Delay and Depth as set (ms).
    delay_ms: f32,
    depth_ms: f32,
    /// The base delay (frames) the doubled copy reads at, and the one a Delay change is
    /// cross-fading to (equal when no fade runs; one at a time, so a drag's newer value
    /// follows once this one lands). Jumping the read position clicked on every tick, and
    /// gliding it bent the copy's pitch hard on any preset recall or click-to-value
    /// (18 -> 50 ms took a 1 kHz copy down to ~400 Hz); cross-fading two steady reads does
    /// neither.
    base: f32,
    base_next: f32,
    base_mix: f32,
    base_step: f32,
    /// The modulation depth (frames), slewed toward Depth at [`DEPTH_SLEW`].
    depth: f32,
    /// Whether the lines have run on audio; until then Delay and Depth apply at once.
    heard: bool,
    rate_hz: f32,
    mix: Smoothed,
    fade: BypassFade,
    sr: f64,
}

impl DoublerNode {
    pub fn new() -> Self {
        let len = (48_000.0 * MAX_SECONDS) as usize;
        DoublerNode {
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            pos: 0,
            stale: 0,
            dirty: false,
            phase_l: 0.0,
            phase_r: std::f32::consts::PI, // L/R out of phase for width
            delay_ms: 18.0,
            depth_ms: 4.0,
            base: 864.0, // 18 ms at 48 kHz; prepare re-derives it
            base_next: 864.0,
            base_mix: 0.0,
            base_step: 1.0 / (BASE_FADE_S * 48_000.0) as f32,
            depth: 192.0, // 4 ms
            heard: false,
            rate_hz: 0.6,
            mix: Smoothed::new(0.0),
            fade: BypassFade::new(true),
            sr: 48_000.0,
        }
    }

    fn per_ms(&self) -> f32 {
        (0.001 * self.sr) as f32
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
        self.stale = 0; // freshly zeroed lines
        self.dirty = false;
        self.base = self.delay_ms * self.per_ms();
        self.base_next = self.base;
        self.base_step = 1.0 / (BASE_FADE_S * sample_rate) as f32;
        self.depth = self.depth_ms * self.per_ms();
        self.heard = false;
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
            // Progressively overwrite the delay lines with silence while bypassed (bounded
            // per-block work; set_param runs on the audio thread, so no unbounded clear
            // there). Re-activating then never replays stale pre-bypass audio as a ghost.
            // Mirrors DelayNode's bypass branch.
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
        if len < 4 {
            return;
        }
        self.dirty = true;
        self.heard = true;
        let inc = TWO_PI * self.rate_hz / self.sr as f32;
        let want = self.delay_ms * self.per_ms();
        if self.base_next == self.base && want != self.base {
            self.base_next = want;
            self.base_mix = 0.0;
        }
        let depth_target = self.depth_ms * self.per_ms();

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            self.depth += (depth_target - self.depth).clamp(-DEPTH_SLEW, DEPTH_SLEW);
            let ml = self.depth * self.phase_l.sin();
            let mr = self.depth * self.phase_r.sin();
            let mut wl = Self::read(&self.buf_l, self.pos, self.base + ml);
            let mut wr = Self::read(&self.buf_r, self.pos, self.base + mr);
            if self.base_next != self.base {
                let nl = Self::read(&self.buf_l, self.pos, self.base_next + ml);
                let nr = Self::read(&self.buf_r, self.pos, self.base_next + mr);
                wl += (nl - wl) * self.base_mix;
                wr += (nr - wr) * self.base_mix;
                self.base_mix += self.base_step;
                if self.base_mix >= 1.0 {
                    self.base = self.base_next;
                }
            }
            self.buf_l[self.pos] = l;
            self.buf_r[self.pos] = r;
            // Keep the dry mostly intact; the wet copy thickens / widens. Blended toward the
            // dry input while the bypass switch fades.
            let wet = self.mix.next() * self.fade.next();
            out[CHANNELS * i] = l + (wl - 0.5 * l) * wet;
            out[CHANNELS * i + 1] = r + (wr - 0.5 * r) * wet;
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
            param::DELAY_MS => {
                self.delay_ms = value.clamp(1.0, 50.0);
                if !self.heard {
                    self.base = self.delay_ms * self.per_ms();
                    self.base_next = self.base;
                }
            }
            param::DEPTH_MS => {
                self.depth_ms = value.clamp(0.0, 20.0);
                if !self.heard {
                    self.depth = self.depth_ms * self.per_ms();
                }
            }
            param::RATE_HZ => self.rate_hz = value.clamp(0.05, 8.0),
            param::MIX => self.mix.set_target(value.clamp(0.0, 1.0)),
            param::BYPASS => {
                let bypass = value >= 0.5;
                if bypass && !self.fade.bypassed() {
                    // Entering bypass: schedule the progressive line clear (see process),
                    // which starts once the switch has faded out.
                    self.stale = self.buf_l.len();
                }
                self.fade.set_bypass(bypass);
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        // Seek clears the modulated delay lines and resets the LFO phase so the
        // doubler doesn't smear pre-jump audio across the discontinuity. Lines known to be
        // silent are left alone (see `dirty`).
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
        // Land any on/off fade (see DelayNode::reset): a fade-out left running would write
        // post-seek audio into the cleared lines with no sweep left to clear them.
        self.fade.settle();
        // The lines are silent: land any Delay fade and Depth slew at once.
        self.base = self.base_next;
        self.depth = self.depth_ms * self.per_ms();
        self.phase_l = 0.0;
        self.phase_r = std::f32::consts::PI;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_block(d: &mut DoublerNode, input: &[Smp]) {
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
        // Same contract as the delay's: a never-enabled doubler's lines are already silent,
        // so a seek's reset() must not sweep them on the audio thread.
        let mut d = DoublerNode::new();
        d.prepare(48_000.0, 256);
        run_block(&mut d, &[0.5; 512]); // bypassed: nothing reaches the lines
        d.buf_l[123] = 1.0;
        d.reset();
        assert_eq!(d.buf_l[123], 1.0, "reset swept a line nothing had written");

        d.buf_l[123] = 0.0;
        d.set_param(param::BYPASS, 0.0);
        run_block(&mut d, &[0.5; 512]);
        d.reset();
        assert!(
            d.buf_l.iter().chain(d.buf_r.iter()).all(|&s| s == 0.0),
            "reset must clear lines that were written"
        );
    }
}
