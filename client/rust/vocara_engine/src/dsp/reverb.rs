//! Freeverb (Schroeder–Moorer) stereo reverb: 8 parallel damped feedback comb filters into
//! 4 series allpass filters, per channel, with a stereo-spread offset on the right channel.
//! Width is applied in the reverb's own stereo frame (there is no pan after the return, so
//! width must live here). Feedback paths are anti-denormal biased.

use crate::dsp::denormals::{sanitize, ANTI_DENORMAL};
use crate::dsp::smooth::Smoothed;
use crate::node::{Node, ProcessIo};
use crate::sample::CHANNELS;

// Jezar's tunings, defined at 44.1 kHz; scaled to the device rate at prepare().
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
const FIXED_GAIN: f32 = 0.015;
const ALLPASS_FEEDBACK: f32 = 0.5;
/// A wet gain this small (-120 dB) is silence: once the wet target is 0 and the ramp has
/// fallen below it, the reverb goes idle (see `process`).
const WET_IDLE: f32 = 1.0e-6;

pub mod param {
    pub const ROOM_SIZE: u16 = 0;
    pub const DAMPING: u16 = 1;
    pub const WET: u16 = 2;
    pub const DRY: u16 = 3;
    pub const WIDTH: u16 = 4;
}

struct Comb {
    buf: Box<[f32]>,
    idx: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Comb {
            buf: vec![0.0; len.max(1)].into_boxed_slice(),
            idx: 0,
            store: 0.0,
        }
    }
    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp1: f32, damp2: f32) -> f32 {
        let out = self.buf[self.idx];
        self.store = sanitize(out * damp2 + self.store * damp1);
        self.buf[self.idx] = sanitize(input + (self.store + ANTI_DENORMAL) * feedback);
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
    fn clear(&mut self) {
        for s in self.buf.iter_mut() {
            *s = 0.0;
        }
        self.store = 0.0;
        self.idx = 0;
    }
}

struct Allpass {
    buf: Box<[f32]>,
    idx: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Allpass {
            buf: vec![0.0; len.max(1)].into_boxed_slice(),
            idx: 0,
        }
    }
    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let bufout = self.buf[self.idx];
        let out = -input + bufout;
        self.buf[self.idx] = sanitize(input + bufout * ALLPASS_FEEDBACK);
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
    fn clear(&mut self) {
        for s in self.buf.iter_mut() {
            *s = 0.0;
        }
        self.idx = 0;
    }
}

struct Channel {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Channel {
    fn new(sr: f64, spread: usize) -> Self {
        let scale = sr / 44_100.0;
        let len = |base: usize| ((base + spread) as f64 * scale).round() as usize;
        Channel {
            combs: COMB_TUNING.iter().map(|&b| Comb::new(len(b))).collect(),
            allpasses: ALLPASS_TUNING
                .iter()
                .map(|&b| Allpass::new(len(b)))
                .collect(),
        }
    }
    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp1: f32, damp2: f32) -> f32 {
        let mut out = 0.0;
        for c in &mut self.combs {
            out += c.process(input, feedback, damp1, damp2);
        }
        for a in &mut self.allpasses {
            out = a.process(out);
        }
        out
    }
    fn clear(&mut self) {
        for c in &mut self.combs {
            c.clear();
        }
        for a in &mut self.allpasses {
            a.clear();
        }
    }
}

pub struct ReverbNode {
    left: Channel,
    right: Channel,
    room_size: f32,
    damping: f32,
    feedback: Smoothed,
    damp1: Smoothed,
    wet: Smoothed,
    dry: Smoothed,
    width: Smoothed,
    /// Whether the comb/allpass lines may hold non-zero state (any processed block since
    /// the last clear). A clean reverb lets reset() — run on the audio thread for every
    /// reverb in the graph on each seek — skip sweeping ~110 KB of lines.
    dirty: bool,
    sr: f64,
}

