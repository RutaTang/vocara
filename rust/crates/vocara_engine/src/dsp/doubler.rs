//! Vocal doubler / chorus: two short LFO-modulated delay lines (L/R out of phase) that
//! thicken and widen the voice. Fractional read with linear interpolation; no feedback.
//! Buffers sized in `prepare`.

use crate::dsp::ring_index;
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

    /// The deepest the modulation may go: the delay the taps sit at, less the one frame a read
    /// needs. Deeper, `base + depth·sin` reached "now", the read clamped to a frame, and for
    /// that part of every cycle the copy collapsed onto the dry (+3.5 dB at full mix), its
    /// modulation stopped and its pitch snapping at the edges (Delay below Depth).
    fn depth_room(&self) -> f32 {
        (self.base.min(self.base_next) - 1.0).max(0.0)
    }

    #[inline]
    fn read(buf: &[Smp], pos: usize, delay_samples: f32) -> Smp {
        let len = buf.len();
        let d = delay_samples.clamp(1.0, (len - 2) as f32);
        let base = d.floor();
        let frac = d - base;
        // `base` lies in 0..=len - 2 (0 only for a NaN delay, which `as` saturates to).
        let i0 = ring_index::back(pos, base as usize, len);
        let i1 = ring_index::back(i0, 1, len);
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
        self.depth = (self.depth_ms * self.per_ms()).min(self.depth_room());
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
                self.pos = ring_index::next(self.pos, len);
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
        let depth_target = (self.depth_ms * self.per_ms()).min(self.depth_room());
        // A landed mix ramp / on-off fade is one value all block (see `Smoothed::settled`).
        let (mix_held, fade_held) = (self.mix.settled(frames), self.fade.settled());

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
            let mix = mix_held.unwrap_or_else(|| self.mix.next());
            let wet = mix * fade_held.unwrap_or_else(|| self.fade.next());
            out[CHANNELS * i] = l + (wl - 0.5 * l) * wet;
            out[CHANNELS * i + 1] = r + (wr - 0.5 * r) * wet;
            self.pos = ring_index::next(self.pos, len);
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
                    self.depth = (self.depth_ms * self.per_ms()).min(self.depth_room());
                }
            }
            param::DEPTH_MS => {
                self.depth_ms = value.clamp(0.0, 20.0);
                if !self.heard {
                    self.depth = (self.depth_ms * self.per_ms()).min(self.depth_room());
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

    /// The doubled copy carries on: the lines and the LFO's phase are swapped in, the delay,
    /// depth and mix stay this node's. A bypassed node sweeps what it took (see DelayNode).
    fn carry_state(&mut self, old: &mut dyn Node) {
        let Some(old) = crate::node::same_kind::<DoublerNode>(old) else {
            return;
        };
        if old.sr != self.sr || old.buf_l.len() != self.buf_l.len() {
            return;
        }
        std::mem::swap(&mut self.buf_l, &mut old.buf_l);
        std::mem::swap(&mut self.buf_r, &mut old.buf_r);
        self.pos = old.pos;
        self.dirty = old.dirty;
        self.stale = if self.fade.bypassed() {
            self.buf_l.len()
        } else {
            old.stale
        };
        self.phase_l = old.phase_l;
        self.phase_r = old.phase_r;
        self.heard = old.heard;
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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
        // Within the delay too: past it, every seek (and every pause that rewinds) put the
        // depth back over the delay to slew down for up to a second, clamped meanwhile.
        self.depth = (self.depth_ms * self.per_ms()).min(self.depth_room());
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

    /// The old fractional read, index `%` and all.
    fn read_reference(buf: &[Smp], pos: usize, delay_samples: f32) -> Smp {
        let len = buf.len();
        let d = delay_samples.clamp(1.0, (len - 2) as f32);
        let base = d.floor();
        let frac = d - base;
        let i0 = (pos + len - base as usize) % len;
        let i1 = (i0 + len - 1) % len;
        buf[i0] * (1.0 - frac) + buf[i1] * frac
    }

    /// `process` as it was before the ring-index and landed-mix rewrites, verbatim.
    fn reference(d: &mut DoublerNode, inp: &[Smp], out: &mut [Smp]) {
        let frames = inp.len() / CHANNELS;
        out.copy_from_slice(inp);
        if d.fade.begin_block() {
            let len = d.buf_l.len();
            if len == 0 || d.stale == 0 {
                return;
            }
            let clear = frames.min(d.stale);
            for _ in 0..clear {
                d.buf_l[d.pos] = 0.0;
                d.buf_r[d.pos] = 0.0;
                d.pos = (d.pos + 1) % len;
            }
            d.stale -= clear;
            if d.stale == 0 {
                d.dirty = false;
            }
            return;
        }
        let len = d.buf_l.len();
        if len < 4 {
            return;
        }
        d.dirty = true;
        d.heard = true;
        let inc = TWO_PI * d.rate_hz / d.sr as f32;
        let want = d.delay_ms * d.per_ms();
        if d.base_next == d.base && want != d.base {
            d.base_next = want;
            d.base_mix = 0.0;
        }
        // The depth cap is new semantics, not part of the rewrite this test pins.
        let depth_target = (d.depth_ms * d.per_ms()).min(d.depth_room());
        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            d.depth += (depth_target - d.depth).clamp(-DEPTH_SLEW, DEPTH_SLEW);
            let ml = d.depth * d.phase_l.sin();
            let mr = d.depth * d.phase_r.sin();
            let mut wl = read_reference(&d.buf_l, d.pos, d.base + ml);
            let mut wr = read_reference(&d.buf_r, d.pos, d.base + mr);
            if d.base_next != d.base {
                let nl = read_reference(&d.buf_l, d.pos, d.base_next + ml);
                let nr = read_reference(&d.buf_r, d.pos, d.base_next + mr);
                wl += (nl - wl) * d.base_mix;
                wr += (nr - wr) * d.base_mix;
                d.base_mix += d.base_step;
                if d.base_mix >= 1.0 {
                    d.base = d.base_next;
                }
            }
            d.buf_l[d.pos] = l;
            d.buf_r[d.pos] = r;
            let wet = d.mix.next() * d.fade.next();
            out[CHANNELS * i] = l + (wl - 0.5 * l) * wet;
            out[CHANNELS * i + 1] = r + (wr - 0.5 * r) * wet;
            d.pos = (d.pos + 1) % len;
            d.phase_l += inc;
            d.phase_r += inc;
            if d.phase_l >= TWO_PI {
                d.phase_l -= TWO_PI;
            }
            if d.phase_r >= TWO_PI {
                d.phase_r -= TWO_PI;
            }
        }
    }

    /// Bit for bit what the old loop rendered: switching on, mix ramps, Delay cross-fades to the
    /// line's extremes, the deepest and fastest modulation (reads wrapping the ring from both
    /// sides), the bypass fade-out and sweep, and re-enabling.
    #[test]
    fn the_rewritten_loop_matches_the_old_one_bit_for_bit() {
        let edits = [
            (0, param::BYPASS, 0.0),
            (0, param::MIX, 0.6),
            (4, param::DEPTH_MS, 20.0),
            (4, param::RATE_HZ, 8.0),
            (10, param::DELAY_MS, 50.0),
            (25, param::DELAY_MS, 1.0),
            (40, param::MIX, 1.0),
            (60, param::BYPASS, 1.0),
            (90, param::BYPASS, 0.0),
            (95, param::DEPTH_MS, 0.0),
        ];
        let (mut fast, mut slow) = (DoublerNode::new(), DoublerNode::new());
        fast.prepare(48_000.0, 1024);
        slow.prepare(48_000.0, 1024);
        crate::dsp::test_support::assert_matches_reference(
            &mut fast, &mut slow, reference, &edits, 130,
        );
    }

    #[test]
    fn a_delay_shorter_than_the_depth_never_reads_past_now() {
        // Delay 2 ms under the default 4 ms depth: the tap swung to "now" for a third of
        // every cycle, where the read clamps and the copy collapses onto the dry.
        let mut d = DoublerNode::new();
        d.prepare(48_000.0, 512);
        d.set_param(param::BYPASS, 0.0);
        d.set_param(param::DELAY_MS, 2.0);
        assert!(d.depth <= d.base - 1.0, "capped before the first block");
        let input = vec![0.25; 512 * CHANNELS];
        let mut deepest = 0.0f32;
        for _ in 0..200 {
            run_block(&mut d, &input);
            deepest = deepest.max(d.depth);
        }
        assert!(deepest <= d.base - 1.0, "{deepest} > {}", d.base - 1.0);
        assert!(d.depth > 0.0, "the copy is still modulated");
        // A seek, and a pause that rewinds, reset the node; the stream restarting prepares it.
        d.reset();
        assert!(d.depth <= d.base - 1.0, "capped after a reset");
        d.prepare(48_000.0, 512);
        assert!(d.depth <= d.base - 1.0, "capped after a prepare");
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
