//! Disk-streaming track feeders (P1 of the streaming migration,
//! docs/audio-streaming-plan.md): each file-backed track gets an SPSC ring a
//! host-side feeder keeps filled AHEAD of the transport, and the RT node
//! ([`crate::source::StreamingSource`]) pops frames — RAM scales with track
//! COUNT (one small ring each), never track LENGTH.
//!
//! ## The fence protocol
//!
//! Every transport discontinuity — seek, pause/play flush, loop-parameter
//! change, and (deliberately) every LOOP WRAP — starts a new GENERATION:
//! the feeder publishes `(gen, base_tl)` in the shared [`StreamCtrl`], pushes
//! an in-band FENCE frame (left = NaN, right = the generation bits; real
//! audio can never carry NaN — every producing path scrubs), and then pushes
//! frames for the strictly CONTIGUOUS run `base_tl..run_end`. Treating the
//! wrap as a discontinuity is what keeps a generation monotonic: the node
//! needs no loop arithmetic, catch-up comparisons are plain integers, and
//! the feeder replans once per loop cycle (nothing at audio rate).
//!
//! The node discards ring content until it meets the fence of the CURRENT
//! generation, then pops exactly one frame per in-span timeline position.
//! Both sides skip out-of-span positions by the track's static span, so no
//! per-frame coordination exists beyond the ring itself. Underrun emits
//! counted silence WITHOUT advancing, so the stream self-heals when the
//! feeder catches up.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::live::Shared;
use crate::ring::Producer;
use crate::sample::{Smp, CHANNELS};

/// Feeder → RT-node control block for one streaming track.
pub struct StreamCtrl {
    /// Newest generation (matches the newest fence in the ring or about to
    /// be pushed) — the node's eager-resync target after a FLUSH.
    pub gen: AtomicU32,
    /// Bumped ONLY for discarding discontinuities (transport flush, loop
    /// parameter change, play/pause) — the node then discards ring content
    /// up to the newest fence. A LOOP WRAP deliberately does NOT bump it:
    /// the pre-wrap tail must still play out, so the node adopts the wrap's
    /// fence IN BAND when it reaches it.
    pub flush_seq: AtomicU32,
    /// Underruns the RT node observed (ring empty when audio was needed) —
    /// the streaming-health signal the FFI aggregates for the UI. Offline
    /// equivalence is bit-exact; whether a REAL device's callback jitter
    /// ever outruns the 10 ms pump is only observable on hardware, and this
    /// counter is that instrument.
    pub xruns: std::sync::atomic::AtomicU64,
    /// Where the RT node sits parked AHEAD of the transport — armed at a ring
    /// position the render has not reached, no fence at the ring head — as a
    /// [`parked_word`], or 0. The node rewrites it after every block it
    /// renders and clears it while stopped; the feeder reads it to catch a
    /// park that a loop wrap has stranded (see `TrackFeeder::node_stranded`).
    pub parked: std::sync::atomic::AtomicU64,
}

impl StreamCtrl {
    pub fn new() -> Arc<Self> {
        Arc::new(StreamCtrl {
            gen: AtomicU32::new(0),
            flush_seq: AtomicU32::new(0),
            xruns: std::sync::atomic::AtomicU64::new(0),
            parked: std::sync::atomic::AtomicU64::new(0),
        })
    }
}

/// Packs a node park (see [`StreamCtrl::parked`]): the generation in the high
/// half, the parked position in the low one. A park is always ahead of a
/// non-negative transport, so its position is never 0 and 0 can mean "not
/// parked"; a position past 32 bits (over a day of timeline) goes unreported.
#[inline]
pub fn parked_word(generation: u32, pos: i64) -> u64 {
    match u32::try_from(pos) {
        Ok(p) => ((generation as u64) << 32) | p as u64,
        Err(_) => 0,
    }
}