impl ReverbNode {
    pub fn new(room_size: f32, damping: f32, wet: f32, dry: f32, width: f32) -> Self {
        let sr = 48_000.0;
        // Clamp here too (not only in set_param): new() is fed raw values from the build
        // path, which can originate from persisted/synced JSON. A room_size > ~1.07 pushes
        // the comb feedback target past 1.0 and the filter diverges to garbage.
        let room_size = room_size.clamp(0.0, 1.0);
        let damping = damping.clamp(0.0, 1.0);
        let wet = wet.clamp(0.0, 1.0);
        let dry = dry.clamp(0.0, 1.0);
        let width = width.clamp(0.0, 1.0);
        ReverbNode {
            left: Channel::new(sr, 0),
            right: Channel::new(sr, STEREO_SPREAD),
            room_size,
            damping,
            feedback: Smoothed::new(room_size * 0.28 + 0.7),
            damp1: Smoothed::new(damping * 0.4),
            wet: Smoothed::new(wet),
            dry: Smoothed::new(dry),
            width: Smoothed::new(width),
            dirty: false,
            sr,
        }
    }

    /// A reverb-send-bus return: medium room, fully wet, no dry (the dry path is separate).
    pub fn default_bus() -> Self {
        ReverbNode::new(0.5, 0.5, 1.0, 0.0, 1.0)
    }

    /// Whether the lines have run on audio since they were last cleared (tests: proves a
    /// skipped reverb never ran).
    #[cfg(test)]
    pub(crate) fn lines_dirty(&self) -> bool {
        self.dirty
    }
}

