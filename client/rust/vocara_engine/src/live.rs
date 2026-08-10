//! The live engine: a control-side [`EngineHandle`] and an audio-side [`RtProcessor`] that
//! share transport state through atomics and exchange a graph through an RCU cell. The
//! cpal callback calls `RtProcessor::process`; everything else (params, PCM feed, graph
//! edits) is driven from the control thread through wait-free seams.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{fence, AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
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
/// scans recent slots only. Every loop wrap ends a segment, so the shortest
/// wrap distance bounds how fast the ring recycles: with loops clamped to
/// [`MIN_LOOP_FRAMES`], 128 slots cover at least 128 × 256 = 32768 frames of
/// in-flight window — an order of magnitude past the deepest device buffer
/// plus FIFO lag (a 4096-frame VPIO slice twice over is ~8k frames).
const SEG_RING: usize = 128;

/// Shortest loop the engine honors, in frames (~5 ms at engine rate). The
/// FFI boundary would happily accept a 10-frame loop; it has no musical
/// meaning, but one render block then wraps dozens of times, each wrap
/// pushing a segment, and the segment ring gets overwritten inside the very
/// window the consumed-cursor mapping still needs — the playhead detaches
/// from what the listener hears. Clamping the loop length bounds
/// wraps-per-block so the ring always covers the in-flight window. An end at
/// or before start still means "no active loop", exactly as before.
pub const MIN_LOOP_FRAMES: i64 = 256;

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
    /// Hard safety mute for the monitor, owned by the audio host — NOT by the UI. Raised while
    /// playback has left the output device the user selected (see `device::OutputBinding`), where
    /// the "these are headphones, monitoring is safe" judgement made from the SELECTION no longer
    /// describes what is actually making sound. Deliberately a separate flag from [`Self::
    /// mon_target`]: the fader writes that one continuously, so a safety decision stored there
    /// would be stomped by the next drag.
    mon_muted: AtomicBool,
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
            mon_muted: AtomicBool::new(false),
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
        // ACQUIRE on the begin-write bump: the data stores below must not become
        // visible before the version turns odd (Release would only order what came
        // BEFORE the bump, letting a reader see new fields under an even version).
        seg.ver.fetch_add(1, Ordering::Acquire); // odd: write in progress
        seg.r_start.store(r_start, Ordering::Relaxed);
        seg.r_end.store(r_end, Ordering::Relaxed);
        seg.tl_end.store(tl_end, Ordering::Relaxed);
        seg.ver.fetch_add(1, Ordering::Release); // even: consistent (publishes the data)
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
                // ACQUIRE fence before the validating recheck: the data loads above
                // must be satisfied before v2 is read (v1's acquire only orders them
                // AFTER v1), or a slot overwritten between the loads could pass
                // v1 == v2 with fields mixed from writes SEG_RING pushes apart.
                fence(Ordering::Acquire);
                let v2 = seg.ver.load(Ordering::Relaxed);
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

    /// Engine frames delivered to the output device so far (the consumed
    /// cursor's raw counter) — the OUTPUT-clock reference the drift
    /// estimator compares the input counter against.
    pub fn consumed_frames(&self) -> u64 {
        self.consumed_total.load(Ordering::Acquire)
    }

    /// Engine frames PRODUCED so far. Delivery can never outrun production, which is the
    /// invariant a retired callback pushing un-rendered frames into a FIFO would break.
    ///
    /// Gated on `device` as well as `test`: its only caller lives in `device`'s test module,
    /// so a plain `cargo test` (no features) compiled it with nothing to call it and reported
    /// it as dead code.
    #[cfg(all(test, feature = "device"))]
    pub(crate) fn rendered_frames_for_test(&self) -> u64 {
        self.rendered_total.load(Ordering::Acquire)
    }

    /// Raise/clear the host's monitor safety mute (see [`Shared::mon_muted`]). Owned by
    /// `device::reconcile_output_safety`; idempotent, and it clears itself once playback is back
    /// on the user's chosen output, so no other caller has to remember to lift it.
    pub fn set_monitor_muted(&self, muted: bool) {
        self.mon_muted.store(muted, Ordering::Release);
    }

    /// Whether the host's monitor safety mute is engaged.
    pub fn monitor_muted(&self) -> bool {
        self.mon_muted.load(Ordering::Acquire)
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
        // The host's safety mute wins over the fader (see `Shared::mon_muted`), and goes through
        // the same smoother, so engaging it ducks over the 10 ms ramp instead of clicking.
        let target = if self.shared.mon_muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.shared.mon_target.load(Ordering::Relaxed))
        };
        self.mon_gain.set_target(target);

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
/// There is exactly ONE of these for the engine's whole life, and every output path — the cpal
/// identity callback, the output resampler, the VPIO output element — holds a clone of the same
/// `Arc`. The host thread's discipline (drop the old stream, which joins its callback, before
/// building the new one) is what used to make that safe, and it rests on a teardown that CAN
/// fail: `DeviceHandle::drop` and `Vpio::stop` both note a failed stop and carry on, after which
/// the replacement stream's callback and a surviving zombie would take `&mut RtProcessor`
/// concurrently — undefined behaviour, and a data race on the mic ring's consumer cursor.
///
/// So the cell excludes them itself, with two mechanisms that cover different halves:
///
///  * `owner` — the stream generation entitled to render. The host [`RtCell::claim`]s a new one
///    before each build and the callback stamps it, so a zombie retires PERMANENTLY instead of
///    winning alternate callbacks forever (which a bare mutual-exclusion flag would allow, and
///    which would be a worse, deterministic regression).
///  * `busy` — one RT-safe compare-exchange, covering the zombie that is already INSIDE
///    `process` when the new stream starts, which no epoch check can catch.
///
/// A callback denied by either must write silence and touch nothing else — in particular not the
/// transport cursors, or the playhead would advance over audio nobody rendered.
pub struct RtCell {
    inner: UnsafeCell<RtProcessor>,
    /// The transport atomics, cloned out at construction so boundary code (the output
    /// resampler) can reach them WITHOUT dereferencing the single-accessor cell.
    shared: Arc<Shared>,
    /// Generation entitled to the cell; see the type doc. Bumped only on the host thread.
    owner: AtomicU64,
    /// Whether a callback is inside the processor right now; see the type doc.
    busy: AtomicBool,
}

