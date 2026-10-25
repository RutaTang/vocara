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
    /// [`process`](Self::process) over a run of samples, in place.
    fn process_run(&mut self, io: &mut [f32]) {
        let mut i = 0;
        while i < io.len() {
            // Up to the line's wrap point: a plain slice walk, no per-sample wrap check.
            let seg = (io.len() - i).min(self.buf.len() - self.idx);
            let line = &mut self.buf[self.idx..self.idx + seg];
            for (x, b) in io[i..i + seg].iter_mut().zip(line.iter_mut()) {
                let input = *x;
                let bufout = *b;
                *x = -input + bufout;
                *b = sanitize(input + bufout * ALLPASS_FEEDBACK);
            }
            self.idx += seg;
            if self.idx >= self.buf.len() {
                self.idx = 0;
            }
            i += seg;
        }
    }
    fn clear(&mut self) {
        for s in self.buf.iter_mut() {
            *s = 0.0;
        }
        self.idx = 0;
    }
}

/// Four combs over a run of samples at constant feedback/damping: `acc[i] += c0 + c1 + c2 + c3`
/// (each comb's [`Comb::process`] output, added in that order), the combs' states held in
/// registers. Four independent damping recursions in one loop keep the FPU busy where one comb
/// at a time would wait on its own recursion every sample.
fn comb4_run(
    cs: &mut [Comb],
    input: &[f32],
    acc: &mut [f32],
    feedback: f32,
    damp1: f32,
    damp2: f32,
) {
    let [c0, c1, c2, c3] = cs else {
        unreachable!("comb4_run takes exactly four combs")
    };
    let n = input.len();
    let mut i = 0;
    while i < n {
        // Up to the nearest wrap point among the four lines.
        let seg = (n - i)
            .min(c0.buf.len() - c0.idx)
            .min(c1.buf.len() - c1.idx)
            .min(c2.buf.len() - c2.idx)
            .min(c3.buf.len() - c3.idx);
        let b0 = &mut c0.buf[c0.idx..c0.idx + seg];
        let b1 = &mut c1.buf[c1.idx..c1.idx + seg];
        let b2 = &mut c2.buf[c2.idx..c2.idx + seg];
        let b3 = &mut c3.buf[c3.idx..c3.idx + seg];
        let x = &input[i..i + seg];
        let o = &mut acc[i..i + seg];
        let (mut s0, mut s1, mut s2, mut s3) = (c0.store, c1.store, c2.store, c3.store);
        for j in 0..seg {
            let xin = x[j];
            let y0 = b0[j];
            s0 = sanitize(y0 * damp2 + s0 * damp1);
            b0[j] = sanitize(xin + (s0 + ANTI_DENORMAL) * feedback);
            let y1 = b1[j];
            s1 = sanitize(y1 * damp2 + s1 * damp1);
            b1[j] = sanitize(xin + (s1 + ANTI_DENORMAL) * feedback);
            let y2 = b2[j];
            s2 = sanitize(y2 * damp2 + s2 * damp1);
            b2[j] = sanitize(xin + (s2 + ANTI_DENORMAL) * feedback);
            let y3 = b3[j];
            s3 = sanitize(y3 * damp2 + s3 * damp1);
            b3[j] = sanitize(xin + (s3 + ANTI_DENORMAL) * feedback);
            o[j] = o[j] + y0 + y1 + y2 + y3;
        }
        for (c, s) in [
            (&mut *c0, s0),
            (&mut *c1, s1),
            (&mut *c2, s2),
            (&mut *c3, s3),
        ] {
            c.store = s;
            c.idx += seg;
            if c.idx >= c.buf.len() {
                c.idx = 0;
            }
        }
        i += seg;
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
    /// [`process`](Self::process) over a run of samples at constant feedback/damping, into
    /// `out`: the same per-sample arithmetic and the same order of every sum, run comb by comb
    /// (four at a time) and then allpass by allpass across the run instead of sample by sample.
    /// Nothing couples the combs but their sum, and each allpass only sees the previous one's
    /// output, so the reordering visits the same operations on the same values.
    fn process_run(&mut self, input: &[f32], out: &mut [f32], feedback: f32, d1: f32, d2: f32) {
        out.fill(0.0);
        let mut groups = self.combs.chunks_exact_mut(4);
        for g in &mut groups {
            comb4_run(g, input, out, feedback, d1, d2);
        }
        let rest = groups.into_remainder();
        if !rest.is_empty() {
            for (o, &x) in out.iter_mut().zip(input) {
                let mut acc = *o;
                for c in rest.iter_mut() {
                    acc += c.process(x, feedback, d1, d2);
                }
                *o = acc;
            }
        }
        for a in &mut self.allpasses {
            a.process_run(out);
        }
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

/// Frames per [`Channel::process_run`] call on the settled path (stack scratch, no allocation).
const RUN: usize = 128;

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

    /// The wet path with the room settled (feedback and damping landed — the steady state):
    /// the tank runs a run of frames at a time ([`Channel::process_run`]); wet, dry and width
    /// still step per frame unless they have landed too. Same arithmetic as the per-frame loop
    /// in `process`, frame for frame.
    fn process_settled(&mut self, inp: &[f32], out: &mut [f32], frames: usize, fb: f32, d1: f32) {
        let d2 = 1.0 - d1;
        let held = (
            self.wet.settled(frames),
            self.dry.settled(frames),
            self.width.settled(frames),
        );
        let mut start = 0;
        while start < frames {
            let m = (frames - start).min(RUN);
            let (mut in_l, mut in_r) = ([0.0f32; RUN], [0.0f32; RUN]);
            for (j, f) in inp[CHANNELS * start..CHANNELS * (start + m)]
                .chunks_exact(CHANNELS)
                .enumerate()
            {
                in_l[j] = f[0] * FIXED_GAIN;
                in_r[j] = f[1] * FIXED_GAIN;
            }
            let (mut wl, mut wr) = ([0.0f32; RUN], [0.0f32; RUN]);
            self.left.process_run(&in_l[..m], &mut wl[..m], fb, d1, d2);
            self.right.process_run(&in_r[..m], &mut wr[..m], fb, d1, d2);
            let o = &mut out[CHANNELS * start..CHANNELS * (start + m)];
            let x = &inp[CHANNELS * start..CHANNELS * (start + m)];
            for j in 0..m {
                let wet = held.0.unwrap_or_else(|| self.wet.next());
                let dry = held.1.unwrap_or_else(|| self.dry.next());
                let width = held.2.unwrap_or_else(|| self.width.next());
                let (il, ir) = (x[CHANNELS * j], x[CHANNELS * j + 1]);
                let (wl, wr) = (wl[j], wr[j]);
                // Width in the reverb's own stereo frame.
                let w1 = width * 0.5 + 0.5;
                let w2 = (1.0 - width) * 0.5;
                let out_l = wl * w1 + wr * w2;
                let out_r = wr * w1 + wl * w2;
                o[CHANNELS * j] = out_l * wet + il * dry;
                o[CHANNELS * j + 1] = out_r * wet + ir * dry;
            }
            start += m;
        }
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
                if let Some(dry) = self.dry.settled(frames) {
                    for (o, &x) in out[..n].iter_mut().zip(&inp[..n]) {
                        *o = x * dry;
                    }
                } else {
                    for i in 0..frames {
                        let dry = self.dry.next();
                        out[CHANNELS * i] = inp[CHANNELS * i] * dry;
                        out[CHANNELS * i + 1] = inp[CHANNELS * i + 1] * dry;
                    }
                }
            }
            Some(inp) => {
                self.dirty = true;
                if let (Some(fb), Some(d1)) =
                    (self.feedback.settled(frames), self.damp1.settled(frames))
                {
                    self.process_settled(inp, out, frames, fb, d1);
                    return;
                }
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

    /// `process` as it was before the settled-room block path: every frame steps all five
    /// smoothers and runs both tanks sample by sample.
    fn reference(r: &mut ReverbNode, inp: &[f32], out: &mut [f32]) {
        let frames = inp.len() / CHANNELS;
        if r.wet.target() == 0.0 && r.wet.current() <= WET_IDLE {
            r.wet.snap(0.0);
            if r.dirty {
                r.left.clear();
                r.right.clear();
                r.dirty = false;
            }
            for i in 0..frames {
                let dry = r.dry.next();
                out[CHANNELS * i] = inp[CHANNELS * i] * dry;
                out[CHANNELS * i + 1] = inp[CHANNELS * i + 1] * dry;
            }
            return;
        }
        r.dirty = true;
        for i in 0..frames {
            let il = inp[CHANNELS * i];
            let ir = inp[CHANNELS * i + 1];
            let fb = r.feedback.next();
            let d1 = r.damp1.next();
            let d2 = 1.0 - d1;
            let wet = r.wet.next();
            let dry = r.dry.next();
            let width = r.width.next();
            let wl = r.left.process(il * FIXED_GAIN, fb, d1, d2);
            let wr = r.right.process(ir * FIXED_GAIN, fb, d1, d2);
            let w1 = width * 0.5 + 0.5;
            let w2 = (1.0 - width) * 0.5;
            let out_l = wl * w1 + wr * w2;
            let out_r = wr * w1 + wl * w2;
            out[CHANNELS * i] = out_l * wet + il * dry;
            out[CHANNELS * i + 1] = out_r * wet + ir * dry;
        }
    }

    /// The settled-room path (tanks run a run of frames at a time, combs four abreast,
    /// allpasses one after another; landed wet/dry/width as constants) renders exactly what the
    /// frame-by-frame loop renders — through room and damping ramps (the per-frame path), wet,
    /// dry and width ramps over a settled room, the wet-0 idle stretch and its wake-up, both
    /// tunings' line wraps at every alignment, and ragged blocks.
    #[test]
    fn the_settled_room_path_matches_the_frame_by_frame_loop_bit_for_bit() {
        for (fresh, edits) in [
            (
                ReverbNode::default_bus as fn() -> ReverbNode,
                vec![
                    (6, param::ROOM_SIZE, 0.9),
                    (14, param::WET, 0.4),
                    (15, param::DAMPING, 0.1),
                    (30, param::WIDTH, 0.2),
                    (31, param::DRY, 0.5),
                    (60, param::WET, 0.0),
                    (140, param::WET, 0.7),
                    (150, param::ROOM_SIZE, 0.2),
                ],
            ),
            (
                || ReverbNode::new(0.5, 0.5, 0.0, 1.0, 1.0), // the insert default (wet 0)
                vec![(5, param::WET, 0.3), (40, param::DRY, 0.0)],
            ),
        ] {
            let (mut fast, mut slow) = (fresh(), fresh());
            fast.prepare(48_000.0, 1024);
            slow.prepare(48_000.0, 1024);
            crate::dsp::test_support::assert_matches_reference(
                &mut fast, &mut slow, reference, &edits, 200,
            );
        }
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
