//! The live engine: a control-side [`EngineHandle`] and an audio-side [`RtProcessor`] that
//! share transport state through atomics and exchange a graph through an RCU cell. The
//! cpal callback calls `RtProcessor::process`; everything else (params, PCM feed, graph
//! edits) is driven from the control thread through wait-free seams.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::command::{command_channel, CommandReceiver, CommandSender};
use crate::dsp::denormals::FlushGuard;
use crate::dsp::reverb::{param as rparam, ReverbNode};
use crate::dsp::smooth::Smoothed;
use crate::graph::{Graph, NodeId};
use crate::graph_cell::GraphCell;
use crate::node::{Node, ProcessIo};
use crate::ring::{Consumer, Producer};
use crate::sample::{Smp, CHANNELS, ENGINE_SR};

/// Max parameter commands applied per audio block (bounds RT work).
const MAX_CMDS_PER_BLOCK: usize = 64;

/// One contiguous PLAYING render span, mapping a range of the rendered-frame
/// counter to its END position on the timeline (seqlock-published by the
/// audio thread, read by the control thread). Silence spans (stopped
/// transport) are GAPS in rendered-counter space — no segment is written.
struct RenderSegment {
    /// Seqlock version: odd while the audio thread is writing.
    ver: AtomicU32,
    r_start: AtomicU64,
    r_end: AtomicU64,
    tl_end: AtomicI64,
}

/// Segment-ring capacity. The consumed cursor only ever lags the render
/// counter by the output FIFO (a handful of render blocks), so the mapping
/// scans recent slots only; 64 covers the deepest in-flight window (an 8 kHz
/// device, step 6) with an order of magnitude to spare.
const SEG_RING: usize = 64;

