//! A buffer-backed source node: owns interleaved-stereo PCM placed at `start_frame` on the
//! shared timeline, emitting silence before/after its span. This is the deterministic
//! offline counterpart of the live ring-fed source (which reads decoded PCM from an SPSC
//! ring); both place audio against the one sample-counter clock, so multi-source sync is
//! structural — there is no per-source drift correction.

use std::sync::Arc;

use crate::node::{Node, ProcessIo};
use crate::ring::Consumer;
use crate::sample::{Smp, CHANNELS};

pub struct BufferSource {
    pcm: Arc<[Smp]>, // interleaved stereo
    start_frame: i64,
    len_frames: i64,
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
        }
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
                let p = local as usize * CHANNELS;
                out[CHANNELS * i] = self.pcm[p];
                out[CHANNELS * i + 1] = self.pcm[p + 1];
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
        }
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
        match self.rx.pop() {
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
        match self.rx.pop() {
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
}

impl Node for StreamingSource {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn num_in(&self) -> usize {
        0
    }

    fn process(&mut self, io: &mut ProcessIo) {
        use std::sync::atomic::Ordering;
        // A DISCARDING discontinuity (flush/loop-change/play-state): drain to
        // the newest fence before rendering anything — the ring's older
        // content predates the transport change. A loop WRAP does not bump
        // flush_seq; its fence is adopted in band below, after the pre-wrap
        // tail plays out.
        let flush = self.ctrl.flush_seq.load(Ordering::Acquire);
        if flush != self.cur_flush {
            let want = self.ctrl.gen.load(Ordering::Acquire);
            // The feeder pushes the flush's fence BEFORE bumping flush_seq,
            // and a starved node polls the ring every frame while flush_seq
            // is read once per block — so `want` may be fully adopted IN BAND
            // already. That adoption consumed the fence: disarming here would
            // hunt a fence that never comes again, discarding every frame the
            // feeder pushes from now on. The adopted state IS the post-drain
            // state; just acknowledge the flush.
            if !(self.cur_gen == want && self.armed) {
                self.armed = false;
                // KEEP a fence already parked at `want`: its generation frame
                // is consumed, so dropping it here strands the base frame as
                // one more discarded frame and leaves the drain below hunting
                // a fence that will never be pushed again — every later block
                // would drain the ring to silence until the next replan.
                if self.pending_fence != Some(want) {
                    self.pending_fence = None;
                }
                loop {
                    if self.cur_gen == want && self.armed {
                        break;
                    }
                    if let Some(g) = self.pending_fence {
                        if !self.adopt_fence(g) {
                            break; // base not visible yet: retry next block
                        }
                        continue;
                    }
                    match self.rx.pop() {
                        Some(f) => {
                            if let Some(g) = Self::fence_gen(f) {
                                if g == want {
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
            if self.cur_gen == want && self.armed {
                self.cur_flush = flush;
            }
        }
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
                continue;
            }
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
                } else if self.pending_fence.is_some() || self.rx.is_empty() {
                    break; // starved (or mid-fence): retry next frame
                }
            }
            // A BACKWARD jump (the render wrapped to loop_start) parks the
            // next run's fence at the ring head — adopt it. A FORWARD base
            // (a plan starting later than tl) has that future audio at the
            // head instead, which must wait untouched; peek tells them
            // apart without consuming anything.
            while self.armed && self.ring_pos > tl {
                let head_is_fence = self.pending_fence.is_some()
                    || self.rx.peek().is_some_and(|f| Self::fence_gen(f).is_some());
                if head_is_fence {
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
                continue;
            }
            match self.pop_audio() {
                Some(f) => {
                    out[o] = f[0];
                    out[o + 1] = f[1];
                    self.ring_pos += 1;
                }
                None => {
                    out[o] = 0.0;
                    out[o + 1] = 0.0;
                    if self.rx.is_empty() && self.pending_fence.is_none() {
                        // Wait-free health signal, read via the ctrl by the
                        // FFI aggregate (ve_stream_xruns).
                        self.ctrl
                            .xruns
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
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
