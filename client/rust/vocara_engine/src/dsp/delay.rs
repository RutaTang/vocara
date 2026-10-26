//! A simple stereo feedback delay (echo / throw). Per-channel ring buffer; the delayed signal
//! is fed back (clamped < 1) and mixed wet/dry. Buffers are sized in `prepare` (control thread),
//! never on the audio callback.

use crate::dsp::denormals::sanitize;
use crate::dsp::ring_index;
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
    /// Ramped like the mix: a stepped feedback gain jumped the level of everything
    /// recirculating in the line — a click one delay after the edit, echoed (and decaying by
    /// the new feedback) every delay after that.
    feedback: Smoothed,
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
            feedback: Smoothed::new(0.3),
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
        self.feedback.set_time(0.01, sample_rate);
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
                self.pos = ring_index::next(self.pos, len);
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
        // A landed feedback / mix ramp or on-off fade is one value all block (see
        // `Smoothed::settled`).
        let fb_held = self.feedback.settled(frames);
        let (mix_held, fade_held) = (self.mix.settled(frames), self.fade.settled());

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            // Taps lie in 1..len (wanted_tap clamps them).
            let read = ring_index::back(self.pos, self.tap, len);
            let (mut dl, mut dr) = (self.buf_l[read], self.buf_r[read]);
            if self.tap_next != self.tap {
                let next = ring_index::back(self.pos, self.tap_next, len);
                dl += (self.buf_l[next] - dl) * self.tap_mix;
                dr += (self.buf_r[next] - dr) * self.tap_mix;
                self.tap_mix += self.tap_step;
                if self.tap_mix >= 1.0 {
                    self.tap = self.tap_next;
                }
            }
            // Sanitize the feedback write (like reverb/biquad/compressor/gate): one non-finite
            // input would otherwise re-inject NaN*fb into the ring every `d` samples forever.
            let fb = fb_held.unwrap_or_else(|| self.feedback.next());
            self.buf_l[self.pos] = sanitize(l + dl * fb);
            self.buf_r[self.pos] = sanitize(r + dr * fb);
            // The wet share, blended toward the dry input while the bypass switch fades.
            let mix = mix_held.unwrap_or_else(|| self.mix.next());
            let wet = mix * fade_held.unwrap_or_else(|| self.fade.next());
            out[CHANNELS * i] = l + (dl - l) * wet;
            out[CHANNELS * i + 1] = r + (dr - r) * wet;
            self.pos = ring_index::next(self.pos, len);
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::TIME_MS => self.time_ms = value.clamp(1.0, (MAX_SECONDS * 1000.0) as f32),
            param::FEEDBACK => self.feedback.set_target(value.clamp(0.0, 0.98)),
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
                                  // ...and any on/off fade: a fade-out left running would write post-seek audio into
                                  // the cleared line with no sweep left to clear it (a ghost echo on re-enabling).
        self.fade.settle();
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

    /// `process` as it was before the ring-index and landed-mix rewrites (index `%` per
    /// sample, mix and fade stepped per sample), with the feedback ramp stepped per sample.
    fn reference(d: &mut DelayNode, inp: &[Smp], out: &mut [Smp]) {
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
        if len == 0 {
            return;
        }
        d.dirty = true;
        let want = d.wanted_tap();
        if d.tap_next == d.tap && want != d.tap {
            d.tap_next = want;
            d.tap_mix = 0.0;
        }
        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            let read = (d.pos + len - d.tap) % len;
            let (mut dl, mut dr) = (d.buf_l[read], d.buf_r[read]);
            if d.tap_next != d.tap {
                let next = (d.pos + len - d.tap_next) % len;
                dl += (d.buf_l[next] - dl) * d.tap_mix;
                dr += (d.buf_r[next] - dr) * d.tap_mix;
                d.tap_mix += d.tap_step;
                if d.tap_mix >= 1.0 {
                    d.tap = d.tap_next;
                }
            }
            let fb = d.feedback.next();
            d.buf_l[d.pos] = sanitize(l + dl * fb);
            d.buf_r[d.pos] = sanitize(r + dr * fb);
            let wet = d.mix.next() * d.fade.next();
            out[CHANNELS * i] = l + (dl - l) * wet;
            out[CHANNELS * i + 1] = r + (dr - r) * wet;
            d.pos = (d.pos + 1) % len;
        }
    }

    /// Bit for bit what the old loop rendered: switching on (the fade-in), mix ramps, Time
    /// fades both ways (down to the shortest tap), feedback, the bypass fade-out and the
    /// sweep that follows it across the whole line, and re-enabling — over more than one trip
    /// around the ring.
    #[test]
    fn the_rewritten_loop_matches_the_old_one_bit_for_bit() {
        let edits = [
            (0, param::BYPASS, 0.0),
            (0, param::MIX, 0.4),
            (3, param::FEEDBACK, 0.7),
            (9, param::TIME_MS, 1.0),
            (20, param::TIME_MS, 1_900.0),
            (30, param::MIX, 1.0),
            (60, param::BYPASS, 1.0),
            (400, param::BYPASS, 0.0),
            (410, param::TIME_MS, 250.0),
        ];
        let (mut fast, mut slow) = (DelayNode::new(), DelayNode::new());
        fast.prepare(48_000.0, 1024);
        slow.prepare(48_000.0, 1024);
        crate::dsp::test_support::assert_matches_reference(
            &mut fast, &mut slow, reference, &edits, 460,
        );
    }

    /// The largest |second difference| of the left channel over `out[from..to]` (frames).
    fn curvature(out: &[Smp], from: usize, to: usize) -> f32 {
        (from.max(2)..to)
            .map(|i| (out[2 * i] - 2.0 * out[2 * (i - 1)] + out[2 * (i - 2)]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_feedback_change_does_not_click_or_echo_a_click() {
        // Feedback stepped the gain of everything recirculating in the line: one delay after
        // the edit the output jumped (a click ~400x the material's own curvature), and the
        // jump came round again every delay, decaying by the new feedback.
        let (sr, block) = (48_000.0, 128usize);
        let mut d = DelayNode::new();
        d.prepare(sr, block);
        d.set_param(param::BYPASS, 0.0);
        d.set_param(param::TIME_MS, 113.0);
        d.set_param(param::MIX, 0.5);
        d.set_param(param::FEEDBACK, 0.1);
        let total = 96_000usize;
        let edit = 48_000usize;
        let input: Vec<Smp> = (0..total)
            .flat_map(|i| {
                let s = 0.5 * (i as f32 * 220.0 * std::f32::consts::TAU / sr as f32).sin();
                [s, s]
            })
            .collect();
        let mut out = vec![0.0; total * CHANNELS];
        let mut at = 0;
        while at < total {
            if at == edit {
                d.set_param(param::FEEDBACK, 0.9);
            }
            let inputs: [&[Smp]; 1] = [&input[at * CHANNELS..(at + block) * CHANNELS]];
            let mut outs: [&mut [Smp]; 1] = [&mut out[at * CHANNELS..(at + block) * CHANNELS]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut outs[..],
                frames: block,
                tl: at as i64,
                sr,
            };
            d.process(&mut io);
            at += block;
        }
        let steady = curvature(&out, edit - 8_000, edit);
        let after = curvature(&out, edit, total);
        assert!(
            after < steady * 4.0,
            "curvature {after} after the edit vs {steady} before it"
        );
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