/// Transport state shared between the control and audio threads.
pub struct Shared {
    frame: AtomicI64,
    blocks: AtomicU64,
    playing: AtomicBool,
    loop_enabled: AtomicBool,
    loop_start: AtomicI64,
    loop_end: AtomicI64,
    seek_req: AtomicI64, // >= 0: pending seek target; -1: none
    /// Live-input monitor gain target (f32 bits). 0 = muted; the audio thread ramps to it.
    mon_target: AtomicU32,
    /// Monitor reverb params (f32 bits): room size, damping, wet amount.
    mon_rev_room: AtomicU32,
    mon_rev_damping: AtomicU32,
    mon_rev_wet: AtomicU32,
    /// CONSUMED-FRAMES CURSOR (the transport-honesty redesign). Three pieces:
    ///
    ///  * `rendered_total` — every engine frame the audio thread has rendered,
    ///    playing or not (silence included), advanced at render time;
    ///  * `consumed_total` — every rendered frame actually DELIVERED to the
    ///    device: equal to `rendered_total` on the identity 48 kHz path (the
    ///    callback renders straight into the device buffer), advanced by the
    ///    resampler as it drains its FIFO otherwise — and advanced OVER frames
    ///    a flush discards, so the two counters share one coordinate space;
    ///  * the segment ring — playing spans of `rendered_total` mapped to
    ///    timeline positions.
    ///
    /// The audible playhead is then map(consumed_total): exact through loop
    /// wraps (pre-wrap frames map to pre-wrap timeline — the old
    /// backlog-subtraction needed a clamp that misreported the position) and
    /// exact at pause (the fold reads the mapping, not a depth estimate).
    rendered_total: AtomicU64,
    consumed_total: AtomicU64,
    seg_head: AtomicU32,
    segments: [RenderSegment; SEG_RING],
    /// Bumped by seek/pause: the resampler's FIFO still holds pre-seek/pre-pause audio, which
    /// must be dropped (not played out) for the transport to respond within one callback.
    out_flush: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Shared {
            frame: AtomicI64::new(0),
            blocks: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            loop_enabled: AtomicBool::new(false),
            loop_start: AtomicI64::new(0),
            loop_end: AtomicI64::new(0),
            seek_req: AtomicI64::new(-1),
            mon_target: AtomicU32::new(0), // 0.0f32 bits
            mon_rev_room: AtomicU32::new(0.6f32.to_bits()),
            mon_rev_damping: AtomicU32::new(0.5f32.to_bits()),
            mon_rev_wet: AtomicU32::new(0.2f32.to_bits()),
            rendered_total: AtomicU64::new(0),
            consumed_total: AtomicU64::new(0),
            seg_head: AtomicU32::new(0),
            segments: std::array::from_fn(|_| RenderSegment {
                ver: AtomicU32::new(0),
                r_start: AtomicU64::new(0),
                r_end: AtomicU64::new(0),
                tl_end: AtomicI64::new(0),
            }),
            out_flush: AtomicU32::new(0),
        }
    }

    /// Audio-thread only: record one contiguous PLAYING span. Seqlock write —
    /// version odd while the fields are torn.
    fn push_segment(&self, r_start: u64, r_end: u64, tl_end: i64) {
        let slot = (self.seg_head.load(Ordering::Relaxed) as usize) % SEG_RING;
        let seg = &self.segments[slot];
        seg.ver.fetch_add(1, Ordering::Release); // odd: write in progress
        seg.r_start.store(r_start, Ordering::Relaxed);
        seg.r_end.store(r_end, Ordering::Relaxed);
        seg.tl_end.store(tl_end, Ordering::Relaxed);
        seg.ver.fetch_add(1, Ordering::Release); // even: consistent
        self.seg_head.fetch_add(1, Ordering::Release);
    }

    /// Control-thread: the timeline position of the last consumed frame —
    /// map(consumed_total) over the segment ring. A cursor inside a playing
    /// span maps linearly; a cursor in a silence gap (or past the newest
    /// span, e.g. paused refill) holds at the last span it passed. None when
    /// nothing has ever both rendered-playing and reached the device.
    fn map_consumed(&self) -> Option<i64> {
        let consumed = self.consumed_total.load(Ordering::Acquire);
        let mut best: Option<(u64, i64)> = None; // (r_end of span, mapped tl)
        for seg in &self.segments {
            // Seqlock read: retry a torn slot a few times, then skip it (the
            // cursor lives in RECENT slots; an old slot mid-overwrite is by
            // definition not the one being mapped).
            for _ in 0..3 {
                let v1 = seg.ver.load(Ordering::Acquire);
                if v1 % 2 != 0 {
                    continue;
                }
                let r_start = seg.r_start.load(Ordering::Relaxed);
                let r_end = seg.r_end.load(Ordering::Relaxed);
                let tl_end = seg.tl_end.load(Ordering::Relaxed);
                let v2 = seg.ver.load(Ordering::Acquire);
                if v1 != v2 {
                    continue;
                }
                if r_end == r_start {
                    break; // never-written slot
                }
                if consumed >= r_end {
                    // Fully consumed span: candidate "hold" position.
                    if best.is_none_or(|(e, _)| r_end > e) {
                        best = Some((r_end, tl_end));
                    }
                } else if consumed > r_start {
                    // Cursor inside this span: exact linear map — this is the
                    // newest span the cursor has reached, and spans are
                    // disjoint in rendered space, so it wins outright.
                    return Some(tl_end - (r_end - consumed) as i64);
                }
                break;
            }
        }
        best.map(|(_, tl)| tl)
    }

    /// Publish the output resampler's un-played FIFO depth (engine-rate frames). Called by
    /// the stream callback after each device buffer (the device module; public so the
    /// dependency-free build doesn't flag it dead).
    /// Stream-callback only: [n] more rendered frames reached the device (or
    /// were flush-DISCARDED and skipped over — either way the cursor moves
    /// forward through rendered-counter space, never through content that
    /// will re-render after the flush's clock rewind).
    pub fn advance_consumed(&self, n: u64) {
        if n > 0 {
            self.consumed_total.fetch_add(n, Ordering::Release);
        }
    }

    /// Identity-path (48 kHz) stream callback: the render IS the delivery, so
    /// after rendering straight into the device buffer the cursor equals the
    /// render counter exactly.
    pub fn sync_consumed_to_rendered(&self) {
        self.consumed_total.store(
            self.rendered_total.load(Ordering::Acquire),
            Ordering::Release,
        );
    }

    /// The current flush generation; the resampler drops its FIFO when this changes.
    pub fn out_flush_gen(&self) -> u32 {
        self.out_flush.load(Ordering::Acquire)
    }

    /// The audible playhead (see [`EngineHandle::frame`], which delegates
    /// here): a pending seek reports its target; paused holds the fold;
    /// playing maps the consumed cursor. Also the position source for the
    /// streaming feeders, which only hold a [`Shared`].
    pub fn playhead(&self) -> i64 {
        let seek = self.seek_req.load(Ordering::Acquire);
        if seek >= 0 {
            return seek;
        }
        if !self.playing.load(Ordering::Acquire) {
            return self.frame.load(Ordering::Acquire);
        }
        self.map_consumed()
            .unwrap_or_else(|| self.frame.load(Ordering::Acquire))
    }

    // Plan-basis reads for the streaming feeders (crate::feed): a change in
    // any of these is a plan discontinuity.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }
    pub fn loop_enabled(&self) -> bool {
        self.loop_enabled.load(Ordering::Acquire)
    }
    pub fn loop_start(&self) -> i64 {
        self.loop_start.load(Ordering::Relaxed)
    }
    pub fn loop_end(&self) -> i64 {
        self.loop_end.load(Ordering::Relaxed)
    }
}