/// The in-band fence: TWO frames. Real audio never carries NaN (every
/// producing path scrubs non-finite samples), so a NaN left channel
/// unambiguously tags frame one, whose right channel carries the generation
/// bits; frame two carries the generation's base timeline position as raw
/// f32 bit-patterns (never interpreted as audio).
#[inline]
pub fn fence_frames(generation: u32, base_tl: i64) -> [[Smp; CHANNELS]; 2] {
    let bits = base_tl as u64;
    [
        [f32::NAN, f32::from_bits(generation)],
        [
            f32::from_bits(bits as u32),
            f32::from_bits((bits >> 32) as u32),
        ],
    ]
}

/// Decodes a fence's second frame back into the base timeline position.
#[inline]
pub fn fence_base_tl(f: [Smp; CHANNELS]) -> i64 {
    (f[0].to_bits() as u64 | ((f[1].to_bits() as u64) << 32)) as i64
}

/// Reads engine-rate canonical WAVs (the shape every take and every
/// transcoded import has: 44-byte header, PCM16 or float32) as timeline
/// frames. Non-RT — lives on the feeder side only.
pub struct WavTrackReader {
    file: File,
    path: String,
    data_start: u64,
    /// Frames physically present.
    pub frames: i64,
    /// True: float32 samples; false: PCM16 (legacy takes).
    is_f32: bool,
    channels: u16,
    /// Reusable read buffer — `read_at` runs every ~10 ms per streamed track
    /// and used to allocate a fresh Vec per call.
    scratch: Vec<u8>,
}

impl WavTrackReader {
    /// Probes the canonical layout; `Err` for anything else (the caller
    /// falls back to the decoded-Arc path — foreign files stay supported).
    pub fn open(path: &str) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("open {path}: {e}"))?;
        let mut head = [0u8; 44];
        file.read_exact(&mut head)
            .map_err(|e| format!("header {path}: {e}"))?;
        if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" || &head[12..16] != b"fmt " {
            return Err("not a canonical wav".into());
        }
        let fmt_len = u32::from_le_bytes([head[16], head[17], head[18], head[19]]);
        if fmt_len != 16 || &head[36..40] != b"data" {
            return Err("not the canonical 44-byte layout".into());
        }
        let format = u16::from_le_bytes([head[20], head[21]]);
        let channels = u16::from_le_bytes([head[22], head[23]]);
        let rate = u32::from_le_bytes([head[24], head[25], head[26], head[27]]);
        let bits = u16::from_le_bytes([head[34], head[35]]);
        let is_f32 = format == 3 && bits == 32;
        let is_pcm16 = format == 1 && bits == 16;
        if (!is_f32 && !is_pcm16) || !(1..=2).contains(&channels) {
            return Err("unsupported canonical sample format".into());
        }
        if rate as f64 != crate::sample::ENGINE_SR {
            return Err("not engine-rate".into());
        }
        let declared = u32::from_le_bytes([head[40], head[41], head[42], head[43]]) as u64;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let avail = len.saturating_sub(44);
        let bytes_per_frame = channels as u64 * if is_f32 { 4 } else { 2 };
        let frames = (declared.min(avail) / bytes_per_frame) as i64;
        Ok(WavTrackReader {
            file,
            path: path.to_string(),
            data_start: 44,
            frames,
            is_f32,
            channels,
            scratch: Vec::new(),
        })
    }

    /// Refresh the underlying file handle (transient-error recovery: a stale
    /// handle on a network/USB volume can keep failing after the volume
    /// recovers, while a fresh open succeeds). The committed geometry
    /// (frames, format) is intentionally kept — the graph was built for it.
    pub fn reopen(&mut self) -> Result<(), String> {
        self.file = File::open(&self.path).map_err(|e| format!("reopen {}: {e}", self.path))?;
        Ok(())
    }

    /// Reads up to `out.len()` frames starting at track-local frame `at`
    /// into `out` (stereo; mono duplicates). Returns frames produced —
    /// short only at end of file.
    pub fn read_at(&mut self, at: i64, out: &mut [[Smp; CHANNELS]]) -> Result<usize, String> {
        if at < 0 || at >= self.frames {
            return Ok(0);
        }
        let want = out.len().min((self.frames - at) as usize);
        let bps = if self.is_f32 { 4usize } else { 2 };
        let frame_bytes = self.channels as usize * bps;
        self.file
            .seek(SeekFrom::Start(
                self.data_start + at as u64 * frame_bytes as u64,
            ))
            .map_err(|e| e.to_string())?;
        self.scratch.resize(want * frame_bytes, 0);
        let buf = &mut self.scratch[..want * frame_bytes];
        let mut read = 0;
        while read < buf.len() {
            match self.file.read(&mut buf[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) => return Err(e.to_string()),
            }
        }
        let got = read / frame_bytes;
        let buf = &self.scratch[..want * frame_bytes];
        for (i, frame) in out.iter_mut().enumerate().take(got) {
            let base = i * frame_bytes;
            let sample = |c: usize| -> Smp {
                let o = base + c * bps;
                let v = if self.is_f32 {
                    f32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]])
                } else {
                    i16::from_le_bytes([buf[o], buf[o + 1]]) as f32 / 32768.0
                };
                // The fence protocol RESERVES NaN; a corrupt file must not
                // forge one (finite clamp mirrors the decode root).
                if v.is_finite() {
                    v.clamp(-16.0, 16.0)
                } else {
                    0.0
                }
            };
            let l = sample(0);
            let r = if self.channels > 1 { sample(1) } else { l };
            *frame = [l, r];
        }
        Ok(got)
    }
}

