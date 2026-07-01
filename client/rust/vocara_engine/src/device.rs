//! The live OS audio path (macOS CoreAudio via cpal) + file decode (symphonia).
//!
//! The cpal output `Stream` is `!Send`, so all stream lifecycle (build / drop / device switch)
//! is confined to one dedicated [`AudioHost`] thread; FFI threads only message it. The audio
//! thread renders the engine's fixed `ENGINE_SR` mix and `build_stream` resamples it to the
//! device's native rate at the I/O boundary. `decode_file` + `resample_linear` turn any
//! supported file into engine-rate interleaved-stereo PCM the feeder pushes to a source ring.
//! Gated behind the `device` feature so the DSP core stays dependency-free.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::live::RtCell;
use crate::ring::{Consumer, Producer};
use crate::sample::{Smp, CHANNELS, ENGINE_SR};

/// `ve_poll_device_event` bit: the active output device was lost (stream error / disconnect).
pub const EVENT_OUTPUT_LOST: u32 = 1;
/// `ve_poll_device_event` bit: the active input device was lost (stream error / disconnect).
pub const EVENT_INPUT_LOST: u32 = 2;

/// Keeps the cpal stream alive; dropping it stops audio. `cpal::Stream` is `!Send`, so this
/// only ever lives on the [`AudioHost`] thread that created it.
pub struct DeviceHandle {
    _stream: cpal::Stream,
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
/// [`RtProcessor`] into a small pre-allocated FIFO and linearly interpolates the device-rate
/// output. RT-safe: no alloc/lock/syscall in `process` (the FIFO never reallocs because each
/// callback is processed in `max_block`-bounded sub-chunks and consumed frames are drained).
///
/// Linear interpolation is the interim boundary kernel (a windowed-sinc/`rubato` upgrade lands
/// with the input-side drift compensation in a later phase). It is only used when the device
/// rate differs from `ENGINE_SR`; at 48 kHz the engine renders straight into the cpal buffer.
struct OutResampler {
    rt: Arc<RtCell>,
    /// Engine input frames advanced per device output frame (`ENGINE_SR / device_rate`).
    step: f64,
    /// Fractional read position within `fifo` (kept in `[0, 1)` after each sub-chunk drain).
    pos: f64,
    /// Pending engine-rate frames awaiting interpolation.
    fifo: Vec<[Smp; CHANNELS]>,
    /// Scratch for one engine render block.
    scratch: Vec<Smp>,
    max_block: usize,
}

impl OutResampler {
    fn new(device_rate: f64, max_block: usize, rt: Arc<RtCell>) -> Self {
        let step = ENGINE_SR / device_rate;
        // Peak FIFO occupancy in a sub-chunk is `need = floor(pos + (chunk-1)*step) + 2`
        // (chunk <= max_block, pos in [0,1)) plus up to one extra render block (max_block) to
        // reach it. Size from the ACTUAL step so `push` never reallocates on the RT callback —
        // the old fixed `4*max_block` underflowed for device rates below ~12 kHz (step > 4,
        // e.g. an 8 kHz Bluetooth HFP route), forcing a heap realloc inside the audio callback.
        let cap = (max_block as f64 * step).ceil() as usize + max_block + CHANNELS + 2;
        OutResampler {
            rt,
            step,
            pos: 0.0,
            fifo: Vec::with_capacity(cap),
            scratch: vec![0.0; max_block * CHANNELS],
            max_block,
        }
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
        let mut done = 0;
        while done < out_frames {
            let chunk = (out_frames - done).min(self.max_block);
            // Interpolation touches up to index floor(pos + (chunk-1)*step) + 1, so it needs
            // floor(pos + (chunk-1)*step) + 2 frames present. The drain below advances `pos`
            // by chunk*step and removes floor(pos + chunk*step) frames, so the FIFO must ALSO
            // hold at least that many. Sizing `need` to the max keeps the drained count equal
            // to the `pos` adjustment; without it, on device rates <= 24 kHz (step >= 2) the
            // drop was clamped below `consumed` and the read position drifted (audible glitch).
            let need_interp =
                (self.pos + (chunk.saturating_sub(1)) as f64 * self.step).floor() as usize + 2;
            let need_consumed = (self.pos + chunk as f64 * self.step).floor() as usize;
            let need = need_interp.max(need_consumed);
            // The FIFO is sized in `new` so reaching `need` never reallocates (no RT alloc).
            debug_assert!(need + self.max_block <= self.fifo.capacity());
            while self.fifo.len() < need {
                self.render_block();
            }
            for k in 0..chunk {
                let i = self.pos.floor() as usize;
                let frac = (self.pos - i as f64) as Smp;
                let a = self.fifo[i];
                let b = self.fifo[i + 1];
                let o = (done + k) * CHANNELS;
                out[o] = a[0] + (b[0] - a[0]) * frac;
                out[o + 1] = a[1] + (b[1] - a[1]) * frac;
                self.pos += self.step;
            }
            let consumed = self.pos.floor() as usize;
            if consumed > 0 {
                // `need >= need_consumed` guarantees `consumed <= fifo.len()`, so `drop`
                // equals `consumed`; subtract the actually-dropped count to stay consistent.
                let drop = consumed.min(self.fifo.len());
                self.fifo.drain(0..drop);
                self.pos -= drop as f64;
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
    pos: f64,
    prev: [Smp; CHANNELS],
    primed: bool,
    /// Input peak level (f32 bits) for the meter — read by the FFI thread.
    level: Arc<AtomicU32>,
    /// Total input frames the callback has received — a diagnostic for "is the mic delivering?".
    frames_in: Arc<AtomicU64>,
}

impl InputState {
    /// Fan one resampled frame out to the monitor ring (always) + the capture/recording rings
    /// (when their consumer is active). Drops on a full ring (back-pressure; never blocks).
    #[inline]
    fn emit(&mut self, f: [Smp; CHANNELS], cap: bool, rec: bool) {
        self.mic.push(f);
        if cap {
            self.cap.push(f);
        }
        if rec {
            self.rec.push(f);
        }
    }
}

impl InputCell {
    pub fn new(
        mic: Producer<[Smp; CHANNELS]>,
        cap: Producer<[Smp; CHANNELS]>,
        rec: Producer<[Smp; CHANNELS]>,
        cap_active: Arc<AtomicBool>,
        rec_active: Arc<AtomicBool>,
        level: Arc<AtomicU32>,
        frames_in: Arc<AtomicU64>,
    ) -> Arc<InputCell> {
        Arc::new(InputCell(UnsafeCell::new(InputState {
            mic,
            cap,
            rec,
            cap_active,
            rec_active,
            in_channels: 1,
            step: 1.0,
            pos: 0.0,
            prev: [0.0, 0.0],
            primed: false,
            level,
            frames_in,
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
        st.frames_in.fetch_add(frames as u64, Ordering::Relaxed);
        let identity = (st.step - 1.0).abs() < 1e-9;
        let mut peak = 0.0f32;
        for fi in 0..frames {
            let base = fi * ch;
            let l = data[base];
            let r = if ch > 1 { data[base + 1] } else { l };
            peak = peak.max(l.abs()).max(r.abs());
            if identity {
                st.emit([l, r], cap, rec);
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
                    st.pos += st.step;
                }
                st.pos -= 1.0;
                st.prev = cur;
            }
        }
        st.level.store(peak.to_bits(), Ordering::Relaxed);
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
    Start(String),
    /// Stop + finalize the WAV; the writer acks when the file is fully flushed.
    Stop(mpsc::Sender<()>),
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

    pub fn start(&self, path: &str) {
        let _ = self.tx.send(WriterCmd::Start(path.to_string()));
    }

    /// Stop recording and block until the WAV is fully written + finalized (so the caller can
    /// read the file immediately).
    pub fn stop(&self) {
        let (ack, rx) = mpsc::channel();
        if self.tx.send(WriterCmd::Stop(ack)).is_ok() {
            let _ = rx.recv();
        }
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
    loop {
        // Apply any pending commands.
        loop {
            match rx.try_recv() {
                Ok(WriterCmd::Start(path)) => {
                    if let Some(w) = wav.take() {
                        let _ = w.finalize();
                    }
                    wav = crate::wav::WavStream::create(&path, 1, ENGINE_SR as u32).ok();
                }
                Ok(WriterCmd::Stop(ack)) => {
                    // Drain any buffered tail into the file before finalizing.
                    if let Some(w) = wav.as_mut() {
                        while let Some(f) = rec.pop() {
                            let _ = w.write_sample(f[0]);
                        }
                    }
                    if let Some(w) = wav.take() {
                        let _ = w.finalize();
                    }
                    let _ = ack.send(());
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
        // Drain available frames; write the left channel (mono) when recording.
        let mut wrote = false;
        while let Some(f) = rec.pop() {
            if let Some(w) = wav.as_mut() {
                let _ = w.write_sample(f[0]);
                wrote = true;
            }
        }
        if !wrote {
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
    stream.play().map_err(|e| format!("input stream.play: {e}"))?;
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
    /// Open the input stream (begin capture/monitor). Replies 0 / -1.
    StartInput { reply: mpsc::Sender<i32> },
    /// Close the input stream.
    StopInput { reply: mpsc::Sender<i32> },
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

    // Input commands are FIRE-AND-FORGET: VoiceProcessingIO setup (create/initialize/start a
    // duplex AudioUnit) is slow, and these are called from the UI thread via FFI — blocking on the
    // host reply froze the UI (spinner). The input-level meter / frame counter reflect the real
    // result a moment later. Always returns 0 (optimistic).

    /// Open the input (mic capture / monitoring). Fire-and-forget.
    pub fn start_input(&self) -> i32 {
        self.send(|reply| HostCmd::StartInput { reply });
        0
    }

    /// Close the input. Fire-and-forget.
    pub fn stop_input(&self) -> i32 {
        self.send(|reply| HostCmd::StopInput { reply });
        0
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
fn audio_host_run(
    rt: Arc<RtCell>,
    input_cell: Arc<InputCell>,
    event: Arc<AtomicU32>,
    rx: mpsc::Receiver<HostCmd>,
    ready_tx: mpsc::Sender<Result<String, String>>,
) {
    // Open the default output before serving commands; report success/failure.
    let mut out_stream = match build_output(None, &rt, &event) {
        Ok((handle, uid)) => {
            let _ = ready_tx.send(Ok(uid));
            Some(handle)
        }
        Err(e) => {
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
            HostCmd::StartInput { reply } => {
                if in_stream.is_some() {
                    let _ = reply.send(0);
                } else {
                    // Duplex VPIO renders the engine through its own output element, so drop the
                    // cpal output FIRST (only one path may touch RtCell). Fall back to the default
                    // input if the selected device can't be opened.
                    if VPIO_DUPLEX_AEC {
                        out_stream = None;
                    }
                    let built = open_mic(cur_in_uid.as_deref(), &rt, &input_cell, &event)
                        .or_else(|_| open_mic(None, &rt, &input_cell, &event));
                    match built {
                        Ok(handle) => {
                            in_stream = Some(handle);
                            let _ = reply.send(0);
                        }
                        Err(_) => {
                            // Mic failed to come up → restore cpal output so playback survives.
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
                }
            }
            HostCmd::StopInput { reply } => {
                let was_on = in_stream.take().is_some();
                // VPIO was rendering the output in duplex; hand playback back to cpal, restoring
                // the user's last chosen output device (a SetOutput during duplex deferred it),
                // falling back to the system default only if that device can't be opened.
                if VPIO_DUPLEX_AEC && was_on && out_stream.is_none() {
                    out_stream = build_output(cur_out_uid.as_deref(), &rt, &event)
                        .or_else(|_| build_output(None, &rt, &event))
                        .ok()
                        .map(|(h, _)| h);
                }
                let _ = reply.send(0);
            }
            HostCmd::SetInput { uid, reply } => {
                cur_in_uid = Some(uid.clone());
                if in_stream.is_some() {
                    // Reopen on the new device (drop old first → single-accessor; cpal output is
                    // already off in duplex).
                    in_stream = None;
                    match open_mic(Some(&uid), &rt, &input_cell, &event)
                        .or_else(|_| open_mic(None, &rt, &input_cell, &event))
                    {
                        Ok(handle) => {
                            in_stream = Some(handle);
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
                in_stream = None;
                out_stream = None;
                break;
            }
        }
    }
    // Both streams (and this thread's `rt`/`input_cell` clones) drop here, on the host thread.
    drop(in_stream);
    drop(out_stream);
}

/// Spawn the audio-host thread, open the default output, and start rendering `rt`. Blocks until
/// the initial stream is up (or fails). Returns the host handle plus the opened device's UID.
pub fn spawn_audio_host(
    rt: Arc<RtCell>,
    input_cell: Arc<InputCell>,
    event: Arc<AtomicU32>,
) -> Result<(AudioHost, String), String> {
    let (tx, rx) = mpsc::channel::<HostCmd>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
    let join = std::thread::Builder::new()
        .name("vocara-audio-host".to_string())
        .spawn(move || audio_host_run(rt, input_cell, event, rx, ready_tx))
        .map_err(|e| format!("spawn audio host: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(uid)) => Ok((AudioHost { tx, join: Some(join) }, uid)),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(e) => Err(format!("audio host startup: {e}")),
    }
}

/// Decode a file (wav/flac/mp3/aac/m4a/ogg) to interleaved-stereo f32 at its source rate.
/// Mono is duplicated; >2 channels take the first two.
pub fn decode_file(path: &str) -> Result<(Vec<[Smp; CHANNELS]>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
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
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "no default track".to_string())?;
    let track_id = track.id;
    let src_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("make decoder: {e}"))?;

    let mut out: Vec<[Smp; CHANNELS]> = Vec::new();
    // Iterate packets until next_packet errors (EOF or read error).
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        let mut sbuf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sbuf.copy_interleaved_ref(decoded);
        for frame in sbuf.samples().chunks(ch.max(1)) {
            let l = frame[0];
            let r = if ch > 1 { frame[1] } else { l };
            out.push([l, r]);
        }
    }
    if out.is_empty() {
        return Err(format!("decoded no audio from {path}"));
    }
    Ok((out, src_rate))
}

/// Linear-interpolation sample-rate conversion (interleaved stereo). Simple and correct; a
/// production build swaps in a sinc/FFT resampler (rubato) on the feeder thread.
pub fn resample_linear(frames: &[[Smp; CHANNELS]], from: u32, to: u32) -> Vec<[Smp; CHANNELS]> {
    if from == to || frames.is_empty() {
        return frames.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((frames.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let i0 = src.floor() as usize;
        let frac = (src - i0 as f64) as f32;
        let a = frames[i0.min(frames.len() - 1)];
        let b = frames[(i0 + 1).min(frames.len() - 1)];
        out.push([
            a[0] + (b[0] - a[0]) * frac,
            a[1] + (b[1] - a[1]) * frac,
        ]);
    }
    out
}