/// Audio-thread half. Owns the graph cell (shared), the command consumer, and the transport
/// atomics. `process` is the cpal render callback body.
pub struct RtProcessor {
    cell: Arc<GraphCell>,
    shared: Arc<Shared>,
    cmd_rx: CommandReceiver,
    max_block: usize,
    /// Live-input monitor: mic frames (already at ENGINE_SR) from the input callback. Mixed
    /// into the output AFTER the playback graph, so the singer hears themselves even when the
    /// transport is stopped. Empty (silence) whenever the input stream is closed.
    mic_rx: Consumer<[Smp; CHANNELS]>,
    mon_gain: Smoothed,
    /// Monitor FX (currently reverb): the mic passes through this before mixing, so the singer
    /// hears themselves with space. `dry = 1` keeps the direct voice; `wet` is user-controlled.
    mon_reverb: ReverbNode,
    mon_in: Vec<Smp>,
    mon_out: Vec<Smp>,
}

impl RtProcessor {
    /// Render `frames` of interleaved stereo into `out`, including the live-input monitor.
    pub fn process(&mut self, out: &mut [Smp], frames: usize) {
        self.render_block(out, frames, true);
    }

    /// Render playback ONLY (no live-input monitor). Used for the VPIO output element, which is
    /// both the speaker signal and the AEC reference: the reference must equal the backing track,
    /// because mixing the singer's monitored voice into it destabilizes echo cancellation (the
    /// backing track then bleeds into the recorded mic). On a built-in speaker the singer hears
    /// themselves acoustically, so no self-monitor is routed to the speaker anyway.
    pub fn process_playback(&mut self, out: &mut [Smp], frames: usize) {
        self.render_block(out, frames, false);
    }

