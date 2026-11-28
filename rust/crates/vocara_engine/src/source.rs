//! A buffer-backed source node: owns interleaved-stereo PCM placed at `start_frame` on the
//! shared timeline, emitting silence before/after its span. This is the deterministic
//! offline counterpart of the live ring-fed source (which reads decoded PCM from an SPSC
//! ring); both place audio against the one sample-counter clock, so multi-source sync is
//! structural — there is no per-source drift correction.

use std::sync::Arc;

use crate::declick::fade_shape;
use crate::node::{Node, ProcessIo};
use crate::ring::Consumer;
use crate::sample::{Smp, CHANNELS};

/// A cut edge's fade: 3 ms at the engine rate (see [`Cut`]).
pub const CUT_FADE_FRAMES: i64 = 144;

/// Which edges of a placed span of a clip are cuts and fade. Where clips overlap on a lane
/// only the one stacked above plays, so the one below plays in pieces, cut off where the
/// clip above starts and taken up again where it ends; a cut mid-waveform clicks, so it
/// fades over [`CUT_FADE_FRAMES`] (half the span at most). A clip's own start and end play
/// as recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cut {
    pub fade_in: bool,
    pub fade_out: bool,
}

impl Cut {
    /// Whether either edge fades.
    #[inline]
    pub fn any(self) -> bool {
        self.fade_in || self.fade_out
    }

    /// The gain at frame `local` of a span `len` frames long.
    #[inline]
    pub fn gain(self, local: i64, len: i64) -> f32 {
        let fade = CUT_FADE_FRAMES.min(len / 2).max(1) as f32;
        let mut g = 1.0;
        if self.fade_in {
            g *= fade_shape((local as f32 + 0.5) / fade);
        }
        if self.fade_out {
            g *= fade_shape(((len - local) as f32 - 0.5) / fade);
        }
        g
    }
}

/// The frames of a `total`-frame file a span plays: from `src_offset` (clamped into the
/// file), `len` frames or to the file's end (`None`), never past it.
pub fn span_window(total: i64, src_offset: i64, len: Option<i64>) -> (i64, i64) {
    let total = total.max(0);
    let from = src_offset.clamp(0, total);
    let left = total - from;
    (from, len.map_or(left, |l| l.clamp(0, left)))
}

pub struct BufferSource {
    pcm: Arc<[Smp]>, // interleaved stereo
    start_frame: i64,
    len_frames: i64,
    /// The first frame of `pcm` the span plays (see [`BufferSource::with_span`]).
    src_offset: i64,
    cut: Cut,
}

impl BufferSource {
    pub fn new(pcm: Vec<Smp>, start_frame: i64) -> Self {
        BufferSource::from_arc(pcm.into(), start_frame)
    }

    /// Build from shared PCM (no copy) — lets several graph rebuilds share one decode.
    pub fn from_arc(pcm: Arc<[Smp]>, start_frame: i64) -> Self {
        let len_frames = (pcm.len() / CHANNELS) as i64;
        BufferSource {
            pcm,
            start_frame,
            len_frames,
            src_offset: 0,
            cut: Cut::default(),
        }
    }

    /// Plays only the part of the clip a lane hears: from frame `src_offset` of the PCM, `len`
    /// frames (or to its end), placed at `start_frame`, its cut edges faded (see [`Cut`]).
    pub fn with_span(mut self, src_offset: i64, len: Option<i64>, cut: Cut) -> Self {
        let total = (self.pcm.len() / CHANNELS) as i64;
        (self.src_offset, self.len_frames) = span_window(total, src_offset, len);
        self.cut = cut;
        self
    }

    /// Build from a mono buffer by duplicating to both channels.
    pub fn from_mono(mono: &[Smp], start_frame: i64) -> Self {
        let mut pcm = vec![0.0; mono.len() * CHANNELS];
        for (i, &s) in mono.iter().enumerate() {
            pcm[CHANNELS * i] = s;
            pcm[CHANNELS * i + 1] = s;
        }
        BufferSource::new(pcm, start_frame)
    }

    pub fn len_frames(&self) -> i64 {
        self.len_frames
    }
}

