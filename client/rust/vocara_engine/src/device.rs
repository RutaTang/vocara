//! The live OS audio path (macOS CoreAudio via cpal) + file decode (symphonia).
//!
//! The cpal output `Stream` is `!Send`, so all stream lifecycle (build / drop / device switch)
//! is confined to one dedicated [`AudioHost`] thread; FFI threads only message it. The audio
//! thread renders the engine's fixed `ENGINE_SR` mix and `build_stream` resamples it to the
//! device's native rate at the I/O boundary. `decode_file` + `resample_sinc` turn any
//! supported file into engine-rate interleaved-stereo PCM the feeder pushes to a source ring.
//! Gated behind the `device` feature so the DSP core stays dependency-free.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::live::RtCell;
use crate::ring::{Consumer, Producer};
use crate::sample::{Smp, CHANNELS, ENGINE_SR};

/// `ve_poll_device_event` bit: the active output device was lost (stream error / disconnect).
pub const EVENT_OUTPUT_LOST: u32 = 1;
/// `ve_poll_device_event` bit: the active input device was lost (stream error / disconnect).
pub const EVENT_INPUT_LOST: u32 = 2;
/// The mic opened WITHOUT echo cancellation (VPIO negotiated mismatched element
/// rates — see vpio.rs's bypass rationale: a working mic with echo bleed beats
/// no mic). Raised so the UI can tell the user speaker playback may bleed into
/// the take instead of leaving the degradation silent (stderr is not a UI).
pub const EVENT_AEC_BYPASSED: u32 = 4;

/// Latched when a stream teardown failed to synchronously stop its callback (a cpal `pause()`
/// error, or `AudioOutputUnitStop` failing in `vpio`). The renderer gate's `false` state means
/// "no streams, hence no callbacks, hence retired graphs can be freed immediately" — a failed
/// teardown breaks that proof for the rest of the process (a zombie callback may outlive its
/// handle), so once latched the gate is never settled to `false` again and `EngineHandle::reap`
/// stays on the pre-gate grace-epoch path (worst case: retirees park until teardown, a bounded
/// leak — never a use-after-free under a possibly-live callback).
static STREAM_TEARDOWN_FAILED: AtomicBool = AtomicBool::new(false);

/// Record a failed stream teardown (see [`STREAM_TEARDOWN_FAILED`]). Called from `Drop` paths
/// on the audio-host thread; the `Release` store pairs with `effective_gate`'s `Acquire` load.
pub(crate) fn note_stream_teardown_failure() {
    STREAM_TEARDOWN_FAILED.store(true, Ordering::Release);
}

/// The renderer-gate value to publish for a desired stream state: a latched teardown failure
/// pins the gate raised (grace-epoch reaping only).
fn effective_gate(streams_up: bool) -> bool {
    streams_up || STREAM_TEARDOWN_FAILED.load(Ordering::Acquire)
}

/// Keeps the cpal stream alive; dropping it stops audio. `cpal::Stream` is `!Send`, so this
/// only ever lives on the [`AudioHost`] thread that created it.
pub struct DeviceHandle {
    _stream: cpal::Stream,
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        // Dropping a cpal `Stream` alone does NOT synchronize with a callback in flight:
        // cpal's coreaudio monitor threads (disconnect / default-output listeners) hold a
        // `Weak` to the inner stream state and can transiently upgrade it, in which case our
        // drop merely decrements a refcount and the old callback keeps rendering — racing the
        // replacement stream's callback on the shared `RtCell` (two `&mut RtProcessor` = UB).
        // `pause()` takes the same inner lock those monitor threads use and synchronously
        // executes `AudioOutputUnitStop`, which joins any in-flight render callback; nothing
        // ever restarts a paused stream (the monitors only pause), so after this returns no
        // callback is executing and none will start. A FAILED stop leaves the pre-existing
        // narrow callback race — and would also invalidate the renderer gate's "no streams
        // => no callbacks" proof, so it is latched: the gate then never settles to `false`
        // and retired graphs stay on the grace-epoch path instead of being freed under a
        // possibly-live callback.
        if self._stream.pause().is_err() {
            note_stream_teardown_failure();
        }
    }
}

/// Engine render sub-chunk for the output-boundary resampler (matches the graph's max block).
const RESAMPLE_BLOCK: usize = 1024;

/// A discoverable device, flattened for the FFI boundary.
pub struct DeviceDesc {
    /// Stable identifier (CoreAudio device UID) — the persist key.
    pub uid: String,
    pub name: String,
    /// Native sample rate (0 if unavailable).
    pub rate: u32,
    pub is_default: bool,
    /// Bluetooth transport — a same-device in+out pick degrades output to HFP (hardware limit).
    pub is_bluetooth: bool,
}

/// The CoreAudio device UID for a cpal device (stable across reconnects / duplicate names).
fn device_uid(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.id().to_string())
}

/// The human name + whether the device is on a Bluetooth transport.
fn device_meta(device: &cpal::Device) -> (String, bool) {
    match device.description() {
        Ok(desc) => (
            desc.name().to_string(),
            desc.interface_type() == cpal::InterfaceType::Bluetooth,
        ),
        Err(_) => (device.to_string(), false),
    }
}

/// The system default output device.
fn default_output_device() -> Option<cpal::Device> {
    cpal::default_host().default_output_device()
}

/// Find an output device by its stable UID.
fn output_device_by_uid(uid: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    host.output_devices()
        .ok()?
        .find(|d| device_uid(d).as_deref() == Some(uid))
}

/// A forced-stereo stream config at the device's native rate (CoreAudio picks the buffer size).
fn output_config(device: &cpal::Device) -> Result<cpal::StreamConfig, String> {
    let default = device
        .default_output_config()
        .map_err(|e| format!("default_output_config: {e}"))?;
    Ok(cpal::StreamConfig {
        channels: CHANNELS as u16,
        sample_rate: default.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    })
}

/// Enumerate the available output devices (name + UID + native rate + default flag).
pub fn enum_output_devices() -> Vec<DeviceDesc> {
    let host = cpal::default_host();
    let default_uid = host.default_output_device().and_then(|d| device_uid(&d));
    let mut out = Vec::new();
    let Ok(devices) = host.output_devices() else {
        return out;
    };
    for d in devices {
        let Some(uid) = device_uid(&d) else { continue };
        let (name, is_bluetooth) = device_meta(&d);
        let rate = d
            .default_output_config()
            .map(|c| c.sample_rate())
            .unwrap_or(0);
        let is_default = default_uid.as_deref() == Some(uid.as_str());
        out.push(DeviceDesc {
            uid,
            name,
            rate,
            is_default,
            is_bluetooth,
        });
    }
    out
}

/// The system default input device. (cpal input path — non-Apple; macOS uses VoiceProcessingIO.)
#[cfg(not(target_vendor = "apple"))]
fn default_input_device() -> Option<cpal::Device> {
    cpal::default_host().default_input_device()
}

/// Find an input device by its stable UID. (cpal input path — non-Apple.)
#[cfg(not(target_vendor = "apple"))]
fn input_device_by_uid(uid: &str) -> Option<cpal::Device> {
    cpal::default_host()
        .input_devices()
        .ok()?
        .find(|d| device_uid(d).as_deref() == Some(uid))
}

/// The input device's native config: (stream config, native rate, channel count). Input keeps
/// the device's native channel count; the callback downmixes to stereo. (cpal input — non-Apple.)
#[cfg(not(target_vendor = "apple"))]
fn input_config(device: &cpal::Device) -> Result<(cpal::StreamConfig, f64, usize), String> {
    let default = device
        .default_input_config()
        .map_err(|e| format!("default_input_config: {e}"))?;
    let channels = default.channels();
    // Diagnostic: which device + format cpal actually opened for input (shows in the run console).
    eprintln!(
        "vocara: input device='{}' rate={} channels={} format={:?}",
        device_meta(device).0,
        default.sample_rate(),
        channels,
        default.sample_format(),
    );
    let config = cpal::StreamConfig {
        channels,
        sample_rate: default.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };
    Ok((config, default.sample_rate() as f64, channels as usize))
}

/// Enumerate the available input devices (name + UID + native rate + default flag).
pub fn enum_input_devices() -> Vec<DeviceDesc> {
    let host = cpal::default_host();
    let default_uid = host.default_input_device().and_then(|d| device_uid(&d));
    let mut out = Vec::new();
    let Ok(devices) = host.input_devices() else {
        return out;
    };
    for d in devices {
        let Some(uid) = device_uid(&d) else { continue };
        let (name, is_bluetooth) = device_meta(&d);
        let rate = d
            .default_input_config()
            .map(|c| c.sample_rate())
            .unwrap_or(0);
        let is_default = default_uid.as_deref() == Some(uid.as_str());
        out.push(DeviceDesc {
            uid,
            name,
            rate,
            is_default,
            is_bluetooth,
        });
    }
    out
}

/// Output-boundary sample-rate converter: the engine renders at `ENGINE_SR`; the device
/// callback demands frames at its native rate. This pulls engine-rate frames from
/// [`RtProcessor`] into a small pre-allocated FIFO and produces the device-rate output
/// through a POLYPHASE WINDOWED-SINC kernel (64 taps, 256x oversampled table, Blackman
/// window, cutoff scaled for downsampling) — the production kernel, replacing the linear
/// interim that audibly rolled off and aliased on non-48 kHz devices. RT-safe: the table
/// is precomputed at construction; `process` allocates nothing, takes no lock, makes no
/// syscall (64 multiply-adds per output frame is negligible arithmetic).
///
/// POSITION SEMANTICS ARE UNCHANGED from the linear kernel: the kernel is symmetric
/// around the read position, so output content position == read position, and the
/// consumed cursor advances by the read position's integer progression — every
/// transport-honesty test holds as-is. The FIFO keeps `HALF` frames of history for the
/// kernel's left lobe (zero-prefilled at start/flush, at negative content indices the
/// cursor never counts), and only frames the read position has passed beyond the history
/// margin are dropped.
struct OutResampler {
    rt: Arc<RtCell>,
    /// Engine input frames advanced per device output frame (`ENGINE_SR / device_rate`).
    step: f64,
    /// Read position in CONTENT frames (monotone within a flush generation).
    pos: f64,
    /// Content index of `fifo[0]` (starts at `-HALF`: the zero-prefilled history).
    origin: i64,
    /// Integer content frames the cursor has already been advanced past.
    consumed_floor: i64,
    /// Pending engine-rate frames (history + lookahead) awaiting the kernel.
    fifo: Vec<[Smp; CHANNELS]>,
    /// Scratch for one engine render block.
    scratch: Vec<Smp>,
    /// The polyphase kernel: `(OVERSAMPLE + 1)` rows of `TAPS` unity-DC-normalized
    /// coefficients; adjacent rows are linearly interpolated by the sub-phase.
    table: Vec<f32>,
    max_block: usize,
    /// The last observed transport flush generation: a bump (seek/pause) means the FIFO holds
    /// audio from BEFORE the transport change, which must be dropped, not played out.
    flush_gen: u32,
}

/// Kernel width in engine frames. 64 taps at 256x table resolution yields ~70 dB stopband
/// with a passband flat well past 20 kHz — far beyond the linear kernel — at 64 MACs per
/// output frame (~2.8 M MAC/s at 44.1 kHz: negligible).
const SINC_TAPS: usize = 64;
const SINC_HALF: usize = SINC_TAPS / 2;
const SINC_OVERSAMPLE: usize = 256;

/// Builds the polyphase table for [step] (engine frames per output frame): row `r` holds
/// the windowed-sinc weights for sub-phase `r / OVERSAMPLE`, cutoff scaled below the
/// output Nyquist when downsampling (step > 1), each row normalized to unity DC so the
/// converter has exactly 0 dB gain at every phase.
fn build_sinc_table(step: f64) -> Vec<f32> {
    let fc = if step > 1.0 { 0.92 / step } else { 0.92 };
    let mut table = vec![0.0f32; (SINC_OVERSAMPLE + 1) * SINC_TAPS];
    for r in 0..=SINC_OVERSAMPLE {
        let frac = r as f64 / SINC_OVERSAMPLE as f64;
        let mut sum = 0.0f64;
        for k in 0..SINC_TAPS {
            // Contribution of content index floor(pos) - (HALF-1) + k at offset x from pos.
            let x = k as f64 - (SINC_HALF as f64 - 1.0) - frac;
            let sinc = if x.abs() < 1e-12 {
                1.0
            } else {
                let px = std::f64::consts::PI * fc * x;
                px.sin() / px * fc
            };
            // Blackman window over the kernel span.
            let w = {
                let t = (x + SINC_HALF as f64) / (SINC_TAPS as f64);
                let t = t.clamp(0.0, 1.0);
                0.42 - 0.5 * (2.0 * std::f64::consts::PI * t).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * t).cos()
            };
            let v = sinc * w;
            table[r * SINC_TAPS + k] = v as f32;
            sum += v;
        }
        // Unity DC per row: without this the gain ripples with the sub-phase.
        if sum.abs() > 1e-12 {
            let inv = (1.0 / sum) as f32;
            for k in 0..SINC_TAPS {
                table[r * SINC_TAPS + k] *= inv;
            }
        }
    }
    table
}

impl OutResampler {
    fn new(device_rate: f64, max_block: usize, rt: Arc<RtCell>) -> Self {
        let step = ENGINE_SR / device_rate;
        let flush_gen = rt.transport().out_flush_gen();
        // Peak FIFO occupancy in a sub-chunk: history (TAPS) + the span the chunk's read
        // positions cover + one render block of slack. Sized from the ACTUAL step so `push`
        // never reallocates on the RT callback.
        let cap = (max_block as f64 * step).ceil() as usize + max_block + SINC_TAPS + 4;
        let mut r = OutResampler {
            rt,
            step,
            pos: 0.0,
            origin: 0,
            consumed_floor: 0,
            fifo: Vec::with_capacity(cap),
            scratch: vec![0.0; max_block * CHANNELS],
            table: build_sinc_table(step),
            max_block,
            flush_gen,
        };
        r.reset_stream();
        r
    }

    /// (Re)starts the stream state: empty content, zero history for the kernel's left lobe
    /// at negative content indices the cursor never counts.
    fn reset_stream(&mut self) {
        self.fifo.clear();
        for _ in 0..SINC_HALF {
            self.fifo.push([0.0, 0.0]);
        }
        self.origin = -(SINC_HALF as i64);
        self.pos = 0.0;
        self.consumed_floor = 0;
    }

    #[inline]
    fn render_block(&mut self) {
        let frames = self.max_block;
        let n = frames * CHANNELS;
        // SAFETY: this resampler is owned by the single active stream callback (see RtCell).
        unsafe { self.rt.render(&mut self.scratch[..n], frames) };
        for f in 0..frames {
            self.fifo
                .push([self.scratch[f * CHANNELS], self.scratch[f * CHANNELS + 1]]);
        }
    }