    /// Shared render body. RT-safe: flush-to-zero, drain a bounded number of commands, load the
    /// graph once, advance the sample clock, handle a pending seek and gapless loop wrap. No alloc,
    /// no lock, no syscall. `monitor` adds the live-input monitor on top of the playback mix.
    fn render_block(&mut self, out: &mut [Smp], frames: usize, monitor: bool) {
        let _fz = FlushGuard::new();
        let total = frames * CHANNELS;

        // Load the live graph once for this block (RCU; no refcount, nothing to drop).
        // SAFETY: the control thread's reaper holds a one-block grace epoch before freeing a
        // retired graph, so the pointee outlives this block; we never cache the reference.
        let g: &mut Graph = unsafe { &mut *self.cell.load() };

        self.cmd_rx.apply(g, MAX_CMDS_PER_BLOCK);

        let seek = self.shared.seek_req.swap(-1, Ordering::AcqRel);
        if seek >= 0 {
            self.shared.frame.store(seek, Ordering::Release);
            g.reset();
        }

        if self.shared.playing.load(Ordering::Acquire) {
            let loop_en = self.shared.loop_enabled.load(Ordering::Acquire);
            let ls = self.shared.loop_start.load(Ordering::Relaxed);
            let le = self.shared.loop_end.load(Ordering::Relaxed);
            let mut frame = self.shared.frame.load(Ordering::Relaxed);
            let mut rendered = self.shared.rendered_total.load(Ordering::Relaxed);

            let mut done = 0;
            while done < frames {
                if loop_en && le > ls && frame >= le {
                    frame = ls;
                }
                let mut chunk = (frames - done).min(self.max_block);
                if loop_en && le > ls {
                    let to_end = (le - frame).max(0) as usize;
                    if to_end == 0 {
                        for s in &mut out[done * CHANNELS..total] {
                            *s = 0.0;
                        }
                        // Trailing silence still reaches the device: count it
                        // (a GAP in segment space) so the consumed cursor and
                        // the render counter share one coordinate system.
                        rendered += (frames - done) as u64;
                        break;
                    }
                    chunk = chunk.min(to_end);
                }
                let s = done * CHANNELS;
                let e = (done + chunk) * CHANNELS;
                g.render(chunk, frame, &mut out[s..e]);
                frame += chunk as i64;
                // One PLAYING span per contiguous sub-chunk: loop wraps split
                // chunks here, so every segment is wrap-free and the consumed
                // mapping is exact THROUGH a wrap (pre-wrap frames map to
                // pre-wrap timeline; the old backlog subtraction could not
                // express that and needed a clamp that misreported).
                self.shared
                    .push_segment(rendered, rendered + chunk as u64, frame);
                rendered += chunk as u64;
                done += chunk;
            }
            self.shared.frame.store(frame, Ordering::Release);
            self.shared
                .rendered_total
                .store(rendered, Ordering::Release);
        } else {
            // Stopped transport: silence the playback bed, but still let the monitor through.
            for s in &mut out[..total] {
                *s = 0.0;
            }
            // Silence frames delivered while stopped are rendered frames too
            // — a gap between segments, so a consumed cursor that reaches
            // them HOLDS at the last playing span instead of drifting.
            self.shared
                .rendered_total
                .fetch_add(frames as u64, Ordering::Release);
        }

        if monitor {
            self.add_monitor(out, frames);
        }
        self.shared.blocks.fetch_add(1, Ordering::Release);
    }

    /// Add the live-input monitor on top of an already-rendered buffer, chunked to `max_block`
    /// (`mon_in`/`mon_out` + the reverb scratch are sized for one `max_block`, but a device IO
    /// buffer can exceed it — VPIO permits up to 4096 frames/slice — which would overrun them).
    /// Used both by the normal render path and by the VPIO output (headphone self-monitoring).
    pub fn add_monitor(&mut self, out: &mut [Smp], frames: usize) {
        let mut done = 0;
        while done < frames {
            let chunk = (frames - done).min(self.max_block);
            let s = done * CHANNELS;
            let e = (done + chunk) * CHANNELS;
            self.mix_monitor(&mut out[s..e], chunk);
            done += chunk;
        }
    }

