//! The live engine: a control-side [`EngineHandle`] and an audio-side [`RtProcessor`] that
//! share transport state through atomics and exchange a graph through an RCU cell. The
//! cpal callback calls `RtProcessor::process`; everything else (params, PCM feed, graph
//! edits) is driven from the control thread through wait-free seams.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{fence, AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::command::{command_channel, CommandReceiver, CommandSender};
use crate::declick::{fade_shape, Declick, DECLICK_FRAMES, LOOP_FADE_FRAMES};
use crate::dsp::denormals::FlushGuard;
use crate::dsp::reverb::{param as rparam, ReverbNode};
use crate::dsp::smooth::Smoothed;
use crate::graph::{Graph, NodeId};
use crate::graph_cell::GraphCell;
use crate::node::{Node, ProcessIo};
use crate::ring::{Consumer, Producer};
use crate::sample::{Smp, CHANNELS, ENGINE_SR};

/// Max parameter commands applied per audio block (bounds RT work). Sized for the
/// post-commit storm: a rebuilt graph starts every node at its defaults, and the host then
/// re-pushes every live parameter (~58 per clip). At 64 per block a 40-clip project took
/// ~0.4 s to reach the new graph — its later tracks played dry and centred, then their pans
/// swept back and their FX snapped on, on every resync. Each command is one lookup and a
/// cheap coefficient update, so a full budget costs a fraction of a millisecond, once.
const MAX_CMDS_PER_BLOCK: usize = 1024;