impl Node for BufferSource {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn num_in(&self) -> usize {
        0
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let out = &mut *io.outputs[0];
        for i in 0..io.frames {
            let local = io.tl + i as i64 - self.start_frame;
            if local >= 0 && local < self.len_frames {
                let p = (local + self.src_offset) as usize * CHANNELS;
                let g = if self.cut.any() {
                    self.cut.gain(local, self.len_frames)
                } else {
                    1.0
                };
                out[CHANNELS * i] = self.pcm[p] * g;
                out[CHANNELS * i + 1] = self.pcm[p + 1] * g;
            } else {
                out[CHANNELS * i] = 0.0;
                out[CHANNELS * i + 1] = 0.0;
            }
        }
    }
}

/// A DISK-STREAMED timeline source (P1, docs/audio-streaming-plan.md): pops
/// frames a [`crate::feed::TrackFeeder`] pushed for exactly the in-span
/// timeline positions, synchronizing on in-band generation FENCES at every
/// transport discontinuity (seek/flush/loop change — and every loop wrap,
/// which keeps a generation's positions strictly contiguous so no loop
/// arithmetic exists here at all). Wait-free: pops and atomic loads only.
///
/// Per frame at timeline position `tl`:
///  * out of the track's span → silence, no pop (the feeder skipped it too);
///  * generation changed → discard ring frames until the matching fence,
///    silence meanwhile (bounded by the ring depth, once per discontinuity);
///  * `ring_pos < tl` (render ran ahead during a replan) → discard to catch
///    up; `ring_pos > tl` (plan starts later) → silence until reached;
///  * ring empty → counted-underrun silence WITHOUT advancing, so the
///    stream self-heals when the feeder catches up.
pub struct StreamingSource {
    ctrl: std::sync::Arc<crate::feed::StreamCtrl>,
    rx: Consumer<[Smp; CHANNELS]>,
    start_frame: i64,
    len_frames: i64,
    cur_gen: u32,
    /// Last observed discard sequence (ctrl.flush_seq).
    cur_flush: u32,
    /// Timeline position of the next ring frame (valid while `armed`).
    ring_pos: i64,
    armed: bool,
    /// A fence's generation frame was popped but its base frame is not
    /// visible yet (the two pushes publish separately): finish adoption on
    /// the next pop.
    pending_fence: Option<u32>,
    /// This block's [`crate::feed::StreamCtrl::pop_limit`]: the ring index
    /// the node stops at while the feeder takes the queue back. A cell: the
    /// read-only ring probes refresh it (see [`Self::at_limit`]).
    limit: std::cell::Cell<usize>,
}

impl StreamingSource {
    pub fn new(
        ctrl: std::sync::Arc<crate::feed::StreamCtrl>,
        rx: Consumer<[Smp; CHANNELS]>,
        start_frame: i64,
        len_frames: i64,
    ) -> Self {
        StreamingSource {
            ctrl,
            rx,
            start_frame,
            len_frames,
            cur_gen: 0,
            cur_flush: 0,
            ring_pos: 0,
            armed: false,
            pending_fence: None,
            limit: std::cell::Cell::new(crate::feed::NO_POP_LIMIT),
        }
    }

    /// Enters a block (see the `feed` module doc, "Taking the queue back"):
    /// marks the node inside, then reads the pop limit the block keeps (until
    /// it sees it lifted, see [`Self::at_limit`]). Both SeqCst, against the
    /// feeder's limit store and its read of the mark: that ordering is what
    /// makes every later take-back wait this block out.
    #[inline]
    fn enter_block(&mut self) {
        use std::sync::atomic::Ordering;
        self.ctrl.node_entered.fetch_add(1, Ordering::SeqCst);
        self.limit.set(self.ctrl.pop_limit.load(Ordering::SeqCst));
    }

    /// Leaves a block: publishes whether a fence is half taken, then marks the
    /// node outside (Release: the feeder that sees the mark sees the head and
    /// that flag).
    #[inline]
    fn leave_block(&mut self) {
        use std::sync::atomic::Ordering;
        self.ctrl
            .fence_open
            .store(self.pending_fence.is_some(), Ordering::Relaxed);
        self.ctrl.node_left.fetch_add(1, Ordering::Release);
    }