    /// Add the live-input monitor on top of the playback mix. The mic frames (already at
    /// `ENGINE_SR`, resampled in the input callback) pass through the monitor reverb, then a
    /// smoothed gain so arming/disarming never clicks. The ring is empty (→ silence) whenever
    /// the input stream is closed, so this is cheap when monitoring is off.
    #[inline]
    fn mix_monitor(&mut self, out: &mut [Smp], frames: usize) {
        self.mon_gain.set_target(f32::from_bits(
            self.shared.mon_target.load(Ordering::Relaxed),
        ));

        // Drain the mic ring into the monitor input buffer.
        for i in 0..frames {
            let f = self.mic_rx.pop().unwrap_or([0.0, 0.0]);
            self.mon_in[CHANNELS * i] = f[0];
            self.mon_in[CHANNELS * i + 1] = f[1];
        }

        // Live monitor reverb (dry voice + wet tail). Params are pushed from the control thread.
        self.mon_reverb.set_param(
            rparam::ROOM_SIZE,
            f32::from_bits(self.shared.mon_rev_room.load(Ordering::Relaxed)),
        );
        self.mon_reverb.set_param(
            rparam::DAMPING,
            f32::from_bits(self.shared.mon_rev_damping.load(Ordering::Relaxed)),
        );
        self.mon_reverb.set_param(
            rparam::WET,
            f32::from_bits(self.shared.mon_rev_wet.load(Ordering::Relaxed)),
        );
        let n = frames * CHANNELS;
        {
            let inputs: [&[Smp]; 1] = [&self.mon_in[..n]];
            let mut out_slot: [&mut [Smp]; 1] = [&mut self.mon_out[..n]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut out_slot,
                frames,
                tl: 0,
                sr: ENGINE_SR,
            };
            self.mon_reverb.process(&mut io);
        }

        // Mix the processed monitor into the output with the ramped gain.
        for i in 0..frames {
            let g = self.mon_gain.next();
            out[CHANNELS * i] += self.mon_out[CHANNELS * i] * g;
            out[CHANNELS * i + 1] += self.mon_out[CHANNELS * i + 1] * g;
        }
    }
}

/// Shared cell holding the audio-thread [`RtProcessor`] so the active cpal stream's callback
/// can render it while a device switch rebuilds the stream around it.
///
/// SAFETY: only the single *currently active* stream callback ever dereferences the cell, and
/// the audio-host thread builds a new stream strictly after dropping (which joins) the old one
/// — so there is never concurrent access. This mirrors the single-accessor discipline already
/// used by [`GraphCell`]. The `cmd_rx` therefore lives here for the engine's whole lifetime and
/// is never moved or recreated across device switches.
pub struct RtCell {
    inner: UnsafeCell<RtProcessor>,
    /// The transport atomics, cloned out at construction so boundary code (the output
    /// resampler) can reach them WITHOUT dereferencing the single-accessor cell.
    shared: Arc<Shared>,
}

// SAFETY: see the type doc — exactly one stream callback accesses the inner processor at a time.
unsafe impl Send for RtCell {}
unsafe impl Sync for RtCell {}

impl RtCell {
    pub fn new(rt: RtProcessor) -> Arc<RtCell> {
        let shared = Arc::clone(&rt.shared);
        Arc::new(RtCell {
            inner: UnsafeCell::new(rt),
            shared,
        })
    }

    /// The transport atomics (safe — never touches the processor cell).
    pub fn transport(&self) -> &Shared {
        &self.shared
    }

    /// Render one device callback's worth of audio.
    ///
    /// # Safety
    /// The caller must be the only stream callback active for the duration of this call (the
    /// host thread guarantees this by dropping the old stream before building a new one).
    #[inline]
    pub unsafe fn render(&self, out: &mut [Smp], frames: usize) {
        (*self.inner.get()).process(out, frames);
    }

    /// Render playback only (no monitor) — for the VPIO output / AEC reference. See
    /// [`RtProcessor::process_playback`].
    ///
    /// # Safety
    /// Same single-accessor contract as [`RtCell::render`].
    #[inline]
    pub unsafe fn render_playback(&self, out: &mut [Smp], frames: usize) {
        (*self.inner.get()).process_playback(out, frames);
    }