    fn process(&mut self, out: &mut [Smp], out_frames: usize) {
        // A seek/pause happened since the last callback: everything pre-rendered into the
        // FIFO belongs to the OLD transport position and must not reach the listener —
        // dropping it makes the transport respond within one callback instead of playing out
        // up to a full render block (~21 ms) of stale audio.
        let gen = self.rt.transport().out_flush_gen();
        if gen != self.flush_gen {
            self.flush_gen = gen;
            // The discarded CONTENT frames (rendered, never heard, cursor not yet past
            // them) are skipped so the cursor stays in the render counter's coordinate
            // space. History below the cursor and the zero prefill are already counted
            // or never count.
            let content_end = self.origin + self.fifo.len() as i64;
            let unconsumed = (content_end - self.consumed_floor).max(0);
            self.rt.transport().advance_consumed(unconsumed as u64);
            self.reset_stream();
        }
        let mut done = 0;
        while done < out_frames {
            let chunk = (out_frames - done).min(self.max_block);
            // The kernel for the chunk's LAST read position touches content index
            // floor(pos + (chunk-1)*step) + HALF; render until the FIFO covers it.
            let last_pos = self.pos + (chunk.saturating_sub(1)) as f64 * self.step;
            let need_content = last_pos.floor() as i64 + SINC_HALF as i64;
            let need_len = (need_content - self.origin + 1) as usize;
            // The FIFO is sized in `new` so reaching `need_len` never reallocates.
            debug_assert!(need_len + self.max_block <= self.fifo.capacity());
            while self.fifo.len() < need_len {
                self.render_block();
            }
            for k in 0..chunk {
                let ip = self.pos.floor();
                let frac = self.pos - ip;
                // Polyphase row pair for this sub-phase, linearly interpolated.
                let rf = frac * SINC_OVERSAMPLE as f64;
                let r0 = rf.floor() as usize;
                let rw = (rf - r0 as f64) as f32;
                let row0 = r0 * SINC_TAPS;
                let row1 = (r0 + 1) * SINC_TAPS;
                let base = (ip as i64 - self.origin) as usize - (SINC_HALF - 1);
                let mut l = 0.0f32;
                let mut r = 0.0f32;
                for t in 0..SINC_TAPS {
                    let w =
                        self.table[row0 + t] + (self.table[row1 + t] - self.table[row0 + t]) * rw;
                    let f = self.fifo[base + t];
                    l += f[0] * w;
                    r += f[1] * w;
                }
                let o = (done + k) * CHANNELS;
                out[o] = l;
                out[o + 1] = r;
                self.pos += self.step;
            }
            // Consumed cursor: the read position's integer progression over CONTENT
            // frames — identical semantics to the linear kernel, so every transport
            // test holds unchanged.
            let floor_now = self.pos.floor() as i64;
            if floor_now > self.consumed_floor {
                self.rt
                    .transport()
                    .advance_consumed((floor_now - self.consumed_floor) as u64);
                self.consumed_floor = floor_now;
            }
            // Drain everything below the kernel's history reach.
            let keep_from = floor_now - (SINC_HALF as i64 - 1);
            let drop = (keep_from - self.origin).max(0) as usize;
            let drop = drop.min(self.fifo.len());
            if drop > 0 {
                self.fifo.drain(0..drop);
                self.origin += drop as i64;
            }
            done += chunk;
        }
    }
}

/// Build + start an f32 output stream whose callback renders the shared `rt` at `ENGINE_SR`,
/// resampling to the device's native rate (`config.sample_rate`) at the boundary. At 48 kHz the
/// engine renders directly into the cpal buffer (identity, zero overhead). A stream error
/// (e.g. the device disconnects) raises [`EVENT_OUTPUT_LOST`] in `event` for the host to poll.
///
/// Must be called on the [`AudioHost`] thread (the returned `!Send` stream stays there).
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    rt: Arc<RtCell>,
    event: Arc<AtomicU32>,
) -> Result<DeviceHandle, String> {
    let channels = config.channels as usize;
    let device_rate = config.sample_rate as f64;
    // A previous non-48 kHz stream's resampler died with frames still in its
    // FIFO: the consumed cursor would lag the render counter forever. This
    // stream starts with an empty (or no) FIFO, so the cursor equals the
    // render counter — sync before any callback can run.
    rt.transport().sync_consumed_to_rendered();
    let err_cb = move |err| {
        // No logging/alloc on the error path: flag the loss for the host's device-event poll.
        let _ = err;
        event.fetch_or(EVENT_OUTPUT_LOST, Ordering::Release);
    };
    let stream = if device_rate == ENGINE_SR {
        let rt = Arc::clone(&rt);
        device.build_output_stream(
            *config, // cpal 0.18 takes the config by value (StreamConfig is Copy)
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Forced stereo: interleaved frames = len / 2. process() chunks to max_block.
                let frames = data.len() / channels;
                // SAFETY: exactly one active stream callback dereferences the cell (see RtCell).
                unsafe { rt.render(data, frames) };
                // Identity path: the render IS the delivery.
                rt.transport().sync_consumed_to_rendered();
            },
            err_cb,
            None,
        )
    } else {
        let mut res = OutResampler::new(device_rate, RESAMPLE_BLOCK, rt);
        device.build_output_stream(
            *config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                res.process(data, frames);
            },
            err_cb,
            None,
        )
    }
    .map_err(|e| format!("build_output_stream: {e}"))?;
    stream.play().map_err(|e| format!("stream.play: {e}"))?;
    Ok(DeviceHandle { _stream: stream })
}

/// Open an output stream for the device with `uid` (or the system default when `uid` is None).
fn build_output(
    uid: Option<&str>,
    rt: &Arc<RtCell>,
    event: &Arc<AtomicU32>,
) -> Result<(DeviceHandle, String), String> {
    let device = match uid {
        Some(u) => output_device_by_uid(u).ok_or_else(|| format!("output device {u} not found"))?,
        None => default_output_device().ok_or_else(|| "no default output device".to_string())?,
    };
    let config = output_config(&device)?;
    let stream = build_stream(&device, &config, Arc::clone(rt), Arc::clone(event))?;
    let resolved = device_uid(&device).unwrap_or_default();
    Ok((stream, resolved))
}

/// Input-side shared cell: the active input stream's callback feeds mic PCM through it
/// (downmix to stereo + resample to `ENGINE_SR`) into the mic ring whose consumer lives in the
/// `RtProcessor`. Mirrors [`RtCell`]'s single-accessor rule — only the one active input callback
/// touches it, and the host thread reconfigures it only while no input stream is running. It
/// persists across input-device changes so the mic ring is created exactly once.
pub struct InputCell(UnsafeCell<InputState>);

// SAFETY: see the type doc — exactly one input callback accesses the inner state at a time.
unsafe impl Send for InputCell {}
unsafe impl Sync for InputCell {}

struct InputState {
    /// Mic ring → RtProcessor (monitor). Always fed while the input stream is open.
    mic: Producer<[Smp; CHANNELS]>,
    /// Capture ring → Dart (`ve_input_frames`, live analysis). Fed only while `cap_active`.
    cap: Producer<[Smp; CHANNELS]>,
    /// Recording ring → the WAV writer thread. Fed only while `rec_active`.
    rec: Producer<[Smp; CHANNELS]>,
    cap_active: Arc<AtomicBool>,
    rec_active: Arc<AtomicBool>,
    in_channels: usize,
    /// Push-based linear resampler: input-position advance per engine output frame
    /// (`in_rate / ENGINE_SR`). `1.0` → identity passthrough.
    step: f64,
    /// CLOCK-DRIFT ratio (f64 bits; 1.0 = no correction), published by the
    /// control-side [`DriftEstimator`]: the measured input-clock rate over
    /// the output-clock rate. The effective step is `step * ratio`, so the
    /// emission rate matches what the OUTPUT device actually consumes — a
    /// mic whose crystal runs 30 ppm fast no longer drifts the take ahead
    /// of the backing over a long recording. Exactly 1.0 (the estimator's
    /// deadband) keeps the 48 kHz identity fast path.
    drift_ratio_bits: Arc<AtomicU64>,
    pos: f64,
    prev: [Smp; CHANNELS],
    primed: bool,
    /// Input peak level (f32 bits) for the meter — read by the FFI thread.
    level: Arc<AtomicU32>,
    /// Total ENGINE-RATE (48 kHz, post-resample) frames the callback has emitted — both the
    /// "is the mic delivering?" diagnostic and the clock the take pre-roll measurement
    /// divides by `ENGINE_SR`, so it must never count raw device-rate frames.
    frames_in: Arc<AtomicU64>,
    /// Frames the RECORDING ring refused because it was full (writer thread stalled longer
    /// than the ring's headroom). Those samples are gone from the take, so the count must
    /// surface to the caller — a take with silent holes must never report as clean.
    rec_dropped: Arc<AtomicU64>,
    /// Frames successfully pushed into the RECORDING ring for the CURRENT take (reset at
    /// each recording start). This is the per-take recorded-progress ack: a nonzero value
    /// PROVES audio is in this take's ring — a callback that entered before the arm read
    /// `rec = false` and never pushed, so unlike the global frame counter this can never
    /// count frames that aren't in the WAV. The ready handshake and the pre-roll
    /// measurement both read it.
    rec_written: Arc<AtomicU64>,
}