/// Capacity of the control → audio parameter ring the FFI engine is spawned with. Large
/// enough that a post-commit storm (~58 commands per clip) fits whole: every FFI setter
/// flushes, so the storm then needs no later flush to land — while paused, nothing polls
/// [`EngineHandle::reap`], and an overflowed tail used to wait for the next play.
pub const CMD_RING_CAPACITY: usize = 8192;

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
    /// The loop bounds as ONE value (start in the high 32 bits, end in the low), so a reader
    /// always gets a pair [`EngineHandle::set_loop`] published. As two stores a reader could
    /// see the old start with the new end, usually a valid span: playing at 18 s in [2 s, 20 s)
    /// and moving the loop to [10 s, 12 s) wrapped to 2 s, eight seconds outside the new loop.
    /// 32 bits of frames is ~24.8 h at the engine rate.
    loop_span: AtomicU64,
    seek_req: AtomicI64, // >= 0: pending seek target; -1: none
    /// A pause rewound the clock over frames already rendered (see [`EngineHandle::pause`]):
    /// the audio thread clears the nodes' state at its next block, as for a seek.
    reset_req: AtomicBool,
    /// Live-input monitor gain target (f32 bits). 0 = muted; the audio thread ramps to it.
    mon_target: AtomicU32,
    /// Hard safety mute for the monitor, owned by the audio host — NOT by the UI. Raised while
    /// playback has left the output device the user selected (see `device::OutputBinding`), where
    /// the "these are headphones, monitoring is safe" judgement made from the SELECTION no longer
    /// describes what is actually making sound. Deliberately a separate flag from [`Self::
    /// mon_target`]: the fader writes that one continuously, so a safety decision stored there
    /// would be stomped by the next drag.
    mon_muted: AtomicBool,
    /// Playback is on an output with no acoustic path back to the mic (headphones): the echo
    /// canceller passes the mic through. Owned by `device::reconcile_output_safety`, which sets it
    /// only while the output the host judged echo-free is the one rendering.
    echo_free: AtomicBool,
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
    /// The output's clock for the drift estimator: engine-rate frames the current output
    /// stream handed its device, counted where they leave (one atomic, so it never reads
    /// backwards). Unlike `consumed_total` it is not moved by a flush or by the sync at a
    /// stream's start, which jump the cursor over frames nobody played.
    played_total: AtomicU64,
    /// The largest single callback's share of `played_total` since the drift estimator last
    /// took it: how far the counter can lag its device at any instant.
    played_quantum: AtomicU64,
    /// Output streams started ([`Self::begin_output_stream`]): a new stream may be a new
    /// device, on a new crystal, after a gap — the drift estimator restarts its history.
    output_streams: AtomicU64,
    /// `rendered_total` when the transport last started playing, or the render last
    /// applied a seek: a new run. Spans ending at or before it belong to an EARLIER
    /// run: until the cursor reaches this run's audio, nothing of it has been heard.
    /// Without the mark, map(consumed) right after play held at the previous run's
    /// last RENDERED frame — ahead of what was heard by the output FIFO's backlog —
    /// so a play/pause with no block heard jumped the playhead forward; and right
    /// after a seek it held at the pre-seek position, so the playing playhead
    /// flashed it and a pause in that window folded the clock back there, losing
    /// the seek. Only ever raised (both writers use fetch_max).
    play_mark: AtomicU64,
    seg_head: AtomicU32,
    segments: [RenderSegment; SEG_RING],
    /// Bumped by seek/pause: the resampler's FIFO still holds pre-seek/pre-pause audio, which
    /// must be dropped (not played out) for the transport to respond within one callback.
    out_flush: AtomicU32,
    /// The transport cuts are declicked (see [`crate::declick`]); on unless a test turns it off
    /// (see [`EngineHandle::set_declick`]).
    declick: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Shared {
            frame: AtomicI64::new(0),
            blocks: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            loop_enabled: AtomicBool::new(false),
            loop_span: AtomicU64::new(0),
            seek_req: AtomicI64::new(-1),
            reset_req: AtomicBool::new(false),
            mon_target: AtomicU32::new(0), // 0.0f32 bits
            mon_muted: AtomicBool::new(false),
            echo_free: AtomicBool::new(false),
            mon_rev_room: AtomicU32::new(0.6f32.to_bits()),
            mon_rev_damping: AtomicU32::new(0.5f32.to_bits()),
            mon_rev_wet: AtomicU32::new(0.2f32.to_bits()),
            rendered_total: AtomicU64::new(0),
            consumed_total: AtomicU64::new(0),
            played_total: AtomicU64::new(0),
            played_quantum: AtomicU64::new(0),
            output_streams: AtomicU64::new(0),
            play_mark: AtomicU64::new(0),
            seg_head: AtomicU32::new(0),
            segments: std::array::from_fn(|_| RenderSegment {
                ver: AtomicU32::new(0),
                r_start: AtomicU64::new(0),
                r_end: AtomicU64::new(0),
                tl_end: AtomicI64::new(0),
            }),
            out_flush: AtomicU32::new(0),
            declick: AtomicBool::new(true),
        }
    }

    /// Audio-thread only: record one contiguous PLAYING span. Seqlock write —
    /// version odd while the fields are torn.
    fn push_segment(&self, r_start: u64, r_end: u64, tl_end: i64) {
        let slot = (self.seg_head.load(Ordering::Relaxed) as usize) % SEG_RING;
        let seg = &self.segments[slot];
        // The data stores below must not become visible before the version turns
        // odd. An Acquire RMW alone does not guarantee that — acquire orders later
        // accesses after its LOAD half, not after its store — so a reader could
        // see new fields under the old even version. The release fence after the
        // bump is the seqlock writer pattern (crossbeam's): it pairs with the
        // reader's acquire fence before its validating recheck.
        seg.ver.fetch_add(1, Ordering::Relaxed); // odd: write in progress
        fence(Ordering::Release);
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
    /// nothing of the current run (see [`Self::play_mark`]) has both
    /// rendered-playing and reached the device.
    fn map_consumed(&self) -> Option<i64> {
        let since = self.play_mark.load(Ordering::Acquire);
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
                if r_end == r_start || r_end <= since {
                    break; // never-written slot, or an earlier run's
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

    /// A new output stream starts, on the host thread before any of its callbacks: the
    /// cursor syncs over a dead stream's unplayed FIFO (this one starts with an empty or no
    /// FIFO), and the drift estimator learns that its output clock may be another device's.
    pub fn begin_output_stream(&self) {
        self.consumed_total.store(
            self.rendered_total.load(Ordering::Acquire),
            Ordering::Release,
        );
        self.output_streams.fetch_add(1, Ordering::AcqRel);
    }

    /// Identity-path (48 kHz) stream callback: the render IS the delivery, so
    /// after rendering straight into the device buffer the cursor equals the
    /// render counter exactly — and what it moved over is what the device played.
    pub fn sync_consumed_to_rendered(&self) {
        let rendered = self.rendered_total.load(Ordering::Acquire);
        let before = self.consumed_total.swap(rendered, Ordering::AcqRel);
        self.count_played(rendered.saturating_sub(before));
    }

    /// Stream-callback only: the device was handed `n` more engine-rate frames (see
    /// `played_total`).
    pub fn count_played(&self, n: u64) {
        if n > 0 {
            self.played_total.fetch_add(n, Ordering::Release);
            self.played_quantum.fetch_max(n, Ordering::Relaxed);
        }
    }

    /// Engine-rate frames the output devices have played (see `played_total`).
    pub fn played_frames(&self) -> u64 {
        self.played_total.load(Ordering::Acquire)
    }

    /// The largest callback's [`Self::count_played`] since the last call, which resets it.
    pub fn take_played_quantum(&self) -> u64 {
        self.played_quantum.swap(0, Ordering::Relaxed)
    }

    /// How many output streams have started (see [`Self::begin_output_stream`]).
    pub fn output_streams(&self) -> u64 {
        self.output_streams.load(Ordering::Acquire)
    }

    /// The current flush generation; the resampler drops its FIFO when this changes.
    pub fn out_flush_gen(&self) -> u32 {
        self.out_flush.load(Ordering::Acquire)
    }

    /// Whether transport cuts are declicked (see [`EngineHandle::set_declick`]). RT-safe.
    #[inline]
    pub fn declick_enabled(&self) -> bool {
        self.declick.load(Ordering::Relaxed)
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

    /// Where the NEXT rendered block starts: a pending seek's target, else the
    /// render clock. Ahead of [`Self::playhead`] by the output FIFO on the
    /// resampled path — the position a streaming feeder must plan from, since
    /// it fills the ring the render consumes, not what the listener hears.
    pub fn render_pos(&self) -> i64 {
        let seek = self.seek_req.load(Ordering::Acquire);
        if seek >= 0 {
            return seek;
        }
        self.frame.load(Ordering::Acquire)
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

    /// Mark playback echo-free (see [`Shared::echo_free`]). Owned by
    /// `device::reconcile_output_safety`.
    pub fn set_echo_free(&self, on: bool) {
        self.echo_free.store(on, Ordering::Release);
    }

    /// Whether playback is echo-free (see [`Shared::echo_free`]). RT-safe: one load.
    #[inline]
    pub fn echo_free(&self) -> bool {
        self.echo_free.load(Ordering::Acquire)
    }

    // Plan-basis reads for the streaming feeders (crate::feed): a change in
    // any of these is a plan discontinuity.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }
    pub fn loop_enabled(&self) -> bool {
        self.loop_enabled.load(Ordering::Acquire)
    }
    /// The loop's (start, end) frames, read together (see [`Shared::loop_span`]).
    pub fn loop_bounds(&self) -> (i64, i64) {
        let span = self.loop_span.load(Ordering::Relaxed);
        ((span >> 32) as i64, (span & 0xffff_ffff) as i64)
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
    /// The monitor is off and silent (see [`Self::mix_monitor`]): its reverb and mix are
    /// skipped, and the reverb restarts clean when the gain comes back.
    mon_idle: bool,
    /// The stream generation the processor last rendered as (see [`RtCell`]), and whether the
    /// mic ring still holds what piled up before that stream started (see
    /// [`Self::add_monitor`]).
    mon_stream: u64,
    mon_shed: bool,
    /// The playback output's crossfade across transport cuts (see [`crate::declick`]), and
    /// whether the last block played: a change is a cut.
    declick: Declick,
    was_playing: bool,
}

/// Fade the frames of a pass that lie within [`LOOP_FADE_FRAMES`] of the loop's end `le` out
/// into the seam (`chunk` frames from timeline `frame`, which never cross `le`). The next pass
/// fades back in from there (see `RtProcessor::render_block`).
#[inline]
fn fade_to_seam(out: &mut [Smp], frame: i64, chunk: usize, le: i64) {
    let from = (le - LOOP_FADE_FRAMES as i64 - frame).max(0) as usize;
    for i in from..chunk {
        let left = (le - frame - i as i64) as f32;
        let g = fade_shape(left / LOOP_FADE_FRAMES as f32);
        out[CHANNELS * i] *= g;
        out[CHANNELS * i + 1] *= g;
    }
}

/// A monitor gain this small (-120 dB) is silence: once the target is 0 and the ramp has fallen
/// below it, the monitor goes idle (see `RtProcessor::mix_monitor`).
const MON_IDLE: f32 = 1.0e-6;

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
    ///
    /// Every transport cut is declicked, sample-accurately, from the frame it lands on: play,
    /// pause, a seek and a pause's rewind crossfade over [`DECLICK_FRAMES`] from the frame last
    /// put out (so pause and stop glide to silence and play glides in from it), and a loop's
    /// pass fades out over its last [`LOOP_FADE_FRAMES`] into the seam, the next pass fading
    /// back in. Without them each of these stepped the output, and every loop pass clicked.
    fn render_block(&mut self, out: &mut [Smp], frames: usize, monitor: bool) {
        let _fz = FlushGuard::new();
        let total = frames * CHANNELS;

        // Drain the commands first, each onto the graph current when it is popped: loading the
        // graph before the drain applied a commit's follow-up parameters to the graph that
        // commit had just retired whenever the publish landed mid-drain.
        self.cmd_rx.apply_current(&self.cell, MAX_CMDS_PER_BLOCK);

        // Then load the live graph once for this block (RCU; no refcount, nothing to drop).
        // SAFETY: the control thread frees a retired graph only two blocks after its swap
        // (see `EngineHandle::reap`), so the pointee outlives this block; we never cache the
        // reference.
        let g: &mut Graph = unsafe { &mut *self.cell.load() };
        // A rebuilt graph's first block: its nodes take over the state of the ones they
        // replace (reverb tails, delay lines, filter histories, envelopes).
        let old = g.take_carry();
        if !old.is_null() {
            // SAFETY: `old` is the graph this one replaced, retired at the publish with a grace
            // of two blocks. At most one block was running at the swap (on `old`), so this,
            // the first block on the new graph, ends before the grace does; and the audio
            // thread is the only one that touches a published graph's nodes.
            g.carry_state_from(unsafe { &mut *old });
        }

        // A seek or a pause's rewind applied in this block: the output jumps.
        let mut cut = false;
        let seek = self.shared.seek_req.load(Ordering::Acquire);
        if seek >= 0 {
            // The new clock is published BEFORE the request retires: a feeder reading
            // `render_pos` in between saw neither, planned from the pre-seek clock, and left a
            // backward seek silent until the transport walked back up to it. A newer seek that
            // lands meanwhile fails the exchange and stays pending for the next block.
            // A seek starts a new run for the consumed mapping (see `play_mark`),
            // marked before the clock moves.
            self.shared.play_mark.fetch_max(
                self.shared.rendered_total.load(Ordering::Relaxed),
                Ordering::AcqRel,
            );
            self.shared.frame.store(seek, Ordering::Release);
            let _ = self.shared.seek_req.compare_exchange(
                seek,
                -1,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            g.reset();
            cut = true;
        }
        if self.shared.reset_req.swap(false, Ordering::AcqRel) {
            g.reset();
            cut = true;
        }
        let playing = self.shared.playing.load(Ordering::Acquire);
        let declick = self.shared.declick_enabled();
        if declick && (cut || playing != self.was_playing) {
            self.declick.begin(DECLICK_FRAMES);
        }
        self.was_playing = playing;

        if playing {
            let loop_en = self.shared.loop_enabled.load(Ordering::Acquire);
            let (ls, le) = self.shared.loop_bounds();
            let block_start = self.shared.frame.load(Ordering::Relaxed);
            let mut frame = block_start;
            let mut rendered = self.shared.rendered_total.load(Ordering::Relaxed);

            // set_loop never publishes a span shorter than MIN_LOOP_FRAMES: treat one as no
            // active loop rather than render it frame by frame.
            let loop_en = loop_en && le - ls >= MIN_LOOP_FRAMES;
            let mut done = 0;
            while done < frames {
                if loop_en && le > ls && frame >= le {
                    // A pass that ran to the seam has faded out into it; one that jumps from
                    // past it (the loop moved behind the playhead) has not.
                    if declick {
                        self.declick.begin(if frame == le {
                            LOOP_FADE_FRAMES
                        } else {
                            DECLICK_FRAMES
                        });
                    }
                    frame = ls;
                }
                let mut chunk = (frames - done).min(self.max_block);
                if loop_en && le > ls {
                    let to_end = (le - frame).max(0) as usize;
                    if to_end == 0 {
                        for s in &mut out[done * CHANNELS..total] {
                            *s = 0.0;
                        }
                        self.declick
                            .apply(&mut out[done * CHANNELS..total], frames - done);
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
                if loop_en && declick {
                    fade_to_seam(&mut out[s..e], frame, chunk, le);
                }
                self.declick.apply(&mut out[s..e], chunk);
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
            // A pause landing while this block rendered folded the clock back to
            // what was heard, and its flush drops the rest: resume must start
            // at the fold, not past audio nobody heard. The control thread
            // writes the clock only in that fold, so a changed clock is one.
            let _ = self.shared.frame.compare_exchange(
                block_start,
                frame,
                Ordering::Release,
                Ordering::Relaxed,
            );
            self.shared
                .rendered_total
                .store(rendered, Ordering::Release);
        } else {
            // Stopped transport: silence the playback bed (gliding into it from a pause), but
            // still let the monitor through.
            for s in &mut out[..total] {
                *s = 0.0;
            }
            self.declick.apply(&mut out[..total], frames);
            g.idle();
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
        // A new output stream starts with whatever the mic delivered while none rendered (a
        // rebuild with the input still open, the duplex unit taking over the output as the mic
        // opens). Drained exactly what it plays, the ring kept that as hear-yourself latency for
        // good: 80 ms after a 100 ms gap, a minute later still. It is shed once, keeping this
        // callback's frames — and never in between: the ring's level then is the input
        // bursts' phase against the output's, which slides over minutes when the two run on
        // separate clocks, so a trim by level (over a one-second window) cut audio every
        // second and starved the callbacks when the phase wrapped. What is dropped only ever
        // reached the singer's ears; the take records through its own ring. Exactly this
        // callback's frames are kept, not a cushion: where the bursts and the pulls do not line
        // up, the new stream may starve once and settle at the least latency the two allow,
        // where a cushion kept would be heard as latency for the rest of the session.
        if self.mon_shed {
            self.mon_shed = false;
            for _ in 0..self.mic_rx.len().saturating_sub(frames) {
                let _ = self.mic_rx.pop();
            }
        }
        let mut done = 0;
        while done < frames {
            let chunk = (frames - done).min(self.max_block);
            let s = done * CHANNELS;
            let e = (done + chunk) * CHANNELS;
            self.mix_monitor(&mut out[s..e], chunk);
            done += chunk;
        }
    }

    /// Notes the stream generation a callback renders as: the first callback of a new stream
    /// sheds the mic backlog (see [`Self::add_monitor`]).
    #[inline]
    fn enter_stream(&mut self, epoch: u64) {
        if epoch != self.mon_stream {
            self.mon_stream = epoch;
            self.mon_shed = true;
        }
    }

    /// Add the live-input monitor on top of the playback mix. The mic frames (already at
    /// `ENGINE_SR`, resampled in the input callback) pass through the monitor reverb, then a
    /// smoothed gain so arming/disarming never clicks.
    ///
    /// Monitoring off — the gain's target 0 (the default, and forced on a speaker) and its ramp
    /// down to -120 dB — the monitor adds nothing, so it does nothing: the mic ring is still
    /// drained (no stale backlog when monitoring resumes) but the reverb and the mix are
    /// skipped. Running the reverb's 16 combs and 8 allpasses over silence on every callback
    /// cost as much as the whole rest of an idle graph. The reverb is reset when the gain comes
    /// back, so a re-armed monitor never resumes a tail frozen at the moment it went idle.
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

        if target == 0.0 && self.mon_gain.current() <= MON_IDLE {
            // Off and silent: land the ramp on exact zero, keep the ring drained, add nothing.
            // Still advance the smoother: one never advanced is not yet "live", and its first
            // non-zero target (the session's first arm) jumped straight there — a click.
            self.mon_gain.snap(0.0);
            let _ = self.mon_gain.advance(frames);
            self.mon_idle = true;
            for _ in 0..frames {
                if self.mic_rx.pop().is_none() {
                    break;
                }
            }
            return;
        }
        if self.mon_idle {
            // Coming back: start the reverb clean (its lines last ran before it went idle).
            self.mon_reverb.reset();
            self.mon_idle = false;
        }

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

        // Mix the processed monitor into the output with the ramped gain (a landed ramp is one
        // value for the whole block: see `Smoothed::settled`).
        // Added after the master, the monitor bypassed its safety clip, and a sung peak at the
        // default record gain put the device buffer at +9 dBFS (far more at the top of the
        // gain range), hard-clipped by the device in the singer's ears. It is bent into the
        // headroom the clipped mix leaves instead, which leaves that mix untouched wherever
        // the monitor adds nothing (see `add_within_full_scale`).
        use crate::dsp::master::add_within_full_scale as mix;
        if let Some(g) = self.mon_gain.settled(frames) {
            for (o, &m) in out[..n].iter_mut().zip(&self.mon_out[..n]) {
                *o = mix(*o, m * g);
            }
            return;
        }
        for i in 0..frames {
            let g = self.mon_gain.next();
            let (l, r) = (CHANNELS * i, CHANNELS * i + 1);
            out[l] = mix(out[l], self.mon_out[l] * g);
            out[r] = mix(out[r], self.mon_out[r] * g);
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
        let rt = unsafe { &mut *self.inner.get() };
        rt.enter_stream(epoch);
        rt.process(out, frames);
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
        let rt = unsafe { &mut *self.inner.get() };
        rt.enter_stream(epoch);
        rt.process_playback(out, frames);
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
        let rt = unsafe { &mut *self.inner.get() };
        rt.enter_stream(epoch);
        rt.add_monitor(out, frames);
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
    /// A compiled graph held back from the audio thread (see [`stage_graph`](Self::stage_graph)).
    staged: Option<Box<Graph>>,
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
        // Marked before the flip: every span this run renders starts at or after it.
        self.shared.play_mark.fetch_max(
            self.shared.rendered_total.load(Ordering::Acquire),
            Ordering::AcqRel,
        );
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
        // frames) and only costs the same bounded precision it always did. Nothing of
        // this run (since play or a seek) heard yet (None) leaves the render clock:
        // where play() or the seek put it, plus at most the one block rendered since.
        //
        // A seek still pending was asked for first, and the render applies it, with its
        // own reset, paused or not: nothing to fold. Folded, the render could apply it
        // between the read of the old run and the swap below, which then put the pre-seek
        // position back over it: paused where the seek had left, the UI showing the seek.
        // Read as done (-1), the seek's raised `play_mark` is visible, so the fold reads
        // only the run it started.
        let seek_pending = self.shared.seek_req.load(Ordering::Acquire) >= 0;
        if let Some(heard) = (!seek_pending)
            .then(|| self.shared.map_consumed())
            .flatten()
        {
            // Frames rendered past what was heard are rendered again on resume, but the
            // nodes' delay lines, reverb tank and filter histories already hold them: the
            // resume played on top of its own future, a doubler or delay clicking one
            // delay-length in. Clear that state, as a seek does. Nothing is rewound on
            // the 48 kHz identity path (all of it was heard), so tails carry there.
            if self.shared.frame.swap(heard, Ordering::AcqRel) != heard {
                self.shared.reset_req.store(true, Ordering::Release);
            }
        }
        // Drop the FIFO's pre-pause audio so the output falls silent within one callback.
        self.shared.out_flush.fetch_add(1, Ordering::Release);
    }
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    /// Turn the transport declick (see [`crate::declick`]) off or back on; it is on from the
    /// start. Off, every cut steps the output as the audio beneath it does: for tests that
    /// check that audio sample for sample (the streaming protocol's), not for playback.
    pub fn set_declick(&self, on: bool) {
        self.shared.declick.store(on, Ordering::Relaxed);
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
        let start = start.min(u32::MAX as i64) as u64;
        let end = end.min(u32::MAX as i64) as u64;
        self.shared
            .loop_span
            .store((start << 32) | end, Ordering::Relaxed);
        self.shared.loop_enabled.store(enabled, Ordering::Release);
    }

    /// Set the live-input monitor gain (linear; 0 = muted). The audio thread ramps to it. This is
    /// the FADER's value; it does not override the host's safety mute (see
    /// [`Shared::set_monitor_muted`]).
    ///
    /// A non-finite gain is dropped (the previous one stands): an infinite target ramps the
    /// monitor to infinity, and every sample after it is NaN.
    pub fn set_monitor_gain(&self, gain: f32) {
        if !gain.is_finite() {
            return;
        }
        self.shared
            .mon_target
            .store(gain.max(0.0).to_bits(), Ordering::Release);
    }

    /// Set the monitor reverb (room/damping/wet, each 0..1). The audio thread applies them. A
    /// non-finite value is dropped (that parameter keeps its previous value): `f32::clamp`
    /// passes NaN through, and a NaN wet made the monitor's output NaN — which is ADDED to the
    /// playback mix even at monitor gain 0 (NaN * 0 is NaN), so every later sample was NaN.
    pub fn set_monitor_reverb(&self, room: f32, damping: f32, wet: f32) {
        let store = |cell: &AtomicU32, v: f32| {
            if v.is_finite() {
                cell.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Release);
            }
        };
        store(&self.shared.mon_rev_room, room);
        store(&self.shared.mon_rev_damping, damping);
        store(&self.shared.mon_rev_wet, wet);
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

    /// Queue a parameter change (coalesced). Call [`flush`](Self::flush) to publish. While a
    /// graph is staged the change lands on it directly instead (see
    /// [`stage_graph`](Self::stage_graph)). A non-finite value is dropped here, before it can
    /// coalesce over (and so erase) the last good value still pending for the same parameter.
    pub fn set_param(&mut self, node: NodeId, param: u16, value: f32) {
        if !value.is_finite() {
            return;
        }
        match self.staged.as_mut() {
            Some(g) => g.set_param(node, param, value),
            None => self.cmd_tx.set(node, param, value),
        }
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
        self.publish_boxed(Box::new(graph));
    }

    /// Hold `graph` back from the audio thread while its parameters are set: until
    /// [`publish_staged`](Self::publish_staged), [`set_param`](Self::set_param) applies to it
    /// right here on the control thread instead of crossing the command ring. A rebuilt graph
    /// starts every node at its build defaults and the host re-pushes each live value right
    /// after the commit; published first, the audio thread rendered those defaults for the
    /// blocks the storm took to land, and every smoother then ramped away from them. Staged,
    /// the first rendered block already carries every value (and each smoother snaps to it —
    /// nothing has been heard yet). The previously live graph keeps playing meanwhile.
    pub fn stage_graph(&mut self, graph: Graph) {
        debug_assert!(self.staged.is_none(), "publish the staged graph first");
        self.staged = Some(Box::new(graph));
    }

    /// Hot-swap the staged graph in (see [`publish_graph`](Self::publish_graph)). Returns
    /// whether one was staged.
    pub fn publish_staged(&mut self) -> bool {
        match self.staged.take() {
            Some(g) => {
                self.publish_boxed(g);
                true
            }
            None => false,
        }
    }

    fn publish_boxed(&mut self, mut graph: Box<Graph>) {
        debug_assert_eq!(graph.max_block(), self.max_block);
        // The graph about to retire hands its nodes' state over at the new one's first block.
        // Only this thread publishes, so the cell holds exactly the graph the swap retires.
        graph.set_carry_from(self.cell.load());
        // Commands still queued were sent for the graph this replaces; the new one takes
        // only those sent from here on (see CommandSender::begin_epoch).
        graph.set_epoch(self.cmd_tx.begin_epoch());
        let retired = self.cell.publish(graph);
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
            // No block will take the live graph's carry before these are freed: drop it.
            // SAFETY: the cell's graph is alive while the cell is; the carry is an atomic.
            let _ = unsafe { &*self.cell.load() }.take_carry();
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
        staged: None,
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
        mon_idle: true,
        mon_stream: 0,
        mon_shed: false,
        declick: Declick::default(),
        was_playing: false,
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
mod monitor_tests {
    //! The live-input monitor: skipped while off and silent, identical output either way.

    use super::*;
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::ring::{spsc, Producer};
    use crate::source::BufferSource;
    use std::collections::HashMap;

    const BLOCK: usize = 128;

    /// A playing engine over a deterministic source, and its mic ring's producer.
    fn engine() -> (EngineHandle, RtProcessor, Producer<[Smp; CHANNELS]>) {
        let pcm: Vec<Smp> = (0..48_000 * 4 * CHANNELS)
            .map(|i| ((i / 2) as f32 * 0.013).sin() * 0.3)
            .collect();
        let desc = GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(BufferSource::new(pcm, 0))),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2)],
            master: 2,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 14);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        h.play();
        (h, rt, mic_prod)
    }

    fn feed(mic: &mut Producer<[Smp; CHANNELS]>, frames: usize) {
        for i in 0..frames {
            let v = (i as f32 * 0.05).sin() * 0.5;
            if !mic.push([v, v]) {
                break;
            }
        }
    }

    #[test]
    fn the_sessions_first_arm_ramps_in() {
        // Idle from the start, the smoother was never advanced, so the first arm stepped the
        // voice from silence to full gain in one sample.
        let (h, mut rt, mut mic) = engine();
        h.set_monitor_reverb(0.6, 0.5, 0.0); // dry only
        let mut out = vec![0.0; BLOCK * CHANNELS];
        for _ in 0..20 {
            feed(&mut mic, BLOCK);
            rt.process(&mut out, BLOCK);
        }
        h.set_monitor_gain(1.0);
        assert!(rt.mon_gain.current() <= MON_IDLE);
        feed(&mut mic, BLOCK);
        rt.process(&mut out, BLOCK);
        assert!(
            rt.mon_gain.current() < 0.9,
            "the first arm jumped to {} (no ramp: a click)",
            rt.mon_gain.current()
        );
    }

    #[test]
    fn a_silent_monitor_does_no_reverb_work_and_keeps_the_mic_ring_drained() {
        // Monitor off (gain 0, the default): the reverb ran its combs and allpasses over
        // silence on every callback anyway.
        let (_h, mut rt, mut mic) = engine();
        let mut out = vec![0.0; BLOCK * CHANNELS];
        for _ in 0..50 {
            feed(&mut mic, BLOCK);
            rt.process(&mut out, BLOCK);
        }
        assert!(
            !rt.mon_reverb.lines_dirty(),
            "the monitor reverb ran while off"
        );
        assert_eq!(
            mic.free(),
            mic.capacity(),
            "the mic ring was left to back up"
        );
    }

    #[test]
    fn the_output_is_identical_whether_or_not_the_monitor_is_skipped() {
        // Off from the start, and again once a disarm has ramped out: the monitor path's output
        // equals the playback-only render, sample for sample.
        let (h, mut rt, mut mic) = engine();
        let (_h2, mut twin, _mic2) = engine();
        let mut a = vec![0.0; BLOCK * CHANNELS];
        let mut b = vec![0.0; BLOCK * CHANNELS];
        let same = |a: &[Smp], b: &[Smp]| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits());
        for _ in 0..40 {
            rt.process(&mut a, BLOCK);
            twin.process_playback(&mut b, BLOCK);
            assert!(same(&a, &b), "a monitor that is off changed the output");
        }
        // Arm with a voice, then disarm: once the 10 ms ramp has fallen to -120 dB (~140 ms)
        // the monitor adds nothing again.
        h.set_monitor_gain(1.0);
        for _ in 0..40 {
            feed(&mut mic, BLOCK);
            rt.process(&mut a, BLOCK);
            twin.process_playback(&mut b, BLOCK);
        }
        assert!(!same(&a, &b), "the armed monitor must be heard");
        h.set_monitor_gain(0.0);
        for k in 0..200 {
            feed(&mut mic, BLOCK);
            rt.process(&mut a, BLOCK);
            twin.process_playback(&mut b, BLOCK);
            if k >= 60 {
                assert!(
                    same(&a, &b),
                    "block {k} after the disarm still carries the monitor"
                );
            }
        }
    }

    /// `mix_monitor` as it was before the landed-gain path: the gain stepped per frame.
    fn mix_reference(rt: &mut RtProcessor, out: &mut [Smp], frames: usize) {
        let target = if rt.shared.mon_muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(rt.shared.mon_target.load(Ordering::Relaxed))
        };
        rt.mon_gain.set_target(target);
        if target == 0.0 && rt.mon_gain.current() <= MON_IDLE {
            rt.mon_gain.snap(0.0);
            let _ = rt.mon_gain.advance(frames);
            rt.mon_idle = true;
            for _ in 0..frames {
                if rt.mic_rx.pop().is_none() {
                    break;
                }
            }
            return;
        }
        if rt.mon_idle {
            rt.mon_reverb.reset();
            rt.mon_idle = false;
        }
        for i in 0..frames {
            let f = rt.mic_rx.pop().unwrap_or([0.0, 0.0]);
            rt.mon_in[CHANNELS * i] = f[0];
            rt.mon_in[CHANNELS * i + 1] = f[1];
        }
        for (p, v) in [
            (rparam::ROOM_SIZE, &rt.shared.mon_rev_room),
            (rparam::DAMPING, &rt.shared.mon_rev_damping),
            (rparam::WET, &rt.shared.mon_rev_wet),
        ] {
            rt.mon_reverb
                .set_param(p, f32::from_bits(v.load(Ordering::Relaxed)));
        }
        let n = frames * CHANNELS;
        {
            let inputs: [&[Smp]; 1] = [&rt.mon_in[..n]];
            let mut out_slot: [&mut [Smp]; 1] = [&mut rt.mon_out[..n]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut out_slot,
                frames,
                tl: 0,
                sr: ENGINE_SR,
            };
            rt.mon_reverb.process(&mut io);
        }
        for i in 0..frames {
            let g = rt.mon_gain.next();
            let (l, r) = (CHANNELS * i, CHANNELS * i + 1);
            out[l] = crate::dsp::master::add_within_full_scale(out[l], rt.mon_out[l] * g);
            out[r] = crate::dsp::master::add_within_full_scale(out[r], rt.mon_out[r] * g);
        }
    }

    #[test]
    fn a_landed_monitor_gain_mixes_exactly_like_the_per_sample_ramp() {
        // Arm, re-level, mute (the host's safety mute), unmute, disarm: ramping blocks take the
        // per-sample path, landed ones the constant — the mix must be the old one bit for bit.
        let (h, mut rt, mut mic) = engine();
        let (h2, mut twin, mut mic2) = engine();
        let mut a = vec![0.0; BLOCK * CHANNELS];
        let mut b = vec![0.0; BLOCK * CHANNELS];
        for k in 0..400 {
            match k {
                5 => {
                    h.set_monitor_gain(0.8);
                    h2.set_monitor_gain(0.8);
                }
                60 => {
                    h.set_monitor_reverb(0.8, 0.3, 0.6);
                    h2.set_monitor_reverb(0.8, 0.3, 0.6);
                }
                90 => {
                    h.set_monitor_gain(0.3);
                    h2.set_monitor_gain(0.3);
                }
                150 => {
                    rt.shared.set_monitor_muted(true);
                    twin.shared.set_monitor_muted(true);
                }
                200 => {
                    rt.shared.set_monitor_muted(false);
                    twin.shared.set_monitor_muted(false);
                }
                300 => {
                    h.set_monitor_gain(0.0);
                    h2.set_monitor_gain(0.0);
                }
                _ => {}
            }
            feed(&mut mic, BLOCK);
            feed(&mut mic2, BLOCK);
            rt.process(&mut a, BLOCK);
            twin.process_playback(&mut b, BLOCK);
            mix_reference(&mut twin, &mut b, BLOCK);
            for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                assert_eq!(x.to_bits(), y.to_bits(), "block {k} sample {i}");
            }
        }
    }

    #[test]
    fn a_re_armed_monitor_starts_its_reverb_clean() {
        // A tail that rang out while the monitor was off must not come back frozen at the
        // moment the monitor went idle: re-armed over a silent mic, nothing but silence plays.
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(2, Box::new(MasterNode::unity()))],
            edges: Vec::new(),
            master: 2,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (mut mic, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 14);
        let (h, mut rt) = spawn(g, HashMap::new(), mic_cons, 256);
        h.set_monitor_reverb(0.9, 0.2, 1.0); // a long, loud tail
        h.set_monitor_gain(1.0);
        let mut out = vec![0.0; BLOCK * CHANNELS];
        for _ in 0..200 {
            feed(&mut mic, BLOCK);
            rt.process(&mut out, BLOCK);
        }
        // Off for 20 s: a reverb left running decays to nothing, one skipped keeps its lines.
        h.set_monitor_gain(0.0);
        for _ in 0..(20 * 48_000 / BLOCK) {
            rt.process(&mut out, BLOCK);
        }
        h.set_monitor_gain(1.0);
        let mut peak = 0.0f32;
        for _ in 0..100 {
            rt.process(&mut out, BLOCK);
            peak = out.iter().fold(peak, |m, s| m.max(s.abs()));
        }
        assert!(
            peak < 1e-9,
            "a stale tail replayed on re-arming: peak {peak}"
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

#[cfg(test)]
mod monitor_and_loop_tests {
    use super::*;
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::ring::spsc;
    use crate::source::BufferSource;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;

    const BLOCK: usize = 256;
    const S: i64 = 48_000;

    #[test]
    fn the_monitored_mix_stays_within_full_scale() {
        // The monitor was summed after the master's safety clip: a sung -6 dBFS peak at the
        // default +12 dB record gain, over a full-scale backing, put the device buffer at
        // +9 dBFS.
        let pcm: Vec<Smp> = (0..48_000 * CHANNELS)
            .map(|i| 0.95 * ((i / 2) as f32 * 0.05).sin())
            .collect();
        let peak = |gain: f32| {
            let desc = GraphDesc {
                nodes: vec![
                    NodeEntry::new(1, Box::new(BufferSource::new(pcm.clone(), 0))),
                    NodeEntry::new(2, Box::new(MasterNode::limited())),
                ],
                edges: vec![Edge::simple(1, 2)],
                master: 2,
            };
            let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
            let (mut mic, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 14);
            let (h, mut rt) = spawn(g, HashMap::new(), mic_cons, 256);
            h.play();
            h.set_monitor_gain(gain);
            h.set_monitor_reverb(0.6, 0.5, 0.0);
            let voice = 0.5f32 * 3.981;
            let mut out = vec![0.0; BLOCK * CHANNELS];
            let mut peak = 0.0f32;
            for b in 0..200 {
                for i in 0..BLOCK {
                    let v = voice * ((b * BLOCK + i) as f32 * 0.031).sin();
                    let _ = mic.push([v, v]);
                }
                rt.process(&mut out, BLOCK);
                peak = out.iter().fold(peak, |m, &x| m.max(x.abs()));
            }
            peak
        };
        let (backing, monitored) = (peak(0.0), peak(1.0));
        assert!(monitored <= 1.0, "the device buffer peaked at {monitored}");
        // The backing alone peaks at the master's clip of 0.95 (~0.906).
        assert!(
            monitored > backing + 0.05,
            "the monitor is audible ({monitored}, the backing alone {backing})"
        );
    }

    #[test]
    fn a_new_output_stream_sheds_the_mic_backlog_left_while_none_rendered() {
        // Drained exactly a block's worth each block, what the mic delivered while the output
        // stream was being rebuilt stayed as hear-yourself latency for good.
        fn latency(
            cell: &RtCell,
            epoch: u64,
            mic: &mut crate::ring::Producer<[Smp; CHANNELS]>,
        ) -> usize {
            let mut out = vec![0.0; BLOCK * CHANNELS];
            let mut sent = false;
            for b in 0..400 {
                for i in 0..BLOCK {
                    let v = if !sent && i == 0 { 1.0 } else { 0.0 };
                    let _ = mic.push([v, v]);
                }
                sent = true;
                assert!(cell.try_render(epoch, &mut out, BLOCK));
                if let Some(i) = (0..BLOCK).find(|&i| out[CHANNELS * i] > 0.5) {
                    return b * BLOCK + i;
                }
            }
            usize::MAX
        }
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: vec![],
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        // Room for the whole gap: a full ring drops what arrives next, the probe included.
        let (mut mic, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 13);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        h.set_monitor_gain(1.0);
        h.set_monitor_reverb(0.6, 0.5, 0.0);
        let cell = RtCell::new(rt);
        let first = cell.claim();
        let mut out = vec![0.0; BLOCK * CHANNELS];
        for _ in 0..100 {
            for _ in 0..BLOCK {
                let _ = mic.push([0.0, 0.0]);
            }
            assert!(cell.try_render(first, &mut out, BLOCK));
        }
        let before = latency(&cell, first, &mut mic);
        // ~100 ms with no output stream while the input keeps delivering, then the rebuilt one.
        for _ in 0..4800 {
            let _ = mic.push([0.0, 0.0]);
        }
        let rebuilt = cell.claim();
        let after = latency(&cell, rebuilt, &mut mic);
        assert!(
            after <= before + BLOCK,
            "latency {before} frames before the gap, {after} after it"
        );
        // The duplex path enters through the monitor alone, and sheds the same way.
        for _ in 0..4800 {
            let _ = mic.push([0.0, 0.0]);
        }
        let duplex = cell.claim();
        assert!(cell.try_mix_monitor(duplex, &mut out, BLOCK));
        let after = latency(&cell, duplex, &mut mic);
        assert!(after <= before + BLOCK, "{after} after the duplex start");
    }

    #[test]
    fn the_monitor_never_sheds_while_one_stream_plays() {
        // On separate clocks the ring's level is the input bursts' phase against the output's,
        // sliding through a whole burst over minutes. Trimmed by its level over a window, the
        // monitor skipped frames every second and starved its callbacks at each wrap. Here the
        // slide is fast (a 2000 ppm offset, rate-matched as the drift correction leaves it), so
        // the phase wraps twice.
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: vec![],
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, 1024).unwrap();
        let (mut mic, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 12);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        h.set_monitor_gain(1.0);
        h.set_monitor_reverb(0.6, 0.5, 0.0);
        // Through the cell, as one stream: every callback carries the same generation.
        let cell = RtCell::new(rt);
        let stream = cell.claim();
        let period = 512.0 / (1.0 - 2000e-6);
        let (mut t_in, mut t_out, mut owed, mut sent) = (0.0f64, 0.0f64, 0.0f64, 0usize);
        let mut out = vec![0.0; 512 * CHANNELS];
        let (mut silent, mut jumps, mut last) = (0, 0, None::<usize>);
        while t_out < 12.0 * 48_000.0 {
            if t_in <= t_out {
                owed += period;
                while (sent as f64) < owed {
                    let v = (sent % 3000 + 1) as f32 * 1.0e-4;
                    let _ = mic.push([v, v]);
                    sent += 1;
                }
                t_in += period;
                continue;
            }
            assert!(cell.try_render(stream, &mut out, 512));
            if t_out >= 48_000.0 {
                for i in 0..512 {
                    let s = out[CHANNELS * i];
                    if s == 0.0 {
                        silent += 1;
                        last = None;
                        continue;
                    }
                    let id = (s / 1.0e-4).round() as usize;
                    if last.is_some_and(|p| id != p % 3000 + 1) {
                        jumps += 1;
                    }
                    last = Some(id);
                }
            }
            t_out += 512.0;
        }
        // Nothing the mic delivered is skipped; a frame of silence is a callback that came a
        // frame before its burst, as with no shedding at all.
        assert_eq!(jumps, 0, "frames skipped ({silent} silent)");
        assert!(silent <= 16, "{silent} silent frames");
    }

    /// Mic frames pushed in bursts of `burst` every `burst` frames of time and rendered in
    /// callbacks of `callback` frames (input first on a tie, as in a duplex cycle), with the
    /// production 1024-frame block: the frames the monitor played as silence, and the jumps in
    /// the mic's sequence it played, over 4 s after a second's warm-up.
    fn monitor_gaps(burst: usize, callback: usize) -> (usize, usize) {
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: vec![],
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, 1024).unwrap();
        let (mut mic, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 13);
        let (h, mut rt) = spawn(g, HashMap::new(), mic_cons, 1024);
        h.set_monitor_gain(1.0);
        h.set_monitor_reverb(0.6, 0.5, 0.0);
        let mut out = vec![0.0; callback * CHANNELS];
        let (mut next, mut silent, mut jumps) = (0usize, 0, 0);
        let mut last: Option<usize> = None;
        for t in 0..5 * 48_000 {
            if t % burst == 0 {
                for _ in 0..burst {
                    // Each frame carries its position in the sequence.
                    let v = (next % 3000 + 1) as f32 * 1.0e-4;
                    let _ = mic.push([v, v]);
                    next += 1;
                }
            }
            if t % callback != 0 {
                continue;
            }
            rt.process(&mut out, callback);
            if t < 48_000 {
                continue;
            }
            for i in 0..callback {
                let s = out[CHANNELS * i];
                if s == 0.0 {
                    silent += 1;
                    last = None;
                    continue;
                }
                let id = (s / 1.0e-4).round() as usize;
                if last.is_some_and(|p| id != p % 3000 + 1) {
                    jumps += 1;
                }
                last = Some(id);
            }
        }
        (silent, jumps)
    }

    #[test]
    fn the_monitor_plays_every_frame_of_a_bursty_input() {
        // Shed whenever the ring held more than a block plus 1024 frames, a mic delivering
        // larger bursts than the output asked for at a time (a 4096-frame device buffer, a
        // 2048-frame USB mic into 512-frame callbacks, a resampled 16 kHz input) lost the
        // excess every period, and the callbacks after it played silence: half the time.
        for (burst, callback) in [
            (256, 256),
            (1024, 1024),
            (4096, 4096),
            (2048, 512),
            (3072, 512),
            (1536, 256),
            (2229, 1024),
            (1600, 512),
            (1115, 1024),
        ] {
            let (silent, jumps) = monitor_gaps(burst, callback);
            // Nothing the mic delivered is skipped; the rare silent frame is a callback that
            // came before its burst (4 in the 1115/1024 run, as with no shedding at all).
            assert_eq!(
                jumps, 0,
                "{burst}-frame bursts into {callback}-frame callbacks"
            );
            assert!(
                silent <= 16,
                "{silent} silent frames: {burst} into {callback}"
            );
        }
    }

    #[test]
    fn an_armed_monitor_leaves_the_backing_as_the_master_clipped_it() {
        // Summed through the master's soft clip a second time, a backing mastered to
        // -0.1 dBFS came out 0.3 dB quieter and more distorted the moment the monitor was
        // armed, the mic silent.
        let pcm: Vec<Smp> = (0..2 * 48_000 * CHANNELS)
            .map(|i| 0.989 * ((i / 2) as f32 * std::f32::consts::TAU * 220.0 / 48_000.0).sin())
            .collect();
        let render = |gain: f32| {
            let desc = GraphDesc {
                nodes: vec![
                    NodeEntry::new(1, Box::new(BufferSource::new(pcm.clone(), 0))),
                    NodeEntry::new(2, Box::new(MasterNode::limited())),
                ],
                edges: vec![Edge::simple(1, 2)],
                master: 2,
            };
            let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
            let (mut mic, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 12);
            let (h, mut rt) = spawn(g, HashMap::new(), mic_cons, 256);
            h.play();
            h.set_monitor_gain(gain);
            let mut all = Vec::new();
            let mut out = vec![0.0; BLOCK * CHANNELS];
            for _ in 0..300 {
                for _ in 0..BLOCK {
                    let _ = mic.push([0.0, 0.0]);
                }
                rt.process(&mut out, BLOCK);
                all.extend_from_slice(&out);
            }
            all
        };
        let off = render(0.0);
        let on = render(1.0);
        assert!(
            off.iter().any(|&x| x.abs() > 0.9),
            "the backing reaches the clip"
        );
        assert_eq!(off, on);
    }

    #[test]
    fn loop_bounds_are_read_as_the_pair_set_loop_published() {
        // As two stores, a reader could see the old start with the new end: a valid-length
        // span, playing outside the new loop.
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: vec![],
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, 512).unwrap();
        let (_p, c) = spsc::<[Smp; CHANNELS]>(16);
        let (h, _rt) = spawn(g, HashMap::new(), c, 256);
        h.set_loop(true, 2 * S, 20 * S);
        assert_eq!(h.shared_arc().loop_bounds(), (2 * S, 20 * S));
        let shared = Arc::clone(h.shared_arc());
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let reader = std::thread::spawn(move || {
            let mut torn = 0u64;
            while !stop2.load(Ordering::Relaxed) {
                let pair = shared.loop_bounds();
                if pair != (2 * S, 20 * S) && pair != (10 * S, 12 * S) {
                    torn += 1;
                }
            }
            torn
        });
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_millis(300) {
            h.set_loop(true, 10 * S, 12 * S);
            h.set_loop(true, 2 * S, 20 * S);
        }
        stop.store(true, Ordering::Relaxed);
        assert_eq!(reader.join().unwrap(), 0, "a torn pair was read");
        // Bounds past what 32 bits hold (~24.8 h) are held at the top, not wrapped.
        h.set_loop(true, 0, i64::MAX);
        assert_eq!(h.shared_arc().loop_bounds(), (0, u32::MAX as i64));
    }
}

#[cfg(test)]
mod declick_tests {
    //! Every transport cut, rendered through the graph: the output around it is continuous with
    //! the declick, and steps without it (so each scenario does cut).

    use super::*;
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::ring::spsc;
    use crate::source::BufferSource;

    const BLOCK: usize = 128;

    /// A loud 220 Hz tone, its largest natural step between two frames ~0.026.
    fn tone(frames: usize) -> Vec<Smp> {
        (0..frames * CHANNELS)
            .map(|i| 0.9 * ((i / 2) as f32 * std::f32::consts::TAU * 220.0 / 48_000.0).sin())
            .collect()
    }

    fn engine(pcm: Vec<Smp>, declick: bool) -> (EngineHandle, RtProcessor) {
        let desc = GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(BufferSource::new(pcm, 0))),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2)],
            master: 2,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (_mic, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        h.set_declick(declick);
        (h, rt)
    }

    fn render(rt: &mut RtProcessor, blocks: usize, into: &mut Vec<Smp>) {
        let mut out = vec![0.0; BLOCK * CHANNELS];
        for _ in 0..blocks {
            rt.process(&mut out, BLOCK);
            into.extend_from_slice(&out);
        }
    }

    /// The largest step between two consecutive frames (left channel), from silence before
    /// the first.
    fn worst_step(out: &[Smp]) -> f32 {
        let mut prev = 0.0f32;
        let mut worst = 0.0f32;
        for f in out.chunks(CHANNELS) {
            worst = worst.max((f[0] - prev).abs());
            prev = f[0];
        }
        worst
    }

    /// Play from mid-song, then pause mid-phrase and play again.
    fn play_pause_play(declick: bool) -> Vec<Smp> {
        let (h, mut rt) = engine(tone(96_000), declick);
        h.seek(10_000 + 27); // off a zero crossing
        h.play();
        let mut out = Vec::new();
        render(&mut rt, 20, &mut out);
        h.pause();
        render(&mut rt, 10, &mut out);
        h.play();
        render(&mut rt, 10, &mut out);
        out
    }

    #[test]
    fn play_glides_in_and_pause_glides_out() {
        let out = play_pause_play(true);
        assert!(out[0].abs() < 0.01, "play starts at {}", out[0]);
        assert!(worst_step(&out) < 0.04, "a step of {}", worst_step(&out));
        // Past the fade the tone plays untouched, and paused past its fade is silence.
        let src = tone(96_000);
        let at = DECLICK_FRAMES + 10;
        assert_eq!(out[at * CHANNELS], src[(10_027 + at) * CHANNELS]);
        let paused = 20 * BLOCK;
        assert!((out[paused * CHANNELS] - out[(paused - 1) * CHANNELS]).abs() < 0.04);
        assert!(
            out[(paused + DECLICK_FRAMES) * CHANNELS..(paused + 10 * BLOCK) * CHANNELS]
                .iter()
                .all(|&x| x == 0.0)
        );

        let stepped = worst_step(&play_pause_play(false));
        assert!(
            stepped > 0.3,
            "without the declick play and pause step ({stepped})"
        );
    }

    /// A seek while playing, between two stretches of the tone far apart in phase.
    fn seek_while_playing(declick: bool) -> Vec<Smp> {
        let (h, mut rt) = engine(tone(96_000), declick);
        h.seek(1_000);
        h.play();
        let mut out = Vec::new();
        render(&mut rt, 10, &mut out);
        h.seek(40_000 + 55);
        render(&mut rt, 10, &mut out);
        out
    }

    #[test]
    fn a_seek_crossfades_from_the_old_audio_to_the_new() {
        let out = seek_while_playing(true);
        // From the frame before the seek on (the opening fade-in is the test above's).
        let cut = 10 * BLOCK;
        let around = &out[(cut - 1) * CHANNELS..];
        let worst = around
            .chunks(CHANNELS)
            .zip(around[CHANNELS..].chunks(CHANNELS))
            .map(|(a, b)| (b[0] - a[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.04, "the seek stepped by {worst}");
        let src = tone(96_000);
        let at = cut + DECLICK_FRAMES + 3;
        assert_eq!(out[at * CHANNELS], src[(40_055 + at - cut) * CHANNELS]);

        let off = seek_while_playing(false);
        assert!((off[cut * CHANNELS] - off[(cut - 1) * CHANNELS]).abs() > 0.3);
    }

    /// Three passes of a loop whose ends meet the tone far apart in phase.
    fn three_passes(declick: bool) -> Vec<Smp> {
        let (h, mut rt) = engine(tone(96_000), declick);
        h.set_loop(true, 2_000, 6_013);
        h.seek(2_000);
        h.play();
        let mut out = Vec::new();
        render(&mut rt, 100, &mut out);
        out
    }

    #[test]
    fn every_loop_pass_fades_through_the_seam() {
        let out = three_passes(true);
        assert!(
            worst_step(&out) < 0.04,
            "a seam stepped by {}",
            worst_step(&out)
        );
        // Each seam dips to silence and the pass between plays untouched.
        let len = 4_013;
        let src = tone(96_000);
        for pass in 1..3 {
            let seam = pass * len;
            assert!(
                out[(seam - 1) * CHANNELS].abs() < 1e-3,
                "pass {pass} ends at a dip"
            );
            assert!(
                out[seam * CHANNELS].abs() < 0.03,
                "pass {pass} starts at a dip"
            );
            let mid = seam + len / 2;
            assert_eq!(out[mid * CHANNELS], src[(2_000 + len / 2) * CHANNELS]);
        }

        let stepped = worst_step(&three_passes(false));
        assert!(
            stepped > 0.3,
            "without the declick each seam steps ({stepped})"
        );
    }
}

#[cfg(test)]
mod carry_tests {
    //! A graph rebuilt during playback: the effects it keeps carry their state over, so their
    //! tails ring on through the swap.

    use super::*;
    use crate::dsp::delay::{param as dparam, DelayNode};
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::ring::spsc;
    use crate::source::BufferSource;

    const BLOCK: usize = 128;

    /// A 10 ms burst at the timeline's start, then silence.
    fn burst() -> Vec<Smp> {
        (0..48_000 * CHANNELS)
            .map(|i| {
                if i / 2 < 480 {
                    0.5 * ((i / 2) as f32 * 0.3).sin()
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// Source(1) -> effect(2) -> master(3), the effect as `effect` builds it.
    fn graph(effect: Box<dyn Node>) -> Graph {
        GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(BufferSource::new(burst(), 0))),
                NodeEntry::new(2, effect),
                NodeEntry::new(3, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2), Edge::simple(2, 3)],
            master: 3,
        }
        .compile(ENGINE_SR, BLOCK)
        .unwrap()
    }

    fn rms(x: &[Smp]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Plays the burst through `effect`, rebuilds the graph (a fresh `effect`, its settings
    /// pushed onto the staged graph as the host does) 50 ms in, and returns what played after.
    fn rebuilt_mid_tail(
        effect: impl Fn() -> Box<dyn Node>,
        settings: &[(u16, f32)],
        fresh: bool,
    ) -> Vec<Smp> {
        let (_mic, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (mut h, mut rt) = spawn(graph(effect()), HashMap::new(), mic_cons, 256);
        h.set_declick(false);
        for &(p, v) in settings {
            h.set_param(2, p, v);
        }
        h.flush();
        h.play();
        let mut out = vec![0.0; BLOCK * CHANNELS];
        for _ in 0..(2_400 / BLOCK) {
            rt.process(&mut out, BLOCK);
        }
        let mut next = graph(effect());
        if fresh {
            next.start_fresh(2, 3);
        }
        h.stage_graph(next);
        for &(p, v) in settings {
            h.set_param(2, p, v);
        }
        assert!(h.publish_staged());
        let mut after = Vec::new();
        for _ in 0..(12_000 / BLOCK) {
            rt.process(&mut out, BLOCK);
            after.extend_from_slice(&out);
        }
        after
    }

    #[test]
    fn a_delay_kept_by_a_rebuild_still_echoes_what_played_before_it() {
        // 100 ms echoes of a burst at 0: the rebuild at 50 ms comes between the burst and its
        // first echo, which only the carried line still holds.
        let delay = || -> Box<dyn Node> { Box::new(DelayNode::new()) };
        let settings = [
            (dparam::BYPASS, 0.0),
            (dparam::TIME_MS, 100.0),
            (dparam::FEEDBACK, 0.5),
            (dparam::MIX, 1.0),
        ];
        // The first echo lands at 100 ms: 50 ms after the swap.
        let echo = |out: &[Smp]| rms(&out[2_400 * CHANNELS..2_880 * CHANNELS]);
        let carried = rebuilt_mid_tail(delay, &settings, false);
        assert!(
            echo(&carried) > 0.1,
            "the echo is gone ({})",
            echo(&carried)
        );
        let reset = rebuilt_mid_tail(delay, &settings, true);
        assert!(echo(&reset) < 1e-6, "a fresh delay has nothing to echo");
    }

    #[test]
    fn the_shared_reverb_rings_on_through_a_rebuild() {
        let reverb = || -> Box<dyn Node> { Box::new(ReverbNode::default_bus()) };
        let carried = rebuilt_mid_tail(reverb, &[], false);
        let reset = rebuilt_mid_tail(reverb, &[], true);
        // Right after the swap the tail plays on at the level it had; a fresh tank is silent.
        let head = |out: &[Smp]| rms(&out[..BLOCK * CHANNELS]);
        assert!(
            head(&carried) > 0.01,
            "the tail is cut ({})",
            head(&carried)
        );
        assert!(head(&reset) < 1e-6);
    }
}