    /// Add the live-input monitor (mic → reverb → smoothed gain) on top of `out`. For the VPIO
    /// output to route self-monitoring to the speaker/headphones WITHOUT putting the voice into the
    /// AEC reference (which is captured before this is called). Silent when the monitor gain is 0.
    ///
    /// # Safety
    /// Same single-accessor contract as [`RtCell::render`].
    #[inline]
    pub unsafe fn mix_monitor(&self, out: &mut [Smp], frames: usize) {
        (*self.inner.get()).add_monitor(out, frames);
    }
}

/// Control-thread half. Drives transport, parameters, PCM feed, and graph hot-swaps.
pub struct EngineHandle {
    cell: Arc<GraphCell>,
    shared: Arc<Shared>,
    cmd_tx: CommandSender,
    sources: HashMap<NodeId, Producer<[Smp; CHANNELS]>>,
    retired: Vec<(*mut Graph, u64)>, // (ptr, block after which it's safe to free)
    /// Whether a stream callback that can render the graph may be running. Maintained by the
    /// audio host (device feature): it holds the lock while setting `true` BEFORE any stream is
    /// built and `false` only AFTER every stream has been dropped (drop joins the callback).
    /// `reap` reads it under the same lock, so a `false` read proves no RT thread can hold a
    /// retired graph pointer — retirees are then freed immediately (the grace epoch would never
    /// elapse, since `blocks` only advances while a callback renders). Defaults to `true`
    /// (conservative: without a host the grace-epoch path behaves exactly as before).
    renderer_gate: Arc<Mutex<bool>>,
    sr: f64,
    max_block: usize,
}

impl EngineHandle {
    /// The transport atomics (plan basis for the streaming feeders).
    pub fn transport(&self) -> &Shared {
        &self.shared
    }

    /// The transport Arc itself — for the feeder pump thread, which outlives
    /// any single borrow of the handle.
    pub fn shared_arc(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn play(&self) {
        // IDEMPOTENT: only a genuine stopped→playing transition acts. A repeated play()
        // while already playing would run the flush below against LIVE audio sitting in
        // the resampler FIFO — an audible skip of up to one render block. Single control
        // thread, so load-then-act cannot race another play().
        if self.shared.playing.load(Ordering::Acquire) {
            return;
        }
        // Drop whatever the output resampler pre-rendered while the transport was stopped —
        // a paused callback keeps the FIFO topped up with SILENCE, so without this flush a
        // resume on a non-48 kHz device played up to one render block of it before the
        // freshly rendered audio. Flushed before the playing flip so the first playing
        // callback starts from an empty FIFO.
        self.flush_out_boundary();
        self.shared.playing.store(true, Ordering::Release);
    }
    pub fn pause(&self) {
        // IDEMPOTENT: swap-and-check so only the playing→paused transition folds.
        if !self.shared.playing.swap(false, Ordering::AcqRel) {
            return;
        }
        // Fold the clock back to what the listener actually HEARD, read from
        // the consumed-frames cursor: map(consumed_total) is exact — through
        // loop wraps too, where the old backlog-depth subtraction could land
        // before loop_start and had to clamp (misreporting the position).
        // Rewinding means resume re-renders exactly the frames that never
        // reached the ears — no gap, no repeat. A callback already in flight
        // when `playing` flipped can still deliver up to one device buffer
        // after this read; that race is physical (the listener heard those
        // frames) and only costs the same bounded precision it always did.
        if let Some(heard) = self.shared.map_consumed() {
            self.shared.frame.store(heard, Ordering::Release);
        }
        // Drop the FIFO's pre-pause audio so the output falls silent within one callback.
        self.shared.out_flush.fetch_add(1, Ordering::Release);
    }
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }
    /// Request a seek; the audio thread applies it at the next block (resets node tails).
    pub fn seek(&self, frame: i64) {
        self.shared.seek_req.store(frame.max(0), Ordering::Release);
        self.flush_out_boundary();
    }