impl InputState {
    /// Fan one resampled frame out to the monitor ring (always) + the capture/recording rings
    /// (when their consumer is active). Drops on a full ring (back-pressure; never blocks) —
    /// but a drop on the RECORDING ring is counted: monitor/capture drops only cost a live
    /// view a frame, a recording drop is a permanent hole in the take on disk.
    #[inline]
    fn emit(&mut self, f: [Smp; CHANNELS], cap: bool, rec: bool) {
        self.mic.push(f);
        if cap {
            self.cap.push(f);
        }
        if rec {
            if self.rec.push(f) {
                self.rec_written.fetch_add(1, Ordering::Relaxed);
            } else {
                self.rec_dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl InputCell {
    // One positional slot per fan-out ring/flag/meter: a builder or params struct would only
    // restate the InputState field list this constructor forwards to.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mic: Producer<[Smp; CHANNELS]>,
        cap: Producer<[Smp; CHANNELS]>,
        rec: Producer<[Smp; CHANNELS]>,
        cap_active: Arc<AtomicBool>,
        rec_active: Arc<AtomicBool>,
        level: Arc<AtomicU32>,
        frames_in: Arc<AtomicU64>,
        rec_dropped: Arc<AtomicU64>,
        rec_written: Arc<AtomicU64>,
        drift_ratio_bits: Arc<AtomicU64>,
    ) -> Arc<InputCell> {
        Arc::new(InputCell(UnsafeCell::new(InputState {
            mic,
            cap,
            rec,
            cap_active,
            rec_active,
            in_channels: 1,
            step: 1.0,
            drift_ratio_bits,
            pos: 0.0,
            prev: [0.0, 0.0],
            primed: false,
            level,
            frames_in,
            rec_dropped,
            rec_written,
        })))
    }

    /// RT-safe entry for the VPIO input callback (vpio.rs): forward interleaved f32 to the
    /// existing downmix/resample/fan-out path. Single-accessor (only the active input callback).
    #[cfg(target_vendor = "apple")]
    #[inline]
    pub fn feed_rt(&self, data: &[f32]) {
        // SAFETY: exactly one active input callback dereferences the cell (see InputCell docs).
        unsafe { self.feed(data) }
    }

    /// Reconfigure for a new input stream (native rate + channels). Only call with no active
    /// input callback (the host thread, between dropping the old stream and building the new).
    pub fn reconfigure(&self, in_rate: f64, in_channels: usize) {
        // SAFETY: no input callback runs during reconfigure (host-thread invariant).
        let st = unsafe { &mut *self.0.get() };
        st.in_channels = in_channels.max(1);
        st.step = in_rate / ENGINE_SR;
        st.pos = 0.0;
        st.primed = false;
        st.prev = [0.0, 0.0];
    }

    /// Feed one input callback's interleaved samples: downmix to stereo, resample to `ENGINE_SR`,
    /// fan out to the mic / capture / recording rings, and update the peak meter.
    ///
    /// # Safety
    /// Only the single active input callback may call this.
    unsafe fn feed(&self, data: &[f32]) {
        let st = &mut *self.0.get();
        let ch = st.in_channels;
        if ch == 0 {
            return;
        }
        let cap = st.cap_active.load(Ordering::Relaxed);
        let rec = st.rec_active.load(Ordering::Relaxed);
        let frames = data.len() / ch;
        // Clock-drift compensation: the estimator's published ratio scales
        // the position advance, so emission tracks the OUTPUT clock instead
        // of the input crystal's nominal rate. Ratio 1.0 (the deadband)
        // preserves the identity fast path bit-for-bit.
        let ratio = f64::from_bits(st.drift_ratio_bits.load(Ordering::Relaxed));
        let step = st.step * ratio;
        let identity = (step - 1.0).abs() < 1e-9;
        let mut peak = 0.0f32;
        // Counted in EMITTED (post-resample, ENGINE_SR) frames, not raw device frames:
        // consumers convert this counter to time by dividing by the engine rate (take
        // pre-roll measurement), so a device-rate count would skew every derived duration
        // whenever the input's native rate is not 48 kHz.
        let mut emitted: u64 = 0;
        for fi in 0..frames {
            let base = fi * ch;
            let l = data[base];
            let r = if ch > 1 { data[base + 1] } else { l };
            peak = peak.max(l.abs()).max(r.abs());
            if identity {
                st.emit([l, r], cap, rec);
                emitted += 1;
            } else {
                if !st.primed {
                    st.prev = [l, r];
                    st.primed = true;
                    continue;
                }
                let cur = [l, r];
                while st.pos < 1.0 {
                    let t = st.pos as f32;
                    let f = [
                        st.prev[0] + (cur[0] - st.prev[0]) * t,
                        st.prev[1] + (cur[1] - st.prev[1]) * t,
                    ];
                    st.emit(f, cap, rec);
                    emitted += 1;
                    st.pos += step;
                }
                st.pos -= 1.0;
                st.prev = cur;
            }
        }
        st.frames_in.fetch_add(emitted, Ordering::Relaxed);
        st.level.store(peak.to_bits(), Ordering::Relaxed);
    }
}

/// Control-side clock-drift estimator: compares the growth of the input's
/// emitted engine-rate frame counter against the output's consumed cursor
/// over ~1 s ticks and publishes a smoothed correction ratio for the input
/// resampler ([`InputState::drift_ratio_bits`]).
///
/// Two independent crystals never run at exactly the same rate; a mic 30 ppm
/// fast drifts a take ~18 ms ahead of the backing over 10 minutes, which
/// pre-roll (a START-time constant) cannot fix. Aligning the input to the
/// OUTPUT clock is the goal — the take must sit against what the listener
/// heard — so both absolute crystal errors cancel out of the design.
///
/// Robustness over reactivity, deliberately:
///  * a tick only measures when BOTH counters advanced ≥ [`MIN_DELTA`]
///    (mic closed or transport paused → the estimate FREEZES, never decays);
///  * instantaneous ratios beyond ±5000 ppm are glitches (a stall, a stream
///    rebuild) and are discarded, not averaged;
///  * an EMA (α = 0.05 per tick, ~20 s time constant) smooths the estimate,
///    so the published ratio slews far below audibility;
///  * a ±2 ppm deadband publishes EXACTLY 1.0 (keeping the identity fast
///    path for healthy 48 kHz chains), and the correction clamps at
///    ±500 ppm — anything larger is a broken device, not drift.
///
/// The mechanism is fully simulation-tested; hardware-in-the-loop
/// CALIBRATION (real crystal pairs) remains the follow-up noted in memory.
pub struct DriftEstimator {
    ratio_bits: Arc<AtomicU64>,
    last_in: u64,
    last_out: u64,
    primed: bool,
    ema: f64,
}

/// Minimum per-tick advance (engine frames) on each counter before a tick
/// measures: ~100 ms of audio, enough to average callback jitter away.
const MIN_DELTA: u64 = 4_800;

impl DriftEstimator {
    pub fn new(ratio_bits: Arc<AtomicU64>) -> Self {
        ratio_bits.store(1.0f64.to_bits(), Ordering::Relaxed);
        DriftEstimator {
            ratio_bits,
            last_in: 0,
            last_out: 0,
            primed: false,
            ema: 1.0,
        }
    }

    /// One ~1 s tick with the CURRENT counter values (monotonic totals).
    /// Publishes the updated ratio; returns it for tests.
    pub fn tick(&mut self, frames_in: u64, frames_out: u64) -> f64 {
        let din = frames_in.wrapping_sub(self.last_in);
        let dout = frames_out.wrapping_sub(self.last_out);
        // Prime on first observation; measure only when both sides moved
        // enough to mean something (a paused transport or a closed mic
        // freezes the estimate rather than corrupting it).
        if !self.primed {
            self.primed = true;
            self.last_in = frames_in;
            self.last_out = frames_out;
            return self.published();
        }
        if din < MIN_DELTA || dout < MIN_DELTA {
            return self.published();
        }
        self.last_in = frames_in;
        self.last_out = frames_out;
        let inst = din as f64 / dout as f64;
        if !(0.995..=1.005).contains(&inst) {
            return self.published(); // glitch (stall/rebuild), not drift
        }
        self.ema += 0.05 * (inst - self.ema);
        let err = self.ema - 1.0;
        let corrected = if err.abs() < 2e-6 {
            1.0 // deadband: exactly 1.0 keeps the identity fast path
        } else {
            1.0 + err.clamp(-500e-6, 500e-6)
        };
        self.ratio_bits.store(corrected.to_bits(), Ordering::Relaxed);
        corrected
    }

    fn published(&self) -> f64 {
        f64::from_bits(self.ratio_bits.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod drift_tests {
    //! The clock-drift compensation MECHANISM, simulation-tested end to end
    //! (the estimator's convergence/freeze/reject behavior, and the input
    //! resampler applying the ratio). Hardware CALIBRATION — real crystal
    //! pairs — is the documented follow-up; nothing here needs a device.

    use super::DriftEstimator;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn ratio_of(bits: &Arc<AtomicU64>) -> f64 {
        f64::from_bits(bits.load(Ordering::Relaxed))
    }

    /// Simulated 1 s ticks: the mic crystal runs [ppm] fast relative to the
    /// output clock, both counters advancing accordingly.
    fn run_ticks(est: &mut DriftEstimator, ppm: f64, seconds: u64) -> f64 {
        let mut last = 0.0;
        for s in 1..=seconds {
            let out = s * 48_000;
            let inn = (out as f64 * (1.0 + ppm * 1e-6)) as u64;
            last = est.tick(inn, out);
        }
        last
    }

    #[test]
    fn converges_to_the_true_drift_within_the_ema_time_constant() {
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        let r = run_ticks(&mut est, 30.0, 120); // 30 ppm fast mic, 2 min
        assert!(
            ((r - 1.0) * 1e6 - 30.0).abs() < 3.0,
            "converged correction {} ppm, expected ~30",
            (r - 1.0) * 1e6
        );
        assert_eq!(r, ratio_of(&bits), "published ratio matches");
    }

    #[test]
    fn a_paused_output_or_closed_mic_freezes_the_estimate() {
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        let before = run_ticks(&mut est, 30.0, 120);
        // Output stops (transport paused): the input keeps counting, the
        // estimate must HOLD — decaying toward 1.0 would re-drift the next
        // take, and averaging the stall would corrupt it.
        let (inn0, out0) = (300 * 48_000, 120 * 48_000);
        for s in 0..60u64 {
            est.tick(inn0 + s * 48_100, out0); // out frozen
        }
        assert_eq!(ratio_of(&bits), before, "estimate must freeze, not drift");
        // Mic closed: same.
        for _ in 0..60 {
            est.tick(inn0 + 60 * 48_100, out0);
        }
        assert_eq!(ratio_of(&bits), before);
    }

    #[test]
    fn glitches_are_rejected_and_the_correction_clamps() {
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        let before = run_ticks(&mut est, 20.0, 60);
        // A stream rebuild produces a wild instantaneous ratio: discarded.
        est.tick(60 * 48_000 * 2, 61 * 48_000);
        assert_eq!(ratio_of(&bits), before, "a glitch tick must not move it");

        // A pathologically fast clock clamps at +500 ppm (a broken device,
        // not drift — larger corrections would be audible pitch).
        let bits2 = Arc::new(AtomicU64::new(0));
        let mut est2 = DriftEstimator::new(Arc::clone(&bits2));
        let r = run_ticks(&mut est2, 4_000.0, 600);
        assert!(
            ((r - 1.0) * 1e6 - 500.0).abs() < 1.0,
            "clamped at +500 ppm, got {} ppm",
            (r - 1.0) * 1e6
        );
    }

    #[test]
    fn the_deadband_publishes_exactly_one_for_healthy_chains() {
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        let r = run_ticks(&mut est, 0.5, 300); // half a ppm: below deadband
        assert_eq!(r, 1.0, "sub-deadband drift must publish EXACTLY 1.0");
    }

    #[test]
    fn the_input_resampler_applies_the_ratio_to_its_emission_rate() {
        // A 48 kHz-nominal mic whose crystal is 1000 ppm fast (exaggerated
        // for a short test): with the correction published, feeding N input
        // frames emits ~N / (1 + 1000 ppm) engine frames — matched to what
        // the output actually consumes, which is the whole point.
        use crate::ring::spsc;
        use crate::sample::{Smp, CHANNELS};
        let (mic_prod, mut mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 17);
        let (cap_prod, _cap_cons) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, _rec_cons) = spsc::<[Smp; CHANNELS]>(8);
        let frames_in = Arc::new(AtomicU64::new(0));
        let ratio = Arc::new(AtomicU64::new((1.0f64 + 1000e-6).to_bits()));
        let cell = super::InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicU32::new(0)),
            Arc::clone(&frames_in),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&ratio),
        );
        cell.reconfigure(crate::sample::ENGINE_SR, 1);
        let block = vec![0.25f32; 4800];
        for _ in 0..10 {
            cell.feed_rt(&block);
        }
        let emitted = frames_in.load(Ordering::Relaxed);
        let expected = (48_000.0 / (1.0 + 1000e-6)) as u64;
        assert!(
            (emitted as i64 - expected as i64).unsigned_abs() <= 2,
            "emitted {emitted}, expected ~{expected} (rate matched to output)"
        );
        // And the emitted audio is still the same content (a constant here).
        let mut n = 0;
        while let Some(f) = mic_cons.pop() {
            assert!((f[0] - 0.25).abs() < 1e-6);
            n += 1;
        }
        assert_eq!(n as u64, emitted);
    }
}

/// Background WAV writer: owns the recording ring consumer for the engine's lifetime and writes
/// frames to a file only while a recording is active. Never touches the audio thread. Mono (the
/// vocal mic's left channel) at `ENGINE_SR`.
pub struct RecordWriter {
    tx: mpsc::Sender<WriterCmd>,
    join: Option<JoinHandle<()>>,
}

enum WriterCmd {
    /// Start a new recording; the writer acks `true` once the file is created, `false` when
    /// creation failed (disk full / permissions) so the caller can fail loudly.
    Start(String, mpsc::Sender<bool>),
    /// Stop + finalize the WAV; the writer acks when the file is fully flushed. `true` = the
    /// take on disk is clean; `false` = a create/write/finalize error damaged (or lost) it.
    Stop(mpsc::Sender<bool>),
    Shutdown,
}

impl RecordWriter {
    pub fn spawn(rec: Consumer<[Smp; CHANNELS]>) -> RecordWriter {
        let (tx, rx) = mpsc::channel::<WriterCmd>();
        let join = std::thread::Builder::new()
            .name("vocara-rec-writer".to_string())
            .spawn(move || writer_run(rec, rx))
            .ok();
        RecordWriter { tx, join }
    }

    /// Start recording to `path`, blocking until the writer thread has created the file
    /// (file creation is not on the RT path). Returns `false` when the file could not be
    /// created, so the caller reports failure instead of silently losing the take.
    pub fn start(&self, path: &str) -> bool {
        let (ack, rx) = mpsc::channel();
        if self
            .tx
            .send(WriterCmd::Start(path.to_string(), ack))
            .is_err()
        {
            return false;
        }
        rx.recv().unwrap_or(false)
    }

    /// Stop recording and block until the WAV is fully written + finalized (so the caller can
    /// read the file immediately). Returns `false` when any IO error occurred since start —
    /// the take on disk is missing, damaged, or incomplete.
    pub fn stop(&self) -> bool {
        let (ack, rx) = mpsc::channel();
        if self.tx.send(WriterCmd::Stop(ack)).is_err() {
            return false;
        }
        rx.recv().unwrap_or(false)
    }
}

impl Drop for RecordWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(WriterCmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn writer_run(mut rec: Consumer<[Smp; CHANNELS]>, rx: mpsc::Receiver<WriterCmd>) {
    let mut wav: Option<crate::wav::WavStream> = None;
    // Sticky IO-error flag for the current recording: set on any create/write/finalize
    // failure and reported through Stop's ack, so the caller learns the take is damaged
    // instead of the UI silently reporting success (disk full, revoked permission).
    let mut failed = false;
    loop {
        // Apply any pending commands.
        loop {
            match rx.try_recv() {
                Ok(WriterCmd::Start(path, ack)) => {
                    // A Start while already recording finalizes the previous take first,
                    // draining its buffered tail into it (mirroring Stop) so those frames
                    // land in the take they belong to. IO errors on that drain/finalize are
                    // carried into `failed` (matching Stop) rather than swallowed.
                    let mut old_take_damaged = false;
                    if let Some(mut w) = wav.take() {
                        while let Some(f) = rec.pop() {
                            if w.write_sample(f[0]).is_err() {
                                old_take_damaged = true;
                            }
                        }
                        if w.finalize().is_err() {
                            old_take_damaged = true;
                        }
                    }
                    // Discard any stale frames still in the ring before creating the file:
                    // the RT callback samples `rec_active` once per buffer, so a tail pushed
                    // after the previous Stop's drain (or between drops of `rec_active`)
                    // would otherwise be PREPENDED to the new take's WAV.
                    while rec.pop().is_some() {}
                    match crate::wav::WavStream::create(&path, 1, ENGINE_SR as u32) {
                        Ok(w) => {
                            wav = Some(w);
                            // Carry old-take damage forward instead of resetting: the
                            // restart path has no ack channel of its own, so the NEXT Stop
                            // reports it (a surfaced-if-misattributed failure beats silent
                            // corruption; the normal stop-then-start flow never reaches
                            // here with a live take).
                            failed = old_take_damaged;
                            let _ = ack.send(true);
                        }
                        Err(_) => {
                            wav = None;
                            failed = true;
                            let _ = ack.send(false);
                        }
                    }
                }
                Ok(WriterCmd::Stop(ack)) => {
                    // Drain any buffered tail into the file before finalizing.
                    if let Some(w) = wav.as_mut() {
                        while let Some(f) = rec.pop() {
                            if w.write_sample(f[0]).is_err() {
                                failed = true;
                            }
                        }
                    }
                    if let Some(w) = wav.take() {
                        if w.finalize().is_err() {
                            failed = true;
                        }
                    }
                    let _ = ack.send(!failed);
                    failed = false;
                }
                Ok(WriterCmd::Shutdown) => {
                    if let Some(w) = wav.take() {
                        let _ = w.finalize();
                    }
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        // Drain available frames; write the left channel (mono) when recording. A write
        // failure sets the sticky flag (reported at Stop) but keeps draining, so the ring
        // never backs up and any samples that still fit on disk are kept.
        let mut wrote = false;
        while let Some(f) = rec.pop() {
            if let Some(w) = wav.as_mut() {
                if w.write_sample(f[0]).is_err() {
                    failed = true;
                }
                wrote = true;
            }
        }
        if wrote {
            // Periodic in-place header patch (~1 s of audio cadence, writer thread only): a
            // TRUE hard kill (force quit / crash / kill -9) never reaches Stop/Shutdown, so
            // without this the WAV on disk would keep its zero-size placeholder header and
            // the take would be unreadable. With it the file parses at (nearly) every
            // instant — covering all samples up to the last patch — so the boot-time
            // staging sweep can RECOVER the take. Graceful paths still get the exact final
            // sizes from finalize. An IO error here is the same sticky failure as a failed
            // sample write (disk full), reported through Stop's ack.
            if let Some(w) = wav.as_mut() {
                if w.sync_header_if_due().is_err() {
                    failed = true;
                }
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// Build + start an input stream feeding `cell`. A stream error (disconnect) raises
/// [`EVENT_INPUT_LOST`]. Must be called on the [`AudioHost`] thread. (Non-Apple fallback; macOS
/// uses VoiceProcessingIO — see `vpio.rs`.)
#[cfg(not(target_vendor = "apple"))]
fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    cell: Arc<InputCell>,
    event: Arc<AtomicU32>,
) -> Result<DeviceHandle, String> {
    let err_cb = move |err| {
        let _ = err;
        event.fetch_or(EVENT_INPUT_LOST, Ordering::Release);
    };
    let stream = device
        .build_input_stream(
            *config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // SAFETY: exactly one active input callback dereferences the cell (see InputCell).
                unsafe { cell.feed(data) };
            },
            err_cb,
            None,
        )
        .map_err(|e| format!("build_input_stream: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("input stream.play: {e}"))?;
    Ok(DeviceHandle { _stream: stream })
}

/// Open an input stream for `uid` (or the default input when None), after reconfiguring `cell`
/// for the device's native rate + channels. Returns the stream handle plus the resolved UID.
/// (Non-Apple fallback; macOS uses VoiceProcessingIO.)
#[cfg(not(target_vendor = "apple"))]
fn build_input(
    uid: Option<&str>,
    cell: &Arc<InputCell>,
    event: &Arc<AtomicU32>,
) -> Result<(DeviceHandle, String), String> {
    let device = match uid {
        Some(u) => input_device_by_uid(u).ok_or_else(|| format!("input device {u} not found"))?,
        None => default_input_device().ok_or_else(|| "no default input device".to_string())?,
    };
    let (config, rate, channels) = input_config(&device)?;
    cell.reconfigure(rate, channels);
    let stream = build_input_stream(&device, &config, Arc::clone(cell), Arc::clone(event))?;
    let resolved = device_uid(&device).unwrap_or_default();
    Ok((stream, resolved))
}

/// A command sent to the [`AudioHost`] thread (the only thread allowed to touch `!Send` streams).
enum HostCmd {
    /// Switch the active output device to `uid`. Replies 0 on success, -1 on failure.
    SetOutput {
        uid: String,
        reply: mpsc::Sender<i32>,
    },
    /// Nudge: converge the input stream to the CURRENT desired-state cell (see
    /// `audio_host_run`'s `input_intent`). Carries no payload on purpose — the payload is
    /// whatever the cell says at CONVERGENCE time, which is what makes stale nudges
    /// harmless. Replies 0 (fire-and-forget on the FFI side).
    SyncInput { reply: mpsc::Sender<i32> },
    /// Set the input device UID (reopens the stream live if capture is active). Replies 0 / -1.
    SetInput {
        uid: String,
        reply: mpsc::Sender<i32>,
    },
    /// Stop the streams and exit the thread.
    Shutdown,
}

/// Owns the dedicated audio-host thread. The `!Send` cpal stream lives entirely on that thread;
/// FFI threads drive it only through messages. Dropping it stops audio and joins the thread.
pub struct AudioHost {
    tx: mpsc::Sender<HostCmd>,
    join: Option<JoinHandle<()>>,
}

impl AudioHost {
    /// Switch the active output device by UID. Returns 0 on success, negative on failure (the
    /// host falls back to the default device if the requested one can't be opened).
    pub fn set_output_device(&self, uid: &str) -> i32 {
        self.request(|reply| HostCmd::SetOutput {
            uid: uid.to_string(),
            reply,
        })
    }

    // The input nudge is FIRE-AND-FORGET: VoiceProcessingIO setup (create/initialize/start a
    // duplex AudioUnit) is slow, and it is called from the UI thread via FFI — blocking on the
    // host reply froze the UI (spinner). Readiness is proven by the per-take recorded-progress
    // ack plus the applied-sequence watermark, never by this return value.

    /// Ask the host to converge the input stream to the desired-state cell. Fire-and-forget.
    pub fn sync_input_nudge(&self) {
        self.send(|reply| HostCmd::SyncInput { reply });
    }

    /// Set the input device by UID (reopens live if capture is active). Fire-and-forget.
    pub fn set_input_device(&self, uid: &str) -> i32 {
        self.send(|reply| HostCmd::SetInput {
            uid: uid.to_string(),
            reply,
        });
        0
    }

    /// Send a command and block for its i32 reply (-2 if the host thread is gone).
    fn request(&self, make: impl FnOnce(mpsc::Sender<i32>) -> HostCmd) -> i32 {
        let (reply, rx) = mpsc::channel();
        if self.tx.send(make(reply)).is_err() {
            return -2;
        }
        rx.recv().unwrap_or(-2)
    }

    /// Send a command without waiting for the reply (the reply channel is dropped).
    fn send(&self, make: impl FnOnce(mpsc::Sender<i32>) -> HostCmd) {
        let (reply, _rx) = mpsc::channel();
        let _ = self.tx.send(make(reply));
    }
}

impl Drop for AudioHost {
    fn drop(&mut self) {
        let _ = self.tx.send(HostCmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The audio-host thread body: open the default output, report readiness, then serve device +
/// monitor commands. It owns both `!Send` streams. The `*_stream` bindings are held only for
/// their `Drop` (which stops the cpal callback), so their reassignments look "unread".
/// Whether VPIO runs DUPLEX (playback routed through element 0). Required: VPIO is a duplex unit
/// whose I/O cycle is driven by the output element, so input-only delivers ZERO frames — and the
/// duplex output is also the AEC reference that cancels the speaker from the mic. When true, the
/// host tears down the cpal output while VPIO runs and restores it when the mic stops.
#[cfg(target_vendor = "apple")]
const VPIO_DUPLEX_AEC: bool = true;
/// Non-Apple uses the cpal input path; there is no VPIO output swap.
#[cfg(not(target_vendor = "apple"))]
const VPIO_DUPLEX_AEC: bool = false;

/// The opaque mic-input handle: a VoiceProcessingIO unit on macOS, a cpal input stream elsewhere.
#[cfg(target_vendor = "apple")]
type MicHandle = crate::vpio::Vpio;
#[cfg(not(target_vendor = "apple"))]
type MicHandle = DeviceHandle;

/// Open the mic input on the host thread. macOS uses VoiceProcessingIO (captures + optional AEC,
/// and works where a second cpal input AUHAL was fed silence); other targets use cpal input.
#[cfg(target_vendor = "apple")]
fn open_mic(
    uid: Option<&str>,
    rt: &Arc<RtCell>,
    input_cell: &Arc<InputCell>,
    event: &Arc<AtomicU32>,
) -> Result<MicHandle, String> {
    // Safe default; `Vpio::start` re-reconfigures the input cell from VPIO's *negotiated* format
    // (mono/stereo, native rate) after the unit initializes, which is what actually governs feed.
    input_cell.reconfigure(ENGINE_SR, CHANNELS);
    // VPIO renders playback at 48 kHz with no boundary resampler: sync the
    // consumed cursor over anything a dying non-48 kHz cpal stream left in
    // its FIFO (see build_stream).
    rt.transport().sync_consumed_to_rendered();
    crate::vpio::Vpio::start(
        uid,
        Arc::clone(input_cell),
        Arc::clone(rt),
        VPIO_DUPLEX_AEC,
        Arc::clone(event),
    )
}
#[cfg(not(target_vendor = "apple"))]
fn open_mic(
    uid: Option<&str>,
    _rt: &Arc<RtCell>,
    input_cell: &Arc<InputCell>,
    event: &Arc<AtomicU32>,
) -> Result<MicHandle, String> {
    build_input(uid, input_cell, event).map(|(h, _)| h)
}

#[allow(unused_assignments)]
// One positional slot per shared atomic/collaborator the host thread owns for its whole
// life; a params struct would only restate this list.
#[allow(clippy::too_many_arguments)]
fn audio_host_run(
    rt: Arc<RtCell>,
    input_cell: Arc<InputCell>,
    event: Arc<AtomicU32>,
    renderer_gate: Arc<Mutex<bool>>,
    input_state: Arc<AtomicU64>,
    input_intent: Arc<AtomicU64>,
    input_applied: Arc<AtomicU64>,
    rx: mpsc::Receiver<HostCmd>,
    ready_tx: mpsc::Sender<Result<String, String>>,
) {
    // Input-stream state word: (generation << 1) | running. One atomic so a consumer can
    // SNAPSHOT "which stream, and is it up" in a single read: the ready handshake reads it,
    // verifies the frame counter advanced, then reads it AGAIN — an unchanged word with the
    // running bit set proves the SAME stream was up across the whole window (any teardown
    // clears the bit, any restart bumps the generation), so the observed frames cannot have
    // come from a dying predecessor. Two separate atomics could interleave exactly that way.
    let input_started = |st: &Arc<AtomicU64>| {
        let cur = st.load(Ordering::Acquire);
        st.store((((cur >> 1) + 1) << 1) | 1, Ordering::Release);
    };
    let input_stopped = |st: &Arc<AtomicU64>| {
        let cur = st.load(Ordering::Acquire);
        st.store(cur & !1, Ordering::Release);
    };
    // Renderer-activity gate shared with `EngineHandle::reap`: it must read `true` from
    // BEFORE any stream is built (a callback can fire the instant a build succeeds) until
    // AFTER every stream has been dropped (drop joins the callback). A `false` read on the
    // control thread then proves no RT callback can hold a retired graph pointer — which is
    // why a latched teardown failure (`effective_gate`) pins the gate raised: "dropped" no
    // longer implies "joined" then. A poisoned lock is impossible here (no panics while
    // held), but degrade to a no-op regardless.
    let set_gate = |v: bool| {
        let v = effective_gate(v);
        if let Ok(mut g) = renderer_gate.lock() {
            *g = v;
        }
    };
    set_gate(true);
    // Open the default output before serving commands; report success/failure.
    let mut out_stream = match build_output(None, &rt, &event) {
        Ok((handle, uid)) => {
            let _ = ready_tx.send(Ok(uid));
            Some(handle)
        }
        Err(e) => {
            set_gate(false);
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    // Input is opened lazily (only while monitoring/recording), so the mic indicator isn't held
    // when idle. `cur_in_uid` = None means the system default input.
    let mut in_stream: Option<MicHandle> = None;
    let mut cur_in_uid: Option<String> = None;
    // The user's last explicit output choice (None = system default). Tracked so it can be
    // restored when duplex VPIO releases the output, instead of always falling back to default.
    let mut cur_out_uid: Option<String> = None;

    while let Ok(cmd) = rx.recv() {
        // Raise the gate BEFORE any command that may build a stream, then settle it to the
        // real stream state after the command completes (Shutdown breaks out and settles to
        // false after the loop). Conservative: a raised gate only means retirees wait for
        // their grace epoch, exactly the pre-gate behavior.
        set_gate(true);
        match cmd {
            HostCmd::SetOutput { uid, reply } => {
                // While duplex VPIO owns the output (mic active), do NOT bring up a competing cpal
                // output — two renderers of RtCell would violate the single-accessor invariant.
                if VPIO_DUPLEX_AEC && in_stream.is_some() {
                    // Record the request so StopInput restores THIS device (not the system
                    // default) when the mic stops, and report the deferred success.
                    cur_out_uid = Some(uid);
                    let _ = reply.send(0);
                } else {
                    // Drop the old stream FIRST (joins its callback) so only one stream ever touches
                    // the shared RtCell, then build the new one. Fall back to default if the
                    // requested device can't be opened.
                    out_stream = None;
                    let built = build_output(Some(&uid), &rt, &event)
                        .or_else(|_| build_output(None, &rt, &event));
                    match built {
                        Ok((handle, _)) => {
                            out_stream = Some(handle);
                            cur_out_uid = Some(uid);
                            let _ = reply.send(0);
                        }
                        Err(_) => {
                            event.fetch_or(EVENT_OUTPUT_LOST, Ordering::Release);
                            let _ = reply.send(-1);
                        }
                    }
                }
            }
            HostCmd::SyncInput { reply } => {
                // LINEARIZED desired-state convergence: act on what the cell says NOW,
                // never on what the state was when this nudge was queued. A stale
                // teardown can no longer execute after the arm that superseded it —
                // "monitoring off, then REC" reads want=on here and keeps (or opens)
                // the stream; the old imperative Start/Stop commands executed history
                // instead. Idempotent, so any number of queued nudges converge to the
                // same state.
                let intent = input_intent.load(Ordering::Acquire);
                let want = intent & 1 == 1;
                // Whether this convergence actually REACHED the desired state — the
                // applied watermark below is published only then, so a consumer can
                // never read "applied >= my arm" off a convergence that FAILED (the
                // reviewed splice: old warm frames + a failed reopen still advanced
                // applied, and two true-but-unrelated conditions assembled a false
                // ready).
                let mut converged = true;
                if want && in_stream.is_none() {
                    // Duplex VPIO renders the engine through its own output element, so
                    // drop the cpal output FIRST (only one path may touch RtCell). Fall
                    // back to the default input if the selected device can't be opened.
                    if VPIO_DUPLEX_AEC {
                        out_stream = None;
                    }
                    let built = open_mic(cur_in_uid.as_deref(), &rt, &input_cell, &event)
                        .or_else(|_| open_mic(None, &rt, &input_cell, &event));
                    match built {
                        Ok(handle) => {
                            in_stream = Some(handle);
                            input_started(&input_state);
                        }
                        Err(_) => {
                            converged = false;
                            input_stopped(&input_state);
                            // Mic failed to come up → restore cpal output so playback
                            // survives. Restore the user's chosen output (not the system
                            // default); fall back to default only if it can't open.
                            if VPIO_DUPLEX_AEC && out_stream.is_none() {
                                out_stream = build_output(cur_out_uid.as_deref(), &rt, &event)
                                    .or_else(|_| build_output(None, &rt, &event))
                                    .ok()
                                    .map(|(h, _)| h);
                            }
                            event.fetch_or(EVENT_INPUT_LOST, Ordering::Release);
                        }
                    }
                } else if !want && in_stream.is_some() {
                    // Clear the running bit BEFORE the drop joins the dying callback: a
                    // consumer snapshotting the state word must never count the old
                    // stream's final frames as evidence that a NEW stream is delivering.
                    input_stopped(&input_state);
                    in_stream = None;
                    // VPIO was rendering the output in duplex; hand playback back to
                    // cpal, restoring the user's last chosen output device (a SetOutput
                    // during duplex deferred it), falling back to the system default
                    // only if that device can't be opened.
                    if VPIO_DUPLEX_AEC && out_stream.is_none() {
                        out_stream = build_output(cur_out_uid.as_deref(), &rt, &event)
                            .or_else(|_| build_output(None, &rt, &event))
                            .ok()
                            .map(|(h, _)| h);
                    }
                }
                // Publish AFTER converging, ONLY on success, and PAIRED with the input
                // generation the convergence left behind: (seq << 24) | generation. The
                // consumer requires the applied pair's generation to equal the CURRENT
                // state word's generation with the running bit set — all three ready
                // facts (progress, running, applied) are then anchored to one stream
                // instance, and evidence from two different generations can no longer
                // be spliced into a false ready. Generations are never reused, so the
                // 24-bit field wraps after ~16M stream starts — beyond any session.
                if converged {
                    let generation = input_state.load(Ordering::Acquire) >> 1;
                    input_applied.store(
                        ((intent >> 1) << 24) | (generation & 0xFF_FFFF),
                        Ordering::Release,
                    );
                }
                let _ = reply.send(0);
            }
            HostCmd::SetInput { uid, reply } => {
                cur_in_uid = Some(uid.clone());
                if in_stream.is_some() {
                    // Reopen on the new device (drop old first → single-accessor; cpal output is
                    // already off in duplex). Not-running for the whole swap window (see
                    // StopInput on why the store precedes the drop).
                    input_stopped(&input_state);
                    in_stream = None;
                    match open_mic(Some(&uid), &rt, &input_cell, &event)
                        .or_else(|_| open_mic(None, &rt, &input_cell, &event))
                    {
                        Ok(handle) => {
                            in_stream = Some(handle);
                            input_started(&input_state);
                            // The reopen minted a NEW stream generation while the applied
                            // pair still carried the old one, so a recorder arming right
                            // after a device switch waited on a convergence that had in
                            // fact already happened — until some unrelated input intent
                            // nudged the host. Re-attest the SAME applied seq under the
                            // new generation: the desired state (input running) is
                            // satisfied by the stream just opened, which is exactly what
                            // "applied" means. Failure republishes nothing — the state
                            // word is not running and EVENT_INPUT_LOST fires.
                            let seq = input_applied.load(Ordering::Acquire) >> 24;
                            let generation = input_state.load(Ordering::Acquire) >> 1;
                            input_applied
                                .store((seq << 24) | (generation & 0xFF_FFFF), Ordering::Release);
                            let _ = reply.send(0);
                        }
                        Err(_) => {
                            if VPIO_DUPLEX_AEC && out_stream.is_none() {
                                // Restore the user's chosen output (not the system default),
                                // matching StopInput; fall back to default only if it can't open.
                                out_stream = build_output(cur_out_uid.as_deref(), &rt, &event)
                                    .or_else(|_| build_output(None, &rt, &event))
                                    .ok()
                                    .map(|(h, _)| h);
                            }
                            event.fetch_or(EVENT_INPUT_LOST, Ordering::Release);
                            let _ = reply.send(-1);
                        }
                    }
                } else {
                    let _ = reply.send(0);
                }
            }
            HostCmd::Shutdown => {
                input_stopped(&input_state);
                in_stream = None;
                out_stream = None;
                break;
            }
        }
        set_gate(out_stream.is_some() || in_stream.is_some());
    }
    // Both streams (and this thread's `rt`/`input_cell` clones) drop here, on the host thread.
    drop(in_stream);
    drop(out_stream);
    set_gate(false);
}

/// Spawn the audio-host thread, open the default output, and start rendering `rt`. Blocks until
/// the initial stream is up (or fails). Returns the host handle plus the opened device's UID.
/// `renderer_gate` (from [`crate::live::EngineHandle::renderer_gate`]) tracks whether any stream
/// callback may be running, so the control thread can free retired graphs when none is.
pub fn spawn_audio_host(
    rt: Arc<RtCell>,
    input_cell: Arc<InputCell>,
    event: Arc<AtomicU32>,
    renderer_gate: Arc<Mutex<bool>>,
    input_state: Arc<AtomicU64>,
    input_intent: Arc<AtomicU64>,
    input_applied: Arc<AtomicU64>,
) -> Result<(AudioHost, String), String> {
    let (tx, rx) = mpsc::channel::<HostCmd>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
    let join = std::thread::Builder::new()
        .name("vocara-audio-host".to_string())
        .spawn(move || {
            audio_host_run(
                rt,
                input_cell,
                event,
                renderer_gate,
                input_state,
                input_intent,
                input_applied,
                rx,
                ready_tx,
            )
        })
        .map_err(|e| format!("spawn audio host: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(uid)) => Ok((
            AudioHost {
                tx,
                join: Some(join),
            },
            uid,
        )),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(e) => Err(format!("audio host startup: {e}")),
    }
}

#[cfg(test)]
thread_local! {
    /// Counts [`decode_file`] entries on THIS thread (tests only — see the note at the
    /// function's top). Thread-local so parallel tests cannot pollute each other's deltas:
    /// every counter-sensitive test decodes on its own test thread.
    pub static DECODE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Probe `path`'s DECLARED duration in milliseconds without decoding a single packet:
/// container probe only, so an import gate can refuse an hours-long file before any copy or
/// allocation happens. `Ok(None)` = the container declares no frame count (e.g. a plain mp3
/// without a Xing header) — the caller falls back to the decode-time budget, which bails
/// mid-decode with peak memory bounded by the budget.
pub fn probe_duration_ms(path: &str) -> Result<Option<u64>, String> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let track = probed
        .format
        .default_track()
        .ok_or_else(|| "no default track".to_string())?;
    let rate = track.codec_params.sample_rate.unwrap_or(0) as u64;
    Ok(match (track.codec_params.n_frames, rate) {
        // Ceiling division: a gate comparing `declared > cap` must see a file that is even
        // ONE sample past the cap as strictly longer — flooring would round it down onto
        // the boundary and admit it.
        (Some(n), r) if r > 0 => Some(n.saturating_mul(1000).div_ceil(r)),
        _ => None,
    })
}

/// Why [`decode_file`] refused a file. The variants are DIAGNOSTIC taxonomy,
/// not decoration: every refusal used to collapse into "too long", so a
/// 30-second 384 kHz file was reported as over-duration and a mid-file disk
/// error as a clean EOF.
#[derive(Debug)]
pub enum DecodeError {
    /// The audio exceeds the caller's DURATION budget. Raised BEFORE decoding
    /// when the container declares its frame count, or the moment the budget
    /// is crossed mid-decode — either way peak memory stays bounded by the
    /// budget, never by the file.
    TooLong,
    /// The audio exceeds a caller's MEMORY budget (source-rate ceiling or
    /// absolute source-frame cap) without being over-duration — a short but
    /// extremely dense file. Distinct from [`DecodeError::TooLong`] so the UI
    /// can say what is actually wrong.
    TooBig,
    /// The source sample rate sits outside the supported envelope
    /// ([`MIN_SRC_RATE`], [`MAX_SRC_RATE`]) — no real recording, refused
    /// before symphonia parses anything.
    RateUnsupported,
    /// The FILE could not be read (open failure, or a read error mid-decode
    /// that is not a normal end of stream). Distinct from a malformed
    /// container: the bytes may be fine and the disk is not.
    Io(String),
    /// Any other probe/decode failure (unsupported or corrupt container).
    Other(String),
}

/// Decode a file (wav/flac/mp3/aac/m4a/ogg) to interleaved-stereo f32 at its source rate.
/// Mono is duplicated; >2 channels take the first two. `max_secs` (0.0 = unbounded) caps the
/// decoded duration: the full track lives in memory here, so an arbitrary-length input must
/// be refused up front rather than after gigabytes of Vec growth.
/// `max_src_rate` (0 = unbounded) refuses files whose SOURCE sample rate exceeds it, and
/// `max_src_frames` (0 = unbounded) is the ABSOLUTE memory budget in source-rate frames:
/// the duration budget alone never bounded decode memory (a legal 10-minute 384 kHz file
/// decoded ~1.8 GB at its source rate before any resample), and a RATE ceiling alone is
/// not a byte budget either — duration x rate still scales to gigabytes inside both
/// limits. The frame budget is the bound the allocation actually obeys: pre-sizing,
/// mid-decode bail, and the declared-length refusal all clamp against
/// min(duration-derived, absolute).
pub fn decode_file(
    path: &str,
    max_secs: f64,
    max_src_rate: u32,
    max_src_frames: u64,
) -> Result<(Vec<[Smp; CHANNELS]>, u32), DecodeError> {
    // Test-only observability: lets a test PROVE a refusal (or a skip) happened before /
    // without a decode — outcome-only assertions cannot tell "refused up front" from
    // "decoded everything, then refused".
    #[cfg(test)]
    DECODE_CALLS.with(|c| c.set(c.get() + 1));
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let err = |m: String| DecodeError::Other(m);
    let file =
        std::fs::File::open(path).map_err(|e| DecodeError::Io(format!("open {path}: {e}")))?;

    // WAV pre-sniff BEFORE symphonia sees the bytes: a zero sample rate makes symphonia's
    // TimeBase constructor panic inside the probe itself, so the envelope must be judged
    // from our own header parse for the one format an attacker can trivially craft. (The
    // release profile unwinds and every FFI entry catches, so a panic in some other
    // container's probe is still an error, not an abort — this makes the common case a
    // clean deterministic refusal instead of an unwind.)
    if let Ok(Some(rate)) = sniff_wav_sample_rate(path) {
        if !(MIN_SRC_RATE..=MAX_SRC_RATE).contains(&rate) {
            return Err(DecodeError::RateUnsupported);
        }
    }
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| err(format!("probe: {e}")))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| err("no default track".to_string()))?;
    let track_id = track.id;
    let src_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    // Absolute source-rate envelope, independent of any caller ceiling. The FLOOR is a
    // memory/CPU bound, not taste: resampling a 1 Hz source to 48 kHz is a 48000x ratio,
    // which makes the sinc resampler plan ~49M output frames per 1024-frame input chunk —
    // a decoder-driven allocation/CPU DoS from a tiny file. No real recording sits
    // outside [8 kHz, 384 kHz]; the same envelope the Dart analysis gate enforces.
    if !(MIN_SRC_RATE..=MAX_SRC_RATE).contains(&src_rate) {
        return Err(DecodeError::RateUnsupported);
    }
    // A caller's source-rate ceiling is a MEMORY budget (per-second decode
    // cost), not a duration bound: a short 384 kHz file is refused as too
    // BIG, not "too long" — the old conflation misdiagnosed every dense file.
    if max_src_rate > 0 && src_rate > max_src_rate {
        return Err(DecodeError::TooBig);
    }
    // The effective frame budget: the duration-derived bound AND the caller's absolute
    // memory budget, whichever is tighter (u64::MAX = unbounded).
    let duration_frames: u64 = if max_secs > 0.0 {
        (max_secs * src_rate as f64).ceil() as u64
    } else {
        u64::MAX
    };
    let absolute = if max_src_frames > 0 {
        max_src_frames
    } else {
        u64::MAX
    };
    let max_frames = duration_frames.min(absolute);
    // Which budget actually tripped decides the DIAGNOSIS: over the duration
    // bound is "too long"; under it but over the absolute source-frame cap is
    // "too big" (a short, dense file the memory budget refuses).
    let budget_err = |frames: u64| -> DecodeError {
        if frames > duration_frames {
            DecodeError::TooLong
        } else {
            DecodeError::TooBig
        }
    };
    // Refuse a declared-too-long track BEFORE decoding a single packet. When the declared
    // length fits, pre-size the Vec so packet pushes never realloc-copy the whole track
    // (the count is clamped by the budget first — a lying header must not drive a huge
    // allocation on its own).
    let mut out: Vec<[Smp; CHANNELS]> = match track.codec_params.n_frames {
        Some(n) if n > max_frames => return Err(budget_err(n)),
        Some(n) => Vec::with_capacity(n.min(max_frames) as usize),
        None => Vec::new(),
    };
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| err(format!("make decoder: {e}")))?;

    // Iterate packets until end of stream. Error taxonomy matters here (#19):
    // a normal EOF ends the loop cleanly, a corrupt PACKET is skipped (the
    // standard symphonia discipline — one damaged frame must not kill a
    // recovering take), but a mid-file READ error is a failing disk, not an
    // end of stream — returning the partial audio as a clean success would
    // silently truncate the user's media.
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break; // normal end of stream
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(symphonia::core::errors::Error::IoError(e)) => {
                return Err(DecodeError::Io(format!("read {path}: {e}")));
            }
            Err(e) => return Err(err(format!("packet: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue, // one corrupt frame is skipped, not fatal
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        let mut sbuf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sbuf.copy_interleaved_ref(decoded);
        let packet_frames = sbuf.samples().len() / ch.max(1);
        // The budget is an ALLOCATION cap, not just a logical-length cap: judged BEFORE
        // this packet's frames are pushed, and the Vec's growth is clamped to the budget
        // (a plain doubling push could hold up to 2x the cap in capacity — a 173M-frame
        // budget transiently backed by a 268M-frame allocation). Containers without a
        // declared frame count (e.g. plain mp3) are bounded exactly the same way,
        // packet by packet.
        let needed = out.len() + packet_frames;
        if needed as u64 > max_frames {
            return Err(budget_err(needed as u64));
        }
        if needed > out.capacity() {
            // Amortized geometric growth, capped at the budget so capacity can never
            // exceed what the budget promised the caller.
            let cap_limit = max_frames.min(usize::MAX as u64) as usize;
            let target = out
                .capacity()
                .max(4096)
                .saturating_mul(2)
                .clamp(needed, cap_limit);
            out.reserve_exact(target - out.len());
        }
        for frame in sbuf.samples().chunks(ch.max(1)) {
            // Scrub NaN/Inf (and clamp absurd amplitudes) at the single deepest decode
            // point: one poisoned sample would otherwise propagate through every FFT,
            // metric, and DSP node fed from here (the WAV streaming reader scrubs
            // independently; this covers the symphonia paths — analysis via ve_decode
            // AND playback staging).
            let l = frame[0];
            let r = if ch > 1 { frame[1] } else { l };
            out.push([scrub_sample(l), scrub_sample(r)]);
        }
    }
    if out.is_empty() {
        return Err(err(format!("decoded no audio from {path}")));
    }
    Ok((out, src_rate))
}

/// Absolute source-sample-rate envelope for every decoded file, mirroring the Dart analysis
/// gate. Outside it nothing is a real recording, and the extremes are attacks: 0 panics
/// symphonia's TimeBase, 1 Hz turns the sinc resampler into an allocation/CPU bomb, and
/// beyond 384 kHz the per-second memory cost escapes every budget derivation.
pub const MIN_SRC_RATE: u32 = 8_000;
pub const MAX_SRC_RATE: u32 = 384_000;

/// One decoded sample, made safe for every consumer downstream: NaN/Inf scrubbed to 0 and
/// the amplitude clamped to ±16 (24 dB over full scale — beyond any real mastering
/// headroom). The clamp is not cosmetic: finite-but-absurd samples (a hostile float WAV of
/// `f32::MAX`) overflow the sinc resampler's convolution into NaN/Inf AFTER the decode-root
/// scrub, and would otherwise reach playback and analysis as poison.
#[inline]
fn scrub_sample(v: Smp) -> Smp {
    if v.is_finite() {
        v.clamp(-16.0, 16.0)
    } else {
        0.0
    }
}

/// Reads just enough of a RIFF/WAVE header to report its declared sample rate; `Ok(None)`
/// when the file is not a WAV (other containers go straight to symphonia). Never reads
/// more than the chunk walk needs.
fn sniff_wav_sample_rate(path: &str) -> std::io::Result<Option<u32>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 12];
    if f.read(&mut head)? < 12 || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Ok(None);
    }
    // Walk chunks looking for `fmt `; bail after a bounded number so a hostile chunk list
    // cannot turn the sniff into a long scan.
    let mut hdr = [0u8; 8];
    for _ in 0..64 {
        if f.read(&mut hdr)? < 8 {
            return Ok(None);
        }
        let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
        if &hdr[0..4] == b"fmt " {
            let mut fmt = [0u8; 8];
            if size < 8 || f.read(&mut fmt)? < 8 {
                return Ok(None);
            }
            return Ok(Some(u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]])));
        }
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Current((size + (size & 1)) as i64))?;
    }
    Ok(None)
}

/// Windowed-sinc sample-rate conversion (interleaved stereo) on the offline decode paths —
/// the production kernel the linear-interpolation interim explicitly promised (linear leaves
/// audible aliasing above a few kHz on non-48 kHz sources). Consumes its input so the
/// identity case moves the buffer through instead of duplicating a whole track (the decode
/// paths' peak memory is measured in full-track copies; when actually resampling the peak
/// is input + output, same as before).
///
/// Alignment: the chunk loop below drives `process_into_buffer` DIRECTLY and cuts the
/// window `[delay, delay + round(n * ratio))` out of the produced stream, flushing zeros
/// until the tail is fully rendered. rubato's own `process_all` convenience was measured
/// mishandling exactly the edges that matter here: a frame-0 impulse appeared at BOTH
/// output 0 and ~69 (the group delay), and a sub-chunk clip came out shifted by the whole
/// delay — the punch-splice paths depend on `k -> round(k * ratio)` holding at the edges,
/// not just in the interior (all pinned by tests). The remaining sub-sample fraction of
/// the group delay is a pure, inaudible time shift.
///
/// Errors are structurally impossible for the validated rates the decode gates admit
/// (ratio > 0, stereo, non-zero chunk); the Result keeps this library-safe rather than
/// panicking should that ever change.
pub fn resample_sinc(
    frames: Vec<[Smp; CHANNELS]>,
    from: u32,
    to: u32,
) -> Result<Vec<[Smp; CHANNELS]>, String> {
    use rubato::audioadapter_buffers::direct::InterleavedSlice;
    use rubato::{
        Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType,
        WindowFunction,
    };

    if from == to || frames.is_empty() {
        return Ok(frames);
    }
    let ratio = to as f64 / from as f64;
    let n = frames.len();
    // rubato's reference offline-quality parameters (SincFixedInput in its examples):
    // 128-tap sinc, 256x oversampled table, quadratic table interpolation, Blackman2
    // window, cutoff derived from the window/taps by the library.
    let params = SincInterpolationParameters::new(128, WindowFunction::Blackman2)
        .oversampling_factor(256)
        .interpolation(SincInterpolationType::Quadratic);
    let mut rs = Async::<Smp>::new_sinc(ratio, 1.0, &params, 1024, CHANNELS, FixedAsync::Input)
        .map_err(|e| format!("resampler construction: {e}"))?;

    let delay = rs.output_delay();
    let exact = ((n as f64) * ratio).round() as usize;
    let needed = delay + exact;
    let out_max = rs.output_frames_max();
    // Room for every produced frame including the final overshooting chunk; the valid
    // window is cut out below.
    let mut outbuf = vec![0.0f32; (needed + out_max) * CHANNELS];

    // [[f32; 2]] is contiguous interleaved stereo — the adapter borrows it in place.
    let input = InterleavedSlice::new(frames.as_flattened(), CHANNELS, n)
        .map_err(|e| format!("resample input adapter: {e}"))?;
    let mut indexing = Indexing {
        input_offset: 0,
        output_offset: 0,
        partial_len: None,
        active_channels_mask: None,
    };
    let mut consumed = 0usize;
    let mut produced = 0usize;
    while consumed < n {
        let next = rs.input_frames_next();
        let remaining = n - consumed;
        indexing.input_offset = consumed;
        indexing.output_offset = produced;
        indexing.partial_len = (remaining < next).then_some(remaining);
        let mut sink = InterleavedSlice::new_mut(&mut outbuf, CHANNELS, needed + out_max)
            .map_err(|e| format!("resample output adapter: {e}"))?;
        let (n_in, n_out) = rs
            .process_into_buffer(&input, &mut sink, Some(&indexing))
            .map_err(|e| format!("resample: {e}"))?;
        consumed += n_in.max(1); // a stalled converter must not loop forever
        produced += n_out;
    }
    let _ = input; // end the borrow of `frames` so the input copy can die here
    drop(frames);
    // Flush with silence until the delayed tail of the real audio has fully emerged.
    indexing.partial_len = Some(0);
    indexing.input_offset = 0;
    let empty_in: [Smp; 0] = [];
    let empty = InterleavedSlice::new(&empty_in[..], CHANNELS, 0)
        .map_err(|e| format!("resample flush adapter: {e}"))?;
    while produced < needed {
        indexing.output_offset = produced;
        let mut sink = InterleavedSlice::new_mut(&mut outbuf, CHANNELS, needed + out_max)
            .map_err(|e| format!("resample output adapter: {e}"))?;
        let (_, n_out) = rs
            .process_into_buffer(&empty, &mut sink, Some(&indexing))
            .map_err(|e| format!("resample flush: {e}"))?;
        if n_out == 0 {
            return Err("resample flush made no progress".to_string());
        }
        produced += n_out;
    }

    // The valid output is exactly [delay, delay + exact): group delay off the head, the
    // deterministic round(n * ratio) duration contract on the length.
    let start = delay * CHANNELS;
    let mut framed = Vec::with_capacity(exact);
    for chunk in outbuf[start..start + exact * CHANNELS].chunks_exact(CHANNELS) {
        // Second scrub line: the convolution of large-but-finite inputs can overflow to
        // NaN/Inf even though the decode root sanitized its samples (and callers may feed
        // this converter PCM that never went through decode_file at all).
        framed.push([scrub_sample(chunk[0]), scrub_sample(chunk[1])]);
    }
    Ok(framed)
}

#[cfg(test)]
mod decode_tests {
    //! Playback compatibility of the take formats: the engine must decode BOTH the current
    //! 32-bit float recordings and legacy 16-bit takes (symphonia wav + pcm codecs).

    fn temp_wav(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vocara_decode_{}_{}.wav", std::process::id(), name))
    }

    #[test]
    fn float32_wav_take_decodes_for_playback() {
        let path = temp_wav("f32");
        // Interleaved stereo, including an over-unity peak (allowed in float takes).
        let samples: Vec<f32> = vec![0.5, 0.5, -0.25, -0.25, 1.5, 1.5, 0.0, 0.0];
        crate::wav::write_wav_f32(&path, &samples, 2, 48_000).unwrap();
        let (frames, rate) =
            super::decode_file(path.to_str().unwrap(), 0.0, 0, 0).expect("f32 wav decodes");
        assert_eq!(rate, 48_000);
        assert_eq!(frames.len(), 4);
        assert!((frames[0][0] - 0.5).abs() < 1e-6);
        assert!((frames[1][0] + 0.25).abs() < 1e-6);
        assert!(
            (frames[2][0] - 1.5).abs() < 1e-6,
            "over-unity float samples must decode verbatim, got {}",
            frames[2][0]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_pcm16_wav_take_still_decodes() {
        let path = temp_wav("i16");
        let samples: Vec<f32> = vec![0.5, 0.5, -0.25, -0.25];
        crate::wav::write_wav_i16(&path, &samples, 2, 48_000).unwrap();
        let (frames, rate) =
            super::decode_file(path.to_str().unwrap(), 0.0, 0, 0).expect("i16 wav decodes");
        assert_eq!(rate, 48_000);
        assert_eq!(frames.len(), 2);
        assert!((frames[0][0] - 0.5).abs() < 1e-3);
        assert!((frames[1][0] + 0.25).abs() < 1e-3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn declared_too_long_file_is_refused_before_decoding() {
        // 100 frames @ 48k declared in the WAV header; a 1 ms budget (48 frames) must refuse
        // it up front with the distinct TooLong error, never with a generic failure.
        let path = temp_wav("too_long");
        let samples: Vec<f32> = vec![0.1; 200]; // 100 stereo frames
        crate::wav::write_wav_f32(&path, &samples, 2, 48_000).unwrap();
        match super::decode_file(path.to_str().unwrap(), 0.001, 0, 0) {
            Err(super::DecodeError::TooLong) => {}
            other => panic!("expected TooLong, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    fn f32_wav_at_rate(path: &std::path::Path, rate: u32, samples: &[f32]) {
        let mut bytes = Vec::new();
        let data_len = (samples.len() * 4) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        bytes.extend_from_slice(&2u16.to_le_bytes()); // stereo
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate.wrapping_mul(8)).to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes()); // block align
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for v in samples {
            bytes.extend_from_slice(&v.to_le_bytes()); // raw bits intact
        }
        std::fs::write(path, &bytes).unwrap();
    }

    #[test]
    fn the_source_rate_envelope_refuses_the_panic_and_dos_extremes() {
        // rate 0 used to PANIC inside symphonia's TimeBase during the probe — with the
        // release profile aborting, that was a process kill from a crafted file. The
        // pre-probe sniff must turn it into a plain decode error.
        let z = temp_wav("rate0");
        f32_wav_at_rate(&z, 0, &[0.1; 8]);
        assert!(matches!(
            super::decode_file(z.to_str().unwrap(), 0.0, 0, 0),
            Err(super::DecodeError::RateUnsupported)
        ));

        // 1 Hz is the resampler DoS: ratio 48000x plans ~49M output frames per input
        // chunk. Refused at the envelope floor, never reaching decode or resample.
        let one = temp_wav("rate1");
        f32_wav_at_rate(&one, 1, &[0.1; 8]);
        assert!(super::decode_file(one.to_str().unwrap(), 0.0, 0, 0).is_err());

        // Just below the floor refused; the floor itself admitted.
        let low = temp_wav("rate4k");
        f32_wav_at_rate(&low, 4_000, &[0.1; 8]);
        assert!(super::decode_file(low.to_str().unwrap(), 0.0, 0, 0).is_err());
        let floor = temp_wav("rate8k");
        f32_wav_at_rate(&floor, 8_000, &[0.1; 8]);
        let (frames, rate) = super::decode_file(floor.to_str().unwrap(), 0.0, 0, 0).unwrap();
        assert_eq!((frames.len(), rate), (4, 8_000));

        for p in [z, one, low, floor] {
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn absurd_finite_amplitudes_are_clamped_at_the_decode_root() {
        // f32::MAX samples are finite, so the NaN scrub alone waved them through — and
        // the sinc resampler's convolution then overflowed them into NaN/Inf (measured:
        // 100% of output frames). The root clamp bounds every decoded sample to ±16
        // (24 dB over full scale), which no convolution with unit-gain kernels can
        // overflow.
        let p = temp_wav("huge");
        f32_wav_at_rate(
            &p,
            44_100,
            &[f32::MAX, f32::MAX, -f32::MAX, -f32::MAX, 0.5, 0.5],
        );
        let (frames, rate) = super::decode_file(p.to_str().unwrap(), 0.0, 0, 0).unwrap();
        assert!(frames
            .iter()
            .all(|f| f[0].abs() <= 16.0 && f[1].abs() <= 16.0));
        assert_eq!(frames[2], [0.5, 0.5], "sane samples pass through unclamped");

        let out = super::resample_sinc(frames, rate, 48_000).unwrap();
        assert!(
            out.iter().all(|f| f[0].is_finite() && f[1].is_finite()),
            "clamped input must stay finite through the sinc kernel"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn nonfinite_samples_are_scrubbed_at_the_decode_root() {
        // A poisoned float sample must never leave decode_file: this is the single
        // deepest point covering BOTH the analysis (ve_decode) and playback staging
        // paths — one NaN would otherwise propagate through every FFT/metric/DSP node.
        //
        // The fixture is HAND-ASSEMBLED bytes: write_wav_f32 scrubs non-finite values
        // on WRITE, so a writer-produced fixture contains no poison and the earlier
        // version of this test passed with the decoder scrub deleted (review finding).
        let path = temp_wav("nan");
        let samples: [f32; 8] = [
            0.5,
            0.5,
            f32::NAN,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -0.25,
            -0.25,
        ];
        let mut bytes = Vec::new();
        let data_len = (samples.len() * 4) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        bytes.extend_from_slice(&2u16.to_le_bytes()); // stereo
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes()); // block align
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for v in samples {
            bytes.extend_from_slice(&v.to_le_bytes()); // raw bits, NaN intact
        }
        std::fs::write(&path, &bytes).unwrap();
        // Prove the fixture really carries the poison the writer would have scrubbed.
        let raw = std::fs::read(&path).unwrap();
        let third = f32::from_le_bytes(raw[44 + 8..44 + 12].try_into().unwrap());
        assert!(third.is_nan(), "fixture must contain a REAL NaN on disk");

        let (frames, _) =
            super::decode_file(path.to_str().unwrap(), 0.0, 0, 0).expect("poisoned wav decodes");
        assert_eq!(frames.len(), 4);
        assert_eq!(frames[0][0], 0.5);
        assert_eq!(frames[1], [0.0, 0.0], "NaN scrubbed to zero");
        assert_eq!(frames[2], [0.0, 0.0], "Inf scrubbed to zero");
        assert_eq!(frames[3][0], -0.25);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_absolute_source_frame_budget_binds_regardless_of_rate() {
        // 100 frames within every duration/rate limit — but over an absolute budget of
        // 50 source frames. The budget must refuse from the DECLARED length, before
        // decoding (one counter entry, no packets).
        let path = temp_wav("budget");
        let samples: Vec<f32> = vec![0.1; 200];
        crate::wav::write_wav_f32(&path, &samples, 2, 48_000).unwrap();
        let before = super::DECODE_CALLS.with(|c| c.get());
        match super::decode_file(path.to_str().unwrap(), 10.0, 0, 50) {
            // A memory-budget refusal, NOT over-duration: the taxonomy must
            // say what is actually wrong (the old conflation reported it as
            // too long).
            Err(super::DecodeError::TooBig) => {}
            other => panic!("expected the absolute-budget refusal, got {other:?}"),
        }
        assert_eq!(super::DECODE_CALLS.with(|c| c.get()), before + 1);
        let (frames, _) = super::decode_file(path.to_str().unwrap(), 10.0, 0, 100)
            .expect("within-budget decodes");
        assert_eq!(frames.len(), 100);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_source_rate_above_the_callers_ceiling_is_refused_before_decoding() {
        // The duration budget alone does not bound decode memory: a legal-duration
        // 96 kHz file decodes twice the samples of the 48 kHz case at its SOURCE rate.
        let path = temp_wav("hi_rate");
        let samples: Vec<f32> = vec![0.1; 200];
        crate::wav::write_wav_f32(&path, &samples, 2, 96_000).unwrap();
        let before = super::DECODE_CALLS.with(|c| c.get());
        match super::decode_file(path.to_str().unwrap(), 10.0, 48_000, 0) {
            // The rate ceiling is a memory budget: refused as too BIG.
            Err(super::DecodeError::TooBig) => {}
            other => panic!("expected the source-rate refusal, got {other:?}"),
        }
        assert_eq!(
            super::DECODE_CALLS.with(|c| c.get()),
            before + 1,
            "one ENTRY, but the refusal happens before any packet decodes"
        );
        // An adequate ceiling admits the same file.
        let (frames, rate) = super::decode_file(path.to_str().unwrap(), 10.0, 96_000, 0)
            .expect("within-ceiling source rate decodes");
        assert_eq!(rate, 96_000);
        assert_eq!(frames.len(), 100);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn probe_duration_rounds_up_so_a_boundary_file_reads_as_strictly_longer() {
        // 100 frames @ 48 kHz = 2.083… ms: flooring to 2 would let a "one sample past the
        // cap" file land exactly ON a millisecond cap and slip through a `>` gate.
        let path = temp_wav("probe_ceil");
        let samples: Vec<f32> = vec![0.1; 200]; // 100 stereo frames
        crate::wav::write_wav_f32(&path, &samples, 2, 48_000).unwrap();
        let ms = super::probe_duration_ms(path.to_str().unwrap())
            .expect("probe succeeds")
            .expect("wav declares its length");
        assert_eq!(ms, 3, "ceil(100 * 1000 / 48000) = 3");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_within_the_budget_decodes_normally() {
        let path = temp_wav("under_cap");
        let samples: Vec<f32> = vec![0.1; 200]; // 100 stereo frames ≈ 2.08 ms @ 48k
        crate::wav::write_wav_f32(&path, &samples, 2, 48_000).unwrap();
        let (frames, rate) = super::decode_file(path.to_str().unwrap(), 1.0, 0, 0)
            .expect("under-budget file decodes");
        assert_eq!(rate, 48_000);
        assert_eq!(frames.len(), 100);
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod record_writer_tests {
    //! The recording writer's IO-failure reporting: `start` must ack file creation and
    //! `stop` must report a clean vs. damaged/missing take (no device needed — the writer
    //! only consumes a ring).

    use super::RecordWriter;
    use crate::ring::spsc;
    use crate::sample::{Smp, CHANNELS};

    fn temp_wav(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "vocara_recwriter_{}_{}.wav",
            std::process::id(),
            name
        ))
    }

    #[test]
    fn start_fails_when_file_cannot_be_created() {
        let (_prod, cons) = spsc::<[Smp; CHANNELS]>(64);
        let w = RecordWriter::spawn(cons);
        // A path under a directory that does not exist cannot be created.
        assert!(
            !w.start("/nonexistent-vocara-dir/take.wav"),
            "start must report a failed file creation"
        );
        // The failed start left no take on disk; stop must report the loss, not success.
        assert!(
            !w.stop(),
            "stop after a failed start must not report a clean take"
        );
    }

    #[test]
    fn clean_recording_writes_all_frames_and_reports_success() {
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(1024);
        let w = RecordWriter::spawn(cons);
        let path = temp_wav("clean");
        assert!(
            w.start(path.to_str().unwrap()),
            "start must ack a successful create"
        );
        for i in 0..100 {
            assert!(prod.push([i as f32 / 200.0, 0.0]));
        }
        // Stop drains the ring tail before finalizing, so all 100 frames must land on disk.
        assert!(w.stop(), "a clean recording must report success");
        let bytes = std::fs::read(&path).expect("recorded WAV must exist");
        // 44-byte header + 100 mono float32 samples, with both size fields patched.
        assert_eq!(bytes.len(), 44 + 400);
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            36 + 400
        );
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 400);
        // Float takes store the L channel verbatim (no quantization).
        let first = f32::from_le_bytes(bytes[44..48].try_into().unwrap());
        assert_eq!(first, 0.0 / 200.0);
        let last = f32::from_le_bytes(bytes[44 + 99 * 4..44 + 100 * 4].try_into().unwrap());
        assert_eq!(last, 99.0 / 200.0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failure_flag_resets_for_the_next_recording() {
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(64);
        let w = RecordWriter::spawn(cons);
        assert!(!w.start("/nonexistent-vocara-dir/take.wav"));
        assert!(!w.stop());
        // The sticky error from the lost take must not bleed into a new, healthy one.
        let path = temp_wav("after_failure");
        assert!(w.start(path.to_str().unwrap()));
        assert!(prod.push([0.5, 0.5]));
        assert!(
            w.stop(),
            "a later clean recording must report success again"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn start_discards_stale_ring_frames_from_the_previous_take() {
        // The RT callback samples `rec_active` once per buffer, so a tail of the PREVIOUS
        // take can land in the ring after Stop's drain. Drive `writer_run` directly so the
        // stale frames are deterministically still in the ring when Start is processed
        // (commands are handled before the drain step): they must be discarded, not
        // prepended to the new take's WAV.
        use super::{writer_run, WriterCmd};
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(1024);
        for _ in 0..30 {
            assert!(prod.push([0.5, 0.0])); // stale frames from the previous take
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || writer_run(cons, rx));
        let path = temp_wav("stale_start");
        let (ack, ack_rx) = std::sync::mpsc::channel();
        tx.send(WriterCmd::Start(path.to_str().unwrap().to_string(), ack))
            .unwrap();
        assert!(ack_rx.recv().unwrap(), "start must ack the file creation");
        for _ in 0..20 {
            assert!(prod.push([0.75, 0.0])); // the new take's frames
        }
        let (sack, sack_rx) = std::sync::mpsc::channel();
        tx.send(WriterCmd::Stop(sack)).unwrap();
        assert!(sack_rx.recv().unwrap());
        tx.send(WriterCmd::Shutdown).unwrap();
        join.join().unwrap();
        let bytes = std::fs::read(&path).expect("recorded WAV must exist");
        // 44-byte header + ONLY the take's 20 mono float32 samples (30 stale ones discarded).
        assert_eq!(
            bytes.len(),
            44 + 20 * 4,
            "stale frames must not lead the new take"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failed_teardown_latches_the_renderer_gate_raised() {
        // A failed pause()/AudioOutputUnitStop means "no streams" no longer proves "no
        // callbacks": once latched, the host must never publish a lowered gate again, so
        // reap() sticks to the grace-epoch path instead of freeing retirees under a
        // possibly-live zombie callback.
        use super::{effective_gate, note_stream_teardown_failure, STREAM_TEARDOWN_FAILED};
        use std::sync::atomic::Ordering;
        assert!(effective_gate(true), "streams up always raises the gate");
        assert!(
            !effective_gate(false),
            "a clean teardown may lower the gate"
        );
        note_stream_teardown_failure();
        assert!(
            effective_gate(false),
            "a latched teardown failure must pin the gate raised"
        );
        assert!(effective_gate(true));
        // Reset the process-global latch so other tests in this binary see a clean state.
        STREAM_TEARDOWN_FAILED.store(false, Ordering::Release);
    }

    #[test]
    fn start_while_recording_finalizes_the_previous_take_and_starts_clean() {
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(1024);
        let w = RecordWriter::spawn(cons);
        let p1 = temp_wav("restart_a");
        let p2 = temp_wav("restart_b");
        assert!(w.start(p1.to_str().unwrap()));
        for _ in 0..25 {
            assert!(prod.push([0.25, 0.0]));
        }
        // Restart straight into take 2 without a Stop: take 1 must keep every frame pushed
        // while it was live (its buffered tail drains into it, mirroring Stop), and take 2
        // must contain only its own frames.
        assert!(w.start(p2.to_str().unwrap()));
        for _ in 0..10 {
            assert!(prod.push([0.75, 0.0]));
        }
        assert!(w.stop());
        let b1 = std::fs::read(&p1).expect("take 1 must exist");
        let b2 = std::fs::read(&p2).expect("take 2 must exist");
        assert_eq!(b1.len(), 44 + 25 * 4, "take 1 keeps its buffered tail");
        assert_eq!(b2.len(), 44 + 10 * 4, "take 2 contains only its own frames");
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);
    }
}

#[cfg(test)]
mod resample_sinc_tests {
    //! The offline windowed-sinc converter: sample-mapping alignment (the punch splice
    //! depends on it), passband fidelity, and the alias rejection that motivated replacing
    //! the linear interim kernel.

    use super::resample_sinc;
    use crate::sample::CHANNELS;

    fn frames_of(mono: impl Iterator<Item = f32>) -> Vec<[f32; CHANNELS]> {
        mono.map(|s| [s, s]).collect()
    }

    #[test]
    fn identity_rate_passes_the_buffer_through_unchanged() {
        let frames = frames_of((0..1000).map(|i| (i as f32 / 999.0) - 0.5));
        let out = resample_sinc(frames.clone(), 48_000, 48_000).unwrap();
        assert_eq!(out, frames);
    }

    #[test]
    fn output_length_is_the_rounded_rate_ratio_exactly() {
        let out = resample_sinc(frames_of((0..44_100).map(|_| 0.25)), 44_100, 48_000).unwrap();
        // round(44100 * 48000/44100) = 48000: one second stays one second.
        assert_eq!(out.len(), 48_000);
    }

    #[test]
    fn an_impulse_lands_at_the_mapped_frame_so_splice_alignment_holds() {
        // The resampler's group delay must be trimmed exactly: a feature at input
        // frame k has to land at round(k * ratio), or every punch splice would be
        // shifted by the sinc latency.
        let n = 20_000;
        let k = 10_000usize;
        let mut frames = vec![[0.0f32; CHANNELS]; n];
        frames[k] = [1.0, 1.0];
        let out = resample_sinc(frames, 44_100, 48_000).unwrap();
        let expected = (k as f64 * 48_000.0 / 44_100.0).round() as usize;
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1[0].abs().partial_cmp(&b.1[0].abs()).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            (peak as i64 - expected as i64).abs() <= 1,
            "impulse at {k} mapped to {peak}, expected ~{expected}"
        );
    }

    #[test]
    fn a_passband_tone_survives_upsampling_with_high_fidelity() {
        // 15 kHz at 44.1 kHz — well inside the passband but exactly where linear
        // interpolation audibly falls apart. The output must match the ideal
        // 15 kHz tone at 48 kHz sample-for-sample (interior; edges carry the
        // windowed tail).
        let sr_in = 44_100.0f64;
        let sr_out = 48_000.0f64;
        let hz = 15_000.0f64;
        let n = 44_100;
        let input = frames_of(
            (0..n).map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / sr_in).sin() as f32),
        );
        let out = resample_sinc(input, 44_100, 48_000).unwrap();
        // Quadrature demodulation at 15 kHz: phase-independent amplitude of the
        // surviving tone. (A pointwise ideal-sine comparison would punish the
        // converter's sub-sample group delay — a pure, inaudible time shift.)
        let skip = 2_000;
        let mut a = 0.0f64;
        let mut b = 0.0f64;
        let mut count = 0usize;
        for (i, f) in out.iter().enumerate().skip(skip).take(out.len() - 2 * skip) {
            let ph = 2.0 * std::f64::consts::PI * hz * i as f64 / sr_out;
            a += f[0] as f64 * ph.sin();
            b += f[0] as f64 * ph.cos();
            count += 1;
        }
        let amplitude = 2.0 * (a * a + b * b).sqrt() / count as f64;
        assert!(
            (amplitude - 1.0).abs() < 0.02,
            "15 kHz passband amplitude {amplitude}, expected ~1.0 \
             (linear interpolation attenuates and smears this tone)"
        );
    }

    /// Index of the peak-magnitude frame (left channel).
    fn peak_index(out: &[[f32; CHANNELS]]) -> usize {
        out.iter()
            .enumerate()
            .max_by(|a, b| a.1[0].abs().partial_cmp(&b.1[0].abs()).unwrap())
            .map(|(i, _)| i)
            .unwrap()
    }

    #[test]
    fn edge_impulses_map_correctly_frame_zero_and_last_frame_long_and_short() {
        // rubato's process_all convenience was measured duplicating a frame-0 impulse
        // (at output 0 AND ~69, the group delay) and shifting sub-chunk clips by the
        // whole delay. The manual drive loop must map edges exactly: k -> round(k*ratio),
        // with no duplicate energy at the stale location.
        let ratio: f64 = 48_000.0 / 44_100.0;

        // Frame 0, long clip.
        let mut long = vec![[0.0f32; CHANNELS]; 20_000];
        long[0] = [1.0, 1.0];
        let out = resample_sinc(long, 44_100, 48_000).unwrap();
        assert!(
            peak_index(&out) <= 1,
            "frame-0 impulse must stay at the head"
        );
        // No duplicate: outside the head lobe the signal is tiny everywhere.
        let stray = out[200..].iter().map(|f| f[0].abs()).fold(0.0f32, f32::max);
        assert!(stray < 0.01, "stray energy {stray} — a duplicated impulse?");

        // Frame 0, SHORT clip (under one 1024-frame chunk).
        let mut short = vec![[0.0f32; CHANNELS]; 500];
        short[0] = [1.0, 1.0];
        let out = resample_sinc(short, 44_100, 48_000).unwrap();
        assert_eq!(out.len(), (500.0 * ratio).round() as usize);
        assert!(
            peak_index(&out) <= 1,
            "sub-chunk clips must not come out shifted by the group delay (was ~69)"
        );

        // LAST frame: the tail flush must render the delayed end of the audio.
        let n = 5_000usize;
        let mut tail = vec![[0.0f32; CHANNELS]; n];
        tail[n - 1] = [1.0, 1.0];
        let out = resample_sinc(tail, 44_100, 48_000).unwrap();
        let expected = ((n - 1) as f64 * ratio).round() as usize;
        let peak = peak_index(&out);
        assert!(
            (peak as i64 - expected as i64).abs() <= 1,
            "last-frame impulse at {peak}, expected ~{expected}"
        );
    }

    #[test]
    fn a_short_clip_keeps_its_amplitude() {
        // A 500-frame 1 kHz burst must come through at full level — the shifted output
        // of the old shape rendered short clips partially (or nearly) silent.
        let n = 500;
        let input: Vec<[f32; CHANNELS]> = (0..n)
            .map(|i| {
                let s = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin() as f32;
                [s, s]
            })
            .collect();
        let out = resample_sinc(input, 44_100, 48_000).unwrap();
        let peak = out.iter().map(|f| f[0].abs()).fold(0.0f32, f32::max);
        assert!(peak > 0.9, "short-clip peak {peak}, expected ~1.0");
    }

    #[test]
    fn overflow_poison_never_leaves_the_converter() {
        // Direct hostile input (bypassing decode_file's clamp): the convolution of
        // f32::MAX samples overflows to NaN/Inf inside the kernel; the output scrub
        // must zero them so no caller can receive poison from this function.
        let frames = vec![[f32::MAX; CHANNELS]; 4096];
        let out = resample_sinc(frames, 44_100, 48_000).unwrap();
        assert!(out.iter().all(|f| f[0].is_finite() && f[1].is_finite()));
    }

    #[test]
    fn a_tone_above_the_target_nyquist_is_rejected_not_aliased() {
        // 23 kHz at 48 kHz downsampled to 44.1 kHz (Nyquist 22.05 kHz): a correct
        // converter suppresses it; linear interpolation folds it into the audible
        // band at nearly full amplitude.
        let sr_in = 48_000.0f64;
        let hz = 23_000.0f64;
        let n = 48_000;
        let input = frames_of(
            (0..n).map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / sr_in).sin() as f32),
        );
        let out = resample_sinc(input, 48_000, 44_100).unwrap();
        let skip = 2_000;
        let mut sq = 0.0f64;
        let mut count = 0usize;
        for f in out.iter().skip(skip).take(out.len() - 2 * skip) {
            sq += (f[0] as f64) * (f[0] as f64);
            count += 1;
        }
        let rms = (sq / count as f64).sqrt();
        assert!(
            rms < 0.02,
            "alias residue RMS {rms} for a tone above the target Nyquist \
             (linear interpolation leaves ~0.7 here)"
        );
    }
}

#[cfg(test)]
mod out_resampler_tests {
    //! The output-boundary resampler's transport honesty: the reported playhead must track
    //! what the listener HEARS (not what has merely rendered into the FIFO), and a
    //! seek/pause must drop pre-rendered audio instead of playing it out.

    use super::OutResampler;
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::live::{spawn, EngineHandle, RtCell};
    use crate::ring::spsc;
    use crate::sample::{Smp, CHANNELS, ENGINE_SR};
    use crate::source::BufferSource;
    use std::collections::HashMap;
    use std::sync::Arc;

    const BLOCK: usize = 128;

    /// A live engine playing a mono ramp (sample i = i * 1e-4) from frame 0, wrapped in an
    /// RtCell + a 44.1 kHz output resampler (step > 1, so the FIFO always retains frames).
    /// An engine playing a pure [hz] tone from frame 0, behind an OutResampler
    /// to [device_rate] — the full-boundary harness for kernel-quality tests.
    fn tone_engine(hz: f64, device_rate: f64) -> (EngineHandle, OutResampler) {
        let n = 96_000usize;
        let mut pcm = Vec::with_capacity(n * CHANNELS);
        for i in 0..n {
            let v = (2.0 * std::f64::consts::PI * hz * i as f64 / ENGINE_SR).sin() as Smp;
            pcm.push(v);
            pcm.push(v);
        }
        let desc = GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(BufferSource::new(pcm, 0))),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2)],
            master: 2,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        let cell = RtCell::new(rt);
        let res = OutResampler::new(device_rate, BLOCK, Arc::clone(&cell));
        (h, res)
    }

    #[test]
    fn a_passband_tone_survives_the_output_boundary_at_unit_amplitude() {
        // 15 kHz through 48 kHz -> 44.1 kHz: exactly where the old linear
        // kernel rolled off and smeared. Quadrature demodulation at the
        // DEVICE rate is phase-independent, so the kernel's sub-sample
        // delay cannot masquerade as attenuation.
        let (h, mut res) = tone_engine(15_000.0, 44_100.0);
        h.play();
        let frames = 44_100usize;
        let mut out = vec![0.0; frames * CHANNELS];
        res.process(&mut out, frames);
        let skip = 2_000;
        let mut a = 0.0f64;
        let mut b = 0.0f64;
        let mut count = 0usize;
        for i in skip..frames - skip {
            let ph = 2.0 * std::f64::consts::PI * 15_000.0 * i as f64 / 44_100.0;
            a += out[i * CHANNELS] as f64 * ph.sin();
            b += out[i * CHANNELS] as f64 * ph.cos();
            count += 1;
        }
        let amplitude = 2.0 * (a * a + b * b).sqrt() / count as f64;
        assert!(
            (amplitude - 1.0).abs() < 0.03,
            "15 kHz passband amplitude {amplitude}, expected ~1.0 \
             (the linear kernel attenuated and aliased here)"
        );
    }

    #[test]
    fn a_tone_above_the_device_nyquist_is_suppressed_not_aliased() {
        // 23 kHz exists at the 48 kHz engine rate but is above the 44.1 kHz
        // device Nyquist (22.05 kHz): the kernel's downsampling cutoff must
        // suppress it; the linear kernel folded it into the audible band at
        // near full amplitude.
        let (h, mut res) = tone_engine(23_000.0, 44_100.0);
        h.play();
        let frames = 44_100usize;
        let mut out = vec![0.0; frames * CHANNELS];
        res.process(&mut out, frames);
        let skip = 2_000;
        let mut sq = 0.0f64;
        let mut count = 0usize;
        for i in skip..frames - skip {
            let v = out[i * CHANNELS] as f64;
            sq += v * v;
            count += 1;
        }
        let rms = (sq / count as f64).sqrt();
        assert!(
            rms < 0.05,
            "alias residue RMS {rms} for a tone above the device Nyquist"
        );
    }

    fn ramp_engine() -> (EngineHandle, OutResampler) {
        let n = 48_000usize;
        let mut pcm = Vec::with_capacity(n * CHANNELS);
        for i in 0..n {
            let v = i as Smp * 1e-4;
            pcm.push(v);
            pcm.push(v);
        }
        let desc = GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(BufferSource::new(pcm, 0))),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2)],
            master: 2,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        let cell = RtCell::new(rt);
        let res = OutResampler::new(44_100.0, BLOCK, Arc::clone(&cell));
        (h, res)
    }

    #[test]
    fn reported_playhead_subtracts_the_unplayed_fifo_backlog() {
        let (h, mut res) = ramp_engine();
        h.play();
        // 441 device frames consume exactly 480 engine frames; rendering happens in whole
        // 128-frame blocks, so the raw render clock lands PAST 480 with the excess parked
        // in the FIFO. The reported playhead must reflect the consumed 480, not the render.
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        let frame = h.frame();
        assert!(
            (frame - 480).abs() <= 2,
            "playhead must track audio delivered to the device (~480), got {frame}"
        );
    }

    #[test]
    fn seek_flushes_prerendered_audio_and_restarts_at_the_target() {
        let (h, mut res) = ramp_engine();
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        // Mid-ramp now. Seek home: the FIFO still holds pre-seek samples (values ~0.048);
        // the next callback must start from the ramp's beginning, not play them out.
        h.seek(0);
        res.process(&mut out, 441);
        assert!(
            out[0].abs() < 1e-3,
            "first post-seek sample must come from the seek target, got {}",
            out[0]
        );
        let frame = h.frame();
        assert!(
            (frame - 480).abs() <= 2,
            "post-seek playhead restarts at the target (~480 after one buffer), got {frame}"
        );
    }

    #[test]
    fn pause_folds_the_backlog_and_reports_the_heard_position() {
        let (h, mut res) = ramp_engine();
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        h.pause();
        // pause() folds the un-played FIFO depth into the clock: the paused position is the
        // ~480 frames actually delivered, NOT the block-rounded render clock (which would
        // read ahead by the parked backlog — the old forward jump on pause).
        let paused_at = h.frame();
        assert!(
            (paused_at - 480).abs() <= 2,
            "paused position must be what was heard (~480), got {paused_at}"
        );
        res.process(&mut out, 441);
        assert!(
            out.iter().all(|s| s.abs() < 1e-6),
            "paused output must be silence, not the FIFO's stale tail"
        );
        assert_eq!(h.frame(), paused_at, "paused playhead must hold still");
    }

    #[test]
    fn resume_after_pause_plays_audio_immediately_not_the_parked_silence() {
        let (h, mut res) = ramp_engine();
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        h.pause();
        // The paused callback keeps topping the FIFO up with silence…
        res.process(&mut out, 441);
        // …which play() must flush: the first resumed buffer starts with the ramp content
        // at the paused position (~frame 480 → value ~0.048), not a block of that silence.
        h.play();
        res.process(&mut out, 441);
        assert!(
            out[0] > 0.01,
            "resume must start with fresh audio, got leading sample {}",
            out[0]
        );
        assert!(
            (out[0] - 0.048).abs() < 0.005,
            "resume must continue from the paused position (~0.048), got {}",
            out[0]
        );
    }

    #[test]
    fn pause_right_after_a_loop_wrap_reports_the_exact_heard_position() {
        let (h, mut res) = ramp_engine();
        // Loop [200, 700): the device consumes 480 engine frames from the seek target
        // (no wrap HEARD yet) while the renderer runs ahead in 128-frame blocks ACROSS
        // the wrap. The old render-clock-minus-backlog fold computed a position BEFORE
        // loop_start here and clamped to 200 — inside the region, but wrong by 480
        // frames. The consumed-cursor mapping reports the heard position EXACTLY.
        h.set_loop(true, 200, 700);
        h.seek(200);
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        h.pause();
        let paused_at = h.frame();
        // The interpolator holds ONE lookahead frame (out[i] needs fifo[i+1]),
        // so the cursor sits at engine frame 479.<fraction> — exact to within
        // the sub-frame the listener is mid-way through hearing.
        assert!(
            (679..=680).contains(&paused_at),
            "paused position must be the exact heard frame (~200 + 480), got {paused_at}"
        );
    }

    #[test]
    fn the_playhead_maps_exactly_through_a_loop_wrap() {
        let (h, mut res) = ramp_engine();
        // Loop [200, 700), seek to 200. Two device buffers consume 960 engine frames:
        // 500 reach the wrap (timeline 200..700), the next 460 are post-wrap
        // (timeline 200..660). The old arithmetic sat in its clamp plateau at 200
        // through this window; the segment mapping reports 660 exactly — and never a
        // false back-jump below loop_start on the way.
        h.set_loop(true, 200, 700);
        h.seek(200);
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        let pre_wrap = h.frame();
        assert!(
            (679..=680).contains(&pre_wrap),
            "pre-wrap position is exact (~680, one interpolation lookahead), got {pre_wrap}"
        );
        res.process(&mut out, 441);
        let after_wrap = h.frame();
        assert!(
            (659..=660).contains(&after_wrap),
            "post-wrap position must map into the loop region exactly \
             (~960 consumed = 500 to the wrap + ~460 after it), got {after_wrap}"
        );
    }

    #[test]
    fn pause_and_play_are_idempotent() {
        let (h, mut res) = ramp_engine();
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        h.pause();
        let paused_at = h.frame();
        // The paused callback refills the FIFO with silence and republishes its depth…
        res.process(&mut out, 441);
        // …which a SECOND pause must not fold into the clock (that was a position rewind).
        h.pause();
        assert_eq!(h.frame(), paused_at, "double pause must not rewind");
        h.play();
        res.process(&mut out, 441);
        let resumed_first = out[0];
        // A second play must not flush LIVE audio out of the FIFO: the next buffer has to
        // continue the ramp seamlessly (one buffer = 480 engine frames = +0.048).
        h.play();
        res.process(&mut out, 441);
        assert!(
            (out[0] - (resumed_first + 0.048)).abs() < 0.005,
            "double play must not skip: expected ~{} got {}",
            resumed_first + 0.048,
            out[0]
        );
    }

    #[test]
    fn loop_wrap_never_reports_a_position_before_the_loop_start() {
        let (h, mut res) = ramp_engine();
        // Loop [200, 680): consuming 480 engine frames from 200 lands exactly on the wrap.
        h.set_loop(true, 200, 680);
        h.seek(200);
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        let frame = h.frame();
        assert!(
            (200..=680).contains(&frame),
            "post-wrap playhead must stay inside the loop region, got {frame}"
        );
    }
}

#[cfg(test)]
mod input_cell_tests {
    //! The capture fan-out's overflow accounting: a full recording ring must COUNT every
    //! dropped frame (the take on disk has a hole there), never lose them silently.

    use super::InputCell;
    use crate::ring::spsc;
    use crate::sample::{Smp, CHANNELS};
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;

    struct RecCell {
        cell: Arc<InputCell>,
        rec_active: Arc<AtomicBool>,
        frames_in: Arc<AtomicU64>,
        rec_dropped: Arc<AtomicU64>,
        rec_written: Arc<AtomicU64>,
    }

    fn cell_with_rec_capacity(rec_cap: usize) -> RecCell {
        let (mic_prod, _mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 10);
        let (cap_prod, _cap_cons) = spsc::<[Smp; CHANNELS]>(1 << 10);
        let (rec_prod, _rec_cons) = spsc::<[Smp; CHANNELS]>(rec_cap);
        let rec_active = Arc::new(AtomicBool::new(true));
        let frames_in = Arc::new(AtomicU64::new(0));
        let rec_dropped = Arc::new(AtomicU64::new(0));
        let rec_written = Arc::new(AtomicU64::new(0));
        let cell = InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&rec_active),
            Arc::new(AtomicU32::new(0)),
            Arc::clone(&frames_in),
            Arc::clone(&rec_dropped),
            Arc::clone(&rec_written),
            Arc::new(AtomicU64::new(1.0f64.to_bits())),
        );
        // Identity resample path (input already at ENGINE_SR), mono input.
        cell.reconfigure(crate::sample::ENGINE_SR, 1);
        RecCell {
            cell,
            rec_active,
            frames_in,
            rec_dropped,
            rec_written,
        }
    }

    #[test]
    fn recording_ring_overflow_is_counted() {
        let c = cell_with_rec_capacity(4);
        // 12 mono frames into a 4-slot recording ring: 4 fit (written), 8 dropped.
        let data = [0.5f32; 12];
        // SAFETY: no input callback exists in this test; we are the single accessor.
        unsafe { c.cell.feed(&data) };
        assert_eq!(c.frames_in.load(Ordering::Relaxed), 12);
        assert_eq!(
            c.rec_dropped.load(Ordering::Relaxed),
            8,
            "every frame refused by the full recording ring must be counted"
        );
        assert_eq!(
            c.rec_written.load(Ordering::Relaxed),
            4,
            "written counts exactly the frames that made it into the ring"
        );
    }

    #[test]
    fn no_drops_are_counted_while_the_ring_has_room() {
        let c = cell_with_rec_capacity(64);
        let data = [0.25f32; 32];
        // SAFETY: single accessor (see above).
        unsafe { c.cell.feed(&data) };
        assert_eq!(c.rec_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(c.rec_written.load(Ordering::Relaxed), 32);
        // Frames emitted while recording is DISARMED never touch the ring or either
        // counter — this is exactly why rec_written is the per-take proof the global
        // frame counter cannot be: a pre-arm callback advances frames_in but not this.
        c.rec_active.store(false, Ordering::Release);
        unsafe { c.cell.feed(&data) };
        assert_eq!(c.rec_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(c.rec_written.load(Ordering::Relaxed), 32);
        assert_eq!(c.frames_in.load(Ordering::Relaxed), 64);
    }
}