    /// Whether this block's pop limit stops the node at its ring head.
    ///
    /// Standing at it, the node reads the limit again: a block can enter in
    /// the moment a take-back holds it (between the feeder's raise and its
    /// lift), and held there for the whole block it dropped a callback out of
    /// steady playback — a loop-handle drag replans on every tick. Seen
    /// lifted (Acquire, against the feeder's Release lift), the take-back is
    /// over: its new fence published, or, after a wait that timed out, nothing
    /// taken back at all; either way the block plays on. A later take-back
    /// waits this block out before it retracts anything, as it does every
    /// block that entered before its raise.
    #[inline]
    fn at_limit(&self) -> bool {
        use std::sync::atomic::Ordering;
        let limit = self.limit.get();
        if limit == crate::feed::NO_POP_LIMIT
            || (self.rx.position().wrapping_sub(limit) as isize) < 0
        {
            return false;
        }
        if self.ctrl.pop_limit.load(Ordering::Acquire) == crate::feed::NO_POP_LIMIT {
            self.limit.set(crate::feed::NO_POP_LIMIT);
            return false;
        }
        true
    }

    /// The ring's next frame (consumed), short of the pop limit.
    #[inline]
    fn ring_pop(&mut self) -> Option<[Smp; CHANNELS]> {
        if self.at_limit() {
            None
        } else {
            self.rx.pop()
        }
    }

    /// The ring's next frame (not consumed), short of the pop limit.
    #[inline]
    fn ring_peek(&self) -> Option<[Smp; CHANNELS]> {
        if self.at_limit() {
            None
        } else {
            self.rx.peek()
        }
    }

    /// Nothing to pop: an empty ring, or the pop limit.
    #[inline]
    fn ring_empty(&self) -> bool {
        self.at_limit() || self.rx.is_empty()
    }

    /// Underruns observed so far (mirrors ctrl.xruns for tests).
    pub fn xruns(&self) -> u64 {
        self.ctrl.xruns.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// True when `f` is a generation fence's FIRST frame (NaN left channel —
    /// real audio is scrubbed finite on every producing path).
    #[inline]
    fn fence_gen(f: [Smp; CHANNELS]) -> Option<u32> {
        if f[0].is_nan() {
            Some(f[1].to_bits())
        } else {
            None
        }
    }

    /// Adopts fence generation [g]: pops its base frame (arming) or parks it
    /// pending when the base is not visible yet. Returns whether armed.
    #[inline]
    fn adopt_fence(&mut self, g: u32) -> bool {
        match self.ring_pop() {
            Some(base) => {
                self.cur_gen = g;
                self.ring_pos = crate::feed::fence_base_tl(base);
                self.armed = true;
                self.pending_fence = None;
                true
            }
            None => {
                self.armed = false;
                self.pending_fence = Some(g);
                false
            }
        }
    }

    /// One pop that transparently completes/handles fences. Returns the next
    /// AUDIO frame, or None when the ring is empty or a fence consumed the
    /// pop (adoption already updated the state).
    #[inline]
    fn pop_audio(&mut self) -> Option<[Smp; CHANNELS]> {
        if let Some(g) = self.pending_fence {
            self.adopt_fence(g);
            return None;
        }
        match self.ring_pop() {
            Some(f) => {
                if let Some(g) = Self::fence_gen(f) {
                    self.adopt_fence(g);
                    None
                } else {
                    Some(f)
                }
            }
            None => None,
        }
    }

    /// Whether the next thing the node takes is a fence: one parked by an
    /// earlier pop, or a fence frame at the ring head (peeked, not consumed).
    #[inline]
    fn head_is_fence(&self) -> bool {
        self.pending_fence.is_some()
            || self
                .ring_peek()
                .is_some_and(|f| Self::fence_gen(f).is_some())
    }

    /// Drain to the newest discarding fence (see `process`). Also runs while the
    /// transport is stopped, so a paused replan's stale backlog leaves the ring.
    fn take_flush(&mut self) {
        use std::sync::atomic::Ordering;
        let flush = self.ctrl.flush_seq.load(Ordering::Acquire);
        if flush != self.cur_flush {
            let want = self.ctrl.gen.load(Ordering::Acquire);
            // Generations rise monotonically in ring order and only DISCARDING fences
            // publish `want`: a node armed at a LATER generation (an in-band wrap/jump
            // fence adopted after `want` was itself adopted in band) is past the drain.
            let reached = |g: u32| (g.wrapping_sub(want) as i32) >= 0;
            // The feeder pushes the flush's fence BEFORE bumping flush_seq,
            // and a starved node polls the ring every frame while flush_seq
            // is read once per block — so `want` may be fully adopted IN BAND
            // already. That adoption consumed the fence: disarming here would
            // hunt a fence that never comes again, discarding every frame the
            // feeder pushes from now on. The adopted state IS the post-drain
            // state; just acknowledge the flush.
            if !(reached(self.cur_gen) && self.armed) {
                self.armed = false;
                // KEEP a fence already parked at `want`: its generation frame
                // is consumed, so dropping it here strands the base frame as
                // one more discarded frame and leaves the drain below hunting
                // a fence that will never be pushed again — every later block
                // would drain the ring to silence until the next replan.
                if !self.pending_fence.is_some_and(reached) {
                    self.pending_fence = None;
                }
                loop {
                    if reached(self.cur_gen) && self.armed {
                        break;
                    }
                    if let Some(g) = self.pending_fence {
                        if !self.adopt_fence(g) {
                            break; // base not visible yet: retry next block
                        }
                        continue;
                    }
                    match self.ring_pop() {
                        Some(f) => {
                            if let Some(g) = Self::fence_gen(f) {
                                if reached(g) {
                                    self.adopt_fence(g);
                                }
                                // A stale fence's base frame is just another
                                // discarded frame on the next iteration.
                            }
                        }
                        None => break, // fence not pushed yet: silence this block
                    }
                }
            }
            if reached(self.cur_gen) && self.armed {
                self.cur_flush = flush;
            }
        }
    }
}

impl Node for StreamingSource {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn num_in(&self) -> usize {
        0
    }