impl Node for ReverbNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        self.left = Channel::new(sample_rate, 0);
        self.right = Channel::new(sample_rate, STEREO_SPREAD);
        self.dirty = false;
        let tau = 0.02;
        self.feedback.set_time(tau, sample_rate);
        self.damp1.set_time(tau, sample_rate);
        self.wet.set_time(0.01, sample_rate);
        self.dry.set_time(0.01, sample_rate);
        self.width.set_time(0.01, sample_rate);
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let frames = io.frames;
        let n = frames * CHANNELS;
        let inp = io.inputs.first().copied();
        let out = &mut *io.outputs[0];
        // Need the input both as the reverb feed and the optional dry blend; copy it out.
        // (Outputs and inputs are distinct buffers, so read input first.)
        let mut feed = [0.0f32; 2];
        match inp {
            Some(inp) if self.wet.target() == 0.0 && self.wet.current() <= WET_IDLE => {
                // Idle: nothing of the wet path is audible. Every vocal clip carries a
                // reverb insert whose "off" is wet 0, and running its 16 combs + 8
                // allpasses per frame for silence was pure cost — pass the dry blend only.
                // The lines are dropped on the way in, so turning the wet back up starts a
                // fresh tail instead of resuming one frozen at this moment.
                self.wet.snap(0.0);
                if self.dirty {
                    self.left.clear();
                    self.right.clear();
                    self.dirty = false;
                }
                for i in 0..frames {
                    let dry = self.dry.next();
                    out[CHANNELS * i] = inp[CHANNELS * i] * dry;
                    out[CHANNELS * i + 1] = inp[CHANNELS * i + 1] * dry;
                }
            }
            Some(inp) => {
                self.dirty = true;
                for i in 0..frames {
                    let il = inp[CHANNELS * i];
                    let ir = inp[CHANNELS * i + 1];
                    feed[0] = il;
                    feed[1] = ir;

                    let fb = self.feedback.next();
                    let d1 = self.damp1.next();
                    let d2 = 1.0 - d1;
                    let wet = self.wet.next();
                    let dry = self.dry.next();
                    let width = self.width.next();

                    let wl = self.left.process(feed[0] * FIXED_GAIN, fb, d1, d2);
                    let wr = self.right.process(feed[1] * FIXED_GAIN, fb, d1, d2);

                    // Width in the reverb's own stereo frame.
                    let w1 = width * 0.5 + 0.5;
                    let w2 = (1.0 - width) * 0.5;
                    let out_l = wl * w1 + wr * w2;
                    let out_r = wr * w1 + wl * w2;

                    out[CHANNELS * i] = out_l * wet + il * dry;
                    out[CHANNELS * i + 1] = out_r * wet + ir * dry;
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
        // Clamp every reverb parameter to [0,1] like the sibling DSP nodes. ROOM_SIZE is the
        // critical one: feedback = value*0.28 + 0.7, so value > ~1.07 makes the comb feedback
        // coefficient exceed 1.0 and the filter diverges (sanitize only catches NaN/Inf, not
        // unbounded finite growth). Out-of-range values reach here via synced/migrated JSON.
        let value = value.clamp(0.0, 1.0);
        match param {
            param::ROOM_SIZE => {
                self.room_size = value;
                self.feedback.set_target(value * 0.28 + 0.7);
            }
            param::DAMPING => {
                self.damping = value;
                self.damp1.set_target(value * 0.4);
            }
            param::WET => self.wet.set_target(value),
            param::DRY => self.dry.set_target(value),
            param::WIDTH => self.width.set_target(value),
            _ => {}
        }
    }

    fn reset(&mut self) {
        if self.dirty {
            self.left.clear();
            self.right.clear();
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_block(r: &mut ReverbNode, input: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; input.len()];
        {
            let inputs: [&[f32]; 1] = [input];
            let mut outs: [&mut [f32]; 1] = [&mut out[..]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut outs[..],
                frames: input.len() / CHANNELS,
                tl: 0,
                sr: 48_000.0,
            };
            r.process(&mut io);
        }
        out
    }

    #[test]
    fn a_seek_does_not_sweep_lines_nothing_wrote() {
        // A freshly built reverb's comb/allpass lines are already silent: a seek's reset()
        // (audio thread, every reverb in the graph) must not sweep them.
        let mut r = ReverbNode::default_bus();
        r.prepare(48_000.0, 256);
        r.left.combs[0].buf[7] = 1.0;
        r.reset();
        assert_eq!(r.left.combs[0].buf[7], 1.0, "reset swept untouched lines");

        // Lines the reverb has run through ARE cleared.
        r.left.combs[0].buf[7] = 0.0;
        run_block(&mut r, &[0.5; 512]);
        r.reset();
        let all_zero = |c: &Channel| {
            c.combs
                .iter()
                .all(|k| k.buf.iter().all(|&s| s == 0.0) && k.store == 0.0)
                && c.allpasses.iter().all(|a| a.buf.iter().all(|&s| s == 0.0))
        };
        assert!(
            all_zero(&r.left) && all_zero(&r.right),
            "reset must clear a used reverb"
        );
    }

    #[test]
    fn an_insert_at_wet_zero_does_no_reverb_work_and_wakes_without_a_stale_tail() {
        // Every vocal clip carries a reverb insert, and "off" is wet 0: it used to run the
        // full 16 combs + 8 allpasses per frame regardless, with nothing audible.
        let mut r = ReverbNode::new(0.5, 0.5, 0.0, 1.0, 1.0); // build.rs's insert default
        r.prepare(48_000.0, 256);
        let tone: Vec<f32> = (0..9600).map(|i| 0.5 * (i as f32 * 0.05).sin()).collect();
        let out = run_block(&mut r, &tone);
        assert_eq!(out, tone, "wet 0 / dry 1 must pass the input through");
        assert!(
            r.left.combs.iter().all(|c| c.buf.iter().all(|&s| s == 0.0)),
            "the combs ran with the wet path silenced"
        );

        // Wet up, build a tail, then back to 0 long enough to go idle: turning it up again
        // over silence must not resume the tail frozen at the moment it went idle.
        r.set_param(param::WET, 0.5);
        run_block(&mut r, &tone);
        r.set_param(param::WET, 0.0);
        for _ in 0..30 {
            run_block(&mut r, &[0.0; 960]); // 0.3 s: the wet smoother settles to idle
        }
        r.set_param(param::WET, 0.5);
        let tail = (0..10)
            .flat_map(|_| run_block(&mut r, &[0.0; 960]))
            .fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(tail < 1e-9, "a stale tail replayed on waking: peak {tail}");
    }
}