/// Transport facts a feeder plan depends on; a change in ANY of them is a
/// discontinuity (new generation).
#[derive(Clone, Copy, PartialEq, Eq)]
struct PlanBasis {
    flush_gen: u32,
    playing: bool,
    loop_on: bool,
    ls: i64,
    le: i64,
}

/// One file-backed track being streamed.
pub struct TrackFeeder {
    pub ctrl: Arc<StreamCtrl>,
    producer: Producer<[Smp; CHANNELS]>,
    reader: WavTrackReader,
    /// Timeline span: first frame and length (same placement semantics as
    /// BufferSource).
    start_frame: i64,
    len_frames: i64,
    /// Feeder-local generation (mirrors ctrl.gen).
    generation: u32,
    /// Next timeline position to push, and the exclusive end of the current
    /// contiguous run (loop_end while looping, else i64::MAX).
    next_tl: i64,
    run_end: i64,
    basis: Option<PlanBasis>,
    /// Where the newest discarding plan starts (see [`Self::resumes_held_plan`]).
    plan_pos: i64,
    /// Frames pushed since the newest DISCARDING fence, fence included, while
    /// the ring may still hold frames from before it (None once it cannot).
    since_flush: Option<usize>,
    /// Frames pushed since the newest discarding fence, fence included (never cleared).
    plan_pushed: usize,
    scratch: Vec<[Smp; CHANNELS]>,
}

impl TrackFeeder {
    pub fn new(
        ctrl: Arc<StreamCtrl>,
        producer: Producer<[Smp; CHANNELS]>,
        reader: WavTrackReader,
        start_frame: i64,
    ) -> Self {
        let len_frames = reader.frames;
        TrackFeeder {
            ctrl,
            producer,
            reader,
            start_frame,
            len_frames,
            generation: 0,
            next_tl: 0,
            run_end: 0,
            basis: None,
            plan_pos: 0,
            since_flush: None,
            plan_pushed: 0,
            scratch: vec![[0.0; CHANNELS]; 1024],
        }
    }

    /// Refresh the reader's file handle (see [`WavTrackReader::reopen`]);
    /// used by the pump thread's transient-error retry.
    pub fn reopen_reader(&mut self) -> Result<(), String> {
        self.reader.reopen()
    }