    /// Drop the output resampler's pre-rendered FIFO (it holds pre-seek/pre-pause audio the
    /// listener must not hear). The callback advances the consumed cursor
    /// OVER the discarded frames when it processes the flush, keeping the
    /// cursor and the render counter in one coordinate space.
    fn flush_out_boundary(&self) {
        self.shared.out_flush.fetch_add(1, Ordering::Release);
    }
    pub fn set_loop(&self, enabled: bool, start: i64, end: i64) {
        self.shared
            .loop_start
            .store(start.max(0), Ordering::Relaxed);
        self.shared.loop_end.store(end.max(0), Ordering::Relaxed);
        self.shared.loop_enabled.store(enabled, Ordering::Release);
    }

    /// Set the live-input monitor gain (linear; 0 = muted). The audio thread ramps to it.
    pub fn set_monitor_gain(&self, gain: f32) {
        self.shared
            .mon_target
            .store(gain.max(0.0).to_bits(), Ordering::Release);
    }

    /// Set the monitor reverb (room/damping/wet, each 0..1). The audio thread applies them.
    pub fn set_monitor_reverb(&self, room: f32, damping: f32, wet: f32) {
        self.shared
            .mon_rev_room
            .store(room.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        self.shared
            .mon_rev_damping
            .store(damping.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        self.shared
            .mon_rev_wet
            .store(wet.clamp(0.0, 1.0).to_bits(), Ordering::Release);
    }
    /// Current playhead in samples: the CONSUMED-frames cursor mapped back to
    /// the timeline — what the listener is hearing, exactly, including
    /// through loop wraps (pre-wrap frames map to pre-wrap timeline; the old
    /// render-clock-minus-backlog arithmetic could not express a wrap and
    /// clamped). A pending seek reports its target (the audio itself reaches
    /// the device within the same one callback that applies it). Paused, the
    /// clock holds the fold pause() latched. Before anything has ever been
    /// delivered, the render clock is the only truth there is.
    pub fn frame(&self) -> i64 {
        self.shared.playhead()
    }
    pub fn sample_rate(&self) -> f64 {
        self.sr
    }

    /// Queue a parameter change (coalesced). Call [`flush`](Self::flush) to publish.
    pub fn set_param(&mut self, node: NodeId, param: u16, value: f32) {
        self.cmd_tx.set(node, param, value);
    }
    pub fn flush(&mut self) {
        self.cmd_tx.flush();
    }

    /// Feed decoded interleaved-stereo frames to a source's ring. Returns frames accepted
    /// (back-pressure: a short return means slow the feeder; never blocks).
    pub fn push_pcm(&mut self, source: NodeId, frames: &[[Smp; CHANNELS]]) -> usize {
        match self.sources.get_mut(&source) {
            Some(p) => {
                let mut n = 0;
                for &f in frames {
                    if p.push(f) {
                        n += 1;
                    } else {
                        break;
                    }
                }
                n
            }
            None => 0,
        }
    }

    /// Hot-swap the live graph (e.g. after adding a send or a track). The old graph is freed
    /// on this (control) thread after a one-block grace epoch — never on the audio thread.
    pub fn publish_graph(&mut self, graph: Graph) {
        debug_assert_eq!(graph.max_block(), self.max_block);
        let retired = self.cell.publish(Box::new(graph));
        let safe_after = self.shared.blocks.load(Ordering::Acquire) + 2;
        self.retired.push((retired, safe_after));
        self.reap();
    }

    /// The renderer-activity gate shared with the audio host (see the field doc). The host
    /// stores stream up/down transitions through it; `reap` frees retirees immediately on a
    /// `false` read.
    pub fn renderer_gate(&self) -> Arc<Mutex<bool>> {
        Arc::clone(&self.renderer_gate)
    }

    /// Retired graphs still awaiting their grace epoch (diagnostics / tests).
    pub fn retired_count(&self) -> usize {
        self.retired.len()
    }

    /// Free any retired graphs whose grace epoch has elapsed, and drain any parameter
    /// commands a previous [`flush`](Self::flush) left pending against a full ring. Safe to
    /// call anytime on the control thread.
    pub fn reap(&mut self) {
        // Re-flush leftovers from an overflowed flush (e.g. a post-rebuild parameter storm
        // exceeding the ring capacity). This sits on the control-plane hot path — the FFI
        // frame-position getter calls reap() while the UI polls the playhead — so the tail
        // of a storm finishes delivering within milliseconds during playback instead of
        // waiting for the next user edit. Cheap no-op when nothing is pending.
        self.cmd_tx.flush();

        if self.retired.is_empty() {
            return;
        }
        // With no stream callback running (gate false) the grace epoch can never elapse —
        // `blocks` only advances inside a render — so each commit would park one more full
        // graph forever. A false read under the gate lock proves quiescence (the host sets
        // false only after dropping — i.e. joining — every stream, and sets true before
        // building one), so everything can be freed immediately. Poisoned lock → assume a
        // renderer is running (conservative: fall through to the grace-epoch path).
        let idle = self.renderer_gate.lock().map(|g| !*g).unwrap_or(false);
        if idle {
            for (ptr, _) in self.retired.drain(..) {
                // SAFETY: the gate read proved no stream callback exists, and any stream the
                // host starts later loads the CURRENT cell value — this retiree is unreachable.
                unsafe { drop(Box::from_raw(ptr)) };
            }
            return;
        }

        let now = self.shared.blocks.load(Ordering::Acquire);
        let mut i = 0;
        while i < self.retired.len() {
            let (ptr, safe_after) = self.retired[i];
            if now >= safe_after {
                // SAFETY: at least one audio block has fully elapsed since the swap, so the
                // audio thread can no longer hold a borrow of this graph.
                unsafe { drop(Box::from_raw(ptr)) };
                self.retired.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // Reclaim any still-retired graphs (the RT side is gone by the time the handle drops
        // in normal teardown).
        for (ptr, _) in self.retired.drain(..) {
            // SAFETY: teardown — no audio thread is rendering.
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}

/// Build the live engine from a compiled graph and the per-source feed producers (the ring
/// consumers must already live inside the graph's `RingSource` nodes).
pub fn spawn(
    graph: Graph,
    producers: HashMap<NodeId, Producer<[Smp; CHANNELS]>>,
    mic_rx: Consumer<[Smp; CHANNELS]>,
    cmd_capacity: usize,
) -> (EngineHandle, RtProcessor) {
    let sr = graph.sample_rate();
    let max_block = graph.max_block();
    let cell = Arc::new(GraphCell::new(Box::new(graph)));
    let shared = Arc::new(Shared::new());
    let (cmd_tx, cmd_rx) = command_channel(cmd_capacity);
    let handle = EngineHandle {
        cell: Arc::clone(&cell),
        shared: Arc::clone(&shared),
        cmd_tx,
        sources: producers,
        retired: Vec::new(),
        renderer_gate: Arc::new(Mutex::new(true)),
        sr,
        max_block,
    };
    let mut mon_gain = Smoothed::new(0.0);
    mon_gain.set_time(0.010, ENGINE_SR); // 10 ms arm/disarm ramp
                                         // Monitor reverb: a pleasant vocal room, dry kept at 1 so the direct voice is always heard.
    let mut mon_reverb = ReverbNode::new(0.6, 0.5, 0.2, 1.0, 1.0);
    mon_reverb.prepare(ENGINE_SR, max_block);
    let rt = RtProcessor {
        cell,
        shared,
        cmd_rx,
        max_block,
        mic_rx,
        mon_gain,
        mon_reverb,
        mon_in: vec![0.0; max_block * CHANNELS],
        mon_out: vec![0.0; max_block * CHANNELS],
    };
    (handle, rt)
}
