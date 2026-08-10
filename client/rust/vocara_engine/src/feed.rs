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
}

impl StreamCtrl {
    pub fn new() -> Arc<Self> {
        Arc::new(StreamCtrl {
            gen: AtomicU32::new(0),
            flush_seq: AtomicU32::new(0),
            xruns: std::sync::atomic::AtomicU64::new(0),
        })
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
            scratch: vec![[0.0; CHANNELS]; 1024],
        }
    }

    /// Refresh the reader's file handle (see [`WavTrackReader::reopen`]);
    /// used by the pump thread's transient-error retry.
    pub fn reopen_reader(&mut self) -> Result<(), String> {
        self.reader.reopen()
    }

    /// Pushes a two-frame fence when the ring has room for both; on success
    /// publishes the generation. Returns whether it landed (a full ring
    /// retries on the next pump).
    fn push_fence(&mut self, generation: u32, base_tl: i64) -> bool {
        if self.producer.free() < 2 {
            return false;
        }
        let [a, b] = fence_frames(generation, base_tl);
        let ok = self.producer.push(a) && self.producer.push(b);
        debug_assert!(ok, "free() >= 2 guaranteed room for both fence frames");
        self.ctrl.gen.store(generation, Ordering::Release);
        ok
    }

    /// The transport position a plan must start from. The render clock wraps
    /// `frame >= loop_end` back to `loop_start` BEFORE producing anything
    /// (see `RtProcessor::render_block`), so a raw playhead at/past loop end
    /// — a pause fold from a run that played with the loop disengaged, held
    /// until the first post-play block is consumed — must be modeled wrapped:
    /// a linear plan from there streams positions the transport never plays.
    fn planned_pos(t: &Shared, basis: &PlanBasis) -> i64 {
        let pos = t.playhead().max(0);
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
        if self.push_fence(next_gen, pos) {
            self.generation = next_gen;
            self.next_tl = pos;
            self.ctrl.flush_seq.fetch_add(1, Ordering::Release);
        } else {
            // Ring full of doomed frames: force the basis stale so the next
            // pump retries the replan (the node will discard once the fence
            // lands).
            self.basis = None;
        }
    }

    /// Top the ring up. Returns Err only on unreadable file (the caller
    /// tears the streaming source down; playback degrades to silence for
    /// this track rather than corrupting anything).
    pub fn pump(&mut self, t: &Shared) -> Result<(), String> {
        let basis = PlanBasis {
            flush_gen: t.out_flush_gen(),
            playing: t.is_playing(),
            loop_on: t.loop_enabled(),
            ls: t.loop_start(),
            le: t.loop_end(),
        };
        if self.basis != Some(basis) {
            self.replan(t, basis);
        }
        if !basis.playing {
            return Ok(()); // paused: hold the plan; play() bumps flush_gen
        }
        // Starvation recovery: the transport ran AHEAD of the plan (the ring
        // starved and the node discarded everything). Feeding the stale gap
        // would waste a whole ring of catch-up; JUMP the plan to the current
        // position with an in-band fence instead (no flush_seq — nothing in
        // the ring needs discarding; it is empty by definition of having
        // starved).
        // `planned_pos` models the render-side wrap, so while looping `pos`
        // is always inside [ls, le) — a plan behind a wrapped transport shows
        // up here as `pos > next_tl` like any other starvation.
        let pos = Self::planned_pos(t, &basis);
        if pos > self.next_tl {
            let next_gen = self.generation.wrapping_add(1);
            if self.push_fence(next_gen, pos) {
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
                let next_gen = self.generation.wrapping_add(1);
                if !self.push_fence(next_gen, basis.ls) {
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
            let free = self.producer.free();
            let want = self
                .scratch
                .len()
                .min(free)
                .min(run_left.min(span_left) as usize);
            if want == 0 {
                return Ok(()); // ring full (or boundary reached this pump)
            }
            let local = self.next_tl - self.start_frame;
            let got = self.reader.read_at(local, &mut self.scratch[..want])?;
            for i in 0..got {
                if !self.producer.push(self.scratch[i]) {
                    // Raced full: rewind next_tl to the unpushed frame.
                    self.next_tl += i as i64;
                    return Ok(());
                }
            }
            self.next_tl += got as i64;
            if got < want {
                // Physically truncated file: treat the rest as silence.
                return Ok(());
            }
        }
    }
}