    /// The slots the fill leaves free: room for a discarding replan's fence
    /// AND fresh audio behind it — a quarter of the ring, which covers the
    /// largest device callback (VPIO's 4096 frames). The node drops the whole
    /// stale backlog at once when it adopts the fence; with only the fence's
    /// two slots to spare, the fill behind it got just what the node had
    /// consumed since the last pump, and the node ran dry before the next one
    /// — a hole after every seek and on every pump of a loop-handle drag.
    /// Until that backlog is gone, though (see [`Self::since_flush`]), its
    /// space is about to be freed wholesale, so the fill may use all of it but
    /// one more fence: a further replan must still land before the node next
    /// drains (a stopped node drains in `idle`, once per device callback).
    fn fill_reserve(&mut self) -> usize {
        if let Some(n) = self.since_flush {
            // FIFO: a frame from before the fence remains exactly while the
            // ring holds more than was pushed since it.
            if self.producer.capacity() - self.producer.free() <= n {
                self.since_flush = None;
            }
        }
        if self.since_flush.is_some() {
            2
        } else {
            (self.producer.capacity() / 4).max(2)
        }
    }

    fn pushed(&mut self, n: usize) {
        self.plan_pushed += n;
        if let Some(c) = &mut self.since_flush {
            *c += n;
        }
    }

    /// Pushes a two-frame fence when the ring has room for both. A
    /// `discarding` fence publishes its generation as the node's flush
    /// target; an in-band one (a loop wrap, a starvation jump) must not: a
    /// replan's fill reaching loop end before the node took the flush moved
    /// the target past the whole pre-wrap run, and the node discarded it.
    /// Returns whether it landed (a full ring retries on the next pump).
    fn push_fence(&mut self, generation: u32, base_tl: i64, discarding: bool) -> bool {
        if self.producer.free() < 2 {
            return false;
        }
        let [a, b] = fence_frames(generation, base_tl);
        let ok = self.producer.push(a) && self.producer.push(b);
        debug_assert!(ok, "free() >= 2 guaranteed room for both fence frames");
        if discarding {
            self.ctrl.gen.store(generation, Ordering::Release);
            self.since_flush = Some(0);
            self.plan_pushed = 0;
        }
        self.pushed(2);
        ok
    }

    /// The transport position a plan must start from: where the NEXT block
    /// renders ([`Shared::render_pos`]), not the audible playhead. On the
    /// resampled path the playhead trails the render by the output FIFO; once
    /// the render has wrapped to loop_start while the audible position is
    /// still short of loop_end, a plan based at the playhead sits a whole pass
    /// ahead of the node, which renders silence until it catches up.
    /// The render clock wraps `frame >= loop_end` back to `loop_start` BEFORE
    /// producing anything (see `RtProcessor::render_block`), so a raw position
    /// at/past loop end — a pause fold from a run that played with the loop
    /// disengaged, held until the first post-play block is consumed — must be
    /// modeled wrapped: a linear plan from there streams positions the
    /// transport never plays.
    fn planned_pos(t: &Shared, basis: &PlanBasis) -> i64 {
        let pos = t.render_pos().max(0);
        if basis.loop_on && basis.le > basis.ls && pos >= basis.le {
            basis.ls
        } else {
            pos
        }
    }

    /// Re-plan from the CURRENT transport position after a DISCARDING
    /// discontinuity (flush / loop-param / play state change): bump
    /// flush_seq so the node discards to this fence, then start the run.
    fn replan(&mut self, t: &Shared, basis: PlanBasis) {
        let pos = Self::planned_pos(t, &basis);
        let next_gen = self.generation.wrapping_add(1);
        self.basis = Some(basis);
        self.run_end = if basis.loop_on && basis.le > basis.ls {
            basis.le
        } else {
            i64::MAX
        };
        if self.push_fence(next_gen, pos, true) {
            self.generation = next_gen;
            self.next_tl = pos;
            self.plan_pos = pos;
            self.ctrl.flush_seq.fetch_add(1, Ordering::Release);
        } else {
            // Ring full of doomed frames: force the basis stale so the next
            // pump retries the replan (the node will discard once the fence
            // lands).
            self.basis = None;
        }
    }

