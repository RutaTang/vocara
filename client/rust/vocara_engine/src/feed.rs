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
//!
//! ## Taking the queue back
//!
//! A discarding replan makes everything still queued stale, so the feeder
//! takes it back (`TrackFeeder::discard_queued`) and lands the new fence at
//! the node's head with the whole ring behind it. The ring is SPSC, so the
//! node has to stay clear of the slots being rewritten: it counts every block
//! it enters and leaves ([`StreamCtrl::node_entered`], [`StreamCtrl::node_left`])
//! and pops nothing at or past [`StreamCtrl::pop_limit`], read once at the
//! block's start. The
//! feeder raises the limit to the node's head, waits out a block that began
//! before it (the only one that can have missed it), takes the queue back,
//! pushes the new fence, and lifts the limit.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// position the render has not reached (before the track's span: past the
    /// span's first frame), no fence at the ring head — as a
    /// [`parked_word`], or 0. The node rewrites it after every block it
    /// renders and clears it while stopped; the feeder reads it to catch a
    /// park that a loop wrap has stranded (see `TrackFeeder::node_stranded`).
    pub parked: std::sync::atomic::AtomicU64,
    /// Blocks the node has entered (`process`/`idle`), and blocks it has left
    /// (see the module doc). Two counters, not one bumped on entry and exit:
    /// a block that never left — a panic the callback caught mid-block, after
    /// which the engine renders on — flipped a single counter's odd/even
    /// reading for good, and the next take-back then rewrote slots under a
    /// LIVE block. Here such a block just reads as still inside: the feeder
    /// waits it out or falls back to stacking, never retracting under it.
    pub(crate) node_entered: AtomicU32,
    pub(crate) node_left: AtomicU32,
    /// The ring index the node may neither pop nor peek at or past, read at
    /// the start of each block; [`NO_POP_LIMIT`] when free. Raised only while
    /// the feeder takes the queue back (see the module doc).
    pub(crate) pop_limit: AtomicUsize,
    /// The node has taken a fence's first frame and still waits for its base
    /// frame, the next one it pops (written at the end of each block): taking
    /// the queue back must leave that frame in place.
    pub(crate) fence_open: AtomicBool,
}

/// [`StreamCtrl::pop_limit`] when the node may pop freely.
pub const NO_POP_LIMIT: usize = usize::MAX;

/// The longest the feeder waits for a node block that began before its pop
/// limit. A block is one device callback's render; past this the audio
/// thread is stalled, and the replan stacks behind the queue instead.
const BLOCK_WAIT: Duration = Duration::from_millis(50);

impl StreamCtrl {
    pub fn new() -> Arc<Self> {
        Arc::new(StreamCtrl {
            gen: AtomicU32::new(0),
            flush_seq: AtomicU32::new(0),
            xruns: std::sync::atomic::AtomicU64::new(0),
            parked: std::sync::atomic::AtomicU64::new(0),
            node_entered: AtomicU32::new(0),
            node_left: AtomicU32::new(0),
            pop_limit: AtomicUsize::new(NO_POP_LIMIT),
            fence_open: AtomicBool::new(false),
        })
    }

    /// Feeder side: return once no node block that may have begun before the
    /// current pop limit is still running. False when one outlasts
    /// [`BLOCK_WAIT`].
    fn wait_out_block(&self) -> bool {
        // SeqCst against the node's SeqCst entry bump and limit read: either
        // this load sees the bump (and waits for that block to end) or the
        // block's limit read sees the limit stored before it.
        let entered = self.node_entered.load(Ordering::SeqCst);
        // Every block entered by then has left (Acquire against the node's
        // Release exit: its head and fence flag are visible). Wrapping: the
        // counters only ever differ by the blocks in flight.
        let done = || self.node_left.load(Ordering::Acquire).wrapping_sub(entered) as i32 >= 0;
        if done() {
            return true;
        }
        let deadline = Instant::now() + BLOCK_WAIT;
        while !done() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }
}