// SAFETY: see the type doc — the owner epoch plus the `busy` compare-exchange admit at most one
// caller into the inner processor at a time, without blocking the RT thread.
unsafe impl Send for RtCell {}
unsafe impl Sync for RtCell {}

/// Proof that the holder is the cell's single current accessor. Releases on drop.
struct RtGuard<'a> {
    cell: &'a RtCell,
}

impl Drop for RtGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.cell.busy.store(false, Ordering::Release);
    }
}

impl RtCell {
    pub fn new(rt: RtProcessor) -> Arc<RtCell> {
        let shared = Arc::clone(&rt.shared);
        Arc::new(RtCell {
            inner: UnsafeCell::new(rt),
            shared,
            // 0 is reserved as "no stream has claimed the cell", so the first claim is 1 and no
            // callback can be built holding an epoch that matches by accident.
            owner: AtomicU64::new(0),
            busy: AtomicBool::new(false),
        })
    }

    /// Take the next stream generation. Call on the AUDIO-HOST THREAD, immediately before
    /// building a stream; the returned epoch goes into that stream's callback, and taking it
    /// retires every older callback whether or not its teardown actually joined.
    pub fn claim(&self) -> u64 {
        self.owner.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// The transport atomics (safe — never touches the processor cell).
    pub fn transport(&self) -> &Shared {
        &self.shared
    }

    /// Whether `epoch` is still the generation entitled to the cell. For callbacks that do work
    /// AROUND the render (FIFO bookkeeping, transport cursors) and must abandon all of it once
    /// retired — not just the render itself.
    #[inline]
    pub fn is_current(&self, epoch: u64) -> bool {
        self.owner.load(Ordering::Acquire) == epoch
    }

    /// Enter the processor as `epoch`, or `None` when a newer stream owns the cell (this
    /// callback is a zombie and must retire) or another accessor is inside right now.
    #[inline]
    fn try_enter(&self, epoch: u64) -> Option<RtGuard<'_>> {
        if self.owner.load(Ordering::Acquire) != epoch {
            return None;
        }
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(RtGuard { cell: self })
    }