    /// Play from pause, with nothing moved, resumes exactly where the held
    /// plan starts: the pause's replan planned from the folded clock, and a
    /// paused node renders nothing. Replanning here discarded the audio the
    /// fill had queued behind that plan while paused, for a fresh fence that
    /// lands only at the next pump — so when the device callback beat the
    /// pump, every resume opened with a block of silence on this track.
    fn resumes_held_plan(&self, t: &Shared, basis: &PlanBasis) -> bool {
        let Some(old) = self.basis else {
            return false;
        };
        !old.playing
            && basis.playing
            // Nothing of the held plan consumed: a play/pause the pump never saw (or a
            // render in flight at pause) may have popped its head, and a seek back to
            // plan_pos would then resume a plan the node is already past. FIFO: the
            // plan's fence and everything behind it remain while the ring holds at
            // least what was pushed since that fence, less the fence the node may
            // already have adopted.
            && self.producer.capacity() - self.producer.free() + 2 >= self.plan_pushed
            && (old.loop_on, old.ls, old.le) == (basis.loop_on, basis.ls, basis.le)
            && Self::planned_pos(t, basis) == self.plan_pos
    }

    /// Whether the node is parked on THIS plan at a position the transport
    /// has already passed. A plan starts where the next block renders, and the
    /// render only moves forward from there — but for a loop wrap. So a node
    /// parked ahead of the render was overtaken by one: the render left loop
    /// end after the plan read its position (a commit's prime, a seek or loop
    /// edit a callback short of loop end, its audio landing late), or the ring
    /// ran dry short of loop end. The node then waits, over the pre-wrap rest
    /// of its run, for the transport to come round to it — a whole pass of
    /// silence. The one legitimate park is on a seek still pending to that
    /// very base, which `planned_pos` reports as its target.
    fn node_stranded(&self, t: &Shared, basis: &PlanBasis) -> bool {
        let parked = self.ctrl.parked.load(Ordering::Acquire);
        if parked == 0 {
            return false;
        }
        // A park on an older plan drains to the newest one by itself.
        let newest = self.ctrl.gen.load(Ordering::Relaxed);
        let on_this_plan = ((parked >> 32) as u32).wrapping_sub(newest) as i32 >= 0;
        on_this_plan && Self::planned_pos(t, basis) < (parked & 0xffff_ffff) as i64
    }

    /// Brings the plan up to date with the transport (a replan on any
    /// discontinuity): what every fill does first.
    fn sync_plan(&mut self, t: &Shared) -> PlanBasis {
        let basis = PlanBasis {
            flush_gen: t.out_flush_gen(),
            playing: t.is_playing(),
            loop_on: t.loop_enabled(),
            ls: t.loop_start(),
            le: t.loop_end(),
        };
        if self.basis != Some(basis) {
            if self.resumes_held_plan(t, &basis) {
                self.basis = Some(basis);
            } else {
                self.replan(t, basis);
            }
        }
        basis
    }

    /// Top the ring up. Returns Err only on unreadable file (the caller
    /// tears the streaming source down; playback degrades to silence for
    /// this track rather than corrupting anything).
    pub fn pump(&mut self, t: &Shared) -> Result<(), String> {
        self.pump_at_most(t, usize::MAX)
    }