/// The most engine frames one device callback renders out of a stream before
/// the pump can refill it: a 4096-frame device buffer (VPIO's slice ceiling,
/// and the largest buffer an output is expected to run) on a 44.1 kHz device.
/// The output resampler fills its FIFO in whole 1024-frame render blocks, and
/// 4096 x 48/44.1 frames plus its kernel's reach round up to five of them. At
/// the engine rate the same buffer renders exactly 4096.
pub const MAX_CALLBACK_FRAMES: usize = 5 * 1024;

/// The slots a ring of `capacity` frames keeps free (see
/// [`TrackFeeder::fill_reserve`]): one callback's demand plus a replan's fence
/// and one more, so the audio a replan lands behind its fence covers the whole
/// first callback after it. The production ring (16384 frames) then fills
/// 11260 frames ahead of the render (~235 ms) where a quarter's reserve
/// filled 12288 (256 ms): the pump tops it up every 10 ms, and nothing else
/// reads the depth. A ring too small to keep that much free and still hold as
/// much again (the test rings) keeps a quarter of itself free.
fn ring_reserve(capacity: usize) -> usize {
    let quarter = (capacity / 4).max(2);
    let need = MAX_CALLBACK_FRAMES + 4;
    if quarter < need && need <= capacity / 2 {
        need
    } else {
        quarter
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
    /// The newest discarding plan started before the track's span and has
    /// minted no starvation jump since (see [`Self::holds_pre_span_plan`]).
    plan_pre_span: bool,
    /// Frames pushed since the newest DISCARDING fence, fence included, while
    /// the ring may still hold frames from before it (None once it cannot).
    since_flush: Option<usize>,
    /// Frames pushed since the newest discarding fence, fence included (never cleared).
    plan_pushed: usize,
    /// Audio frames read from the file so far (see [`Self::frames_read`]).
    frames_read: u64,
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
            plan_pre_span: false,
            since_flush: None,
            plan_pushed: 0,
            frames_read: 0,
            scratch: vec![[0.0; CHANNELS]; 1024],
        }
    }

    /// Audio frames this feeder has read from its file so far: what its plans
    /// have cost the pump, every replan's refill included. Diagnostics.
    pub fn frames_read(&self) -> u64 {
        self.frames_read
    }

    /// Refresh the reader's file handle (see [`WavTrackReader::reopen`]);
    /// used by the pump thread's transient-error retry.
    pub fn reopen_reader(&mut self) -> Result<(), String> {
        self.reader.reopen()
    }

    /// The slots the fill leaves free: room for a discarding replan's fence
    /// AND fresh audio behind it — enough for the largest device callback
    /// ([`MAX_CALLBACK_FRAMES`]) after both fences (see [`ring_reserve`]). A
    /// quarter of the ring used to be the rule: on the production ring that
    /// put 4092 frames behind a replan, four short of a 4096-frame callback
    /// and 1028 short of the resampler's 5120 — silence at the end of the
    /// first callback after every seek on a large device buffer, since no
    /// pump can add anything until the node drains. The node drops the whole
    /// stale backlog at once when it adopts the fence; with only the fence's
    /// two slots to spare, the fill behind it got just what the node had
    /// consumed since the last pump, and the node ran dry before the next one
    /// — a hole after every seek and on every pump of a loop-handle drag.
    /// Until that backlog is gone, though (see [`Self::since_flush`]), its
    /// space is about to be freed wholesale, so it counts toward the reserve,
    /// down to one more fence: a further replan must still land before the
    /// node next drains (a stopped node drains in `idle`, once per device
    /// callback). Only its own size counts: a clip the playhead had passed
    /// keeps a two-frame jump fence it never pops, the reserve behind the next
    /// replan's fence fell to two, and the fill took the whole ring — so the
    /// replan after that found room for two frames of audio, and the first
    /// callback into the clip played them and then silence.
    ///
    /// A replan normally takes the stale queue back first
    /// ([`Self::discard_queued`]) and has the whole ring behind its fence;
    /// the reserve is what one still has when a node block outlasts the wait
    /// and the fence stacks behind the queue.
    fn fill_reserve(&mut self) -> usize {
        let used = self.producer.capacity() - self.producer.free();
        if let Some(n) = self.since_flush {
            // FIFO: a frame from before the fence remains exactly while the
            // ring holds more than was pushed since it.
            if used <= n {
                self.since_flush = None;
            }
        }
        let reserve = ring_reserve(self.producer.capacity());
        match self.since_flush {
            Some(n) => reserve.saturating_sub(used - n).max(2),
            None => reserve,
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

    /// Takes back everything still queued — all of it stale once a
    /// discarding replan starts — so the replan's fence lands at the node's
    /// head with the whole ring behind it. Left queued, the stale lookahead
    /// held the ring until the node drained it in its next callback (popping
    /// it frame by frame on the audio thread), and a second replan before
    /// that callback — a ruler drag, two quick seeks — found the first
    /// replan's audio in the room it needed: its fence landed with nothing
    /// behind it, and the whole callback was silent.
    ///
    /// Returns whether it raised the pop limit, which the caller lifts once
    /// the new fence is pushed. Nothing is taken back (and the replan stacks
    /// behind the queue as before) when a node block outlasts the wait.
    fn discard_queued(&mut self) -> bool {
        let head = self.producer.consumer_pos();
        if head == self.producer.producer_pos() {
            return false; // nothing queued
        }
        // The node stops at `head` from its next block on; a block already
        // running may still pass it, so wait that one out.
        self.ctrl.pop_limit.store(head, Ordering::SeqCst);
        if !self.ctrl.wait_out_block() {
            self.ctrl.pop_limit.store(NO_POP_LIMIT, Ordering::Release);
            return false;
        }
        // Stable now: every later block stops at the limit, at or below it.
        let head = self.producer.consumer_pos();
        // A fence's first frame already taken: its base is the next pop.
        let keep = usize::from(self.ctrl.fence_open.load(Ordering::Acquire));
        // SAFETY: the pop limit (at or below `head`) keeps the node from every
        // slot at `head + keep` and beyond until the caller lifts it, after the
        // new fence is published; no block that missed the limit is running.
        if unsafe { self.producer.retract(head.wrapping_add(keep)) } {
            // Nothing of the old plans remains but a kept base frame.
            self.since_flush = None;
        }
        true
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
        let limited = self.discard_queued();
        let pushed = self.push_fence(next_gen, pos, true);
        if limited {
            self.ctrl.pop_limit.store(NO_POP_LIMIT, Ordering::Release);
        }
        if pushed {
            self.generation = next_gen;
            self.next_tl = pos;
            self.plan_pos = pos;
            self.plan_pre_span = pos <= self.start_frame;
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

    /// A seek (or pause, or play) that stays before the track's span changes
    /// nothing this track will play. A plan that starts before the span queues
    /// the same frames whatever its start: the fill skips straight to the
    /// span, and the node renders the pre-span positions as silence without
    /// popping, then clamps to the span's first frame. So the held plan stands
    /// while its node has consumed none of it (the FIFO check of
    /// [`Self::resumes_held_plan`]) and the loop is unchanged. Replanning
    /// instead landed a discarding fence behind the whole queue: the node
    /// popped every queued frame on the audio thread to reach it, and the pump
    /// read the same frames from disk again — for every clip ahead of the
    /// playhead, on every seek of a ruler drag.
    fn holds_pre_span_plan(&self, t: &Shared, basis: &PlanBasis) -> bool {
        let Some(old) = self.basis else {
            return false;
        };
        self.plan_pre_span
            && self.producer.capacity() - self.producer.free() + 2 >= self.plan_pushed
            && (old.loop_on, old.ls, old.le) == (basis.loop_on, basis.ls, basis.le)
            && Self::planned_pos(t, basis) <= self.start_frame
    }

    /// Whether the node is parked on THIS plan at a position the transport
    /// has already passed. A plan starts where the next block renders, and the
    /// render only moves forward from there — but for a loop wrap. So a node
    /// parked ahead of the render was overtaken by one: the render left loop
    /// end after the plan read its position (a commit's prime, a seek or loop
    /// edit a callback short of loop end, its audio landing late), or the ring
    /// ran dry short of loop end. The node then waits, over the pre-wrap rest
    /// of its run, for the transport to come round to it — a whole pass of
    /// silence. A wrap into a loop's stretch before the track's span strands
    /// it the same way (the clip's head would play silent), and the node
    /// reports that park too. The one legitimate park is on a seek still
    /// pending to that very base, which `planned_pos` reports as its target.
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

    /// The transport facts a plan made now would rest on.
    fn basis_of(t: &Shared) -> PlanBasis {
        PlanBasis {
            flush_gen: t.out_flush_gen(),
            playing: t.is_playing(),
            loop_on: t.loop_enabled(),
            ls: t.loop_start(),
            le: t.loop_end(),
        }
    }

    /// Brings the plan up to date with the transport (a replan on any
    /// discontinuity): what every fill does first.
    fn sync_plan(&mut self, t: &Shared) -> PlanBasis {
        let basis = Self::basis_of(t);
        if self.basis != Some(basis) {
            if self.resumes_held_plan(t, &basis) || self.holds_pre_span_plan(t, &basis) {
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
        // The one moment the model hides a starved plan is the render clock
        // standing exactly AT loop end (a block ended there): `pos` is loop
        // start, behind the plan, while the render has run past the plan
        // this pass and is about to wrap to a node stuck where the ring ran
        // dry — the next pass silent up to there. So the test is on the
        // render's own position, and the jump lands on the modelled one.
        // The ring must actually be DRAINED: right after a wrap fence the
        // plan restarts at loop_start while the transport is still playing
        // the pre-wrap tail out of the ring, so `pos > next_tl` alone would
        // jump the next pass ahead of the node and silence most of it. Past a
        // non-looped track's end the node never pops the one jump fence, so
        // the same check keeps every later pump from minting another.
        let pos = Self::planned_pos(t, &basis);
        let render = t.render_pos().max(0);
        if basis.playing
            && render > self.next_tl
            && self.producer.free() == self.producer.capacity()
        {
            let next_gen = self.generation.wrapping_add(1);
            if self.push_fence(next_gen, pos, false) {
                self.generation = next_gen;
                self.next_tl = pos;
                // The run now starts where the jump put it, not where the plan did.
                self.plan_pre_span = false;
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
            self.frames_read += got as u64;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::live::{spawn, EngineHandle};
    use crate::node::{Node, ProcessIo};
    use crate::ring::spsc;
    use crate::source::StreamingSource;
    use std::collections::HashMap;

    const FRAMES: usize = 20_000;

    /// A block the node entered and never left (a panic the callback caught
    /// mid-block; the engine renders on) must read as still inside for good,
    /// so no take-back ever rewrites slots under the block after it. With one
    /// counter bumped on entry and exit, that block's lost exit flipped its
    /// odd/even reading: the next block, running, read as outside.
    #[test]
    fn a_block_that_never_left_keeps_the_feeder_from_taking_the_queue_back() {
        let ctrl = StreamCtrl::new();
        // Clean blocks: entered and left.
        for _ in 0..3 {
            ctrl.node_entered.fetch_add(1, Ordering::SeqCst);
            ctrl.node_left.fetch_add(1, Ordering::Release);
        }
        assert!(ctrl.wait_out_block(), "nothing in flight");
        // A block enters and never leaves; the next one enters and runs.
        ctrl.node_entered.fetch_add(1, Ordering::SeqCst);
        ctrl.node_entered.fetch_add(1, Ordering::SeqCst);
        let t = Instant::now();
        assert!(!ctrl.wait_out_block(), "read the running block as outside");
        assert!(t.elapsed() >= BLOCK_WAIT, "it must wait, then fall back");
    }

    fn value(i: usize) -> Smp {
        (i as Smp * 1e-3).sin() * 0.5 + 0.25
    }

    /// A transport to plan from (its render never runs: `render_pos` is the
    /// seek target the test sets), a streamed track at 0, and its node.
    fn rig(
        tag: &str,
    ) -> (
        EngineHandle,
        TrackFeeder,
        StreamingSource,
        std::path::PathBuf,
    ) {
        let path =
            std::env::temp_dir().join(format!("vocara_feed_{tag}_{}.wav", std::process::id()));
        let pcm: Vec<Smp> = (0..FRAMES).flat_map(|i| [value(i), -value(i)]).collect();
        crate::wav::write_wav_f32(
            &path,
            &pcm,
            CHANNELS as u16,
            crate::sample::ENGINE_SR as u32,
        )
        .unwrap();
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: Vec::<Edge>::new(),
            master: 1,
        };
        let g = desc.compile(crate::sample::ENGINE_SR, 512).unwrap();
        let (_mic, mic_cons) = spsc::<[Smp; CHANNELS]>(16);
        let (h, _rt) = spawn(g, HashMap::new(), mic_cons, 16);
        let ctrl = StreamCtrl::new();
        let (prod, cons) = spsc::<[Smp; CHANNELS]>(4096);
        let reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
        let node = StreamingSource::new(Arc::clone(&ctrl), cons, 0, FRAMES as i64);
        (h, TrackFeeder::new(ctrl, prod, reader, 0), node, path)
    }

    fn block(node: &mut StreamingSource, tl: i64, frames: usize) -> Vec<Smp> {
        let mut out = vec![0.0; frames * CHANNELS];
        let inputs: [&[Smp]; 0] = [];
        let mut outs: [&mut [Smp]; 1] = [&mut out[..]];
        let mut io = ProcessIo {
            inputs: &inputs,
            outputs: &mut outs[..],
            frames,
            tl,
            sr: crate::sample::ENGINE_SR,
        };
        node.process(&mut io);
        out
    }

    #[test]
    fn taking_the_queue_back_keeps_the_base_of_a_half_taken_fence() {
        // The node can take a fence's first frame in the moment before its base is pushed,
        // and wait for the base on its next pop. Taking the queue back from the node's head
        // would hand it the NEW fence's first frame as that base: an armed position read
        // from a NaN's bits, the new fence consumed by the misreading, and every frame after
        // it drained to silence for want of a fence that never comes again.
        let (h, mut f, mut node, path) = rig("halffence");
        h.play();
        f.pump(h.transport()).unwrap();
        let mut tl = 0i64;
        while f.producer.free() < f.producer.capacity() {
            block(&mut node, tl, 512); // play out the whole queue
            tl += 512;
        }
        // An in-band fence's first frame alone, as the node can find it mid-push.
        let gen = f.generation.wrapping_add(1);
        f.generation = gen;
        let [first, base] = fence_frames(gen, tl);
        assert!(f.producer.push(first));
        block(&mut node, tl, 512);
        assert!(
            f.ctrl.fence_open.load(Ordering::Acquire),
            "the node holds the fence"
        );
        assert!(f.producer.push(base));
        f.pushed(2);
        tl += 512;

        // A seek's replan, with a node block landing after the queue was taken back and the
        // new fence pushed but before the flush is announced.
        h.seek(10_000);
        let basis = TrackFeeder::basis_of(h.transport());
        let next_gen = f.generation.wrapping_add(1);
        assert!(f.discard_queued(), "the queue is taken back");
        assert!(f.push_fence(next_gen, 10_000, true));
        f.ctrl.pop_limit.store(NO_POP_LIMIT, Ordering::Release);
        f.basis = Some(basis);
        f.generation = next_gen;
        f.next_tl = 10_000;
        f.plan_pos = 10_000;
        f.run_end = i64::MAX;
        block(&mut node, tl, 512);
        f.ctrl.flush_seq.fetch_add(1, Ordering::Release);
        f.pump(h.transport()).unwrap();

        // The render jumps to the seek target: every frame there plays.
        let out = block(&mut node, 10_000, 512);
        let _ = std::fs::remove_file(&path);
        for (i, fr) in out.chunks(2).enumerate() {
            assert_eq!(fr[0], value(10_000 + i), "frame {i} after the seek");
        }
    }

    #[test]
    fn the_production_ring_reserves_a_whole_callback_behind_both_fences() {
        // ffi streams every track through a 16384-frame ring.
        let r = ring_reserve(16_384);
        assert!(r - 4 >= MAX_CALLBACK_FRAMES, "reserve {r}");
        assert!(
            16_384 - r >= MAX_CALLBACK_FRAMES * 2,
            "lookahead {}",
            16_384 - r
        );
        // Small (test) rings keep their quarter.
        assert_eq!(ring_reserve(8192), 2048);
        assert_eq!(ring_reserve(2048), 512);
        assert_eq!(ring_reserve(32_768), 8192);
    }
}