    /// Render one device callback's worth of audio as `epoch`. `false` = denied; `out` is
    /// untouched and the caller must emit silence WITHOUT advancing any transport counter.
    #[inline]
    pub fn try_render(&self, epoch: u64, out: &mut [Smp], frames: usize) -> bool {
        let Some(_g) = self.try_enter(epoch) else {
            return false;
        };
        // SAFETY: the guard is the cell's single-accessor proof for this call.
        unsafe { (*self.inner.get()).process(out, frames) };
        true
    }

    /// Render with no device stream around the cell — the offline harness the rlib exists for
    /// (tests, benches). Claims the cell on first use so the same admission path a real callback
    /// takes is exercised, and panics rather than silently producing silence if a live stream
    /// already owns it: offline rendering and a stream must never share one processor.
    pub fn render_offline(&self, out: &mut [Smp], frames: usize) {
        let mut epoch = self.owner.load(Ordering::Acquire);
        if epoch == 0 {
            epoch = self.claim();
        }
        assert!(
            self.try_render(epoch, out, frames),
            "offline render must own the cell"
        );
    }

    /// Render playback only (no monitor) — for the VPIO output / AEC reference. See
    /// [`RtProcessor::process_playback`]. Denial rules as [`RtCell::try_render`].
    #[inline]
    pub fn try_render_playback(&self, epoch: u64, out: &mut [Smp], frames: usize) -> bool {
        let Some(_g) = self.try_enter(epoch) else {
            return false;
        };
        // SAFETY: the guard is the cell's single-accessor proof for this call.
        unsafe { (*self.inner.get()).process_playback(out, frames) };
        true
    }

    /// Add the live-input monitor (mic → reverb → smoothed gain) on top of `out`. For the VPIO
    /// output to route self-monitoring to the speaker/headphones WITHOUT putting the voice into the
    /// AEC reference (which is captured before this is called). Silent when the monitor gain is 0.
    /// Denial rules as [`RtCell::try_render`] — a denied call simply adds nothing.
    #[inline]
    pub fn try_mix_monitor(&self, epoch: u64, out: &mut [Smp], frames: usize) -> bool {
        let Some(_g) = self.try_enter(epoch) else {
            return false;
        };
        // SAFETY: the guard is the cell's single-accessor proof for this call.
        unsafe { (*self.inner.get()).add_monitor(out, frames) };
        true
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
        let start = start.max(0);
        let mut end = end.max(0);
        // Degenerate-length loops are clamped (see MIN_LOOP_FRAMES); an
        // end at or before start keeps meaning "no active loop".
        if end > start && end - start < MIN_LOOP_FRAMES {
            end = start + MIN_LOOP_FRAMES;
        }
        self.shared.loop_start.store(start, Ordering::Relaxed);
        self.shared.loop_end.store(end, Ordering::Relaxed);
        self.shared.loop_enabled.store(enabled, Ordering::Release);
    }

    /// Set the live-input monitor gain (linear; 0 = muted). The audio thread ramps to it. This is
    /// the FADER's value; it does not override the host's safety mute (see
    /// [`Shared::set_monitor_muted`]).
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

#[cfg(test)]
mod rt_cell_guard_tests {
    //! The cell's own exclusion, which replaces "the host dropped the old stream first" — a
    //! guarantee that rests on a teardown the code already knows can fail.

    use super::*;
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::ring::spsc;
    use crate::sample::{Smp, CHANNELS, ENGINE_SR};
    use std::collections::HashMap;

    const BLOCK: usize = 128;

    fn cell() -> Arc<RtCell> {
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: Vec::<Edge>::new(),
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        std::mem::forget(h); // the handle's drop would join the engine; the cell is the subject
        RtCell::new(rt)
    }

    /// Claiming for a replacement stream retires the previous one PERMANENTLY. A bare
    /// mutual-exclusion flag would let a zombie keep winning callbacks — output stuttering
    /// between two renderers forever, which is worse than the race it replaced.
    #[test]
    fn a_new_claim_retires_the_previous_generation_for_good() {
        let c = cell();
        let old = c.claim();
        let new = c.claim();
        assert_ne!(old, new);

        let mut out = vec![0.0; BLOCK * CHANNELS];
        assert!(c.try_render(new, &mut out, BLOCK));
        for _ in 0..4 {
            assert!(
                !c.try_render(old, &mut out, BLOCK),
                "a retired callback must never win a later round either"
            );
        }
        assert!(!c.is_current(old));
        assert!(c.is_current(new));
    }