    /// [`Self::pump`], pushing at most [`frames`] frames of audio: a commit
    /// made while paused primes each new ring with one device callback's
    /// worth on the caller's thread (so a Play right after it opens on audio,
    /// whatever order the pump thread fills the rings in) and leaves the rest
    /// of the reading to the pump thread.
    pub fn pump_at_most(&mut self, t: &Shared, frames: usize) -> Result<(), String> {
        let basis = self.sync_plan(t);
        if basis.playing && self.node_stranded(t, &basis) {
            self.replan(t, basis);
        }
        let mut budget = frames;
        // Paused, the fill still runs: the plan starts where play resumes.
        // Starvation recovery: the transport ran AHEAD of the plan (the ring
        // starved and the node discarded everything). Feeding the stale gap
        // would waste a whole ring of catch-up; JUMP the plan to the current
        // position with an in-band fence instead (no flush_seq — nothing in
        // the ring needs discarding; it is empty by definition of having
        // starved).
        // `planned_pos` models the render-side wrap, so while looping `pos`
        // is always inside [ls, le) — a plan behind a wrapped transport shows
        // up here as `pos > next_tl` like any other starvation once the
        // transport is past the plan's position again. Short of it, the node
        // is parked ahead of the transport instead: `node_stranded` above.
        // The ring must actually be DRAINED: right after a wrap fence the
        // plan restarts at loop_start while the transport is still playing
        // the pre-wrap tail out of the ring, so `pos > next_tl` alone would
        // jump the next pass ahead of the node and silence most of it. Past a
        // non-looped track's end the node never pops the one jump fence, so
        // the same check keeps every later pump from minting another.
        let pos = Self::planned_pos(t, &basis);
        if basis.playing && pos > self.next_tl && self.producer.free() == self.producer.capacity() {
            let next_gen = self.generation.wrapping_add(1);
            if self.push_fence(next_gen, pos, false) {
                self.generation = next_gen;
                self.next_tl = pos;
            }
        }
        loop {
            if self.next_tl >= self.run_end {
                // A wrap whose ENTIRE next run misses the track's span must
                // not push a fence: the node renders out-of-span silence
                // without popping, so the fence would sit in the ring forever
                // — and looping here once per simulated wrap used to fill the
                // whole ring with back-to-back fences, which then wedged the
                // replan (its own fence needs 2 free slots) the moment the
                // loop moved back onto the track. Nothing more to feed under
                // this plan: hold until the basis changes.
                let span_end = self.start_frame + self.len_frames;
                if basis.le <= self.start_frame || basis.ls >= span_end {
                    return Ok(());
                }
                // Loop wrap: a NEW contiguous run from loop_start, adopted
                // by the node IN BAND (no flush_seq bump — the pre-wrap tail
                // still has to play out of the ring first).
                // The in-band fence must not eat the fill reserve either.
                let next_gen = self.generation.wrapping_add(1);
                if self.producer.free() < 2 + self.fill_reserve()
                    || !self.push_fence(next_gen, basis.ls, false)
                {
                    return Ok(()); // ring full: the wrap fence retries next pump
                }
                self.generation = next_gen;
                self.next_tl = basis.ls;
                self.run_end = basis.le;
            }
            // Skip positions outside the track's span without pushing — the
            // node skips the same positions without popping (same static
            // span on both sides, so no coordination is needed).
            if self.next_tl < self.start_frame {
                self.next_tl = self.start_frame.min(self.run_end);
                if self.next_tl >= self.run_end {
                    continue; // the whole remaining run is pre-span: wrap
                }
            }
            let span_end = self.start_frame + self.len_frames;
            if self.next_tl >= span_end {
                // Past the span: nothing left to push in this run (the node
                // renders silence there without popping).
                if self.run_end == i64::MAX {
                    return Ok(());
                }
                self.next_tl = self.run_end; // jump to the wrap
                continue;
            }
            let run_left = (self.run_end - self.next_tl).max(0);
            let span_left = span_end - self.next_tl;
            let free = self.producer.free().saturating_sub(self.fill_reserve());
            let want = self
                .scratch
                .len()
                .min(free)
                .min(budget)
                .min(run_left.min(span_left) as usize);
            if want == 0 {
                return Ok(()); // ring full, budget spent (or boundary reached)
            }
            let local = self.next_tl - self.start_frame;
            let got = self.reader.read_at(local, &mut self.scratch[..want])?;
            for i in 0..got {
                if !self.producer.push(self.scratch[i]) {
                    // Raced full: rewind next_tl to the unpushed frame.
                    self.next_tl += i as i64;
                    self.pushed(i);
                    return Ok(());
                }
            }
            self.next_tl += got as i64;
            self.pushed(got);
            budget -= got;
            if got < want {
                // Physically truncated file: treat the rest as silence.
                return Ok(());
            }
        }
    }
}