    fn idle(&mut self) {
        self.enter_block();
        self.take_flush();
        self.ctrl
            .parked
            .store(0, std::sync::atomic::Ordering::Release);
        self.leave_block();
    }

    fn process(&mut self, io: &mut ProcessIo) {
        self.enter_block();
        self.render(io);
        self.leave_block();
    }
}

#[cfg(test)]
impl StreamingSource {
    /// [`Node::process`] with `between` run after the block's entry (its
    /// pop-limit read) and before its render: what the feeder does in that
    /// moment.
    pub(crate) fn process_entered(&mut self, io: &mut ProcessIo, between: impl FnOnce()) {
        self.enter_block();
        between();
        self.render(io);
        self.leave_block();
    }
}

impl StreamingSource {
    /// One playing block (see [`Node::process`]).
    fn render(&mut self, io: &mut ProcessIo) {
        // A DISCARDING discontinuity (flush/loop-change/play-state): drain to
        // the newest fence before rendering anything — the ring's older
        // content predates the transport change. A loop WRAP does not bump
        // flush_seq; its fence is adopted in band below, after the pre-wrap
        // tail plays out.
        self.take_flush();
        // Whether the frame just rendered left the node parked AHEAD of the
        // transport (published as StreamCtrl::parked after the block).
        let mut parked = false;
        // Whether that frame lay BEFORE the track's span (see the end of the block).
        let mut pre_span = false;
        for i in 0..io.frames {
            // Complete a fence parked by an earlier pop (its base frame was
            // not visible yet — the two pushes publish separately). Every
            // arming path below is gated on `armed`, which a parked fence
            // clears, so without this retry nothing would ever finish the
            // adoption and the source would sit silent until the next flush.
            if let Some(g) = self.pending_fence {
                self.adopt_fence(g);
            }
            let out = &mut *io.outputs[0];
            let o = CHANNELS * i;
            let tl = io.tl + i as i64;
            let in_span = tl >= self.start_frame && tl < self.start_frame + self.len_frames;
            if !in_span || !self.armed {
                out[o] = 0.0;
                out[o + 1] = 0.0;
                parked = false;
                pre_span = tl < self.start_frame;
                continue;
            }
            pre_span = false;
            // The feeder pushes NOTHING for pre-span positions: advance the
            // position without popping, mirroring its skip exactly.
            if self.ring_pos < self.start_frame {
                self.ring_pos = self.start_frame;
            }
            // Catch up if the render ran ahead of the plan (positions within
            // a generation are contiguous ints; fences are handled
            // transparently and re-arm at their own base).
            while self.armed && self.ring_pos < tl {
                if self.pop_audio().is_some() {
                    self.ring_pos += 1;
                } else if self.pending_fence.is_some() || self.ring_empty() {
                    break; // starved (or mid-fence): retry next frame
                }
            }
            // A BACKWARD jump (the render wrapped to loop_start) parks the
            // next run's fence at the ring head — adopt it. A FORWARD base
            // (a plan starting later than tl) has that future audio at the
            // head instead, which must wait untouched; peek tells them
            // apart without consuming anything. In step (a discarding replan
            // at the playhead) the fence is adopted too: left for the pop
            // below, it took this frame's place and the frame played silent.
            while self.armed && self.ring_pos >= tl {
                if self.head_is_fence() {
                    let _ = self.pop_audio(); // adopts; updates ring_pos
                } else {
                    break;
                }
            }
            // The adoption above can re-arm at a pre-span base (a loop_start
            // before start_frame): the feeder skipped those positions, so skip
            // them here too — the clamp at the top of this iteration already
            // ran, and deferring it a frame silences this one and then discards
            // the track's real first frame in the next frame's catch-up.
            if self.armed && self.ring_pos < self.start_frame {
                self.ring_pos = self.start_frame;
            }
            if !self.armed || self.ring_pos != tl {
                out[o] = 0.0;
                out[o + 1] = 0.0;
                // Armed past `tl` with no fence at the head (the loop above
                // adopts one): waiting for the transport to arrive.
                parked = self.armed && self.ring_pos > tl;
                continue;
            }
            parked = false;
            match self.pop_audio() {
                Some(f) => {
                    out[o] = f[0];
                    out[o + 1] = f[1];
                    self.ring_pos += 1;
                }
                None => {
                    out[o] = 0.0;
                    out[o + 1] = 0.0;
                    // Held at a take-back's limit is the feeder mid-replan, not
                    // the ring run dry: no underrun. The limit is read first, so
                    // a lift between the two reads finds the ring as it now is.
                    if !self.at_limit() && self.rx.is_empty() && self.pending_fence.is_none() {
                        // Wait-free health signal, read via the ctrl by the
                        // FFI aggregate (ve_stream_xruns).
                        self.ctrl
                            .xruns
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        // Before the span the next position this node needs is the span's
        // FIRST frame, so armed past it with no fence at the ring head it is
        // parked ahead of the transport exactly as an in-span park is: a loop
        // that starts before the clip wraps the render into that stretch, and
        // reporting nothing there left a node stranded mid-clip waiting out
        // the clip's head on the next pass. A wrap whose fence is already
        // queued is not a park (the fence re-arms the node before the span).
        // Checked once, for the frame the word describes.
        if pre_span && self.armed && self.ring_pos > self.start_frame && !self.head_is_fence() {
            parked = true;
        }
        // The feeder replans a park the render has wrapped past (it can tell
        // a stranded park from a pending seek's; the node cannot).
        let word = if parked {
            crate::feed::parked_word(self.cur_gen, self.ring_pos)
        } else {
            0
        };
        self.ctrl
            .parked
            .store(word, std::sync::atomic::Ordering::Release);
    }
}

/// The live streaming source: pops decoded interleaved-stereo frames from an SPSC ring fed
/// by a decoder/feeder thread. On underrun it emits silence and bumps a counter (a 1-2 ms
/// fade at the discontinuity would be added for production; the counter is the health
/// signal the UI reads). Sequential streaming — timeline placement/seek are handled by the
/// feeder refilling the ring from the right file offset.
pub struct RingSource {
    rx: Consumer<[Smp; CHANNELS]>,
    xrun: u64,
}

impl RingSource {
    pub fn new(rx: Consumer<[Smp; CHANNELS]>) -> Self {
        RingSource { rx, xrun: 0 }
    }

    /// Underruns observed so far (ring empty when audio was needed).
    pub fn xruns(&self) -> u64 {
        self.xrun
    }
}

impl Node for RingSource {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn num_in(&self) -> usize {
        0
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let out = &mut *io.outputs[0];
        for i in 0..io.frames {
            match self.rx.pop() {
                Some(f) => {
                    out[CHANNELS * i] = f[0];
                    out[CHANNELS * i + 1] = f[1];
                }
                None => {
                    out[CHANNELS * i] = 0.0;
                    out[CHANNELS * i + 1] = 0.0;
                    self.xrun = self.xrun.wrapping_add(1);
                }
            }
        }
    }
}