    /// The epoch cannot cover a zombie that is ALREADY inside `process` when the replacement
    /// starts; the busy flag is what makes that window safe.
    #[test]
    fn two_callers_of_the_same_generation_never_overlap() {
        let c = cell();
        let epoch = c.claim();
        let inside = c.try_enter(epoch).expect("first caller is admitted");
        let mut out = vec![1.0; BLOCK * CHANNELS];
        assert!(
            !c.try_render(epoch, &mut out, BLOCK),
            "a second accessor must be refused while one is inside"
        );
        assert_eq!(out[0], 1.0, "a refused render leaves the buffer untouched");
        drop(inside);
        assert!(
            c.try_render(epoch, &mut out, BLOCK),
            "the guard releases on drop"
        );
    }

    /// A denied caller must not move the transport: the atomics are race-free, but a zombie
    /// stamping them reports a playhead over audio nobody rendered.
    #[test]
    fn a_denied_render_advances_no_transport_counter() {
        let c = cell();
        let live = c.claim();
        let stale = live - 1;
        let mut out = vec![0.0; BLOCK * CHANNELS];
        assert!(c.try_render(live, &mut out, BLOCK));
        c.transport().sync_consumed_to_rendered();
        let consumed = c.transport().consumed_frames();
        let blocks = c.transport().blocks.load(Ordering::Acquire);

        assert!(!c.try_render(stale, &mut out, BLOCK));
        assert_eq!(
            c.transport().consumed_frames(),
            consumed,
            "a retired callback must not deliver frames it never rendered"
        );
        assert_eq!(
            c.transport().blocks.load(Ordering::Acquire),
            blocks,
            "and must not have entered the processor at all"
        );
    }
}

#[cfg(test)]
mod segment_ring_tests {
    //! The segment ring's seqlock under concurrent overwrite. An accepted torn slot
    //! (fields mixed from two writes SEG_RING pushes apart) brackets the consumed
    //! cursor with a stale timeline end, so `map_consumed` — the playhead and
    //! `pause`'s clock fold — would return a position seconds away from what was
    //! actually heard.

    use super::{Shared, SEG_RING};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// The writer pushes contiguous spans whose timeline equals rendered space
    /// (`tl_end == r_end`), so every CONSISTENT read maps the cursor either exactly
    /// (in-span) or to a span end (hold) — a multiple of `SPAN`. The cursor is parked
    /// mid-span just ahead of the slot being overwritten, where only a torn accept
    /// (old `r_start`/`tl_end` under a new `r_end`) can produce anything else.
    #[test]
    fn concurrent_overwrite_never_yields_a_torn_mapping() {
        const SPAN: u64 = 1_000;
        let sh = Arc::new(Shared::new());
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let sh = Arc::clone(&sh);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut k: u64 = 0;
                while !stop.load(Ordering::Relaxed) {
                    sh.push_segment(k * SPAN, (k + 1) * SPAN, ((k + 1) * SPAN) as i64);
                    k += 1;
                }
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let head = sh.seg_head.load(Ordering::Acquire) as u64;
            if head < SEG_RING as u64 + 8 {
                continue;
            }
            // Three spans ahead of the slot under overwrite in scan order: a torn
            // slot is then checked before the span that consistently holds the
            // cursor, so a bad accept cannot hide behind the good one.
            let k = head - SEG_RING as u64 + 3;
            let consumed = k * SPAN + SPAN / 2; // mid-span: never a span end
            sh.consumed_total.store(consumed, Ordering::Release);
            if let Some(tl) = sh.map_consumed() {
                assert!(
                    tl == consumed as i64 || tl % SPAN as i64 == 0,
                    "torn segment accepted: consumed={consumed} mapped to tl={tl}"
                );
            }
        }
        stop.store(true, Ordering::Release);
        writer.join().unwrap();
    }
}
