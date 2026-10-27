//! The live OS audio path (macOS CoreAudio via cpal) + file decode (symphonia).
//!
//! The cpal output `Stream` is `!Send`, so all stream lifecycle (build / drop / device switch)
//! is confined to one dedicated [`AudioHost`] thread; FFI threads only message it. The audio
//! thread renders the engine's fixed `ENGINE_SR` mix and `build_stream` resamples it to the
//! device's native rate at the I/O boundary. `decode_file` + `resample_sinc` turn any
//! supported file into engine-rate interleaved-stereo PCM the feeder pushes to a source ring.
//! Gated behind the `device` feature so the DSP core stays dependency-free.

use std::cell::UnsafeCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::capture_layout::{InputChannelMix, INPUT_CHANNEL_AUTO};
use crate::live::{RtCell, Shared};
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
/// The SELECTED input device could not be opened and the host fell back to the
/// system default: capture works, but not from the microphone the UI shows.
/// SetInput is fire-and-forget (a blocking reply froze the UI on slow VPIO
/// setups), so this event — plus `ve_actual_input_device` — is how the truth
/// reaches the user instead of a silently wrong device label.
pub const EVENT_INPUT_FELL_BACK: u32 = 8;
/// The active recording reached the longest take a canonical WAV header can
/// describe ([`crate::wav::MAX_F32_SAMPLES`], about 6 h 12 min of the mono
/// 48 kHz take format). Capture past that point is refused rather than written
/// where no reader could address it, so the UI must finalize the take —
/// without this the recording clock would keep running over audio nothing is
/// storing.
pub const EVENT_RECORD_LIMIT: u32 = 16;
/// Playback is NOT coming out of the device the user selected. Raised on the
/// transition into that state, so the UI can say so once rather than every tick.
/// Two ways in: a requested output device that could not be opened (the host
/// fell back to the system default), and — the one no UI could otherwise see —
/// duplex VPIO taking over the output while the mic is open. VPIO is a single
/// communication endpoint bound to the MIC, so it renders to that device (when
/// it has output streams) or to the system default, and never consults the
/// app-selected output at all. The engine mutes the hear-yourself monitor for as
/// long as this holds (see [`OutputBinding`]): "headphones are selected" stops
/// being evidence that the monitor is safe once playback has moved elsewhere.
pub const EVENT_OUTPUT_REROUTED: u32 = 32;

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
    /// An OUTPUT stream built from the system default device (not a uid). cpal builds those
    /// on a DefaultOutput unit, which follows the system default by itself — so the device it
    /// renders to is whatever the default is NOW, not the uid resolved when it was built.
    follows_default: bool,
    /// The stream's sample rate (Hz).
    rate: u32,
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
    /// How the device is attached. Bluetooth matters twice: a same-device in+out pick degrades
    /// output to HFP (hardware limit), and only some transports can carry a headphones mark.
    pub transport: Transport,
}

/// How a device is attached (on macOS, the HAL's transport type). It decides which outputs the
/// user may mark as headphones-only: a port or a sink with its own speakers (built-in, HDMI,
/// DisplayPort, AirPlay) never, whatever is plugged in or however the system names it. The
/// discriminants cross the FFI as `VeDeviceInfo::transport`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transport {
    #[default]
    Unknown = 0,
    BuiltIn = 1,
    Usb = 2,
    Bluetooth = 3,
    Hdmi = 4,
    DisplayPort = 5,
    Network = 6,
    Thunderbolt = 7,
    FireWire = 8,
    Pci = 9,
    Aggregate = 10,
    Virtual = 11,
    Other = 12,
}

impl Transport {
    /// From CoreAudio's `kAudioDevicePropertyTransportType` four-char code.
    pub fn from_core_audio(code: u32) -> Self {
        match &code.to_be_bytes() {
            b"bltn" => Self::BuiltIn,
            b"usb " => Self::Usb,
            b"blue" | b"blea" => Self::Bluetooth,
            b"hdmi" => Self::Hdmi,
            b"dprt" => Self::DisplayPort,
            b"airp" | b"eavb" => Self::Network,
            b"thun" => Self::Thunderbolt,
            b"1394" => Self::FireWire,
            b"pci " => Self::Pci,
            b"grup" | b"fgrp" => Self::Aggregate,
            b"virt" => Self::Virtual,
            [0, 0, 0, 0] => Self::Unknown,
            _ => Self::Other,
        }
    }

    fn from_cpal(t: cpal::InterfaceType) -> Self {
        use cpal::InterfaceType as I;
        match t {
            I::BuiltIn => Self::BuiltIn,
            I::Usb => Self::Usb,
            I::Bluetooth => Self::Bluetooth,
            I::Hdmi => Self::Hdmi,
            I::DisplayPort => Self::DisplayPort,
            I::Network => Self::Network,
            I::Thunderbolt => Self::Thunderbolt,
            I::FireWire => Self::FireWire,
            I::Pci => Self::Pci,
            I::Aggregate => Self::Aggregate,
            I::Virtual => Self::Virtual,
            I::Unknown => Self::Unknown,
            _ => Self::Other,
        }
    }
}

/// The CoreAudio device UID for a cpal device (stable across reconnects / duplicate names).
fn device_uid(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.id().to_string())
}

/// The human name + how the device with `uid` is attached. cpal's CoreAudio backend names no
/// transport but Aggregate — every Mac device read as not Bluetooth — so there the HAL is asked.
fn device_meta(device: &cpal::Device, uid: &str) -> (String, Transport) {
    let (name, transport) = match device.description() {
        Ok(desc) => (
            desc.name().to_string(),
            Transport::from_cpal(desc.interface_type()),
        ),
        Err(_) => (device.to_string(), Transport::Unknown),
    };
    #[cfg(target_vendor = "apple")]
    let transport = crate::vpio::device_transport(uid).unwrap_or(transport);
    #[cfg(not(target_vendor = "apple"))]
    let _ = uid;
    (name, transport)
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
        let (name, transport) = device_meta(&d, &uid);
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
            transport,
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
        device_meta(device, "").0,
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
        let (name, transport) = device_meta(&d, &uid);
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
            transport,
        });
    }
    out
}

/// The input channel count of the device with `uid` (empty = the system default input), across
/// all its input streams; 0 when the device is absent or unreadable. What the settings panel
/// decides on whether to offer an input-channel choice — with no engine and no open mic.
pub fn input_device_channels(uid: &str) -> u32 {
    let host = cpal::default_host();
    let device = if uid.is_empty() {
        host.default_input_device()
    } else {
        host.input_devices()
            .ok()
            .and_then(|mut ds| ds.find(|d| device_uid(d).as_deref() == Some(uid)))
    };
    let Some(device) = device else {
        return 0;
    };
    let default = || {
        device
            .default_input_config()
            .ok()
            .map(|c| u32::from(c.channels()))
    };
    // On macOS the widest supported config counts the channels of every input stream, where the
    // default config is the FIRST stream's format — an aggregate device or a multi-stream
    // interface read as mono there, and the panel hid the choice. Elsewhere (ALSA) the supported
    // configs are channel RANGES up to 64, so the widest says nothing about the mic: the default
    // config is what the input stream opens with.
    #[cfg(target_vendor = "apple")]
    let count = device
        .supported_input_configs()
        .ok()
        .and_then(|cs| cs.map(|c| u32::from(c.channels())).max())
        .or_else(default);
    #[cfg(not(target_vendor = "apple"))]
    let count = default();
    count.unwrap_or(0)
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
    /// The stream generation this resampler's callback was built with. The cell denies it once
    /// a newer stream has claimed the cell, which is how a zombie callback surviving a failed
    /// teardown stops racing its replacement (see [`RtCell`]).
    epoch: u64,
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
    /// Fractional carry of the engine-rate frames the device was handed (`out_frames * step`)
    /// for the output clock ([`Shared::count_played`]). Kept across flushes: resetting it
    /// with `pos` lost up to a frame per seek, a bias the drift estimator read as drift.
    played_acc: f64,
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
            // At x = 0 the scaled sinc's limit is `fc`, not 1: the centre tap of the phase-0
            // row stood fc⁻¹ times too tall, and the row's unity-DC normalization then
            // squashed every other tap under it — that row all but passed its input through
            // unfiltered, and a downsampling device (a 16-32 kHz call-profile output) heard
            // the engine's top octave folded down, 8-14 dB under the signal.
            let sinc = if x.abs() < 1e-12 {
                fc
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

/// Where one device frame's kernel reads: the polyphase row pair for its sub-phase (rows
/// `r0`, `r0 + 1`, linearly interpolated by `rw`) and the FIFO index of its first tap.
#[derive(Clone, Copy)]
struct KernelAt {
    row0: usize,
    rw: f32,
    base: usize,
}

impl KernelAt {
    #[inline(always)]
    fn at(pos: f64, origin: i64) -> KernelAt {
        let ip = pos.floor();
        let frac = pos - ip;
        // Polyphase row pair for this sub-phase, linearly interpolated.
        let rf = frac * SINC_OVERSAMPLE as f64;
        let r0 = rf.floor() as usize;
        KernelAt {
            row0: r0 * SINC_TAPS,
            rw: (rf - r0 as f64) as f32,
            base: (ip as i64 - origin) as usize - (SINC_HALF - 1),
        }
    }
}

/// The polyphase kernel for `out.len() / CHANNELS` consecutive device frames, read from `fifo`
/// (content index of `fifo[0]` = `origin`) starting at read position `*pos`, which advances by
/// `step` per frame.
///
/// Each frame's two sums run over the taps in order, from zero — a chain of 64 dependent adds,
/// so one frame at a time the converter waited on its own accumulator. Four frames are summed
/// side by side instead (eight independent chains), their interpolated weights formed first in
/// one vectorizable pass; every frame still forms exactly the weights, the products and the
/// sums, in exactly the order, it always did, from the same positions.
fn sinc_run(
    table: &[f32],
    fifo: &[[Smp; CHANNELS]],
    origin: i64,
    pos: &mut f64,
    step: f64,
    out: &mut [Smp],
) {
    let frames = out.len() / CHANNELS;
    let mut quads = out[..frames * CHANNELS].chunks_exact_mut(4 * CHANNELS);
    for o in &mut quads {
        let mut w = [[0.0f32; SINC_TAPS]; 4];
        let mut f: [&[[Smp; CHANNELS]]; 4] = [&[]; 4];
        for (wj, fj) in w.iter_mut().zip(f.iter_mut()) {
            let a = KernelAt::at(*pos, origin);
            *pos += step;
            let r0 = &table[a.row0..a.row0 + SINC_TAPS];
            let r1 = &table[a.row0 + SINC_TAPS..a.row0 + 2 * SINC_TAPS];
            for t in 0..SINC_TAPS {
                wj[t] = r0[t] + (r1[t] - r0[t]) * a.rw;
            }
            *fj = &fifo[a.base..a.base + SINC_TAPS];
        }
        let ([w0, w1, w2, w3], [f0, f1, f2, f3]) = (&w, f);
        let (mut l0, mut l1, mut l2, mut l3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let (mut r0, mut r1, mut r2, mut r3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for t in 0..SINC_TAPS {
            l0 += f0[t][0] * w0[t];
            r0 += f0[t][1] * w0[t];
            l1 += f1[t][0] * w1[t];
            r1 += f1[t][1] * w1[t];
            l2 += f2[t][0] * w2[t];
            r2 += f2[t][1] * w2[t];
            l3 += f3[t][0] * w3[t];
            r3 += f3[t][1] * w3[t];
        }
        o.copy_from_slice(&[l0, r0, l1, r1, l2, r2, l3, r3]);
    }
    for o in quads.into_remainder().chunks_exact_mut(CHANNELS) {
        let a = KernelAt::at(*pos, origin);
        let (r0, r1) = (
            &table[a.row0..a.row0 + SINC_TAPS],
            &table[a.row0 + SINC_TAPS..a.row0 + 2 * SINC_TAPS],
        );
        let f = &fifo[a.base..a.base + SINC_TAPS];
        let mut l = 0.0f32;
        let mut r = 0.0f32;
        for t in 0..SINC_TAPS {
            let w = r0[t] + (r1[t] - r0[t]) * a.rw;
            l += f[t][0] * w;
            r += f[t][1] * w;
        }
        o[0] = l;
        o[1] = r;
        *pos += step;
    }
}

impl OutResampler {
    fn new(device_rate: f64, max_block: usize, rt: Arc<RtCell>, epoch: u64) -> Self {
        let step = ENGINE_SR / device_rate;
        let flush_gen = rt.transport().out_flush_gen();
        // Peak FIFO occupancy in a sub-chunk: history (TAPS) + the span the chunk's read
        // positions cover + one render block of slack. Sized from the ACTUAL step so `push`
        // never reallocates on the RT callback.
        let cap = (max_block as f64 * step).ceil() as usize + max_block + SINC_TAPS + 4;
        let mut r = OutResampler {
            rt,
            epoch,
            step,
            pos: 0.0,
            origin: 0,
            consumed_floor: 0,
            fifo: Vec::with_capacity(cap),
            scratch: vec![0.0; max_block * CHANNELS],
            table: build_sinc_table(step),
            max_block,
            flush_gen,
            played_acc: 0.0,
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

    /// Renders one engine block into the FIFO. `false` = the cell denied this callback, and the
    /// FIFO was NOT extended — the caller must abandon the whole callback rather than pushing
    /// silence, because the consumed cursor is advanced from FIFO read progress: frames the
    /// processor never rendered would push `consumed_total` past `rendered_total` and jump the
    /// reported playhead.
    #[inline]
    #[must_use]
    fn render_block(&mut self) -> bool {
        let frames = self.max_block;
        let n = frames * CHANNELS;
        if !self
            .rt
            .try_render(self.epoch, &mut self.scratch[..n], frames)
        {
            return false;
        }
        for f in 0..frames {
            self.fifo
                .push([self.scratch[f * CHANNELS], self.scratch[f * CHANNELS + 1]]);
        }
        true
    }

    fn process(&mut self, out: &mut [Smp], out_frames: usize) {
        // Retired by a newer stream: own NOTHING from here on — not the FIFO, not `pos`, not
        // `consumed_floor`, and above all not the transport cursors. A zombie stamping those
        // produces a visibly wrong playhead even though the atomics themselves are race-free.
        // Checked before anything is read or advanced; silence is the honest output.
        if !self.rt.is_current(self.epoch) {
            out[..out_frames * CHANNELS].fill(0.0);
            return;
        }
        // The device plays this whole buffer, whatever below fills it (silence included).
        let handed = out_frames as f64 * self.step;
        self.played_acc += handed;
        let whole = self.played_acc as u64;
        self.played_acc -= whole as f64;
        self.rt.transport().count_played(whole);
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
            // One frame of margin: the kernel reaches that position by adding `step`
            // chunk-1 times, and the iterated sum drifts above the single
            // multiply-add here by rounding error (hundreds of ULPs, far below a
            // frame) — enough to cross an integer boundary, one frame past the
            // FIFO's end (an index panic on the IO thread). It grows with `pos`,
            // which only seek/pause/play resets: hours of uptime at 44.1 kHz with a
            // large device buffer reach it. The constructor's slack absorbs the frame.
            let last_pos = self.pos + (chunk.saturating_sub(1)) as f64 * self.step;
            let need_content = last_pos.floor() as i64 + SINC_HALF as i64 + 1;
            let need_len = (need_content - self.origin + 1) as usize;
            // The FIFO is sized in `new` so reaching `need_len` never reallocates.
            debug_assert!(need_len + self.max_block <= self.fifo.capacity());
            while self.fifo.len() < need_len {
                if !self.render_block() {
                    // Denied mid-callback (retired, or a zombie is still inside). Frames already
                    // written are real; the rest is silence, and nothing further is advanced.
                    out[done * CHANNELS..out_frames * CHANNELS].fill(0.0);
                    return;
                }
            }
            sinc_run(
                &self.table,
                &self.fifo,
                self.origin,
                &mut self.pos,
                self.step,
                &mut out[done * CHANNELS..(done + chunk) * CHANNELS],
            );
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
    rt.transport().begin_output_stream();
    let err_cb = move |err: cpal::Error| {
        // A stream following the system default reports a default change as DeviceChanged:
        // cpal already rerouted it and documents that it stays active with no rebuild needed.
        // Flagging it as lost made the host rebuild a working stream (an audible gap) and the
        // stale binding read as a reroute, hard-muting the monitor behind a false warning.
        if err.kind() == cpal::ErrorKind::DeviceChanged {
            return;
        }
        // No logging/alloc on the error path: flag the loss for the host's device-event poll.
        event.fetch_or(EVENT_OUTPUT_LOST, Ordering::Release);
    };
    // Retire every older output callback before this stream can produce a sample: a teardown
    // that failed to stop its predecessor no longer means two callbacks racing the cell.
    let epoch = rt.claim();
    let stream = if device_rate == ENGINE_SR {
        let rt = Arc::clone(&rt);
        device.build_output_stream(
            *config, // cpal 0.18 takes the config by value (StreamConfig is Copy)
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // A panic must not unwind into CoreAudio (cpal does not catch it, so the
                // process aborts): contain it and deliver silence, as the VPIO callbacks
                // and every FFI entry point do.
                let rendered = catch_unwind(AssertUnwindSafe(|| {
                    // Forced stereo: interleaved frames = len / 2. process() chunks to
                    // max_block.
                    let frames = data.len() / channels;
                    if !rt.try_render(epoch, data, frames) {
                        // Retired (or a zombie is still inside): silence, and DO NOT sync
                        // the consumed cursor — nothing was rendered to deliver.
                        data.fill(0.0);
                        return;
                    }
                    // Identity path: the render IS the delivery.
                    rt.transport().sync_consumed_to_rendered();
                }));
                if rendered.is_err() {
                    data.fill(0.0);
                }
            },
            err_cb,
            None,
        )
    } else {
        let mut res = OutResampler::new(device_rate, RESAMPLE_BLOCK, rt, epoch);
        device.build_output_stream(
            *config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Contained like the identity path's callback: a panic here aborted the app.
                let frames = data.len() / channels;
                if catch_unwind(AssertUnwindSafe(|| res.process(data, frames))).is_err() {
                    data.fill(0.0);
                }
            },
            err_cb,
            None,
        )
    }
    .map_err(|e| format!("build_output_stream: {e}"))?;
    stream.play().map_err(|e| format!("stream.play: {e}"))?;
    Ok(DeviceHandle {
        _stream: stream,
        follows_default: false,
        rate: config.sample_rate,
    })
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
    let mut stream = build_stream(&device, &config, Arc::clone(rt), Arc::clone(event))?;
    stream.follows_default = uid.is_none();
    let resolved = device_uid(&device).unwrap_or_default();
    Ok((stream, resolved))
}

/// Restore the cpal output after duplex VPIO released it (mic stopped) or failed to take it
/// (mic open/reopen failed): try the user's chosen device first, fall back to the system
/// default. Returns the handle (`None` when BOTH attempts failed) and the resolved uid
/// (empty when nothing opened). A double failure raises [`EVENT_OUTPUT_LOST`] — the same
/// contract as SetOutput's double failure — because with nothing open the binding reports
/// no reroute, the playhead's only clock (the output callback) is gone, and the Dart device
/// watch re-opens the output ONLY on this event; silence here would leave playback dead for
/// the rest of the session. Generic over the build attempt so the contract is testable
/// without CoreAudio.
fn restore_output_or_flag<H>(
    preferred: Option<&str>,
    event: &AtomicU32,
    mut build: impl FnMut(Option<&str>) -> Result<(H, String), String>,
) -> (Option<H>, String) {
    let built = build(preferred).or_else(|_| build(None)).ok();
    let resolved = built.as_ref().map(|(_, u)| u.clone()).unwrap_or_default();
    let handle = built.map(|(h, _)| h);
    if handle.is_none() {
        event.fetch_or(EVENT_OUTPUT_LOST, Ordering::Release);
    }
    (handle, resolved)
}

/// Whether duplex VPIO renders the engine's output in the cpal stream's place.
#[derive(Default, Clone, PartialEq, Eq, Debug)]
enum VpioOutput {
    /// No: the cpal stream, if one is open, renders.
    #[default]
    Off,
    /// A unit is being built (the mic opening, or reopening on another device). The cpal output
    /// is already gone and VPIO renders the engine — monitor included — from the moment it
    /// starts, before the host can say which endpoint it landed on: nothing the binding knows is
    /// evidence of where sound is going until the host rebinds to `Bound` or back to `Off`.
    Opening,
    /// Running, bound (via CurrentDevice) to this mic uid; empty = the system-default mic, where
    /// VPIO binds no device at all.
    Bound(String),
}

/// Whether the mic duplex VPIO is bound to is present right now AND has output streams, i.e. serves
/// the output leg too (see [`OutputBinding::renders_to_default`]). Asked of that one device on
/// every poll — two HAL reads, UID to device and its output stream list — rather than a walk over
/// every device (which ran on the Dart thread under the binding lock) or a fact cached at bind:
/// a bound headset can be unplugged, and one mid profile switch when the unit binds can report
/// no outputs for a moment, and either cached answer then held for the rest of the session.
#[cfg(target_vendor = "apple")]
fn bound_mic_serves_output(mic: &str) -> bool {
    crate::vpio::device_serves_output(mic)
}
/// Off Apple there is no duplex VPIO, so no mic is ever bound; the lookup only keeps this total.
#[cfg(not(target_vendor = "apple"))]
fn bound_mic_serves_output(mic: &str) -> bool {
    output_device_by_uid(mic).is_some()
}

/// What the engine's output is bound to right now, as the raw facts the audio-host thread owns.
/// The device actually rendering is DERIVED from these on demand ([`OutputBinding::actual_uid`])
/// rather than cached: while duplex VPIO owns the output and its bound mic has no output streams,
/// playback follows the SYSTEM DEFAULT output for the rest of the session, so a value resolved
/// once at mic-open time goes stale the moment the user changes the default in Sound preferences.
#[derive(Default)]
pub struct OutputBinding {
    /// The user's explicit output pick; empty = follow the system default.
    requested: String,
    /// The resolved uid of the open cpal output stream; empty = no cpal output.
    cpal_uid: String,
    /// The open cpal stream follows the system default (see [`DeviceHandle`]): `cpal_uid` is
    /// only what the default was when it was built.
    cpal_follows_default: bool,
    /// Duplex VPIO's share of the output (see [`VpioOutput`]).
    vpio: VpioOutput,
    /// Consecutive reconciles that found playback rerouted (see [`Self::rerouted`]). The event
    /// fires once, on the poll that makes it [`REROUTE_ANNOUNCE_POLLS`], instead of on every
    /// poll.
    rerouted_polls: u8,
    /// The open cpal stream's sample rate; 0 = none open.
    cpal_rate: u32,
    /// Consecutive reconciles that found a default-following stream stuck at a call-profile
    /// rate (see [`CALL_PROFILE_RATE_MAX`]).
    slow_rate_polls: u8,
    /// The output the host judged echo-free (headphones; `ve_set_echo_free`); empty = none. The
    /// echo canceller passes the mic through only while this is the device ACTUALLY rendering
    /// (see [`Self::echo_free_with`]): the host judges at its own pace and from its own device
    /// list, and a judgement left over from before the mic opened, or from before the default
    /// output moved to a speaker, would leave a whole echo in the take.
    echo_free_uid: String,
}

/// Consecutive rerouted polls before [`EVENT_OUTPUT_REROUTED`] is raised. The monitor mutes on
/// the first (safety does not wait), but one poll's answer alone can be a blip — a Bluetooth
/// headset's output list is briefly empty while VPIO switches it to its headset profile, a
/// transient HAL error — and every blip toasted "playback moved off your selected output".
const REROUTE_ANNOUNCE_POLLS: u8 = 2;

/// Rates (Hz) below this are a Bluetooth headset's call profile. A stream following the system
/// default that was built at one — the output restored the moment the mic closed, before the
/// headset had switched back — stays at it when the headset returns to its music profile:
/// nothing reports a rate change for a stream on the default (cpal watches only a bound device),
/// and the converter in front of the device band-limits playback to the call profile's, muffled
/// until something rebuilt the output. Once the default device has run faster for
/// [`REROUTE_ANNOUNCE_POLLS`] polls in a row (a profile switch passes through rates), the output
/// is reported lost, which the host's device watch answers by reopening it at the device's rate.
const CALL_PROFILE_RATE_MAX: u32 = 32_000;

impl OutputBinding {
    /// The device the engine is ACTUALLY rendering to; empty = nothing open (or unknowable, as
    /// while a VPIO unit is still opening).
    pub fn actual_uid(&self) -> String {
        self.actual_uid_given(self.renders_to_default(&bound_mic_serves_output))
    }

    /// [`Self::actual_uid`] once [`Self::renders_to_default`] has been decided.
    fn actual_uid_given(&self, renders_to_default: bool) -> String {
        if renders_to_default {
            return system_default_output_uid();
        }
        match &self.vpio {
            VpioOutput::Bound(mic) => mic.clone(),
            VpioOutput::Opening => String::new(),
            VpioOutput::Off => self.cpal_uid.clone(),
        }
    }

    /// Whether playback goes wherever the system default output is, rather than to one device.
    /// `serves_output` answers for a bound mic ([`bound_mic_serves_output`]; tests pass their own).
    fn renders_to_default(&self, serves_output: &dyn Fn(&str) -> bool) -> bool {
        match &self.vpio {
            // VPIO is ONE communication endpoint bound (via CurrentDevice) to the mic: when that
            // device also has output streams it serves both legs, and otherwise — including the
            // system-default mic, where no device is bound at all, and a bound device that has
            // since disappeared — playback goes to the system default output. The app-selected
            // output device is not part of this path.
            VpioOutput::Bound(mic) => mic.is_empty() || !serves_output(mic),
            // Not known yet: see `VpioOutput::Opening`.
            VpioOutput::Opening => false,
            // A default-following stream renders wherever the default is now; the uid resolved
            // at build time goes stale on the first default change and read as a reroute.
            VpioOutput::Off => self.cpal_follows_default && !self.cpal_uid.is_empty(),
        }
    }

    /// The user's pick VERBATIM — empty = "follow the system default", never resolved to whatever
    /// device that is right now. This is what the UI reads back and what the output-lost self-heal
    /// resubmits, so resolving it here would turn following the default into an explicit pick of
    /// one device and make its disappearance look like a reroute forever.
    pub fn requested_pick(&self) -> String {
        self.requested.clone()
    }

    /// The device the user asked for, resolved (an empty pick means the system default).
    fn requested_uid(&self) -> String {
        if self.requested.is_empty() {
            system_default_output_uid()
        } else {
            self.requested.clone()
        }
    }

    /// Playback is open but not on the device the user asked for. Nothing open is not a reroute:
    /// there is no sound to come out of the wrong speaker.
    pub fn rerouted(&self) -> bool {
        self.rerouted_with(&bound_mic_serves_output)
    }

    /// [`Self::rerouted`], asking `serves_output` about a bound mic (once).
    /// Record the output the host judged echo-free (empty = none); applied at the next reconcile.
    pub fn set_echo_free_uid(&mut self, uid: &str) {
        self.echo_free_uid.clear();
        self.echo_free_uid.push_str(uid);
    }

    /// Whether playback is on the output the host judged echo-free. Never while a VPIO unit is
    /// opening (nothing known renders yet).
    fn echo_free_with(&self, serves_output: &dyn Fn(&str) -> bool) -> bool {
        self.route_verdicts(serves_output).echo_free
    }

    fn rerouted_with(&self, serves_output: &dyn Fn(&str) -> bool) -> bool {
        self.route_verdicts(serves_output).rerouted
    }

    /// Both verdicts a reconcile needs, from ONE read of the route. Asked separately, a poll paid
    /// two hardware reads (each a uid lookup and an output-streams query on the UI thread), and a
    /// headset switching profile between them had the monitor mute and the echo-free flag judge
    /// different routes.
    fn route_verdicts(&self, serves_output: &dyn Fn(&str) -> bool) -> RouteVerdicts {
        let to_default = self.renders_to_default(serves_output);
        // Following the default as asked is never a reroute: resolving the default once per side
        // could straddle a default change and fire a false one (and a monitor mute) for one poll.
        let following = self.requested.is_empty() && to_default;
        let actual = if following && self.echo_free_uid.is_empty() {
            String::new() // nothing below needs it
        } else {
            self.actual_uid_given(to_default)
        };
        RouteVerdicts {
            rerouted: !following && !actual.is_empty() && actual != self.requested_uid(),
            echo_free: !self.echo_free_uid.is_empty() && actual == self.echo_free_uid,
        }
    }
}

/// What one read of the output route says (see [`OutputBinding::route_verdicts`]).
struct RouteVerdicts {
    /// Playback is open, but not on the device the user asked for.
    rerouted: bool,
    /// Playback is on the output the host judged echo-free.
    echo_free: bool,
}

/// The rate (Hz) the system default output device runs at now, or None when it cannot be read.
fn system_default_output_rate() -> Option<u32> {
    default_output_device()?
        .default_output_config()
        .ok()
        .map(|c| c.sample_rate())
}

/// The system default output device's uid, or empty when it cannot be resolved.
fn system_default_output_uid() -> String {
    default_output_device()
        .and_then(|d| device_uid(&d))
        .unwrap_or_default()
}

/// Re-derive whether playback has left the user's chosen output and apply the consequence:
/// the hear-yourself monitor is HARD-MUTED for as long as it has. `monitorAllowed` in Dart judges
/// feedback safety from the device the user picked, which stops being evidence the moment the
/// engine renders somewhere else — most sharply when duplex VPIO takes the output while the mic
/// opens, which moves the backing track AND the (uncancelled) monitor onto the mic's device or
/// the system default, typically the built-in speakers.
///
/// The mute is a separate flag from the UI's monitor gain so a `set_monitor_gain` from the fader
/// cannot lift it. It clears itself as soon as playback is back on the chosen device, so no other
/// thread has to know about it.
///
/// While a VPIO unit is OPENING the mute is held whatever else the binding says: the unit renders
/// the monitor from the moment it starts, and the facts left from before the open (the cpal
/// output it replaced, or the previous unit's mic) describe a device that is no longer playing.
/// The reroute verdict, and with it the event's edge, waits for the host's rebind.
///
/// Callable from the host thread (after any command that can move the binding) and from the FFI
/// poll (so a mid-session change of the SYSTEM default is caught within a tick). The event fires
/// only on the transition into the rerouted state. Returns whether the monitor is now held muted.
/// Every write of the mute happens under the binding lock, so a poll that read the binding just
/// before the host moved it cannot land its verdict after the host's.
pub fn reconcile_output_safety(
    transport: &Shared,
    binding: &Mutex<OutputBinding>,
    event: &AtomicU32,
) -> bool {
    reconcile_output_safety_with(
        transport,
        binding,
        event,
        &bound_mic_serves_output,
        &system_default_output_rate,
    )
}

/// Re-derive only whether playback is echo-free (see [`OutputBinding::echo_free_uid`]), for a
/// new judgement from the host to apply at once. The counters [`reconcile_output_safety`] keeps
/// for its two-poll debounces are left alone: running the whole reconcile here made every host
/// tick count twice (the poll's, then this), and a one-poll blip — a headset's output list empty
/// while it switches profile — toasted "playback moved".
pub fn reconcile_echo_free(transport: &Shared, binding: &Mutex<OutputBinding>) {
    reconcile_echo_free_with(transport, binding, &bound_mic_serves_output);
}

fn reconcile_echo_free_with(
    transport: &Shared,
    binding: &Mutex<OutputBinding>,
    serves_output: &dyn Fn(&str) -> bool,
) {
    if let Ok(b) = binding.lock() {
        transport.set_echo_free(b.echo_free_with(serves_output));
    }
}

/// [`reconcile_output_safety`], asking `serves_output` about a bound mic and `default_rate` for
/// the system default output's current rate (tests pass their own).
fn reconcile_output_safety_with(
    transport: &Shared,
    binding: &Mutex<OutputBinding>,
    event: &AtomicU32,
    serves_output: &dyn Fn(&str) -> bool,
    default_rate: &dyn Fn() -> Option<u32>,
) -> bool {
    let Ok(mut b) = binding.lock() else {
        return false;
    };
    if b.vpio == VpioOutput::Opening {
        transport.set_monitor_muted(true);
        transport.set_echo_free(false);
        return true;
    }
    let RouteVerdicts {
        rerouted,
        echo_free,
    } = b.route_verdicts(serves_output);
    transport.set_monitor_muted(rerouted);
    transport.set_echo_free(echo_free);
    b.rerouted_polls = if rerouted {
        b.rerouted_polls.saturating_add(1)
    } else {
        0
    };
    if b.rerouted_polls == REROUTE_ANNOUNCE_POLLS {
        event.fetch_or(EVENT_OUTPUT_REROUTED, Ordering::Release);
    }
    // See CALL_PROFILE_RATE_MAX. The device is asked only while a stream sits at such a rate.
    let stuck = b.vpio == VpioOutput::Off
        && b.cpal_follows_default
        && b.cpal_rate > 0
        && b.cpal_rate < CALL_PROFILE_RATE_MAX
        && default_rate().is_some_and(|r| r > b.cpal_rate);
    b.slow_rate_polls = if stuck {
        b.slow_rate_polls.saturating_add(1)
    } else {
        0
    };
    if b.slow_rate_polls == REROUTE_ANNOUNCE_POLLS {
        event.fetch_or(EVENT_OUTPUT_LOST, Ordering::Release);
    }
    rerouted
}

/// Input-side shared cell: the active input stream's callback feeds mic PCM through it
/// (downmix to stereo + resample to `ENGINE_SR`) into the mic ring whose consumer lives in the
/// `RtProcessor`. Only the one active input callback touches it, and the host thread reconfigures
/// it only while no input stream is running. It persists across input-device changes so the mic
/// ring is created exactly once.
///
/// The single-accessor rule is enforced the same way [`RtCell`]'s is, and for the same reason: a
/// failed stream teardown can leave a zombie callback running past the handle that owned it, and
/// two input callbacks would be two `&mut InputState` AND two producers on rings that are
/// single-producer by construction — corruption, not merely UB-by-contract. See [`RtCell`] for
/// what the epoch and the busy flag each cover.
pub struct InputCell {
    state: UnsafeCell<InputState>,
    owner: AtomicU64,
    busy: AtomicBool,
    /// Which of a multi-channel mic's inputs make the mono capture (`ve_set_input_channel`; see
    /// [`InputChannelMix`]). Outside the single-accessor state: the control thread writes it, and
    /// the VPIO input callback reads it for its own downmix before it feeds the cell.
    channel_mode: AtomicU32,
    /// The largest callback's share of the nominal input counter since the drift estimator
    /// last took it (see [`Self::take_quantum`]).
    quantum: AtomicU64,
}

// SAFETY: see the type doc — the owner epoch plus the `busy` compare-exchange admit at most one
// caller into the inner state at a time.
unsafe impl Send for InputCell {}
unsafe impl Sync for InputCell {}

/// Proof that the holder is the input cell's single current accessor. Releases on drop.
struct InputGuard<'a> {
    cell: &'a InputCell,
}

impl Drop for InputGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.cell.busy.store(false, Ordering::Release);
    }
}

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
    /// The device's channels to the ONE mono mic signal (under [`InputCell::channel_mode`]),
    /// carried on both stereo lanes, so the monitor, meter and take hear the chosen input centred.
    mix: InputChannelMix,
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
    /// Shared device-event flags (`ve_poll_device_event`). The fan-out raises
    /// [`EVENT_RECORD_LIMIT`] here when a take reaches the length its WAV
    /// header can describe — the same latched-bit channel the input paths use
    /// for device loss, and the only way an RT callback can reach the UI.
    event: Arc<AtomicU32>,
    /// Engine-rate frames this input WOULD have emitted at ratio 1.0 — the
    /// drift estimator's input signal. It must NOT include the correction:
    /// measuring the corrected emission fed the published ratio back into
    /// its own error signal, and the loop settled at sqrt(realRatio) — only
    /// half the ppm ever compensated (~15 ppm residual on a 30 ppm mic).
    nominal_frames_in: Arc<AtomicU64>,
    /// Fractional carry for [`nominal_frames_in`] (device frames / step).
    nominal_acc: f64,
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
            // The take is a canonical RIFF file whose `data`/RIFF sizes are u32: a sample
            // past wav::MAX_F32_SAMPLES can be appended but never ADDRESSED, so recording
            // stops at that count and says so instead of growing an invisible tail. The
            // comparison is on rec_written because the take is MONO — one recorded frame is
            // exactly one stored sample — which is what keeps this gate and the writer
            // thread's own refusal describing the same boundary. Checked per frame, not per
            // callback, so the stop lands exactly on the limit. NOT a drop: a drop is a hole
            // inside an otherwise complete take, and this take is complete, just at its end.
            if self.rec_written.load(Ordering::Relaxed) >= crate::wav::MAX_F32_SAMPLES {
                self.event.fetch_or(EVENT_RECORD_LIMIT, Ordering::Release);
            } else if self.rec.push(f) {
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
        nominal_frames_in: Arc<AtomicU64>,
        event: Arc<AtomicU32>,
    ) -> Arc<InputCell> {
        Arc::new(InputCell {
            state: UnsafeCell::new(InputState {
                mic,
                cap,
                rec,
                cap_active,
                rec_active,
                in_channels: 1,
                mix: InputChannelMix::new(ENGINE_SR),
                step: 1.0,
                drift_ratio_bits,
                pos: 0.0,
                prev: [0.0, 0.0],
                primed: false,
                level,
                frames_in,
                rec_dropped,
                rec_written,
                nominal_frames_in,
                nominal_acc: 0.0,
                event,
            }),
            // 0 = unclaimed; the first claim is 1, so no callback can hold a matching epoch by
            // accident. See [`RtCell`].
            owner: AtomicU64::new(0),
            busy: AtomicBool::new(false),
            channel_mode: AtomicU32::new(INPUT_CHANNEL_AUTO),
            quantum: AtomicU64::new(0),
        })
    }

    /// Choose which input(s) make the mono capture: [`INPUT_CHANNEL_AUTO`], or
    /// `capture_layout::INPUT_CHANNEL_1` / `INPUT_CHANNEL_2`. Takes effect on the next callback,
    /// gliding (see [`InputChannelMix`]). Any thread.
    pub fn set_channel_mode(&self, mode: u32) {
        self.channel_mode.store(mode, Ordering::Relaxed);
    }

    /// The input-channel mode in force (see [`Self::set_channel_mode`]). RT-safe: one load.
    #[inline]
    pub fn channel_mode(&self) -> u32 {
        self.channel_mode.load(Ordering::Relaxed)
    }

    /// Take the next input-stream generation. Call on the AUDIO-HOST THREAD, immediately before
    /// building an input stream; taking it retires every older input callback.
    pub fn claim(&self) -> u64 {
        self.owner.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// The current input-stream generation: it moves whenever a stream (re)opens, which the
    /// drift estimator takes as a new clock.
    pub fn generation(&self) -> u64 {
        self.owner.load(Ordering::Acquire)
    }

    /// The largest single callback's advance of the nominal input counter (engine frames) since
    /// the last call, which resets it: how far that counter can lag its device.
    pub fn take_quantum(&self) -> u64 {
        self.quantum.swap(0, Ordering::Relaxed)
    }

    /// Enter the state as `epoch`, or `None` when a newer input stream owns the cell or another
    /// accessor is inside right now.
    #[inline]
    fn try_enter(&self, epoch: u64) -> Option<InputGuard<'_>> {
        if self.owner.load(Ordering::Acquire) != epoch {
            return None;
        }
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        let guard = InputGuard { cell: self };
        // Re-check under `busy`: `reconfigure` can retire this epoch between the check
        // above and the compare-exchange, and an entry admitted on that stale check
        // would decode its samples under the format installed for the NEXT stream.
        if self.owner.load(Ordering::Acquire) != epoch {
            return None; // the guard's drop releases `busy`
        }
        Some(guard)
    }

    /// Feed one input callback's samples as `epoch`. `false` = denied (a retired callback, or a
    /// zombie still inside): the samples are DROPPED rather than raced into rings that have one
    /// producer by construction.
    #[inline]
    pub fn try_feed(&self, epoch: u64, data: &[f32]) -> bool {
        self.try_feed_gained(epoch, data, 1.0)
    }

    /// [`try_feed`](Self::try_feed), scaling the mono mic signal by `gain` after the channel
    /// mix — the record gain, which the mix's guard against garbage samples (past +24 dBFS)
    /// must never see: applied ahead of it, every peak a hot gain lifted past that was recorded
    /// as silence.
    #[inline]
    pub fn try_feed_gained(&self, epoch: u64, data: &[f32], gain: f32) -> bool {
        let Some(_g) = self.try_enter(epoch) else {
            return false;
        };
        // SAFETY: the guard is the cell's single-accessor proof for this call.
        unsafe { self.feed(data, gain) };
        true
    }

    /// Take the cell's single-accessor flag, SPINNING until it is free. HOST THREAD ONLY: an
    /// audio callback must never block on this (it uses [`try_enter`](Self::try_enter) and drops
    /// its samples instead), and the only contention is a zombie callback finishing one buffer.
    fn enter_blocking(&self) -> InputGuard<'_> {
        while self
            .busy
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        InputGuard { cell: self }
    }

    /// Install the next input stream's native format (rate + channels) and mint the generation
    /// that stream must feed as. Call on the HOST THREAD, after dropping the old stream and
    /// before starting the new one. Retirement is enforced here, not assumed: the owner
    /// generation advances BEFORE the format lands, so a zombie callback that survived a failed
    /// teardown is denied instead of having its samples decoded under a format it never
    /// delivered (e.g. a 24 kHz unit's frames emitted 1:1 as 48 kHz).
    pub fn reconfigure(&self, in_rate: f64, in_channels: usize) -> u64 {
        // Retire first: once the owner moves, no older generation can enter.
        let epoch = self.claim();
        // A retired callback may still be INSIDE `feed` (it entered before the owner moved).
        // Take `busy` — the same exclusion every accessor takes — so the format switch below
        // cannot race its state writes. The wait is bounded by one callback's duration.
        let _g = self.enter_blocking();
        // SAFETY: the guard is the cell's single-accessor proof for this call, and every
        // older generation is retired.
        let st = unsafe { &mut *self.state.get() };
        st.in_channels = in_channels.max(1);
        st.mix = InputChannelMix::new(in_rate);
        st.step = in_rate / ENGINE_SR;
        st.pos = 0.0;
        st.primed = false;
        st.prev = [0.0, 0.0];
        st.nominal_acc = 0.0;
        // The meter holds the previous session's last peak until a callback overwrites it,
        // and the new stream's first callback can be hundreds of ms away (a VPIO open): the
        // next take's live waveform and the mic meter started on that stale peak.
        st.level.store(0, Ordering::Relaxed);
        epoch
    }

    /// Test-only direct feed. Tests drive the cell with no stream around it, so claim the first
    /// generation on first use and then feed as it — exercising the same admission path a real
    /// callback takes rather than side-stepping it.
    #[cfg(test)]
    pub(crate) fn feed_test(&self, data: &[f32]) {
        let mut epoch = self.owner.load(Ordering::Acquire);
        if epoch == 0 {
            epoch = self.claim();
        }
        assert!(
            self.try_feed(epoch, data),
            "the cell must admit its only accessor"
        );
    }

    /// Feed one input callback's interleaved samples: mix the device's channels down to the mono
    /// mic signal (carried as L = R) and scale it by `gain`, resample to `ENGINE_SR`, fan out to
    /// the mic / capture / recording rings, and update the peak meter.
    ///
    /// # Safety
    /// Only the single active input callback may call this.
    unsafe fn feed(&self, data: &[f32], gain: f32) {
        let st = &mut *self.state.get();
        let ch = st.in_channels;
        if ch == 0 {
            return;
        }
        let cap = st.cap_active.load(Ordering::Relaxed);
        let rec = st.rec_active.load(Ordering::Relaxed);
        let mode = self.channel_mode();
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
            // Inputs 1 and 2 used to travel as left and right, and the writer keeps the left: a
            // mic on input 2 recorded silence, monitored hard right.
            let m = st.mix.mix(mode, &data[base..base + ch]) * gain;
            let (l, r) = (m, m);
            peak = peak.max(m.abs());
            if identity {
                st.emit([l, r], cap, rec);
                emitted += 1;
                // Keep the resampler's state coherent while the fast path owns
                // the stream. The drift estimator publishes EXACTLY 1.0 inside
                // its ±2 ppm deadband, so a chain hovering near that edge
                // crosses back and forth; leaving `prev` behind meant the first
                // interpolation after a crossing blended the current sample
                // with one from an arbitrarily older callback — a single-sample
                // step out of nowhere, i.e. a click.
                //
                // The phase left behind is 1.0, not 0.0: positions in the
                // interpolating branch are measured from `prev`, this sample
                // has just been emitted in full, and the next output is due one
                // whole input later — exactly `prev + 1`. Leaving 0.0 would
                // re-emit the sample identity just sent (a duplicate), and the
                // stale value left before this fix was worse still.
                st.prev = [l, r];
                st.primed = true;
                st.pos = 1.0;
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
        // The estimator's signal: engine-rate frames at the NOMINAL step,
        // untouched by the published ratio (see nominal_frames_in). Carried
        // fractionally so non-integer steps lose nothing across callbacks.
        st.nominal_acc += frames as f64 / st.step;
        self.quantum
            .fetch_max((frames as f64 / st.step).ceil() as u64, Ordering::Relaxed);
        let whole = st.nominal_acc as u64;
        if whole > 0 {
            st.nominal_frames_in.fetch_add(whole, Ordering::Relaxed);
            st.nominal_acc -= whole as f64;
        }
        st.level.store(peak.to_bits(), Ordering::Relaxed);
    }
}

/// Control-side clock-drift estimator: compares the growth of the input's
/// NOMINAL engine-rate frame counter (device frames / nominal step — never
/// the corrected emission, which would feed the published ratio back into
/// its own error signal and settle the loop at sqrt of the real ratio)
/// against the frames the output devices played
/// ([`Shared::played_frames`]), and publishes a correction ratio for the
/// input resampler ([`InputState::drift_ratio_bits`]).
///
/// Two independent crystals never run at exactly the same rate; a mic 30 ppm
/// fast drifts a take ~18 ms ahead of the backing over 10 minutes, which
/// pre-roll (a START-time constant) cannot fix. Aligning the input to the
/// OUTPUT clock is the goal — the take must sit against what the listener
/// heard — so both absolute crystal errors cancel out of the design.
///
/// Both counters move in whole callbacks, so each poll reads them up to a
/// callback each off their clocks; a 4096-frame callback is 85 ms, worth
/// 140 ppm of a ten-minute span. The design follows from that, and from
/// what the estimate costs when it is wrong:
///  * one chain on one clock (a duplex device is one crystal; VPIO's input
///    and output share an IO cycle, and with it nothing is estimated at all,
///    see [`MIC_ON_OUTPUT_CLOCK`]) must read EXACTLY 1.0, which keeps the
///    resampler's identity fast path. A noisy fit published as drift slipped
///    such takes by up to 16 ms where nothing drifted. So nothing is published
///    until the fit says it beats the noise: its slope must clear
///    `(4 + 4/dof)` standard errors and 6 ppm, and until then the published
///    ratio holds. A published correction returns to exactly 1.0 once the fit
///    reads under half that bound;
///  * samples are about a second apart on the WALL clock, and only while both
///    counters move (a closed mic or a lost output freezes the estimate). A
///    sample taken when the counters crossed a span landed right after a
///    callback, every time, and read one quantization level for minutes;
///  * ten samples make a point: their means, which keep the slow drift of
///    the callbacks' phases (equal-sized callbacks show drift only as the
///    share of samples one level up), and the samples' spread around them;
///  * the slope is a least-squares fit over ten minutes of points, with one
///    offset per segment: a stall (an underrun, a lost burst of input) moves
///    the counters' difference for good, and a fit through the step took a
///    100 ms output stall for 250 ppm of drift. A change between
///    two samples larger than a callback each way cannot be quantization and
///    splits the segment there. A smaller step shows as the segment's points
///    parting into two offsets under one slope: past six standard errors (and
///    three quarters of the smaller callback, a lost one) it splits the
///    segment, and past three it holds publishing for a few minutes while it
///    might. Early in a segment a step reads as slope (one lost 1024-frame
///    callback published 114 ppm on one clock), so a slope publishes only if
///    it would also hold with a callback-sized step anywhere in the segment;
///  * a segment whose own slope disagrees with the older ones' (a device
///    change on the same stream, at a new rate) drops them;
///  * a new input or output stream — a reopened mic, a rebuilt output, maybe
///    another device — restarts the history. The published ratio holds until
///    the new stream's fit rules it out, or shows a drift of its own more
///    precisely than the published fit did, for the stream's first ten
///    minutes at most; then it counts for nothing, as if the stream had started
///    on 1.0. A mic reopened on the same pair, the common case, keeps its
///    correction; the cost is that takes shorter than that on another pair
///    keep a ratio up to their fits' tolerance off (8 ppm, from 128-frame
///    takes of three minutes at 22 ppm after a stream at 30);
///  * the correction clamps at ±500 ppm: anything larger is a broken device,
///    not drift.
///
/// The mechanism is simulation-tested (`drift_tests`); hardware-in-the-loop
/// CALIBRATION (real crystal pairs) remains the follow-up noted in memory.
pub struct DriftEstimator {
    ratio_bits: Arc<AtomicU64>,
    generation: Option<u64>,
    /// The counters at the start of the history; the fit runs relative to them.
    origin: (u64, u64),
    /// The last sample's counters and wall time.
    last: (u64, u64),
    last_secs: f64,
    /// The largest callback seen on each side since the history started (engine frames).
    quantum: (u64, u64),
    /// The open point's samples: (output, input - output) relative to `origin`.
    window: Vec<(f64, f64)>,
    points: std::collections::VecDeque<DriftPoint>,
    /// The current segment; the next point opens a new one when `split` is set.
    seg: u32,
    split: bool,
    fit: Option<DriftFit>,
    /// The standard error of the fit last published.
    published_se: f64,
    /// The published ratio was carried over from an older stream.
    held_over: bool,
    /// Points closed since the history started.
    history: usize,
}

/// One poll of the counters, for [`DriftEstimator::tick`].
#[derive(Clone, Copy, Debug)]
pub struct DriftObservation {
    /// The input's nominal engine-rate frame counter.
    pub input: u64,
    /// Engine-rate frames the output devices played.
    pub output: u64,
    /// The largest single callback on each side since the last poll (engine frames).
    pub input_quantum: u64,
    pub output_quantum: u64,
    /// Changes whenever either side starts a new stream.
    pub generation: u64,
    /// Wall-clock seconds, from any fixed origin.
    pub secs: f64,
}

#[derive(Clone, Copy)]
struct DriftPoint {
    x: f64,
    m: f64,
    seg: u32,
    /// The samples' spread about the trend: the quantization this point saw.
    range: f64,
}

#[derive(Clone, Copy)]
struct DriftFit {
    slope: f64,
    se: f64,
    dof: f64,
    /// Σ(x - x̄)² within the segments.
    sxx: f64,
}

impl DriftFit {
    /// How far the slope may be from a value the fit still allows.
    fn tol(&self) -> f64 {
        ((4.0 + 4.0 / self.dof) * self.se).max(2.0 * DRIFT_DEADBAND)
    }
}

/// Samples per point (~10 s).
const DRIFT_WINDOW: usize = 10;
/// Points the fit spans (~10 min).
const DRIFT_POINTS: usize = 60;
/// Points a history needs before it publishes (~40 s).
const DRIFT_MIN_POINTS: usize = 4;
/// The least advance (engine frames) on both counters between samples: half a second's.
const DRIFT_SAMPLE_SPAN: u64 = 24_000;
/// A step smaller than this (engine frames) is not looked for: 0.5 ms.
const DRIFT_STEP_FLOOR: f64 = 24.0;
/// Points after a suspected step during which it holds publishing (~4 min).
const DRIFT_STEP_YOUTH: usize = 20;
/// |error| up to this publishes exactly 1.0; leaving 1.0 takes twice it.
const DRIFT_DEADBAND: f64 = 3e-6;
/// The correction's bound: past it the device is broken, not drifting.
const DRIFT_CLAMP: f64 = 500e-6;

impl DriftEstimator {
    pub fn new(ratio_bits: Arc<AtomicU64>) -> Self {
        ratio_bits.store(1.0f64.to_bits(), Ordering::Relaxed);
        DriftEstimator {
            ratio_bits,
            generation: None,
            origin: (0, 0),
            last: (0, 0),
            last_secs: 0.0,
            quantum: (0, 0),
            window: Vec::with_capacity(DRIFT_WINDOW),
            points: std::collections::VecDeque::with_capacity(DRIFT_POINTS + 1),
            seg: 0,
            split: false,
            fit: None,
            published_se: f64::INFINITY,
            held_over: false,
            history: 0,
        }
    }

    /// One poll (~1 s from the song page, 50 ms from the technique page; any
    /// number of pollers). Publishes the updated ratio; returns it for tests.
    pub fn tick(&mut self, o: DriftObservation) -> f64 {
        if self.generation != Some(o.generation) || o.input < self.last.0 || o.output < self.last.1
        {
            self.generation = Some(o.generation);
            self.origin = (o.input, o.output);
            self.last = (o.input, o.output);
            self.last_secs = o.secs;
            // This poll's callbacks may be the old streams'.
            self.quantum = (0, 0);
            self.window.clear();
            self.points.clear();
            self.split = false;
            self.fit = None;
            self.held_over = self.published() != 1.0;
            self.history = 0;
            return self.published();
        }
        self.quantum.0 = self.quantum.0.max(o.input_quantum);
        self.quantum.1 = self.quantum.1.max(o.output_quantum);
        let din = o.input - self.last.0;
        let dout = o.output - self.last.1;
        if o.secs - self.last_secs < 1.0 || din < DRIFT_SAMPLE_SPAN || dout < DRIFT_SAMPLE_SPAN {
            return self.published();
        }
        self.last = (o.input, o.output);
        self.last_secs = o.secs;
        // A change since the last sample that callback granularity cannot make (each counter
        // lags its device by less than one of its callbacks): a step, between these samples.
        // Until a fit says how fast the counters part, any drift short of 1% is allowed for.
        let q = self.quantum.0 as f64 + self.quantum.1 as f64;
        let slack = if self.fit.is_some() { 1e-3 } else { 1e-2 };
        let delta = (din as f64 - dout as f64) - self.reference_slope() * dout as f64;
        if self.quantum.0 > 0
            && self.quantum.1 > 0
            && delta.abs() > q + 2.0 + slack * dout as f64 + DRIFT_STEP_FLOOR
        {
            self.close_window();
            self.split = true;
        }
        let x = (o.output - self.origin.1) as f64;
        self.window.push((x, (o.input - self.origin.0) as f64 - x));
        if self.window.len() == DRIFT_WINDOW {
            match self.step_in_window(slack) {
                Some(at) => {
                    let after = self.window.split_off(at);
                    self.close_window();
                    self.split = true;
                    self.window = after;
                }
                None => self.close_window(),
            }
        }
        self.published()
    }

    fn reference_slope(&self) -> f64 {
        self.fit.map_or(self.published() - 1.0, |f| f.slope)
    }

    /// Where the open window's samples part by more than callback granularity lets them —
    /// a step smaller than both callbacks together passes the check between two samples,
    /// but not this one, and a point averaged across it sat half-stepped at a segment's
    /// edge and tilted the fit by 100 ppm: the boundary that parts them most.
    fn step_in_window(&self, slack: f64) -> Option<usize> {
        let n = self.window.len();
        if self.quantum.0 == 0 || self.quantum.1 == 0 || n < 2 {
            return None;
        }
        let (x0, x1) = (self.window[0].0, self.window[n - 1].0);
        let d = self.reference_slope();
        let v: Vec<f64> = self.window.iter().map(|&(x, z)| z - d * (x - x0)).collect();
        let (lo, hi) = v
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &v| {
                (l.min(v), h.max(v))
            });
        let q = self.quantum.0 as f64 + self.quantum.1 as f64;
        if hi - lo <= q + 2.0 + slack * (x1 - x0) + DRIFT_STEP_FLOOR {
            return None;
        }
        let parting = |at: usize| {
            let (a, b) = v.split_at(at);
            let mean = |s: &[f64]| s.iter().sum::<f64>() / s.len() as f64;
            (mean(b) - mean(a)).abs() * ((a.len() * b.len()) as f64).sqrt()
        };
        (1..n).max_by(|&a, &b| parting(a).total_cmp(&parting(b)))
    }

    /// Turns the open window into a point (half a window at least; less is dropped), or
    /// splits the segment ahead of it.
    fn close_window(&mut self) {
        let n = self.window.len();
        if n < DRIFT_WINDOW / 2 {
            self.window.clear();
            return;
        }
        let xm = self.window.iter().map(|p| p.0).sum::<f64>() / n as f64;
        let m = self.window.iter().map(|p| p.1).sum::<f64>() / n as f64;
        let d = self.reference_slope();
        let (lo, hi) = self
            .window
            .iter()
            .map(|&(x, z)| z - d * (x - xm))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| {
                (l.min(v), h.max(v))
            });
        self.window.clear();
        if self.split || self.points.is_empty() {
            self.seg += 1;
            self.split = false;
        }
        // Samples that all read one quantization level spread by nothing, yet their mean can
        // sit anywhere within a callback of the truth: with equal callbacks in step, every
        // point did, and the fit read its slope to a fraction of a ppm.
        let quantization = self.quantum.0.max(self.quantum.1) as f64;
        self.points.push_back(DriftPoint {
            x: xm,
            m,
            seg: self.seg,
            range: (hi - lo).max(quantization),
        });
        if self.points.len() > DRIFT_POINTS {
            self.points.pop_front();
        }
        self.history += 1;
        self.split_at_step();
        self.prune_inconsistent();
        self.fit = pooled_fit(self.points.iter().copied());
        self.publish();
    }

    /// Splits the current segment at a step inside it (see [`Self::likeliest_step`]) that
    /// clears the noise six times over.
    fn split_at_step(&mut self) {
        let Some((j, z)) = self.likeliest_step() else {
            return;
        };
        if z <= 6.0 {
            return;
        }
        let start = self
            .points
            .iter()
            .position(|p| p.seg == self.seg)
            .unwrap_or(0);
        self.seg += 1;
        let new = self.seg;
        for p in self.points.iter_mut().skip(start + j) {
            p.seg = new;
        }
    }

    /// The smallest step looked for: three quarters of the smaller callback (a lost callback
    /// is a whole one, on either side), at most a quarter of both. `None` until each side has
    /// reported its callbacks.
    fn least_step(&self) -> Option<f64> {
        let (qi, qo) = (self.quantum.0 as f64, self.quantum.1 as f64);
        if qi == 0.0 || qo == 0.0 {
            return None;
        }
        Some((0.25 * (qi + qo)).min(0.75 * qi.min(qo)) + DRIFT_STEP_FLOOR)
    }

    /// The boundary in the current segment where a step is likeliest — where offsets fitted
    /// either side, under a common slope, part the most against their noise — and how many
    /// standard errors that clears. Only steps past [`Self::least_step`] count.
    /// Using every point either side, it finds a step a point-by-point test against the line
    /// cannot: early in a fit the slope is too uncertain to predict a point to within one.
    fn likeliest_step(&self) -> Option<(usize, f64)> {
        let least = self.least_step()?;
        let start = self.points.iter().position(|p| p.seg == self.seg)?;
        let pts: Vec<DriftPoint> = self.points.iter().skip(start).copied().collect();
        let n = pts.len();
        if n < 6 {
            return None;
        }
        // (count, mean x, mean m, Sxx, Sxy, Smm) of a run of points.
        let stats = |ps: &[DriftPoint]| {
            let k = ps.len() as f64;
            let xm = ps.iter().map(|p| p.x).sum::<f64>() / k;
            let mm = ps.iter().map(|p| p.m).sum::<f64>() / k;
            let (mut sxx, mut sxy, mut smm) = (0.0, 0.0, 0.0);
            for p in ps {
                sxx += (p.x - xm) * (p.x - xm);
                sxy += (p.x - xm) * (p.m - mm);
                smm += (p.m - mm) * (p.m - mm);
            }
            (k, xm, mm, sxx, sxy, smm)
        };
        let prior = pts.iter().map(|p| (0.3 * p.range).powi(2)).sum::<f64>() / n as f64;
        let mut best: Option<(usize, f64)> = None;
        for j in 3..=n - 3 {
            let (ka, xa, ma, sxxa, sxya, smma) = stats(&pts[..j]);
            let (kb, xb, mb, sxxb, sxyb, smmb) = stats(&pts[j..]);
            let sxx = sxxa + sxxb;
            if sxx <= 0.0 {
                continue;
            }
            let slope = (sxya + sxyb) / sxx;
            let rss = (smma + smmb - slope * (sxya + sxyb)).max(0.0);
            // Blended with the spreads' prior as in [`pooled_fit`].
            let var = (rss + 4.0 * prior) / (n as f64 - 3.0 + 4.0);
            let step = (mb - slope * xb) - (ma - slope * xa);
            let se = (var * (1.0 / ka + 1.0 / kb + (xb - xa) * (xb - xa) / sxx)).sqrt();
            let z = if se > 0.0 {
                step.abs() / se
            } else {
                f64::INFINITY
            };
            if step.abs() > least && best.is_none_or(|b| z > b.1) {
                best = Some((j, z));
            }
        }
        best
    }

    /// Drops the older segments when the newest one's slope disagrees with theirs.
    fn prune_inconsistent(&mut self) {
        let newest = self.seg;
        let in_newest = self.points.iter().filter(|p| p.seg == newest).count();
        if in_newest < DRIFT_MIN_POINTS || in_newest == self.points.len() {
            return;
        }
        let (Some(new), Some(old)) = (
            pooled_fit(self.points.iter().copied().filter(|p| p.seg == newest)),
            pooled_fit(self.points.iter().copied().filter(|p| p.seg != newest)),
        ) else {
            return;
        };
        if (new.slope - old.slope).abs() > 4.0 * new.se.hypot(old.se) + DRIFT_DEADBAND {
            self.points.retain(|p| p.seg == newest);
        }
    }

    /// Fits of the history with one more step in the current segment, at each boundary where
    /// the fit cannot rule out a step a callback could make: what the slope must hold up
    /// against.
    fn with_a_step(&self) -> Vec<DriftFit> {
        let (Some(least), Some(start)) = (
            self.least_step(),
            self.points.iter().position(|p| p.seg == self.seg),
        ) else {
            return Vec::new();
        };
        let mut pts: Vec<DriftPoint> = self.points.iter().copied().collect();
        let n = pts.len() - start;
        let mean = |ps: &[DriftPoint]| {
            let k = ps.len() as f64;
            (
                ps.iter().map(|p| p.x).sum::<f64>() / k,
                ps.iter().map(|p| p.m).sum::<f64>() / k,
            )
        };
        let mut fits = Vec::new();
        for j in 2..=n.saturating_sub(2) {
            for (i, p) in pts.iter_mut().enumerate().skip(start) {
                p.seg = if i < start + j {
                    self.seg
                } else {
                    self.seg.wrapping_add(1)
                };
            }
            let Some(fit) = pooled_fit(pts.iter().copied()) else {
                continue;
            };
            let (xa, ma) = mean(&pts[start..start + j]);
            let (xb, mb) = mean(&pts[start + j..]);
            let step = (mb - fit.slope * xb) - (ma - fit.slope * xa);
            let var = fit.se * fit.se * fit.sxx;
            let (ka, kb) = (j as f64, (n - j) as f64);
            let se = (var * (1.0 / ka + 1.0 / kb + (xb - xa) * (xb - xa) / fit.sxx)).sqrt();
            if step.abs() + 2.0 * se > least {
                fits.push(fit);
            }
        }
        fits
    }

    fn publish(&mut self) {
        let Some(f) = self.fit else { return };
        if self.points.len() < DRIFT_MIN_POINTS {
            return;
        }
        // A step too young to confirm tilts the fit: wait for it to be split off, or for
        // the evidence to fade. One that stays unconfirmed is left to the check against a
        // step below: a step near a segment's start can never clear six standard errors.
        let in_seg = self.points.iter().filter(|p| p.seg == self.seg).count();
        if self
            .likeliest_step()
            .is_some_and(|(j, z)| z > 3.0 && in_seg.saturating_sub(j) < DRIFT_STEP_YOUTH)
        {
            return;
        }
        let tol = f.tol();
        let alternatives = self.with_a_step();
        // `to` must stay allowed, and `from` ruled out, with a step anywhere in the segment.
        let holds = |to: f64, from: Option<f64>| {
            alternatives.iter().all(|a| {
                (a.slope - to).abs() <= a.tol()
                    && from.is_none_or(|v| (a.slope - v).abs() > a.tol())
            })
        };
        let level = |slope: f64| {
            if slope.abs() <= 0.5 * tol {
                0.0
            } else {
                slope
            }
        };
        // Drift the fit can tell from noise.
        let shows = f.slope.abs() > tol && holds(f.slope, Some(0.0));
        // A ratio carried over from an older stream counts for nothing once the new stream's
        // history spans a whole fit.
        let lapsed = self.held_over && self.history >= DRIFT_POINTS;
        let held = if lapsed { 0.0 } else { self.published() - 1.0 };
        let err = if held == 0.0 {
            // Leave the identity path only for drift that shows.
            if shows {
                f.slope
            } else if lapsed && f.slope.abs() <= tol {
                0.0
            } else {
                return;
            }
        } else if (f.slope - held).abs() > tol && holds(level(f.slope), Some(held)) {
            // A fit that rules the published value out.
            level(f.slope)
        } else if f.se < self.published_se && holds(level(f.slope), None) {
            // A better fit than the one published (a carried ratio's included: a new stream's
            // fit that showed drift replaced it however noisy it was).
            level(f.slope)
        } else {
            return;
        };
        self.held_over = false;
        self.published_se = f.se;
        let ratio = 1.0 + err.clamp(-DRIFT_CLAMP, DRIFT_CLAMP);
        self.ratio_bits.store(ratio.to_bits(), Ordering::Relaxed);
    }

    fn published(&self) -> f64 {
        f64::from_bits(self.ratio_bits.load(Ordering::Relaxed))
    }

    /// The published correction as parts-per-million (0.0 = identity /
    /// deadband). Diagnostics only — hardware verification and the
    /// diagnostics export read it; nothing decides on it.
    pub fn published_ppm(&self) -> f64 {
        (self.published() - 1.0) * 1e6
    }
}

/// Least squares over `points` with one offset per segment and a slope common to all. The
/// residual variance is blended with a prior from the points' spreads (a mean's error is at
/// most a fraction of its window's range), so a fit of few points cannot read as precise
/// because they happened to agree.
fn pooled_fit(points: impl Iterator<Item = DriftPoint> + Clone) -> Option<DriftFit> {
    // (segment, count, Σx, Σm), in order: segments are contiguous runs.
    let mut segs: Vec<(u32, usize, f64, f64)> = Vec::new();
    let (mut n, mut prior) = (0usize, 0.0);
    for p in points.clone() {
        n += 1;
        prior += (0.3 * p.range) * (0.3 * p.range);
        match segs.last_mut() {
            Some(s) if s.0 == p.seg => {
                s.1 += 1;
                s.2 += p.x;
                s.3 += p.m;
            }
            _ => segs.push((p.seg, 1, p.x, p.m)),
        }
    }
    let dof = n as f64 - segs.len() as f64 - 1.0;
    if dof < 1.0 {
        return None;
    }
    let mean = |seg: u32| {
        segs.iter()
            .find(|s| s.0 == seg)
            .map_or((0.0, 0.0), |s| (s.2 / s.1 as f64, s.3 / s.1 as f64))
    };
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for p in points.clone() {
        let (xm, mm) = mean(p.seg);
        sxx += (p.x - xm) * (p.x - xm);
        sxy += (p.x - xm) * (p.m - mm);
    }
    if sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let rss: f64 = points
        .map(|p| {
            let (xm, mm) = mean(p.seg);
            let r = p.m - mm - slope * (p.x - xm);
            r * r
        })
        .sum();
    // Four points' worth of prior.
    const K0: f64 = 4.0;
    let var = (rss + K0 * prior / n as f64) / (dof + K0);
    Some(DriftFit {
        slope,
        se: (var / sxx).sqrt(),
        dof,
        sxx,
    })
}

#[cfg(test)]
mod drift_tests {
    //! The clock-drift compensation MECHANISM, simulation-tested end to end
    //! (the estimator against counters as the callbacks leave them, and the
    //! input resampler applying the ratio). Hardware CALIBRATION — real
    //! crystal pairs — is the documented follow-up; nothing here needs a
    //! device.

    use super::{DriftEstimator, DriftObservation};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn ratio_of(bits: &Arc<AtomicU64>) -> f64 {
        f64::from_bits(bits.load(Ordering::Relaxed))
    }

    /// A simulated chain: counters advancing in whole callbacks, at crystals `ppm` off
    /// nominal, read by a timer that fires about every `poll` seconds, up to 20 ms late.
    #[derive(Clone)]
    struct Chain {
        cb_in: f64,
        cb_out: f64,
        ppm_in: f64,
        ppm_out: f64,
        poll: f64,
        seconds: f64,
        seed: u64,
        /// The input's callback phase this far ahead of the output's, in callbacks; random
        /// when `None`. VPIO runs both in one IO cycle, the input just before the output.
        phase_gap: Option<f64>,
        /// Output stalls (start, seconds): the device plays nothing meanwhile.
        out_stalls: Vec<(f64, f64)>,
        in_stalls: Vec<(f64, f64)>,
        /// From this time on the output runs at `ppm_out2`; with `rebuild`, as a new stream.
        switch_at: f64,
        ppm_out2: f64,
        rebuild: bool,
    }

    impl Chain {
        fn new(cb_in: f64, cb_out: f64, ppm_in: f64) -> Self {
            Chain {
                cb_in,
                cb_out,
                ppm_in,
                ppm_out: 0.0,
                poll: 1.0,
                seconds: 900.0,
                seed: 1,
                phase_gap: None,
                out_stalls: vec![],
                in_stalls: vec![],
                switch_at: f64::INFINITY,
                ppm_out2: 0.0,
                rebuild: false,
            }
        }

        fn truth(&self, t: f64) -> f64 {
            let out = if t < self.switch_at {
                self.ppm_out
            } else {
                self.ppm_out2
            };
            (1.0 + self.ppm_in * 1e-6) / (1.0 + out * 1e-6)
        }
    }

    struct Run {
        /// How far a take slipped against the output (engine frames): the published
        /// correction's error, integrated over the run.
        slip: f64,
        /// Share of the run published as exactly 1.0.
        unity: f64,
        /// The published correction at the end (ppm).
        last_ppm: f64,
        /// The largest |published - true| (ppm) once the first correction published.
        worst_published_ppm: f64,
        /// The smallest |correction| (ppm) published other than exactly 1.0.
        least_published_ppm: f64,
        /// The last poll.
        last: DriftObservation,
    }

    fn run(c: &Chain, est: &mut DriftEstimator, bits: &Arc<AtomicU64>) -> Run {
        let mut rng = c.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut uniform = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 11) as f64 / (1u64 << 53) as f64
        };
        let ph_in = uniform();
        let ph_out = c
            .phase_gap
            .map_or_else(&mut uniform, |g| (ph_in - g).rem_euclid(1.0));
        let lost = |ev: &[(f64, f64)], t: f64| -> f64 {
            ev.iter().map(|&(s, l)| (t - s).clamp(0.0, l)).sum()
        };
        // Output device seconds by wall time t, across the crystal change.
        let out_secs = |t: f64| {
            let tl = t - lost(&c.out_stalls, t);
            let k = |ppm: f64| 1.0 + ppm * 1e-6;
            if tl < c.switch_at {
                tl * k(c.ppm_out)
            } else {
                c.switch_at * k(c.ppm_out) + (tl - c.switch_at) * k(c.ppm_out2)
            }
        };
        let (mut slip, mut unity, mut worst) = (0.0, 0.0, 0.0f64);
        let mut least = f64::INFINITY;
        let mut published_once = false;
        let (mut prev_t, mut k) = (0.0, 1u64);
        let mut last = None;
        loop {
            let t = k as f64 * c.poll + 0.02 * uniform();
            if t > c.seconds {
                break;
            }
            let r = ratio_of(bits);
            slip += (r - c.truth(prev_t)) * (t - prev_t) * 48_000.0;
            if r == 1.0 {
                unity += t - prev_t;
            } else {
                published_once = true;
                least = least.min(((r - 1.0) * 1e6).abs());
            }
            if published_once {
                worst = worst.max(((r - c.truth(prev_t)) * 1e6).abs());
            }
            let ti = t - lost(&c.in_stalls, t);
            let n_in = (ti * 48_000.0 * (1.0 + c.ppm_in * 1e-6) / c.cb_in + ph_in).floor();
            let n_out = (out_secs(t) * 48_000.0 / c.cb_out + ph_out).floor();
            let o = DriftObservation {
                input: (n_in * c.cb_in) as u64,
                output: (n_out * c.cb_out) as u64,
                input_quantum: c.cb_in as u64,
                output_quantum: c.cb_out as u64,
                generation: u64::from(c.rebuild && t >= c.switch_at),
                secs: t,
            };
            est.tick(o);
            last = Some(o);
            prev_t = t;
            k += 1;
        }
        Run {
            slip,
            unity: unity / prev_t,
            last_ppm: (ratio_of(bits) - 1.0) * 1e6,
            worst_published_ppm: worst,
            least_published_ppm: least,
            last: last.expect("the run polled"),
        }
    }

    fn fresh(c: &Chain) -> Run {
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        run(c, &mut est, &bits)
    }

    /// The slip of a take left uncorrected over the whole run (engine frames).
    fn uncorrected(c: &Chain) -> f64 {
        (c.truth(0.0) - 1.0).abs() * c.seconds * 48_000.0
    }

    #[test]
    fn one_clock_publishes_exactly_one() {
        // VPIO's input and output share one IO cycle, and a duplex device has one crystal:
        // nothing drifts, and anything but exactly 1.0 takes the resampler off its identity
        // path for nothing. A fit through callback-quantized counters published 49% of the
        // time at 512-frame callbacks, with errors up to 113 ppm.
        let mut chains = vec![];
        for (cb_in, cb_out) in [
            (512.0, 512.0),
            (480.0, 512.0),
            (1024.0, 1024.0),
            (4096.0, 4096.0),
        ] {
            chains.push(Chain::new(cb_in, cb_out, 0.0));
        }
        for gap in [0.05, 0.5] {
            chains.push(Chain {
                phase_gap: Some(gap),
                ..Chain::new(256.0, 256.0, 0.0)
            });
        }
        for c in chains {
            for poll in [1.0, 0.05] {
                for seed in 1..=3 {
                    let c = Chain {
                        poll,
                        seed,
                        ..c.clone()
                    };
                    let r = fresh(&c);
                    assert_eq!(
                        r.unity, 1.0,
                        "{}/{} callbacks, gap {:?}, {poll} s polls, seed {seed}: published \
                         {:.1} ppm",
                        c.cb_in, c.cb_out, c.phase_gap, r.worst_published_ppm
                    );
                }
            }
        }
    }

    #[test]
    fn one_lost_callback_on_one_clock_publishes_exactly_one() {
        // A lost callback moves the counters' difference by a whole callback for good. Early in
        // a segment that reads as slope: one lost 1024-frame callback published 114 ppm here,
        // and one lost 4096-frame callback the 500 ppm clamp, slipping a take by 123 ms where
        // nothing drifted. Mismatched callbacks lose the smaller one too, a step a quarter of
        // both callbacks together did not look for.
        for (cb_in, cb_out) in [
            (256.0, 1024.0),
            (1024.0, 256.0),
            (480.0, 512.0),
            (1024.0, 1024.0),
            (4096.0, 4096.0),
        ] {
            for at in [60.0, 100.0, 150.0, 300.0] {
                for input in [true, false] {
                    for seed in 1..=2 {
                        let mut c = Chain {
                            seed,
                            seconds: 900.0,
                            ..Chain::new(cb_in, cb_out, 0.0)
                        };
                        if input {
                            c.in_stalls = vec![(at, cb_in / 48_000.0)];
                        } else {
                            c.out_stalls = vec![(at, cb_out / 48_000.0)];
                        }
                        let r = fresh(&c);
                        assert_eq!(
                            r.unity,
                            1.0,
                            "{cb_in}/{cb_out} callbacks, an {} callback lost at {at} s, seed \
                             {seed}: published {:.1} ppm",
                            if input { "input" } else { "output" },
                            r.worst_published_ppm
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn drift_is_corrected_whatever_the_callbacks() {
        // (callbacks, drift, the share of the uncorrected slip a run may keep, how close it
        // ends). Large callbacks quantize the counters coarsely: their fit takes minutes to
        // clear the noise, and until then the take runs uncorrected rather than on a guess.
        for (cb_in, cb_out, ppm, keep, close) in [
            (480.0, 512.0, 30.0, 0.5, 5.0),
            (480.0, 512.0, -100.0, 0.5, 5.0),
            (128.0, 128.0, 30.0, 0.5, 5.0),
            (128.0, 128.0, -100.0, 0.5, 5.0),
            (1024.0, 1024.0, -100.0, 0.6, 10.0),
            (4096.0, 4096.0, -100.0, 0.7, 40.0),
            (128.0, 4096.0, 100.0, 0.7, 40.0),
            (4096.0, 4096.0, 30.0, 1.0, f64::INFINITY),
        ] {
            for poll in [1.0, 0.05] {
                let c = Chain {
                    poll,
                    ..Chain::new(cb_in, cb_out, ppm)
                };
                let r = fresh(&c);
                let label = format!("{cb_in}/{cb_out} callbacks, {ppm} ppm, {poll} s polls");
                assert!(
                    r.slip.abs() < keep * uncorrected(&c),
                    "{label}: slipped {:.0} frames (uncorrected {:.0})",
                    r.slip,
                    uncorrected(&c)
                );
                assert!(
                    (r.last_ppm - ppm).abs() < close,
                    "{label}: ends at {:.1} ppm",
                    r.last_ppm
                );
            }
        }
    }

    #[test]
    fn a_stall_does_not_tilt_the_fit() {
        // An underrun or a lost burst moves the counters' difference for good: a fit through
        // the step took a 100 ms output stall for 250 ppm of drift. 15 ms is less than a
        // callback each way here, so only the points' drift off their line shows it.
        for at in [120.0, 450.0] {
            for len in [0.015, 0.05, 0.1, 0.15, 0.3] {
                for output in [true, false] {
                    let mut c = Chain::new(480.0, 512.0, 30.0);
                    if output {
                        c.out_stalls = vec![(at, len)];
                    } else {
                        c.in_stalls = vec![(at, len)];
                    }
                    let r = fresh(&c);
                    let label = format!(
                        "{} ms {} stall at {at} s",
                        len * 1e3,
                        if output { "output" } else { "input" }
                    );
                    assert!(
                        (r.last_ppm - 30.0).abs() < 5.0,
                        "{label}: ends at {:.1} ppm",
                        r.last_ppm
                    );
                    assert!(
                        r.worst_published_ppm < 15.0,
                        "{label}: published {:.1} ppm off",
                        r.worst_published_ppm
                    );
                }
            }
        }
    }

    #[test]
    fn a_lost_callback_under_drift_does_not_publish_a_tilt() {
        // A step of one callback takes minutes to tell from noise, and until then it tilts
        // the fit: published as it stood, a 480-frame input callback lost at 150 s put the
        // correction 71 ppm off.
        for (cb_in, cb_out) in [(256.0, 256.0), (480.0, 512.0), (512.0, 512.0)] {
            for ppm in [-60.0, 30.0] {
                for at in [60.0, 150.0] {
                    for input in [true, false] {
                        for seed in 1..=2 {
                            let mut c = Chain {
                                seed,
                                ..Chain::new(cb_in, cb_out, ppm)
                            };
                            if input {
                                c.in_stalls = vec![(at, cb_in / 48_000.0)];
                            } else {
                                c.out_stalls = vec![(at, cb_out / 48_000.0)];
                            }
                            let r = fresh(&c);
                            let tag = format!(
                                "{cb_in}/{cb_out} callbacks at {ppm} ppm, an {} callback lost \
                                 at {at} s, seed {seed}",
                                if input { "input" } else { "output" }
                            );
                            assert!(
                                r.worst_published_ppm < 20.0,
                                "{tag}: published {:.1} ppm off",
                                r.worst_published_ppm
                            );
                            // Never publishing would pass the line above too.
                            assert!(
                                (r.last_ppm - ppm).abs() < 10.0,
                                "{tag}: ends at {:.1} ppm",
                                r.last_ppm
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_step_that_stays_unconfirmed_does_not_hold_publishing_for_good() {
        // One lost 256-frame output callback in the segment's first minute: too few points
        // before it ever to clear six standard errors, so it was suspected for good and held
        // the correction back for the rest of the run.
        for seed in 1..=2 {
            let c = Chain {
                seed,
                out_stalls: vec![(60.0, 256.0 / 48_000.0)],
                ..Chain::new(1024.0, 256.0, -60.0)
            };
            let r = fresh(&c);
            assert!(
                (r.last_ppm + 60.0).abs() < 10.0,
                "seed {seed}: ends at {:.1} ppm",
                r.last_ppm
            );
        }
    }

    #[test]
    fn a_step_inside_a_point_is_cut_out_of_it() {
        // A step smaller than both callbacks together can pass between two samples (their
        // quantization pulled the other way); averaged into a point, it sat half-stepped at a
        // segment's edge and tilted the fit. The window's spread, wider than callbacks can make
        // it, gives it away. Counters exact but for a ±900-frame quantization, 1024-frame
        // callbacks, and a 3000-frame step at the 35th sample.
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        let obs = |k: u64| {
            let e: i64 = if k.is_multiple_of(2) { 900 } else { -900 };
            let step = if k >= 35 { 3000 } else { 0 };
            DriftObservation {
                input: (10_000_000 + 48_000 * k as i64 + e + step) as u64,
                output: 10_000_000 + 48_000 * k,
                input_quantum: 1024,
                output_quantum: 1024,
                generation: 0,
                secs: k as f64,
            }
        };
        for k in 0..=44 {
            est.tick(obs(k));
        }
        let ms: Vec<f64> = est.points.iter().map(|p| p.m - est.points[0].m).collect();
        assert!(
            ms.iter()
                .all(|&m| m.abs() < 500.0 || (m - 3000.0).abs() < 500.0),
            "a point averaged across the step: {ms:?}"
        );
        assert_ne!(
            est.points.front().map(|p| p.seg),
            est.points.back().map(|p| p.seg),
            "the step must split the segment"
        );
    }

    #[test]
    fn stalls_every_minute_never_do_worse_than_no_correction() {
        for len in [0.05, 0.1, 0.15] {
            for poll in [1.0, 0.05] {
                let c = Chain {
                    poll,
                    out_stalls: (1..15).map(|m| (m as f64 * 60.0, len)).collect(),
                    ..Chain::new(480.0, 512.0, 30.0)
                };
                let r = fresh(&c);
                assert!(
                    r.slip.abs() <= uncorrected(&c) + 50.0,
                    "{} ms stalls, {poll} s polls: slipped {:.0} frames (uncorrected {:.0})",
                    len * 1e3,
                    r.slip,
                    uncorrected(&c)
                );
            }
        }
    }

    #[test]
    fn a_device_change_is_followed() {
        // The output moves to a crystal 100 ppm away, as a new stream or (a default-device
        // reroute) on the same one. The old pair must not stay in the fit for ten minutes.
        for rebuild in [true, false] {
            for gap in [0.0, 0.1, 0.3] {
                // Eight minutes on: with the old pair still in, the fit would sit nearer -50.
                let c = Chain {
                    ppm_out: 50.0,
                    switch_at: 300.0,
                    ppm_out2: -50.0,
                    rebuild,
                    seconds: 780.0,
                    out_stalls: vec![(300.0, gap)],
                    ..Chain::new(480.0, 512.0, 0.0)
                };
                let r = fresh(&c);
                let label = format!("rebuild {rebuild}, gap {} ms", gap * 1e3);
                assert!(
                    (r.last_ppm - 50.0).abs() < 5.0,
                    "{label}: ends at {:.1} ppm",
                    r.last_ppm
                );
                assert!(
                    r.slip.abs() < 0.6 * uncorrected(&c),
                    "{label}: slipped {:.0} frames (uncorrected {:.0})",
                    r.slip,
                    uncorrected(&c)
                );
            }
        }
    }

    #[test]
    fn a_new_stream_keeps_the_correction_until_its_own_fit_says_otherwise() {
        // A mic reopened on the same pair keeps its correction instead of relearning it from
        // 1.0; another pair's own drift replaces it once it shows; a stream on one clock
        // returns to exactly 1.0 once its fit rules the carried ratio out.
        for (label, cb_in, ppm) in [
            ("the same pair", 480.0, 30.0),
            ("another pair", 480.0, -40.0),
            ("one clock", 512.0, 0.0),
        ] {
            for seed in 1..=3 {
                let bits = Arc::new(AtomicU64::new(0));
                let mut est = DriftEstimator::new(Arc::clone(&bits));
                let first = Chain {
                    seed,
                    seconds: 600.0,
                    ..Chain::new(480.0, 512.0, 30.0)
                };
                run(&first, &mut est, &bits);
                assert!(((ratio_of(&bits) - 1.0) * 1e6 - 30.0).abs() < 5.0);
                // A new stream from t = 0: the counters restart.
                let c = Chain {
                    seed: seed + 10,
                    rebuild: true,
                    switch_at: 0.0,
                    ..Chain::new(cb_in, 512.0, ppm)
                };
                let r = run(&c, &mut est, &bits);
                let tag = format!("{label}, seed {seed}");
                assert!(
                    (r.last_ppm - ppm).abs() < 5.0,
                    "{tag}: ends at {:.1} ppm",
                    r.last_ppm
                );
                if ppm == 30.0 {
                    assert_eq!(r.unity, 0.0, "{tag}: dropped the correction");
                    assert!(
                        r.worst_published_ppm < 10.0,
                        "{tag}: published {:.1} ppm off",
                        r.worst_published_ppm
                    );
                }
                if ppm == 0.0 {
                    assert_eq!(r.last_ppm, 0.0, "{tag}: still corrects");
                    assert!(r.unity > 0.4, "{tag}: held 30 ppm for too long");
                }
            }
        }
    }

    #[test]
    fn a_new_stream_is_judged_by_its_own_callbacks() {
        // The poll that sees a new stream also reports callbacks the old one made: 4096-frame
        // ones, kept as this stream's bounds, hid a lost 256-frame callback from every check.
        for (at, input) in [(60.0, true), (100.0, false), (150.0, true), (300.0, false)] {
            for seed in 1..=2 {
                let bits = Arc::new(AtomicU64::new(0));
                let mut est = DriftEstimator::new(Arc::clone(&bits));
                est.tick(DriftObservation {
                    input: 0,
                    output: 0,
                    input_quantum: 4096,
                    output_quantum: 4096,
                    generation: 1,
                    secs: 0.0,
                });
                let mut c = Chain {
                    seed,
                    rebuild: true,
                    switch_at: 0.0,
                    ..Chain::new(256.0, 256.0, 0.0)
                };
                let lost = vec![(at, 256.0 / 48_000.0)];
                if input {
                    c.in_stalls = lost;
                } else {
                    c.out_stalls = lost;
                }
                let r = run(&c, &mut est, &bits);
                assert_eq!(
                    r.unity, 1.0,
                    "a callback lost at {at} s, seed {seed}: published {:.1} ppm",
                    r.worst_published_ppm
                );
            }
        }
    }

    #[test]
    fn a_reopened_pair_keeps_its_correction_over_a_noisier_fit() {
        // A new stream's fit that showed drift replaced the carried ratio however noisy it
        // was: reopened on the same 1024/1024 pair at -40 ppm, the correction went 20 ppm off
        // for minutes, where the carried one was within 2.3.
        for seed in 1..=6 {
            let bits = Arc::new(AtomicU64::new(0));
            let mut est = DriftEstimator::new(Arc::clone(&bits));
            let first = Chain {
                seed,
                seconds: 1200.0,
                ..Chain::new(1024.0, 1024.0, -40.0)
            };
            run(&first, &mut est, &bits);
            let again = Chain {
                seed: seed + 30,
                seconds: 1800.0,
                ..Chain::new(1024.0, 1024.0, -40.0)
            };
            let r = run(&again, &mut est, &bits);
            assert!(
                r.worst_published_ppm < 8.0,
                "seed {seed}: published {:.1} ppm off",
                r.worst_published_ppm
            );
        }
    }

    #[test]
    fn callbacks_in_step_do_not_make_the_fit_look_precise() {
        // Equal callbacks whose phases sit together read one quantization level at every
        // sample: each point spread by nothing, the fit's standard error fell to a fraction
        // of a ppm, and a minute into a reopen it "ruled out" a correct carried 14.6 ppm.
        for gap in [0.0, 0.99] {
            let bits = Arc::new(AtomicU64::new(0));
            let mut est = DriftEstimator::new(Arc::clone(&bits));
            let first = Chain {
                seed: 2,
                ..Chain::new(480.0, 512.0, 15.0)
            };
            run(&first, &mut est, &bits);
            let again = Chain {
                seed: 5,
                phase_gap: Some(gap),
                ..Chain::new(1024.0, 1024.0, 15.0)
            };
            let r = run(&again, &mut est, &bits);
            assert_eq!(r.unity, 0.0, "phase gap {gap}: dropped the correction");
            assert!(
                r.worst_published_ppm < 5.0,
                "phase gap {gap}: published {:.1} ppm off",
                r.worst_published_ppm
            );
        }
    }

    #[test]
    fn a_carried_ratio_lapses_after_a_whole_fit() {
        // 4096-frame callbacks on one clock cannot rule out a 10 ppm ratio carried from the
        // last stream in ten minutes; past a whole fit's span it counts for nothing.
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        run(
            &Chain {
                seconds: 600.0,
                ..Chain::new(128.0, 128.0, 10.0)
            },
            &mut est,
            &bits,
        );
        let carried = (ratio_of(&bits) - 1.0) * 1e6;
        assert!(
            (carried - 10.0).abs() < 3.0,
            "first stream at {carried:.1} ppm"
        );
        let c = Chain {
            seed: 5,
            seconds: 1500.0,
            rebuild: true,
            switch_at: 0.0,
            ..Chain::new(4096.0, 4096.0, 0.0)
        };
        let r = run(&c, &mut est, &bits);
        assert_eq!(r.last_ppm, 0.0, "the carried ratio never lapsed");
        assert!(
            r.unity < 0.6,
            "it lapsed before the new stream had a whole fit"
        );
    }

    #[test]
    fn a_stopped_stream_freezes_the_estimate() {
        let bits = Arc::new(AtomicU64::new(0));
        let mut est = DriftEstimator::new(Arc::clone(&bits));
        let c = Chain {
            seconds: 600.0,
            ..Chain::new(480.0, 512.0, 30.0)
        };
        let end = run(&c, &mut est, &bits).last;
        let before = ratio_of(&bits);
        assert_ne!(before, 1.0, "the run must have published");
        // The output stops delivering (device gone): the input keeps counting, and the
        // estimate must HOLD — decaying toward 1.0 would re-drift the next take, and fitting
        // the stall would corrupt it. Then the mic closes too.
        for s in 1..=120u64 {
            est.tick(DriftObservation {
                input: end.input + s.min(60) * 48_001,
                output_quantum: 0,
                secs: end.secs + s as f64,
                ..end
            });
        }
        assert_eq!(ratio_of(&bits), before, "estimate must freeze, not drift");
    }

    #[test]
    fn the_correction_clamps() {
        // A pathologically fast clock clamps at +500 ppm (a broken device, not drift — larger
        // corrections would be audible pitch). With small callbacks a second's drift once
        // tripped the check between samples so often that no window ever filled.
        for (cb, ppm) in [(480.0, 4_000.0), (128.0, 5_000.0), (256.0, -9_000.0)] {
            let c = Chain {
                seconds: 300.0,
                ..Chain::new(cb, cb, ppm)
            };
            let r = fresh(&c);
            assert!(
                (r.last_ppm.abs() - 500.0).abs() < 1e-6 && r.last_ppm * ppm > 0.0,
                "{cb}-frame callbacks at {ppm} ppm: clamped at {} ppm",
                r.last_ppm
            );
        }
    }

    #[test]
    fn sub_deadband_drift_publishes_exactly_one() {
        for ppm in [0.5, 1.5, -1.5] {
            let r = fresh(&Chain::new(480.0, 512.0, ppm));
            assert_eq!(r.unity, 1.0, "{ppm} ppm left the identity path");
        }
        // From a published correction: a rebuilt output 28 ppm fast leaves 2 ppm, which ended
        // published at 3.8 ppm. Nothing inside the deadband publishes but 1.0.
        for (cb_in, cb_out) in [(128.0, 128.0), (480.0, 512.0)] {
            let c = Chain {
                switch_at: 600.0,
                ppm_out2: 28.0,
                rebuild: true,
                seconds: 2400.0,
                ..Chain::new(cb_in, cb_out, 30.0)
            };
            let r = fresh(&c);
            assert_eq!(
                r.last_ppm, 0.0,
                "{cb_in}/{cb_out}: ends at {:.2} ppm",
                r.last_ppm
            );
            assert!(
                r.least_published_ppm > 3.0,
                "{cb_in}/{cb_out}: published {:.2} ppm",
                r.least_published_ppm
            );
        }
    }

    #[test]
    fn the_closed_loop_cancels_the_drift_not_half_of_it() {
        // FULL-pipeline simulation with the real feedback topology: device
        // frames arrive at the true (30 ppm fast) rate, the published ratio
        // corrects the cell's emission, and the estimator sees the NOMINAL
        // counter. The old wiring fed the CORRECTED emission back into the
        // estimator, so the correction damped its own error signal and the
        // loop settled at sqrt(1+e) — ~14.7 ppm residual on a 30 ppm mic
        // after 600 s. Steady-state emission must now match the output
        // clock to well under 1 ppm.
        use crate::ring::spsc;
        use crate::sample::{Smp, CHANNELS};
        let ppm = 30.0f64;
        let (mic_prod, mut mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 17);
        let (cap_prod, _cap_cons) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, _rec_cons) = spsc::<[Smp; CHANNELS]>(8);
        let frames_in = Arc::new(AtomicU64::new(0));
        let nominal = Arc::new(AtomicU64::new(0));
        let ratio = Arc::new(AtomicU64::new(1.0f64.to_bits()));
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
            Arc::clone(&nominal),
            Arc::new(std::sync::atomic::AtomicU32::new(0)),
        );
        cell.reconfigure(crate::sample::ENGINE_SR, 1);
        let mut est = DriftEstimator::new(Arc::clone(&ratio));

        let mut dev_acc = 0.0f64;
        let mut emitted_at_500 = 0u64;
        for s in 1..=600u64 {
            // One wall-clock second of device frames at the true fast rate,
            // fed in callback-sized blocks; the mic ring is drained after
            // each second (the monitor consumer stands in for real drain).
            dev_acc += 48_000.0 * (1.0 + ppm * 1e-6);
            let mut n = dev_acc as usize;
            dev_acc -= n as f64;
            let block = vec![0.25f32; 480];
            while n > 0 {
                let take = n.min(480);
                cell.feed_test(&block[..take]);
                n -= take;
            }
            while mic_cons.pop().is_some() {}
            // The output clock consumes exactly 48 000 engine frames/s.
            est.tick(DriftObservation {
                input: nominal.load(Ordering::Relaxed),
                output: s * 48_000,
                input_quantum: cell.take_quantum(),
                output_quantum: 512,
                generation: cell.generation(),
                secs: s as f64,
            });
            if s == 500 {
                emitted_at_500 = frames_in.load(Ordering::Relaxed);
            }
        }
        let emitted = frames_in.load(Ordering::Relaxed) - emitted_at_500;
        let residual_ppm = (emitted as f64 / (100.0 * 48_000.0) - 1.0) * 1e6;
        assert!(
            residual_ppm.abs() < 1.0,
            "steady-state emission must match the output clock \
             (residual {residual_ppm:.2} ppm; the old feedback left ~{:.1})",
            ppm / 2.0
        );
    }

    /// The record gain lands after the channel mix, so a hot gain keeps every peak of the take:
    /// applied ahead of the mix (as VPIO did), each gained sample past +24 dBFS met its guard
    /// against garbage samples and was recorded as silence — at +40 dB, anything over -16 dBFS.
    /// A garbage sample from the device is still left out.
    #[test]
    fn a_hot_record_gain_keeps_every_peak_of_the_take() {
        use crate::ring::spsc;
        use crate::sample::{Smp, CHANNELS};
        use std::sync::atomic::{AtomicBool, AtomicU32};
        let (mic_prod, _mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 14);
        let (cap_prod, _cap_cons) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, mut rec_cons) = spsc::<[Smp; CHANNELS]>(1 << 14);
        let level = Arc::new(AtomicU32::new(0));
        let cell = super::InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            Arc::clone(&level),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(1.0f64.to_bits())),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU32::new(0)),
        );
        // What VPIO's input callback hands the cell: its clean mono voice, one channel.
        let epoch = cell.reconfigure(crate::sample::ENGINE_SR, 1);
        let gain = 100.0; // +40 dB, the top of the Settings slider
        let mut voice: Vec<f32> = (0..4800)
            .map(|n| 0.25 * (std::f32::consts::TAU * 220.0 * n as f32 / 48_000.0).sin())
            .collect();
        voice[2400] = 2.0e19; // garbage from a driver
        assert!(cell.try_feed_gained(epoch, &voice, gain));
        let mut worst = 0.0f32;
        for (n, &v) in voice.iter().enumerate() {
            let got = rec_cons.pop().expect("one recorded frame per input frame")[0];
            let want = if n == 2400 { 0.0 } else { v * gain };
            worst = worst.max((got - want).abs());
        }
        assert!(worst < 1e-3, "the take is off by up to {worst}");
        let peak = f32::from_bits(level.load(Ordering::Relaxed));
        assert!(
            (peak - 25.0).abs() < 0.1,
            "the meter reads {peak}, not the gained peak"
        );
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
            Arc::new(AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicU32::new(0)),
        );
        cell.reconfigure(crate::sample::ENGINE_SR, 1);
        let block = vec![0.25f32; 4800];
        for _ in 0..10 {
            cell.feed_test(&block);
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

    #[test]
    fn crossing_the_deadband_does_not_resume_from_a_stale_sample() {
        // The estimator publishes EXACTLY 1.0 inside its ±3 ppm deadband, so a
        // chain that hovers near that edge alternates between the identity fast
        // path and the interpolating one. The fast path must still carry the
        // resampler's state forward: when it did not, the first interpolation
        // after a crossing blended the current input with `prev` from BEFORE
        // the identity stretch — a sample out of nowhere, i.e. a click.
        use crate::ring::spsc;
        use crate::sample::{Smp, CHANNELS};
        let (mic_prod, mut mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 17);
        let (cap_prod, _cap_cons) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, _rec_cons) = spsc::<[Smp; CHANNELS]>(8);
        let ratio = Arc::new(AtomicU64::new((1.0f64 + 1000e-6).to_bits()));
        let cell = super::InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicU32::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&ratio),
            Arc::new(AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicU32::new(0)),
        );
        cell.reconfigure(crate::sample::ENGINE_SR, 1);

        // Correcting: primes `prev` with the loud pre-deadband signal.
        cell.feed_test(&vec![0.9f32; 512]);
        // Inside the deadband: the identity path owns the stream for a while.
        ratio.store(1.0f64.to_bits(), Ordering::Relaxed);
        cell.feed_test(&vec![0.5f32; 512]);
        while mic_cons.pop().is_some() {}

        // Back out of the deadband, on a signal nowhere near either of those.
        ratio.store((1.0f64 + 1000e-6).to_bits(), Ordering::Relaxed);
        cell.feed_test(&vec![-1.0f32; 512]);

        let first = mic_cons.pop().expect("the crossing must still emit");
        assert!(
            (first[0] + 1.0).abs() < 1e-6,
            "first sample after the crossing is {}, expected -1.0 — the \
             resampler resumed from a sample the identity path left stale",
            first[0]
        );
        // ...and nothing else in the block carries the old level either.
        while let Some(f) = mic_cons.pop() {
            assert!((f[0] + 1.0).abs() < 1e-6, "later sample {} != -1.0", f[0]);
        }
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
    // A command the idle wait below already took off the channel, applied first.
    let mut next: Option<WriterCmd> = None;
    loop {
        // Apply any pending commands.
        loop {
            let cmd = match next.take() {
                Some(cmd) => cmd,
                None => match rx.try_recv() {
                    Ok(cmd) => cmd,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                },
            };
            match cmd {
                WriterCmd::Start(path, ack) => {
                    // A Start while already recording finalizes the previous take first,
                    // draining its buffered tail into it (mirroring Stop) so those frames
                    // land in the take they belong to. IO errors on that drain/finalize are
                    // carried into `failed` (matching Stop) rather than swallowed.
                    let had_live_take = wav.is_some();
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
                            // here with a live take). `failed` is carried only while a live
                            // take existed — then it holds that take's mid-take drain
                            // damage, which a clean tail-drain/finalize must not erase.
                            // Without one, a leftover `failed` is a previous CREATE failure
                            // already reported through its own ack, and keeping it would
                            // damn the new take before a sample is written.
                            failed = (had_live_take && failed) || old_take_damaged;
                            let _ = ack.send(true);
                        }
                        Err(_) => {
                            wav = None;
                            failed = true;
                            let _ = ack.send(false);
                        }
                    }
                }
                WriterCmd::Stop(ack) => {
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
                WriterCmd::Shutdown => {
                    // An engine torn down mid-take: drain the buffered tail into the take
                    // first, as Stop does (it is up to the ring's ~2.7 s of the take's
                    // end). There is no one left to report a failure to.
                    if let Some(mut w) = wav.take() {
                        while let Some(f) = rec.pop() {
                            let _ = w.write_sample(f[0]);
                        }
                        let _ = w.finalize();
                    }
                    return;
                }
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
        } else if wav.is_none() {
            // Not recording: nothing reaches the ring until a Start arms the next take (the
            // fan-out is gated on `rec_active`, and a late tail is swept by that Start), so park
            // on the command channel. Polling here woke the thread every 5 ms for the engine's
            // whole life, take or no take.
            match rx.recv() {
                Ok(cmd) => next = Some(cmd),
                Err(_) => return,
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
    epoch: u64,
    event: Arc<AtomicU32>,
) -> Result<DeviceHandle, String> {
    let err_cb = move |err| {
        let _ = err;
        event.fetch_or(EVENT_INPUT_LOST, Ordering::Release);
    };
    // `epoch` is the generation `reconfigure` minted with this stream's format: every older
    // callback is already retired, and a denied feed drops its samples rather than racing a
    // second producer into single-producer rings.
    let stream = device
        .build_input_stream(
            *config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Contained like the output callbacks: a panic must not unwind into the
                // host audio API.
                let _ = catch_unwind(AssertUnwindSafe(|| cell.try_feed(epoch, data)));
            },
            err_cb,
            None,
        )
        .map_err(|e| format!("build_input_stream: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("input stream.play: {e}"))?;
    Ok(DeviceHandle {
        _stream: stream,
        follows_default: false,
        rate: config.sample_rate,
    })
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
    // Reconfigure retires every older callback and mints the generation the new stream
    // feeds as — the format and the admission epoch move together, so no callback can be
    // admitted under a format it never delivered.
    let epoch = cell.reconfigure(rate, channels);
    let stream = build_input_stream(&device, &config, Arc::clone(cell), epoch, Arc::clone(event))?;
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
    /// Switch the active output device by UID. 0 = the pick was acted on: playback is on it, or
    /// on the default when it could not be opened, or — while the mic unit owns the output — the
    /// pick is recorded for when the mic closes. Negative = nothing could be opened.
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

/// Whether the mic, whenever it is open, runs in the output's own IO cycle: duplex VPIO
/// renders the output and delivers the input in one cycle of one unit, so the two counters
/// [`DriftEstimator`] compares cannot drift apart. Nothing is estimated then and the ratio
/// stays exactly 1.0: a published correction could only be a false one (a lost callback read
/// as slope).
pub const MIC_ON_OUTPUT_CLOCK: bool = VPIO_DUPLEX_AEC;

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
    // (mono/stereo, native rate) after the unit initializes, which is what actually governs
    // feed — the generation minted here is superseded there, so its value is dropped. What
    // matters NOW is the retirement: any zombie callback surviving a failed teardown is denied
    // from this point on, instead of feeding through the multi-ms VPIO build window.
    let _ = input_cell.reconfigure(ENGINE_SR, CHANNELS);
    // VPIO renders playback at 48 kHz with no boundary resampler: sync the
    // consumed cursor over anything a dying non-48 kHz cpal stream left in
    // its FIFO (see build_stream).
    rt.transport().begin_output_stream();
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
    actual_input_uid: Arc<Mutex<String>>,
    output_binding: Arc<Mutex<OutputBinding>>,
    rx: mpsc::Receiver<HostCmd>,
    ready_tx: mpsc::Sender<Result<String, String>>,
) {
    // The UID the OPEN input stream is actually bound to; empty = input
    // closed, or bound to the system default (either chosen or via
    // fallback — EVENT_INPUT_FELL_BACK distinguishes the latter). Written
    // ONLY here on the host thread, read by ve_actual_input_device.
    let set_actual = |v: &str| {
        if let Ok(mut a) = actual_input_uid.lock() {
            a.clear();
            a.push_str(v);
        }
    };
    // Publish where playback is bound (see `OutputBinding`), then re-derive the monitor safety
    // mute from it. Called after EVERY command that can move the binding — including the ones
    // that only defer a request — because "the user picked headphones" is only evidence that
    // monitoring is safe while the engine is actually rendering to them.
    // Duplex VPIO renders the engine, monitor included, from the moment the unit starts — before
    // the host can publish which endpoint it landed on. Publish that a unit is opening BEFORE
    // building it: the safety mute then holds against every reconcile (including the FFI poll's)
    // until one of the paths out of the open rebinds to the unit or to the restored cpal output.
    let begin_vpio_open = |b: &mut OutputBinding| {
        b.cpal_uid.clear();
        b.cpal_follows_default = false;
        b.cpal_rate = 0;
        b.vpio = VpioOutput::Opening;
    };
    let rebind_output = |mutate: &dyn Fn(&mut OutputBinding)| {
        if let Ok(mut b) = output_binding.lock() {
            mutate(&mut b);
        }
        reconcile_output_safety(rt.transport(), &output_binding, &event);
    };
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
            rebind_output(&|b| {
                b.requested.clear();
                b.cpal_uid = uid.clone();
                b.cpal_follows_default = handle.follows_default;
                b.cpal_rate = handle.rate;
                b.vpio = VpioOutput::Off;
            });
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
                    // default) when the mic stops, and report the deferred success. NOTHING is
                    // opened here, so the reply's 0 means "accepted", not "you are on it now" —
                    // re-derive safety so switching output mid-monitor cannot un-mute the
                    // monitor into the device VPIO is still physically rendering to.
                    cur_out_uid = Some(uid.clone());
                    rebind_output(&|b| b.requested = uid.clone());
                    let _ = reply.send(0);
                } else {
                    // Drop the old stream FIRST (joins its callback) so only one stream ever touches
                    // the shared RtCell, then build the new one. Fall back to default if the
                    // requested device can't be opened.
                    out_stream = None;
                    let built = build_output(Some(&uid), &rt, &event)
                        .or_else(|_| build_output(None, &rt, &event));
                    match built {
                        // `cur_out_uid` keeps the REQUESTED device even when the fallback opened
                        // the default: it is the user's intent, and both the restore-after-mic
                        // paths and the device-returned self-heal must keep aiming at it. The
                        // RESOLVED uid goes to the binding instead, so the reroute test compares
                        // what is playing against what was asked for.
                        Ok((handle, resolved)) => {
                            let follows = handle.follows_default;
                            let rate = handle.rate;
                            out_stream = Some(handle);
                            cur_out_uid = Some(uid.clone());
                            rebind_output(&|b| {
                                b.requested = uid.clone();
                                b.cpal_uid = resolved.clone();
                                b.cpal_follows_default = follows;
                                b.cpal_rate = rate;
                                b.vpio = VpioOutput::Off;
                            });
                            let _ = reply.send(0);
                        }
                        Err(_) => {
                            // The pick is still the user's intent: a later restore (the mic
                            // closing) must aim at it, not at the device they moved away from.
                            cur_out_uid = Some(uid.clone());
                            rebind_output(&|b| {
                                b.requested = uid.clone();
                                b.cpal_uid.clear();
                                b.cpal_follows_default = false;
                                b.cpal_rate = 0;
                                b.vpio = VpioOutput::Off;
                            });
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
                    // back to the default input if the selected device can't be opened —
                    // but SAY so: the UI otherwise keeps showing the selected mic while
                    // the take records from the default one.
                    if VPIO_DUPLEX_AEC {
                        out_stream = None;
                        rebind_output(&begin_vpio_open);
                    }
                    let mut fell_back = false;
                    let built =
                        open_mic(cur_in_uid.as_deref(), &rt, &input_cell, &event).or_else(|e| {
                            if cur_in_uid.is_some() {
                                fell_back = true;
                                open_mic(None, &rt, &input_cell, &event)
                            } else {
                                Err(e)
                            }
                        });
                    match built {
                        Ok(handle) => {
                            in_stream = Some(handle);
                            input_started(&input_state);
                            let bound = if fell_back {
                                set_actual("");
                                event.fetch_or(EVENT_INPUT_FELL_BACK, Ordering::Release);
                                String::new()
                            } else {
                                let u = cur_in_uid.clone().unwrap_or_default();
                                set_actual(&u);
                                u
                            };
                            // In duplex, VPIO now OWNS the output: playback follows the mic's
                            // endpoint, not the app-selected device. Publishing that is what
                            // lets the monitor be muted before a single block renders into
                            // whatever speaker VPIO landed on.
                            if VPIO_DUPLEX_AEC {
                                rebind_output(&|b| {
                                    b.cpal_uid.clear();
                                    b.cpal_follows_default = false;
                                    b.cpal_rate = 0;
                                    b.vpio = VpioOutput::Bound(bound.clone());
                                });
                            }
                        }
                        Err(_) => {
                            set_actual("");
                            converged = false;
                            input_stopped(&input_state);
                            // Mic failed to come up → restore cpal output so playback
                            // survives. Restore the user's chosen output (not the system
                            // default); fall back to default only if it can't open.
                            if VPIO_DUPLEX_AEC && out_stream.is_none() {
                                let (built, resolved) =
                                    restore_output_or_flag(cur_out_uid.as_deref(), &event, |uid| {
                                        build_output(uid, &rt, &event)
                                    });
                                out_stream = built;
                                let follows =
                                    out_stream.as_ref().is_some_and(|h| h.follows_default);
                                let rate = out_stream.as_ref().map_or(0, |h| h.rate);
                                rebind_output(&|b| {
                                    b.cpal_uid = resolved.clone();
                                    b.cpal_follows_default = follows;
                                    b.cpal_rate = rate;
                                    b.vpio = VpioOutput::Off;
                                });
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
                    set_actual("");
                    // VPIO was rendering the output in duplex; hand playback back to
                    // cpal, restoring the user's last chosen output device (a SetOutput
                    // during duplex deferred it), falling back to the system default
                    // only if that device can't be opened.
                    if VPIO_DUPLEX_AEC && out_stream.is_none() {
                        let (built, resolved) =
                            restore_output_or_flag(cur_out_uid.as_deref(), &event, |uid| {
                                build_output(uid, &rt, &event)
                            });
                        out_stream = built;
                        let follows = out_stream.as_ref().is_some_and(|h| h.follows_default);
                        let rate = out_stream.as_ref().map_or(0, |h| h.rate);
                        // Playback is back on the user's device, so the safety mute clears
                        // itself here — nobody else has to remember to lift it.
                        rebind_output(&|b| {
                            b.cpal_uid = resolved.clone();
                            b.cpal_follows_default = follows;
                            b.cpal_rate = rate;
                            b.vpio = VpioOutput::Off;
                        });
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
                // Dart sends '' for "System Default": that is None here, the default input —
                // never a uid lookup that cannot match, which "fell back" to the default on
                // every open and warned that the mic the user explicitly chose was missing.
                cur_in_uid = (!uid.is_empty()).then(|| uid.clone());
                if in_stream.is_some() {
                    // Reopen on the new device (drop old first → single-accessor; cpal output is
                    // already off in duplex). Not-running for the whole swap window (see
                    // StopInput on why the store precedes the drop).
                    input_stopped(&input_state);
                    in_stream = None;
                    if VPIO_DUPLEX_AEC {
                        rebind_output(&begin_vpio_open);
                    }
                    let mut fell_back = false;
                    match open_mic(cur_in_uid.as_deref(), &rt, &input_cell, &event).or_else(|e| {
                        if cur_in_uid.is_some() {
                            fell_back = true;
                            open_mic(None, &rt, &input_cell, &event)
                        } else {
                            Err(e)
                        }
                    }) {
                        Ok(handle) => {
                            in_stream = Some(handle);
                            input_started(&input_state);
                            let bound = if fell_back {
                                set_actual("");
                                event.fetch_or(EVENT_INPUT_FELL_BACK, Ordering::Release);
                                String::new()
                            } else {
                                set_actual(&uid);
                                uid.clone()
                            };
                            // The mic device moved, and in duplex the OUTPUT moves with it
                            // (VPIO renders through the mic's endpoint) — re-derive which
                            // speaker is live before the monitor rides on the old answer.
                            if VPIO_DUPLEX_AEC {
                                rebind_output(&|b| {
                                    b.cpal_uid.clear();
                                    b.cpal_follows_default = false;
                                    b.cpal_rate = 0;
                                    b.vpio = VpioOutput::Bound(bound.clone());
                                });
                            }
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
                            set_actual("");
                            if VPIO_DUPLEX_AEC && out_stream.is_none() {
                                // Restore the user's chosen output (not the system default),
                                // matching StopInput; fall back to default only if it can't open.
                                let (built, resolved) =
                                    restore_output_or_flag(cur_out_uid.as_deref(), &event, |uid| {
                                        build_output(uid, &rt, &event)
                                    });
                                out_stream = built;
                                let follows =
                                    out_stream.as_ref().is_some_and(|h| h.follows_default);
                                let rate = out_stream.as_ref().map_or(0, |h| h.rate);
                                rebind_output(&|b| {
                                    b.cpal_uid = resolved.clone();
                                    b.cpal_follows_default = follows;
                                    b.cpal_rate = rate;
                                    b.vpio = VpioOutput::Off;
                                });
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
                set_actual("");
                out_stream = None;
                rebind_output(&|b| {
                    b.cpal_uid.clear();
                    b.cpal_follows_default = false;
                    b.cpal_rate = 0;
                    b.vpio = VpioOutput::Off;
                });
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
#[allow(clippy::too_many_arguments)]
pub fn spawn_audio_host(
    rt: Arc<RtCell>,
    input_cell: Arc<InputCell>,
    event: Arc<AtomicU32>,
    renderer_gate: Arc<Mutex<bool>>,
    input_state: Arc<AtomicU64>,
    input_intent: Arc<AtomicU64>,
    input_applied: Arc<AtomicU64>,
    actual_input_uid: Arc<Mutex<String>>,
    output_binding: Arc<Mutex<OutputBinding>>,
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
                actual_input_uid,
                output_binding,
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
    /// Makes [`decode_file`] panic on THIS thread (tests only): stands in for a decoder
    /// panicking on a corrupt packet, which nothing else can produce on demand.
    pub static DECODE_PANICS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Frames the container says to drop from the decoded stream's start and end: an MP3's
/// encoder delay and padding (its LAME tag), an Ogg stream's pre-skip and end trim. Kept,
/// they put the audio 25 ms late against every other player (a technique example's analysis
/// trailed its own playback; lyrics timed elsewhere read early). Dropped here, not by
/// symphonia's gapless mode, which also cuts the stream at the header's frame count: two
/// MP3s joined end to end lost the second, and a count below the delay wrapped round to an
/// endless file. (MP4/AAC priming is not reported: its reader ignores edit lists.)
///
/// Bounded: real edges are a few thousand frames, but they come from the file, and an Ogg
/// whose last granule ran backwards claimed billions, lifting every budget the decode
/// judges against by as much.
fn edge_trim(params: &symphonia::core::codecs::CodecParameters) -> (u64, u64) {
    const MAX_EDGE: u64 = 1 << 16;
    (
        u64::from(params.delay.unwrap_or(0)).min(MAX_EDGE),
        u64::from(params.padding.unwrap_or(0)).min(MAX_EDGE),
    )
}

/// The track's declared length in frames at its sample rate. symphonia hands an MP4's on as
/// the media header has it, in the track's own timescale, which need not be the sample rate:
/// an HE-AAC file often counts at its core rate, half the declared one, and a Smooth
/// Streaming file in 10 MHz ticks, so read as frames the one was half its length and the
/// other hours long, refused as too long. All ones is the header's "unknown".
///
/// A PCM track also declares no more frames than the file's `file_len` bytes hold: a WAV
/// written to a pipe leaves its data size at all ones (4 GB, never patched), which read as
/// hours, so a 2 s file was refused as too long. Past the file, a declaration counts as none.
fn declared_frames(
    params: &symphonia::core::codecs::CodecParameters,
    file_len: u64,
) -> Option<u64> {
    let n = params.n_frames.filter(|&n| n != u64::MAX)?;
    let frames = match (params.time_base, params.sample_rate) {
        (Some(tb), Some(rate)) if (tb.numer, tb.denom) != (1, rate) && tb.denom > 0 => {
            let frames = (u128::from(n) * u128::from(tb.numer) * u128::from(rate))
                .div_ceil(u128::from(tb.denom));
            u64::try_from(frames).unwrap_or(u64::MAX)
        }
        _ => n,
    };
    // A sample's bytes as the decoder reads them, from its name ("pcm_s24le", "pcm_f32be",
    // "pcm_alaw"): A-law, µ-law and float WAVs declare no bit depth.
    let sample_bytes = symphonia::default::get_codecs()
        .get_codec(params.codec)
        .and_then(|c| c.short_name.strip_prefix("pcm_"))
        .and_then(|name| match name {
            "alaw" | "mulaw" => Some(1),
            _ => name
                .get(1..)?
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<u64>()
                .ok()
                .map(|bits| bits / 8),
        });
    let frame_bytes = sample_bytes.unwrap_or(0) * params.channels.map_or(0, |c| c.count() as u64);
    if frame_bytes > 0 && frames.saturating_mul(frame_bytes) > file_len {
        return None;
    }
    Some(frames)
}

/// Each channel's share of the left and right output, in the decoder's channel order, for a
/// file of more than two: the front pair as is, a centre at -3 dB to both sides, any other
/// left or right channel at -3 dB to its own side, the LFE left out (ffmpeg's default
/// downmix). Kept to its first two channels, a 5.1 file lost its centre, where its vocals
/// are, and its surrounds.
///
/// The LFE is left out of 5.1 and 7.1 only. A WAV that names no layout is given one by its
/// channel count, which places an LFE fourth whatever the file holds: a four-track recorder's
/// fourth track decoded silent. Elsewhere it counts as a centre.
fn downmix_gains(channels: symphonia::core::audio::Channels) -> Vec<[Smp; 2]> {
    use symphonia::core::audio::Channels as C;
    const HALF_POWER: Smp = std::f32::consts::FRAC_1_SQRT_2;
    let left = C::FRONT_LEFT_CENTRE
        | C::REAR_LEFT
        | C::SIDE_LEFT
        | C::TOP_FRONT_LEFT
        | C::TOP_REAR_LEFT
        | C::REAR_LEFT_CENTRE
        | C::FRONT_LEFT_WIDE
        | C::FRONT_LEFT_HIGH;
    let right = C::FRONT_RIGHT_CENTRE
        | C::REAR_RIGHT
        | C::SIDE_RIGHT
        | C::TOP_FRONT_RIGHT
        | C::TOP_REAR_RIGHT
        | C::REAR_RIGHT_CENTRE
        | C::FRONT_RIGHT_WIDE
        | C::FRONT_RIGHT_HIGH;
    let surround = matches!(channels.count(), 6 | 8);
    channels
        .iter()
        .map(|c| {
            if c == C::FRONT_LEFT {
                [1.0, 0.0]
            } else if c == C::FRONT_RIGHT {
                [0.0, 1.0]
            } else if surround && c.intersects(C::LFE1 | C::LFE2) {
                [0.0, 0.0]
            } else if c.intersects(left) {
                [HALF_POWER, 0.0]
            } else if c.intersects(right) {
                [0.0, HALF_POWER]
            } else {
                [HALF_POWER, HALF_POWER]
            }
        })
        .collect()
}

/// The track decoding reads: the first a decoder exists for. symphonia's own default is simply
/// the first track, which in an MP4 holding a video or cover-picture track ahead of its audio
/// is that one, and the decode failed as an unsupported codec.
fn audio_track(
    format: &dyn symphonia::core::formats::FormatReader,
) -> Option<&symphonia::core::formats::Track> {
    let codecs = symphonia::default::get_codecs();
    format
        .tracks()
        .iter()
        .find(|t| codecs.get_codec(t.codec_params.codec).is_some())
}

/// The most frames a decoder may size one packet's buffer for. Real codecs stay far below
/// (MP3 1152, AAC 2048, ALAC 4096, FLAC 65535), but the count comes from the file, and the
/// decoder allocates it (times the channels) as it is built, before any budget here applies:
/// an LPCM .mov or an ALAC cookie declaring billions aborted the app, and an allocation
/// failure is not a panic, so no catch_unwind stops it.
const MAX_FRAMES_PER_PACKET: u64 = 1 << 17;

/// Whether a decoder built for `params` would size its packet buffer past
/// [`MAX_FRAMES_PER_PACKET`]: the container's declared packet size, or for ALAC the frame
/// length its config cookie (24 or 48 bytes, as the decoder reads it) declares.
fn oversized_packets(params: &symphonia::core::codecs::CodecParameters) -> bool {
    let declared = params.max_frames_per_packet.unwrap_or(0);
    let alac = match params.extra_data.as_deref() {
        Some(c)
            if params.codec == symphonia::core::codecs::CODEC_TYPE_ALAC
                && (c.len() == 24 || c.len() == 48) =>
        {
            u64::from(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        }
        _ => 0,
    };
    declared.max(alac) > MAX_FRAMES_PER_PACKET
}

/// `params` as the decoder is to read them. An ALAC cookie can come wrapped in 'frma' and
/// 'alac' atom headers ahead of its config, as older encoders and ffmpeg's CAF writer vend it
/// and as Apple's own decoder skips: symphonia read the wrapping as the config, and refused
/// the file as a newer ALAC version.
fn decoder_params(
    params: &symphonia::core::codecs::CodecParameters,
) -> symphonia::core::codecs::CodecParameters {
    let mut params = params.clone();
    if params.codec == symphonia::core::codecs::CODEC_TYPE_ALAC {
        let unwrapped = params.extra_data.as_deref().and_then(|cookie| {
            let mut config = cookie;
            for tag in [b"frma", b"alac"] {
                if config.len() >= 12 && config[4..8] == tag[..] {
                    config = &config[12..];
                }
            }
            (config.len() != cookie.len()).then(|| Box::<[u8]>::from(config))
        });
        if unwrapped.is_some() {
            params.extra_data = unwrapped;
        }
    }
    params
}

/// Whether building a decoder for `params` would allocate unbounded from what the file
/// declares, before any decode budget applies: an oversized packet buffer (see
/// [`oversized_packets`]), or, for Vorbis, a codebook VQ lookup table sized by the setup
/// header's entry count and dimensions (see [`crate::media_screen::vorbis_setup_overlarge`]).
fn decoder_overallocates(params: &symphonia::core::codecs::CodecParameters) -> bool {
    if oversized_packets(params) {
        return true;
    }
    params.codec == symphonia::core::codecs::CODEC_TYPE_VORBIS
        && params
            .extra_data
            .as_deref()
            .is_some_and(crate::media_screen::vorbis_setup_overlarge)
}

/// Probe `path`'s DECLARED duration in milliseconds without decoding a single packet:
/// container probe only, so an import gate can refuse an hours-long file before any copy or
/// allocation happens. `Ok(None)` = the container declares no frame count (e.g. a plain mp3
/// without a Xing header) — the caller falls back to the decode-time budget, which bails
/// mid-decode with peak memory bounded by the budget.
pub fn probe_duration_ms(path: &str) -> Result<Option<u64>, String> {
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mut file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let file_len = file.metadata().map_or(u64::MAX, |m| m.len());
    // The probe itself sizes allocations by what the file declares (see media_screen), so this
    // gate is only a gate once the file has been screened.
    crate::media_screen::screen(&mut file).map_err(|e| format!("screen {path}: {e}"))?;
    let mss = MediaSourceStream::new(media_source(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &Default::default(), &MetadataOptions::default())
        .map_err(|e| format!("probe: {e}"))?;
    let track =
        audio_track(&*probed.format).ok_or_else(|| "no decodable audio track".to_string())?;
    let rate = track.codec_params.sample_rate.unwrap_or(0) as u64;
    let (delay, padding) = edge_trim(&track.codec_params);
    Ok(
        match (declared_frames(&track.codec_params, file_len), rate) {
            // Ceiling division: a gate comparing `declared > cap` must see a file that is even
            // ONE sample past the cap as strictly longer — flooring would round it down onto
            // the boundary and admit it. What decode_file drops is not counted.
            (Some(n), r) if r > 0 => Some(
                n.saturating_sub(delay.saturating_add(padding))
                    .saturating_mul(1000)
                    .div_ceil(r),
            ),
            _ => None,
        },
    )
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
    /// can say what is actually wrong. Also a file whose container declares a
    /// packet, table or tag larger than it could hold, which would be
    /// allocated before it is read.
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
/// Mono is duplicated; more than 2 channels mix down (see [`downmix_gains`]). `max_secs` (0.0 = unbounded) caps the
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
    #[cfg(test)]
    assert!(!DECODE_PANICS.with(|c| c.get()), "decoder panicked (test)");
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let err = |m: String| DecodeError::Other(m);
    let mut file =
        std::fs::File::open(path).map_err(|e| DecodeError::Io(format!("open {path}: {e}")))?;
    let file_len = file.metadata().map_or(u64::MAX, |m| m.len());

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
    // Where an AIFF's sound ends (see sniff_aiff_frames). Zero is a header never patched, not
    // an empty file.
    let aiff_frames = sniff_aiff_frames(path).ok().flatten().filter(|&n| n > 0);
    // A sample table, packet table or tag that declares more than the file holds: symphonia
    // reserves it whole during the probe, and failing to allocate aborts the app. The full
    // scan (decode reads the whole file anyway) also covers an Ogg stream's later links, whose
    // header packets symphonia parses as it decodes; the import gate's bounded scan does not.
    match crate::media_screen::screen_full(&mut file) {
        Ok(()) => {}
        Err(crate::media_screen::Refusal::Impossible(_)) => return Err(DecodeError::TooBig),
        Err(crate::media_screen::Refusal::Io(e)) => {
            return Err(DecodeError::Io(format!("read {path}: {e}")))
        }
    }
    let mss = MediaSourceStream::new(media_source(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &Default::default(), &MetadataOptions::default())
        .map_err(|e| err(format!("probe: {e}")))?;
    let mut format = probed.format;
    let track = audio_track(&*format).ok_or_else(|| err("no decodable audio track".to_string()))?;
    let track_id = track.id;
    // Per link of a chained stream: each brings its own (see the reset below).
    let (mut skip, mut padding) = edge_trim(&track.codec_params);
    let mut link_start = 0usize;
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
    // The effective frame budget at `rate`: the duration-derived bound AND the caller's
    // absolute memory budget, whichever is tighter (u64::MAX = unbounded).
    let budgets = |rate: u32| -> (u64, u64) {
        let duration_frames: u64 = if max_secs > 0.0 {
            (max_secs * rate as f64).ceil() as u64
        } else {
            u64::MAX
        };
        let absolute = if max_src_frames > 0 {
            max_src_frames
        } else {
            u64::MAX
        };
        (duration_frames, duration_frames.min(absolute))
    };
    let (mut duration_frames, mut max_frames) = budgets(src_rate);
    // Which budget actually tripped decides the DIAGNOSIS: over the duration
    // bound is "too long"; under it but over the absolute source-frame cap is
    // "too big" (a short, dense file the memory budget refuses).
    let budget_err = |frames: u64, duration_frames: u64| -> DecodeError {
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
    // Judged as trimmed, as probe_duration_ms reports it: untrimmed, a file the import gate
    // admitted within a few ms of the cap was refused here.
    let mut out: Vec<[Smp; CHANNELS]> = match declared_frames(&track.codec_params, file_len)
        .map(|n| n.saturating_sub(skip.saturating_add(padding)))
    {
        Some(n) if n > max_frames => return Err(budget_err(n, duration_frames)),
        Some(n) => Vec::with_capacity(n.min(max_frames) as usize),
        None => Vec::new(),
    };
    let params = decoder_params(&track.codec_params);
    if decoder_overallocates(&params) {
        return Err(DecodeError::TooBig);
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| err(format!("make decoder: {e}")))?;

    // Iterate packets until end of stream. Error taxonomy matters here (#19):
    // a normal EOF ends the loop cleanly, a corrupt PACKET is skipped (the
    // standard symphonia discipline — one damaged frame must not kill a
    // recovering take), but a mid-file READ error is a failing disk, not an
    // end of stream — returning the partial audio as a clean success would
    // silently truncate the user's media. ResetRequired is not an end of
    // stream either: the reader changed its track list mid-file (chained
    // streams); treating it as EOF silently truncated everything after the
    // first chain link.
    let mut track_id = track_id;
    let mut skipped_packets: u64 = 0;
    // The rate the decoder PRODUCES, which the container can misstate: symphonia has no SBR,
    // so an HE-AAC track decodes its AAC-LC core at half the rate its sample entry declares.
    // Labelled with the container's rate, such an import played an octave up at double speed,
    // and was baked that way into the library WAV that replaced the original.
    let mut out_rate: Option<u32> = None;
    // The shares of a file of more than two channels, for the layout they were made for.
    let mut downmix: Option<(symphonia::core::audio::Channels, Vec<[Smp; 2]>)> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break; // normal end of stream
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                // Rebuild the decoder against the new audio track and keep
                // going. A mid-stream sample-rate change is refused honestly:
                // this single-rate output cannot represent it.
                let track = audio_track(&*format)
                    .ok_or_else(|| err("reset: no decodable audio track".to_string()))?;
                if track.codec_params.sample_rate.unwrap_or(src_rate) != src_rate {
                    return Err(err(format!(
                        "mid-stream sample-rate change in {path} is unsupported"
                    )));
                }
                track_id = track.id;
                // The finished link's padding goes from its own end, never into an
                // earlier link, and the next link brings its own: kept, the first
                // link's tail sat mid-file and the second's was never cut.
                let kept = (out.len() as u64).saturating_sub(padding);
                out.truncate(kept.max(link_start as u64) as usize);
                (skip, padding) = edge_trim(&track.codec_params);
                link_start = out.len();
                let params = decoder_params(&track.codec_params);
                if decoder_overallocates(&params) {
                    return Err(DecodeError::TooBig);
                }
                decoder = symphonia::default::get_codecs()
                    .make(&params, &DecoderOptions::default())
                    .map_err(|e| err(format!("make decoder after reset: {e}")))?;
                continue;
            }
            Err(symphonia::core::errors::Error::IoError(e)) => {
                return Err(DecodeError::Io(format!("read {path}: {e}")));
            }
            // Ends the decode, never skipped: the MP4 reader keeps a fragment it then fails to
            // finish reading, and media_screen checks only what a fragment's own bounds hold.
            Err(e) => return Err(err(format!("packet: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Corrupt DATA is skipped (one damaged frame must not kill a
            // recovering take) but counted, so a file that decodes to
            // nothing reports how much was thrown away. Anything else —
            // an unsupported codec feature, an IO failure surfacing through
            // the decoder — would fail every remaining packet too; burning
            // through the whole file for a generic "decoded no audio" at the
            // end hides the reason, so fail fast with it instead.
            Err(symphonia::core::errors::Error::DecodeError(_)) => {
                skipped_packets += 1;
                continue;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                decoder.reset();
                skipped_packets += 1;
                continue;
            }
            Err(e) => return Err(err(format!("decode: {e}"))),
        };
        let spec = *decoded.spec();
        match out_rate {
            None => {
                out_rate = Some(spec.rate);
                if spec.rate != src_rate {
                    if !(MIN_SRC_RATE..=MAX_SRC_RATE).contains(&spec.rate) {
                        return Err(DecodeError::RateUnsupported);
                    }
                    if max_src_rate > 0 && spec.rate > max_src_rate {
                        return Err(DecodeError::TooBig);
                    }
                    (duration_frames, max_frames) = budgets(spec.rate);
                }
            }
            Some(r) if r != spec.rate => {
                return Err(err(format!(
                    "mid-stream sample-rate change in {path} is unsupported"
                )));
            }
            Some(_) => {}
        }
        let ch = spec.channels.count();
        let mut sbuf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sbuf.copy_interleaved_ref(decoded);
        let packet_frames = sbuf.samples().len() / ch.max(1);
        // The budget is an ALLOCATION cap, not just a logical-length cap: judged BEFORE
        // this packet's frames are pushed, and the Vec's growth is clamped to the budget
        // (a plain doubling push could hold up to 2x the cap in capacity — a 173M-frame
        // budget transiently backed by a 268M-frame allocation). Containers without a
        // declared frame count (e.g. plain mp3) are bounded exactly the same way,
        // packet by packet. Judged as trimmed: the delay is never pushed, and the
        // padding, cut only once its link ends, may sit past the budget meanwhile.
        let needed = out.len() + (packet_frames as u64).saturating_sub(skip) as usize;
        let budget = max_frames.saturating_add(padding);
        if needed as u64 > budget {
            return Err(budget_err(
                (needed as u64).saturating_sub(padding),
                duration_frames,
            ));
        }
        if needed > out.capacity() {
            // Amortized geometric growth, capped at the budget so capacity can never
            // exceed what the budget promised the caller.
            let cap_limit = budget.min(usize::MAX as u64) as usize;
            let target = out
                .capacity()
                .max(4096)
                .saturating_mul(2)
                .clamp(needed, cap_limit);
            out.reserve_exact(target - out.len());
        }
        if ch > 2
            && downmix
                .as_ref()
                .is_none_or(|(layout, _)| *layout != spec.channels)
        {
            downmix = Some((spec.channels, downmix_gains(spec.channels)));
        }
        for frame in sbuf.samples().chunks(ch.max(1)) {
            if skip > 0 {
                skip -= 1; // the encoder's delay (see edge_trim)
                continue;
            }
            // Scrub NaN/Inf (and clamp absurd amplitudes) at the single deepest decode
            // point: one poisoned sample would otherwise propagate through every FFT,
            // metric, and DSP node fed from here (the WAV streaming reader scrubs
            // independently; this covers the symphonia paths — analysis via ve_decode
            // AND playback staging).
            let (l, r) = match &downmix {
                Some((_, gains)) if ch > 2 => {
                    frame.iter().zip(gains).fold((0.0, 0.0), |(l, r), (&v, g)| {
                        let v = scrub_sample(v);
                        (l + v * g[0], r + v * g[1])
                    })
                }
                _ => (frame[0], if ch > 1 { frame[1] } else { frame[0] }),
            };
            out.push([scrub_sample(l), scrub_sample(r)]);
        }
    }
    // Only the 8 bytes the reader overran, at most 8 frames: a count left short by its writer
    // must not cut real sound.
    if let Some(n) = aiff_frames.filter(|&n| (out.len() as u64).saturating_sub(n) <= 8) {
        out.truncate(usize::try_from(n).unwrap_or(usize::MAX));
    }
    // From the end of what decoded, not where the header says the stream ends, and never
    // into an earlier link.
    let kept = (out.len() as u64).saturating_sub(padding);
    if kept > 0 {
        out.truncate(kept.max(link_start as u64) as usize);
    }
    if out.is_empty() {
        return Err(err(format!(
            "decoded no audio from {path} ({skipped_packets} corrupt packets skipped)"
        )));
    }
    Ok((out, out_rate.unwrap_or(src_rate)))
}

/// Absolute source-sample-rate envelope for every decoded file, mirroring the Dart analysis
/// gate. Outside it nothing is a real recording, and the extremes are attacks: 0 panics
/// symphonia's TimeBase, 1 Hz turns the sinc resampler into an allocation/CPU bomb, and
/// beyond 384 kHz the per-second memory cost escapes every budget derivation.
pub const MIN_SRC_RATE: u32 = 8_000;
pub const MAX_SRC_RATE: u32 = 384_000;

/// One decoded sample, made safe for every consumer downstream (see
/// [`crate::sample::scrub_sample`]).
#[inline]
fn scrub_sample(v: Smp) -> Smp {
    crate::sample::scrub_sample(v)
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

/// Reads just enough of an AIFF or AIFF-C header to report the frame count its COMM chunk
/// declares; `Ok(None)` when the file is not an AIFF. symphonia's reader ends the sound data
/// 8 bytes past its chunk (it counts the chunk's offset and block-size fields as sound), so an
/// AIFF with a chunk after its sound, where iTunes and taggers write an ID3 tag, decoded that
/// chunk's header as a click at its very end.
fn sniff_aiff_frames(path: &str) -> std::io::Result<Option<u64>> {
    use std::io::{Read, Seek};
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 12];
    if f.read(&mut head)? < 12
        || &head[0..4] != b"FORM"
        || !(&head[8..12] == b"AIFF" || &head[8..12] == b"AIFC")
    {
        return Ok(None);
    }
    // Bounded, as the WAV sniff's walk is.
    let mut hdr = [0u8; 8];
    for _ in 0..64 {
        if f.read(&mut hdr)? < 8 {
            return Ok(None);
        }
        let size = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
        if &hdr[0..4] == b"COMM" {
            // The channel count, then the frame count.
            let mut comm = [0u8; 6];
            if size < 6 || f.read(&mut comm)? < 6 {
                return Ok(None);
            }
            return Ok(Some(u64::from(u32::from_be_bytes([
                comm[2], comm[3], comm[4], comm[5],
            ]))));
        }
        f.seek(std::io::SeekFrom::Current((size + (size & 1)) as i64))?;
    }
    Ok(None)
}

/// A file read with one 8-byte field replaced (see [`crate::media_screen::caf_open_data`]).
struct PatchedFile {
    file: std::fs::File,
    at: u64,
    bytes: [u8; 8],
}

impl std::io::Read for PatchedFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Seek;
        let pos = self.file.stream_position()?;
        let n = self.file.read(buf)?;
        for p in pos.max(self.at)..(pos + n as u64).min(self.at + 8) {
            buf[(p - pos) as usize] = self.bytes[(p - self.at) as usize];
        }
        Ok(n)
    }
}

impl std::io::Seek for PatchedFile {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.file.seek(to)
    }
}

impl symphonia::core::io::MediaSource for PatchedFile {
    fn is_seekable(&self) -> bool {
        self.file.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        self.file.byte_len()
    }
}

/// What symphonia reads `file` through: the file itself, or for a CAF whose data runs to its
/// end, the file with that size filled in. The screen judged the file as read this way.
fn media_source(mut file: std::fs::File) -> Box<dyn symphonia::core::io::MediaSource> {
    match crate::media_screen::caf_open_data(&mut file) {
        Ok(Some((at, size))) => Box::new(PatchedFile {
            file,
            at,
            bytes: i64::try_from(size).unwrap_or(i64::MAX).to_be_bytes(),
        }),
        _ => Box::new(file),
    }
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
    // deterministic round(n * ratio) duration contract on the length. Cut the window out
    // IN PLACE — a copy into a second Vec held two full-length outputs alive at peak,
    // doubling this path's high-water mark on hour-long tracks.
    outbuf.truncate((delay + exact) * CHANNELS);
    outbuf.drain(..delay * CHANNELS);
    for v in outbuf.iter_mut() {
        // Second scrub line: the convolution of large-but-finite inputs can overflow to
        // NaN/Inf even though the decode root sanitized its samples (and callers may feed
        // this converter PCM that never went through decode_file at all).
        *v = scrub_sample(*v);
    }
    Ok(frames_from_flat(outbuf))
}

/// Reinterpret a flat interleaved stereo Vec as stereo frames WITHOUT copying — the
/// inverse of the FFI layer's `flatten_stereo`. Sound only when both the length and the
/// allocated capacity are whole frames; `vec![0.0; n * CHANNELS]` allocates exactly its
/// length, and truncate/drain never change capacity, so `resample_sinc` upholds both.
fn frames_from_flat(mut v: Vec<Smp>) -> Vec<[Smp; CHANNELS]> {
    debug_assert_eq!(v.len() % CHANNELS, 0);
    debug_assert_eq!(v.capacity() % CHANNELS, 0);
    let ptr = v.as_mut_ptr() as *mut [Smp; CHANNELS];
    let len = v.len() / CHANNELS;
    let cap = v.capacity() / CHANNELS;
    std::mem::forget(v);
    // SAFETY: same allocation; [Smp; CHANNELS] and CHANNELS consecutive Smp share size
    // and alignment, len/cap are exact multiples (asserted above), and the original Vec
    // is forgotten so ownership transfers exactly once.
    unsafe { Vec::from_raw_parts(ptr, len, cap) }
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
        // 100% of output frames). The root clamp bounds every decoded sample to
        // SAMPLE_CEILING, which no convolution with unit-gain kernels can overflow.
        let p = temp_wav("huge");
        f32_wav_at_rate(
            &p,
            44_100,
            &[f32::MAX, f32::MAX, -f32::MAX, -f32::MAX, 0.5, 0.5],
        );
        let (frames, rate) = super::decode_file(p.to_str().unwrap(), 0.0, 0, 0).unwrap();
        assert!(frames
            .iter()
            .all(|f| f[0].abs() <= crate::sample::SAMPLE_CEILING
                && f[1].abs() <= crate::sample::SAMPLE_CEILING));
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
    fn a_failed_start_does_not_poison_the_next_take() {
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(1024);
        let w = RecordWriter::spawn(cons);
        // The failed create is fully reported by its own ack; the sticky flag must
        // describe only takes, so the NEXT take starts with a clean slate.
        assert!(!w.start("/nonexistent-vocara-dir/take.wav"));
        let path = temp_wav("after_failed_start");
        assert!(w.start(path.to_str().unwrap()), "a valid path must create");
        for i in 0..50 {
            assert!(prod.push([i as f32 / 100.0, 0.0]));
        }
        assert!(
            w.stop(),
            "a clean take after a failed start must not inherit the create failure"
        );
        let _ = std::fs::remove_file(&path);
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

    /// Not recording, the writer has nothing to do until a command arrives, so it parks on its
    /// command channel. It used to wake every 5 ms for the engine's whole life (twice that with
    /// the technique page's engine alive) to drain a ring nothing writes to between takes.
    /// Observable from the producer side: what an idle writer would have drained stays in the
    /// ring until the next Start sweeps it, as a stale tail always has been.
    #[test]
    fn an_idle_writer_parks_instead_of_polling() {
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(64);
        let w = RecordWriter::spawn(cons);
        std::thread::sleep(std::time::Duration::from_millis(50)); // reach the idle wait
        for _ in 0..10 {
            assert!(prod.push([0.5, 0.0]));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            prod.free(),
            prod.capacity() - 10,
            "an idle writer woke up and drained the ring"
        );
        // A command still wakes it at once; the take's Start sweeps the stale frames.
        let path = temp_wav("idle_parks");
        assert!(w.start(path.to_str().unwrap()));
        assert!(w.stop());
        assert_eq!(prod.free(), prod.capacity());
        let bytes = std::fs::read(&path).expect("recorded WAV must exist");
        assert_eq!(bytes.len(), 44, "stale frames must not lead the take");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn shutdown_mid_take_keeps_the_buffered_tail() {
        // ve_destroy drops the writer with a take still recording: the frames still in the
        // ring are the take's end, and Shutdown must write them as Stop does.
        use super::{writer_run, WriterCmd};
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(1024);
        let (tx, rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || writer_run(cons, rx));
        let path = temp_wav("shutdown_tail");
        let (ack, ack_rx) = std::sync::mpsc::channel();
        tx.send(WriterCmd::Start(path.to_str().unwrap().to_string(), ack))
            .unwrap();
        assert!(ack_rx.recv().unwrap(), "start must ack the file creation");
        // Let the writer find the ring empty and park in its idle sleep, so the tail and
        // the Shutdown both wait for it there (commands are handled before the drain).
        std::thread::sleep(std::time::Duration::from_millis(2));
        for _ in 0..20 {
            assert!(prod.push([0.75, 0.0]));
        }
        tx.send(WriterCmd::Shutdown).unwrap();
        join.join().unwrap();
        let bytes = std::fs::read(&path).expect("recorded WAV must exist");
        // 44-byte header + the take's 20 mono float32 samples.
        assert_eq!(
            bytes.len(),
            44 + 20 * 4,
            "the buffered tail must reach the take"
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

    /// The sticky IO-failure flag exists so a take damaged mid-recording is reported through
    /// the next Stop's ack. A Start while that take is still live must CARRY the damage
    /// forward (its own ack only covers the new file's creation), not recompute the flag
    /// from the restart's own tail-drain/finalize — those can succeed even though the take
    /// already lost samples, and the damage would evaporate unreported.
    #[test]
    fn a_restart_keeps_mid_take_damage_sticky_until_stop_reports_it() {
        use super::{writer_run, WriterCmd};
        let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(64);
        let (tx, rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || writer_run(cons, rx));
        // Take A's file creates cleanly but fails its FIRST data write (the injected
        // stand-in for a disk filling mid-take).
        let path_a = temp_wav(&format!("sticky_a_{}", crate::wav::TEST_FAIL_FIRST_WRITE));
        let (ack, ack_rx) = std::sync::mpsc::channel();
        tx.send(WriterCmd::Start(path_a.to_str().unwrap().to_string(), ack))
            .unwrap();
        assert!(ack_rx.recv().unwrap(), "take A's file creation is clean");
        // One frame whose write fails. The drain loop pops it and writes (and records the
        // failure) within the same iteration, before the next command check — so once the
        // ring reports empty, the damage is on the sticky flag and NOT attributable to the
        // restart's own tail-drain (which sees an empty ring and succeeds).
        assert!(prod.push([0.5, 0.0]));
        while prod.free() != prod.capacity() {
            std::thread::yield_now();
        }
        // Restart straight into take B: its create, tail-drain, and finalize all succeed.
        let path_b = temp_wav("sticky_b");
        let (back, back_rx) = std::sync::mpsc::channel();
        tx.send(WriterCmd::Start(path_b.to_str().unwrap().to_string(), back))
            .unwrap();
        assert!(back_rx.recv().unwrap(), "take B's file creation is clean");
        assert!(prod.push([0.25, 0.0]));
        let (sack, sack_rx) = std::sync::mpsc::channel();
        tx.send(WriterCmd::Stop(sack)).unwrap();
        assert!(
            !sack_rx.recv().unwrap(),
            "the Stop after a restart must still report take A's mid-take damage"
        );
        tx.send(WriterCmd::Shutdown).unwrap();
        join.join().unwrap();
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
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
        let res = OutResampler::new(device_rate, BLOCK, Arc::clone(&cell), cell.claim());
        (h, res)
    }

    /// The kernel loop as it was first written (one device frame at a time), kept as the
    /// reference [`super::sinc_run`] is held to bit for bit.
    fn sinc_run_reference(
        table: &[f32],
        fifo: &[[Smp; CHANNELS]],
        origin: i64,
        pos: &mut f64,
        step: f64,
        out: &mut [Smp],
    ) {
        use super::{SINC_HALF, SINC_OVERSAMPLE, SINC_TAPS};
        for k in 0..out.len() / CHANNELS {
            let ip = pos.floor();
            let frac = *pos - ip;
            let rf = frac * SINC_OVERSAMPLE as f64;
            let r0 = rf.floor() as usize;
            let rw = (rf - r0 as f64) as f32;
            let row0 = r0 * SINC_TAPS;
            let row1 = (r0 + 1) * SINC_TAPS;
            let base = (ip as i64 - origin) as usize - (SINC_HALF - 1);
            let mut l = 0.0f32;
            let mut r = 0.0f32;
            for t in 0..SINC_TAPS {
                let w = table[row0 + t] + (table[row1 + t] - table[row0 + t]) * rw;
                let f = fifo[base + t];
                l += f[0] * w;
                r += f[1] * w;
            }
            out[k * CHANNELS] = l;
            out[k * CHANNELS + 1] = r;
            *pos += step;
        }
    }

    #[test]
    fn the_interleaved_kernel_matches_the_frame_by_frame_loop_bit_for_bit() {
        // Every rate the boundary meets (up- and downsampling), ragged run lengths (the
        // four-frame groups and the remainder), a far-advanced read position (uptime), and
        // signed zeros in the FIFO.
        let mut x = 0x5EEDu32;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32) - 0.5
        };
        for device_rate in [
            44_100.0f64,
            88_200.0,
            96_000.0,
            32_000.0,
            22_050.0,
            192_000.0,
        ] {
            let step = ENGINE_SR / device_rate;
            let table = super::build_sinc_table(step);
            let fifo: Vec<[Smp; CHANNELS]> = (0..4096)
                .map(|i| match i % 17 {
                    3 => [-0.0, 0.0],
                    _ => [rnd(), rnd()],
                })
                .collect();
            for (start, frames) in [(40.0f64, 1usize), (40.25, 3), (40.5, 4), (100.125, 517)] {
                for origin in [0i64, 1_000_000] {
                    let mut p_new = start + origin as f64 + 0.3 * step;
                    let mut p_ref = p_new;
                    let mut out_new = vec![0.0f32; frames * CHANNELS];
                    let mut out_ref = vec![1.0f32; frames * CHANNELS];
                    super::sinc_run(&table, &fifo, origin, &mut p_new, step, &mut out_new);
                    sinc_run_reference(&table, &fifo, origin, &mut p_ref, step, &mut out_ref);
                    assert_eq!(
                        p_new.to_bits(),
                        p_ref.to_bits(),
                        "position, rate {device_rate}"
                    );
                    for (k, (a, b)) in out_new.iter().zip(&out_ref).enumerate() {
                        assert_eq!(a.to_bits(), b.to_bits(), "rate {device_rate} sample {k}");
                    }
                }
            }
        }
    }

    /// Timing loop (ignored): the 48 kHz -> 44.1 kHz output boundary, 512-frame device
    /// callbacks. `cargo test --release --features device -- --ignored --nocapture
    /// bench_output_resampler`.
    #[test]
    #[ignore]
    fn bench_output_resampler() {
        let (h, mut res) = tone_engine(1_000.0, 44_100.0);
        h.play();
        let frames = 512usize;
        let mut out = vec![0.0; frames * CHANNELS];
        for _ in 0..50 {
            res.process(&mut out, frames);
        }
        let mut best = f64::MAX;
        for _ in 0..25 {
            let t = std::time::Instant::now();
            for _ in 0..40 {
                res.process(&mut out, frames);
            }
            best = best.min(t.elapsed().as_nanos() as f64 / 40.0);
        }
        println!(
            "output resampler 48k->44.1k, 512-frame callback (incl. render): {:.2} us ({:.2}% of real time)",
            best / 1e3,
            100.0 * best / (frames as f64 / 44_100.0 * 1e9)
        );
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

    #[test]
    fn the_output_clock_counts_what_the_device_played_through_flushes() {
        // The drift estimator's output clock is what the device played. The consumed cursor
        // jumps a flushed FIFO's unplayed frames (hundreds per seek on a 44.1 kHz device), and
        // a count kept off the read position lost the fraction of a frame each flush left in
        // it: 258 pause/play toggles lost 231 frames, a bias the estimator read as drift.
        let (h, mut res) = tone_engine(1_000.0, 44_100.0);
        h.play();
        let t = Arc::clone(&res.rt);
        let mut out = vec![0.0; 512 * CHANNELS];
        // A 512-frame callback at 44.1 kHz plays 557.28 engine frames: the fraction carries.
        let per = 512.0 * ENGINE_SR / 44_100.0;
        let mut jumped = false;
        for k in 0..300 {
            let consumed = t.transport().consumed_frames();
            let played = t.transport().played_frames();
            if k % 3 == 0 {
                h.seek(10_000 + k * 37);
            }
            res.process(&mut out, 512);
            let heard = t.transport().played_frames() - played;
            assert!(
                (557..=558).contains(&heard),
                "callback {k} played {heard} engine frames"
            );
            jumped |= t.transport().consumed_frames() - consumed > 558;
        }
        assert!(jumped, "a flush must have jumped the cursor");
        let played = t.transport().played_frames();
        let want = (300.0 * per) as u64;
        assert!(played.abs_diff(want) <= 1, "played {played} of {want}");
        assert!((557..=558).contains(&t.transport().take_played_quantum()));
    }

    #[test]
    fn a_low_rate_device_hears_nothing_of_the_engines_top_octave() {
        // 23 kHz is above every call-profile output's Nyquist. With the phase-0 row's centre
        // tap at 1 instead of fc, that row passed it nearly unfiltered: -14 dBc of alias at
        // 32 kHz, -9 dBc at 24 kHz.
        for device in [32_000.0, 24_000.0, 16_000.0] {
            let (h, mut res) = tone_engine(23_000.0, device);
            h.play();
            let frames = 1 << 15;
            let mut out = vec![0.0; frames * CHANNELS];
            res.process(&mut out, frames);
            let skip = 4_096;
            let sq: f64 = (skip..frames)
                .map(|i| (out[i * CHANNELS] as f64).powi(2))
                .sum();
            let rms = (sq / (frames - skip) as f64).sqrt();
            assert!(rms < 1e-3, "{device} Hz device: alias residue RMS {rms}");
        }
    }

    #[test]
    fn hours_of_uptime_never_read_past_the_fifo() {
        // The state at the start of a callback after ~5.8 h of 44.1 kHz streaming with no
        // seek/pause/play, captured from an exact-arithmetic simulation: the read
        // position's iterated sum lands one frame past what the fill computed, and the
        // last kernel tap indexed `fifo[len]` — a panic unwinding into CoreAudio.
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: Vec::<Edge>::new(),
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, 1024).unwrap();
        let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (_h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        let cell = RtCell::new(rt);
        let mut res = OutResampler::new(44_100.0, 1024, Arc::clone(&cell), cell.claim());
        res.pos = f64::from_bits(4_741_681_551_278_637_648);
        res.origin = 1_001_160_460;
        res.consumed_floor = res.pos.floor() as i64;
        res.fifo.clear();
        for _ in 0..244 {
            res.fifo.push([0.0, 0.0]);
        }
        let mut out = vec![0.0f32; 2048 * CHANNELS];
        res.process(&mut out, 2048);
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
        let res = OutResampler::new(44_100.0, BLOCK, Arc::clone(&cell), cell.claim());
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
    fn a_degenerate_loop_request_is_clamped_and_still_maps_exactly() {
        let (h, mut res) = ramp_engine();
        // A 10-frame loop request is clamped to MIN_LOOP_FRAMES (256):
        // unclamped, one render block wraps dozens of times and overwrites
        // the whole segment ring inside the mapping window, detaching the
        // playhead from the consumed cursor. Clamped to [200, 456), the 480
        // consumed frames are 256 to the wrap plus 224 after it.
        h.set_loop(true, 200, 210);
        h.seek(200);
        h.play();
        let mut out = vec![0.0; 441 * CHANNELS];
        res.process(&mut out, 441);
        let frame = h.frame();
        assert!(
            (423..=424).contains(&frame),
            "clamped-loop playhead must map exactly (~200 + 480 - 256), got {frame}"
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
        event: Arc<AtomicU32>,
    }

    fn cell_with_rec_capacity(rec_cap: usize) -> RecCell {
        let (mic_prod, _mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 10);
        let (cap_prod, _cap_cons) = spsc::<[Smp; CHANNELS]>(1 << 10);
        let (rec_prod, _rec_cons) = spsc::<[Smp; CHANNELS]>(rec_cap);
        let rec_active = Arc::new(AtomicBool::new(true));
        let frames_in = Arc::new(AtomicU64::new(0));
        let rec_dropped = Arc::new(AtomicU64::new(0));
        let rec_written = Arc::new(AtomicU64::new(0));
        let event = Arc::new(AtomicU32::new(0));
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
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&event),
        );
        // Identity resample path (input already at ENGINE_SR), mono input.
        cell.reconfigure(crate::sample::ENGINE_SR, 1);
        RecCell {
            cell,
            rec_active,
            frames_in,
            rec_dropped,
            rec_written,
            event,
        }
    }

    #[test]
    fn a_take_at_the_wav_addressing_ceiling_stops_and_raises_the_event() {
        // A canonical RIFF header addresses at most MAX_F32_SAMPLES; appending
        // past it does not fail, it produces a file whose header stops covering
        // its own tail — hours of audio no reader can reach, behind a clean
        // stop code and rec_dropped == 0. The fan-out refuses instead, and
        // says so on the one channel an RT callback can reach the UI through.
        let c = cell_with_rec_capacity(1 << 10);
        // Start one frame below the ceiling so this callback straddles it.
        c.rec_written
            .store(crate::wav::MAX_F32_SAMPLES - 1, Ordering::Relaxed);
        let block = [0.25f32; 2];
        c.cell.feed_test(&block);

        assert_eq!(
            c.rec_written.load(Ordering::Relaxed),
            crate::wav::MAX_F32_SAMPLES,
            "recording stops exactly at the addressable ceiling"
        );
        assert_eq!(
            c.rec_dropped.load(Ordering::Relaxed),
            0,
            "a take at its limit is COMPLETE, not holed — that is what dropped means"
        );
        assert_ne!(
            c.event.load(Ordering::Relaxed) & super::EVENT_RECORD_LIMIT,
            0,
            "the UI must learn to finalize; the clock would otherwise keep running"
        );
    }

    #[test]
    fn recording_ring_overflow_is_counted() {
        let c = cell_with_rec_capacity(4);
        // 12 mono frames into a 4-slot recording ring: 4 fit (written), 8 dropped.
        let data = [0.5f32; 12];
        // SAFETY: no input callback exists in this test; we are the single accessor.
        c.cell.feed_test(&data);
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
        c.cell.feed_test(&data);
        assert_eq!(c.rec_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(c.rec_written.load(Ordering::Relaxed), 32);
        // Frames emitted while recording is DISARMED never touch the ring or either
        // counter — this is exactly why rec_written is the per-take proof the global
        // frame counter cannot be: a pre-arm callback advances frames_in but not this.
        c.rec_active.store(false, Ordering::Release);
        c.cell.feed_test(&data);
        assert_eq!(c.rec_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(c.rec_written.load(Ordering::Relaxed), 32);
        assert_eq!(c.frames_in.load(Ordering::Relaxed), 64);
    }

    /// The cpal input path: on a 2-input interface with the mic on INPUT 2 and Input 2 chosen,
    /// input 2 is what the take records — and the monitor hears it in the middle. The feed used to
    /// carry inputs 1 and 2 as left and right and the writer keeps the left channel, so a mic on
    /// input 2 recorded silence and was monitored hard right.
    #[test]
    fn input_2_is_what_gets_recorded() {
        let (mic_prod, mut mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 12);
        let (cap_prod, _cap_cons) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, mut rec_cons) = spsc::<[Smp; CHANNELS]>(1 << 12);
        let cell = InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(1.0f64.to_bits())),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU32::new(0)),
        );
        cell.reconfigure(crate::sample::ENGINE_SR, 2);
        cell.set_channel_mode(crate::capture_layout::INPUT_CHANNEL_2);
        // Input 1 idle (silence), the voice on input 2.
        let voice = |n: usize| 0.3 * (n as f32 * 0.05).sin();
        let block: Vec<f32> = (0..512).flat_map(|n| [0.0, voice(n)]).collect();
        cell.feed_test(&block);

        let mut n = 0;
        while let Some(f) = rec_cons.pop() {
            assert_eq!(f[0], voice(n), "the take's frame {n} is input 2");
            n += 1;
        }
        assert_eq!(n, 512);
        while let Some(f) = mic_cons.pop() {
            assert_eq!(
                f[0], f[1],
                "the monitor hears the mic centred, not hard right"
            );
        }
    }
}

#[cfg(test)]
mod output_safety_tests {
    //! What the engine renders to is not what the UI asked for, and the hear-yourself monitor's
    //! feedback judgement has to follow the former. These pin the decision itself; the two device
    //! lookups it wraps need real CoreAudio, so every case here fixes both uids explicitly.

    use super::{
        reconcile_echo_free_with, reconcile_output_safety, reconcile_output_safety_with,
        restore_output_or_flag, system_default_output_uid, OutputBinding, VpioOutput,
        EVENT_OUTPUT_LOST, EVENT_OUTPUT_REROUTED,
    };
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::live::{spawn, EngineHandle};
    use crate::ring::spsc;
    use crate::sample::{Smp, CHANNELS, ENGINE_SR};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    fn engine() -> EngineHandle {
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: Vec::<Edge>::new(),
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, 128).unwrap();
        let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (h, _rt) = spawn(g, HashMap::new(), mic_cons, 256);
        h
    }

    /// A cpal output on the requested device is the ordinary case: nothing is rerouted, so the
    /// monitor is left to the fader.
    #[test]
    fn playback_on_the_requested_device_leaves_the_monitor_alone() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-A".into(),
            cpal_uid: "dev-A".into(),
            cpal_follows_default: false,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        assert!(!reconcile_output_safety(h.transport(), &b, &event));
        assert!(!h.transport().monitor_muted());
        assert_eq!(event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED, 0);
    }

    /// The canceller passes the mic through only while the output the host judged echo-free is
    /// the one rendering: a judgement from before the output moved (a fallback, the mic opening)
    /// left a whole speaker echo in the take until the host's next look.
    #[test]
    fn echo_free_holds_only_on_the_judged_output() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "airpods".into(),
            cpal_uid: "airpods".into(),
            vpio: VpioOutput::Off,
            ..Default::default()
        });
        reconcile_output_safety(h.transport(), &b, &event);
        assert!(!h.transport().echo_free(), "nothing judged yet");

        b.lock().unwrap().set_echo_free_uid("airpods");
        reconcile_output_safety(h.transport(), &b, &event);
        assert!(h.transport().echo_free());

        // A mic opening: nothing known renders until the unit is up.
        b.lock().unwrap().vpio = VpioOutput::Opening;
        reconcile_output_safety(h.transport(), &b, &event);
        assert!(!h.transport().echo_free());

        // Playback lands on another device (here a fallback output): the speaker's echo is
        // cancelled whatever the host last judged.
        {
            let mut g = b.lock().unwrap();
            g.vpio = VpioOutput::Off;
            g.cpal_uid = "desk-speakers".into();
        }
        reconcile_output_safety(h.transport(), &b, &event);
        assert!(!h.transport().echo_free());

        // Back on the judged output it holds again, and the host clearing it ends it.
        b.lock().unwrap().cpal_uid = "airpods".into();
        reconcile_output_safety(h.transport(), &b, &event);
        assert!(h.transport().echo_free());
        b.lock().unwrap().set_echo_free_uid("");
        reconcile_output_safety(h.transport(), &b, &event);
        assert!(!h.transport().echo_free());
    }

    /// The host names its echo-free judgement on every tick, right after the tick's poll. That
    /// must not count as a second poll: a one-tick blip (a headset's output list empty while it
    /// switches profile) would then be announced as a reroute.
    #[test]
    fn a_new_echo_free_judgement_does_not_count_as_a_poll() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-headset".into(),
            vpio: VpioOutput::Bound("dev-headset".into()),
            ..Default::default()
        });
        let tick = std::cell::Cell::new(0u32);
        // The headset's output list is empty during tick 3 only.
        let serves = |_: &str| tick.get() != 3;
        for t in 0..10 {
            tick.set(t);
            reconcile_output_safety_with(h.transport(), &b, &event, &serves, &|| None);
            b.lock().unwrap().set_echo_free_uid("dev-headset");
            reconcile_echo_free_with(h.transport(), &b, &serves);
            assert_eq!(h.transport().echo_free(), t != 3, "tick {t}");
        }
        assert_eq!(event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED, 0);
    }

    /// One reconcile reads the route once: the monitor mute and the echo-free flag are judged
    /// from the same answer, so a headset's output list flickering between two reads can't split
    /// them (and the poll pays one hardware read, not two).
    #[test]
    fn one_reconcile_reads_the_route_once() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-headset".into(),
            vpio: VpioOutput::Bound("dev-headset".into()),
            echo_free_uid: "dev-headset".into(),
            ..Default::default()
        });
        let reads = std::cell::Cell::new(0u32);
        // Serves output on the first read only: a second read would see the default instead.
        let serves = |_: &str| {
            reads.set(reads.get() + 1);
            reads.get() == 1
        };
        reconcile_output_safety_with(h.transport(), &b, &event, &serves, &|| None);
        assert_eq!(reads.get(), 1);
        assert!(!h.transport().monitor_muted());
        assert!(h.transport().echo_free());
    }

    /// The requested device could not be opened and the fallback took the default: the UI still
    /// shows (and judges feedback safety from) the device the user picked, which is no longer the
    /// one making sound.
    #[test]
    fn a_fallback_output_mutes_the_monitor_and_reports_once() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-A".into(),
            cpal_uid: "dev-B".into(),
            cpal_follows_default: false,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        // Muted at once; announced once the reroute has held for a second poll.
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert!(h.transport().monitor_muted());
        assert_eq!(event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED, 0);
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert_eq!(
            event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED,
            EVENT_OUTPUT_REROUTED
        );
        // Edge-triggered: the poller reconciles about once a second, and a level-triggered event
        // would re-toast for as long as the reroute lasts.
        event.store(0, Ordering::Relaxed);
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert_eq!(event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED, 0);
    }

    /// One poll's reroute can be a blip — a Bluetooth headset's output list is briefly empty while
    /// VPIO switches its profile — so it mutes the monitor but is not announced; every blip used
    /// to toast "playback moved".
    #[test]
    fn a_one_poll_blip_mutes_the_monitor_without_announcing_it() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-headset".into(),
            cpal_uid: String::new(),
            cpal_follows_default: false,
            vpio: VpioOutput::Bound("dev-headset".into()),
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        let blip = std::cell::Cell::new(0u32);
        let probe = |_: &str| {
            blip.set(blip.get() + 1);
            blip.get() != 3 && blip.get() != 11
        };
        let mut muted_polls = 0;
        for _ in 0..20 {
            reconcile_output_safety_with(h.transport(), &b, &event, &probe, &|| None);
            if h.transport().monitor_muted() {
                muted_polls += 1;
            }
        }
        assert_eq!(muted_polls, 2, "each blip mutes its own poll");
        assert_eq!(event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED, 0);
    }

    /// Nothing open is not a reroute — there is no sound to come out of the wrong speaker, and
    /// muting here would leave the monitor down after the stream is rebuilt.
    #[test]
    fn a_closed_output_is_not_a_reroute() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-A".into(),
            cpal_uid: String::new(),
            cpal_follows_default: false,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        assert!(!reconcile_output_safety(h.transport(), &b, &event));
        assert!(!h.transport().monitor_muted());
    }

    /// The pick reported back to the UI is VERBATIM, never resolved. `ve_current_output_device`
    /// returns this, and the output-lost self-heal resubmits whatever it returns — so an empty
    /// pick has to stay empty. Reporting the system default's resolved uid instead promoted
    /// "follow the default" to an explicit pick of that one device, and unplugging it then left
    /// `rerouted` true forever (monitor hard-muted, reroute toast) for a user who never chose an
    /// output at all.
    #[test]
    fn the_reported_pick_is_verbatim_not_resolved() {
        let following_default = OutputBinding {
            requested: String::new(),
            cpal_uid: "whatever-the-default-resolved-to".into(),
            cpal_follows_default: true,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        };
        assert_eq!(following_default.requested_pick(), "");

        let explicit = OutputBinding {
            requested: "dev-A".into(),
            // A failed open left the fallback playing: the PICK is still dev-A, so resubmitting it
            // re-attaches that device when it comes back.
            cpal_uid: "dev-B".into(),
            cpal_follows_default: false,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        };
        assert_eq!(explicit.requested_pick(), "dev-A");
    }

    /// A stream built on the system default follows it (cpal's DefaultOutput unit reroutes by
    /// itself), so the uid resolved at build time is stale after the first default change.
    /// Reading that snapshot as "what is playing" made every default change (headphones
    /// plugged in) look like a reroute away from the user's choice — monitor hard-muted, a
    /// false "playback moved" warning — for a user who never picked an output. (The one case
    /// here that consults the real default: both sides of the comparison resolve it through
    /// the same lookup, so it holds on any machine, including one with no output at all.)
    #[test]
    fn a_stream_following_the_default_is_not_rerouted_when_the_default_moves() {
        let following = OutputBinding {
            requested: String::new(),
            cpal_uid: "the-default-when-the-stream-was-built".into(),
            cpal_follows_default: true,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        };
        assert_eq!(following.actual_uid(), system_default_output_uid());
        assert!(!following.rerouted());
    }

    /// While a VPIO unit opens, the binding still held the output it replaced — the cpal stream on
    /// the chosen device — so the FFI poll's reconcile read "not rerouted" and lifted the mute the
    /// host had just set, while the new unit was already rendering the monitor into whatever
    /// endpoint it landed on. The opening state holds the mute against every reconcile until the
    /// host rebinds; here the open fails and playback is restored to the chosen device.
    #[test]
    fn a_vpio_unit_opening_holds_the_mute_until_the_host_rebinds() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-A".into(),
            cpal_uid: "dev-A".into(),
            cpal_follows_default: false,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        assert!(!reconcile_output_safety(h.transport(), &b, &event));
        assert!(!h.transport().monitor_muted());

        // The host drops the cpal output and publishes the open (its `begin_vpio_open`).
        {
            let mut g = b.lock().unwrap();
            g.cpal_uid.clear();
            g.vpio = VpioOutput::Opening;
        }
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert!(h.transport().monitor_muted());
        // Polls while the unit is still being built must not lift it.
        for _ in 0..3 {
            assert!(reconcile_output_safety(h.transport(), &b, &event));
            assert!(
                h.transport().monitor_muted(),
                "a poll lifted the mute mid-open"
            );
        }
        assert_eq!(
            event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED,
            0,
            "an open in progress is not a reroute to report"
        );

        // The open failed and the chosen output came back: the host's rebind lifts the mute.
        {
            let mut g = b.lock().unwrap();
            g.cpal_uid = "dev-A".into();
            g.vpio = VpioOutput::Off;
        }
        assert!(!reconcile_output_safety(h.transport(), &b, &event));
        assert!(!h.transport().monitor_muted());
    }

    /// A headset VPIO is bound to that disappears takes the output leg with it: playback goes to
    /// the system default, and the poll must see that — reroute, monitor muted — instead of the
    /// headset it was bound to. Bound while it was present (with outputs), it is no device the
    /// machine has now; the real per-poll probe answers.
    #[test]
    fn an_unplugged_bound_headset_sends_playback_to_the_default() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-headset".into(),
            cpal_uid: String::new(),
            cpal_follows_default: false,
            vpio: VpioOutput::Bound("dev-headset".into()),
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        assert_eq!(b.lock().unwrap().actual_uid(), system_default_output_uid());
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert!(h.transport().monitor_muted());
    }

    /// Whether the bound mic serves the output is asked afresh on every poll, never latched: a
    /// headset mid profile switch that shows no outputs as the unit binds reads as a reroute for
    /// that poll only, and its unplugging later is seen on the next one. (A fact cached at bind
    /// held the first answer for the session: a false mute and "playback moved" toast, or an
    /// unmuted monitor on a headset that was gone.)
    #[test]
    fn a_bound_headset_is_judged_afresh_on_every_poll() {
        let h = engine();
        let event = AtomicU32::new(0);
        let serves = Cell::new(false);
        let probe = |mic: &str| {
            assert_eq!(mic, "dev-headset", "only the bound mic is asked");
            serves.get()
        };
        let b = Mutex::new(OutputBinding {
            requested: "dev-headset".into(),
            cpal_uid: String::new(),
            cpal_follows_default: false,
            vpio: VpioOutput::Bound("dev-headset".into()),
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        // Mid profile switch: no outputs yet, so playback is on the default for now.
        assert!(reconcile_output_safety_with(
            h.transport(),
            &b,
            &event,
            &probe,
            &|| None
        ));
        assert!(h.transport().monitor_muted());
        // The switch completed: the headset serves both legs, which is what the user picked.
        serves.set(true);
        assert!(!reconcile_output_safety_with(
            h.transport(),
            &b,
            &event,
            &probe,
            &|| None
        ));
        assert!(!h.transport().monitor_muted());
        assert!(!b.lock().unwrap().rerouted_with(&probe));
        // Unplugged mid-session: the next poll mutes again.
        serves.set(false);
        assert!(reconcile_output_safety_with(
            h.transport(),
            &b,
            &event,
            &probe,
            &|| None
        ));
        assert!(h.transport().monitor_muted());
    }

    /// A stream following the default that was built at a Bluetooth headset's call-profile rate
    /// (the mic just closed, the headset not yet back on its music profile) is reported lost once
    /// the default has run faster for two polls, so the device watch reopens it at the device's
    /// rate: nothing else ever did, and playback stayed band-limited to the call profile's. A
    /// stream at a music rate, or bound to its device (cpal watches those itself), is left alone.
    #[test]
    fn a_default_output_stuck_at_a_call_profile_rate_is_reopened() {
        let h = engine();
        let binding = |rate: u32, follows: bool| {
            Mutex::new(OutputBinding {
                requested: if follows {
                    String::new()
                } else {
                    "dev-headset".into()
                },
                cpal_uid: "dev-headset".into(),
                cpal_follows_default: follows,
                vpio: VpioOutput::Off,
                cpal_rate: rate,
                ..Default::default()
            })
        };
        let lost_after = |b: &Mutex<OutputBinding>, polls: usize, now: u32| {
            let event = AtomicU32::new(0);
            let mut lost = Vec::new();
            for _ in 0..polls {
                reconcile_output_safety_with(h.transport(), b, &event, &|_| false, &|| Some(now));
                lost.push(event.swap(0, Ordering::Relaxed) & EVENT_OUTPUT_LOST != 0);
            }
            lost
        };
        // Stuck at 16 kHz on a default now at 48 kHz: reported on the second poll, and once.
        assert_eq!(
            lost_after(&binding(16_000, true), 4, 48_000),
            [false, true, false, false]
        );
        // Still in its call profile: nothing to rebuild.
        assert_eq!(lost_after(&binding(16_000, true), 3, 16_000), [false; 3]);
        // A music rate, or a stream bound to its device.
        assert_eq!(lost_after(&binding(44_100, true), 3, 48_000), [false; 3]);
        assert_eq!(lost_after(&binding(16_000, false), 3, 48_000), [false; 3]);
    }

    /// An open does not reset the reroute edge: playback that was already off the chosen device
    /// before a mic reopen, and is still off it after, is not announced a second time.
    #[test]
    fn an_open_keeps_the_reroute_edge() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-A".into(),
            cpal_uid: "dev-B".into(),
            cpal_follows_default: false,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert_eq!(
            event.swap(0, Ordering::Relaxed) & EVENT_OUTPUT_REROUTED,
            EVENT_OUTPUT_REROUTED
        );
        {
            let mut g = b.lock().unwrap();
            g.cpal_uid.clear();
            g.vpio = VpioOutput::Opening;
        }
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        {
            let mut g = b.lock().unwrap();
            g.cpal_uid = "dev-B".into();
            g.vpio = VpioOutput::Off;
        }
        assert!(reconcile_output_safety(h.transport(), &b, &event));
        assert!(h.transport().monitor_muted());
        assert_eq!(event.load(Ordering::Relaxed) & EVENT_OUTPUT_REROUTED, 0);
    }

    /// The mute is not a latch: once playback is back on the chosen device it lifts itself, so no
    /// other thread has to remember to clear it.
    #[test]
    fn restoring_the_chosen_output_clears_the_mute() {
        let h = engine();
        let event = AtomicU32::new(0);
        let b = Mutex::new(OutputBinding {
            requested: "dev-A".into(),
            cpal_uid: "dev-B".into(),
            cpal_follows_default: false,
            vpio: VpioOutput::Off,
            rerouted_polls: 0,
            cpal_rate: 0,
            slow_rate_polls: 0,
            echo_free_uid: String::new(),
        });
        reconcile_output_safety(h.transport(), &b, &event);
        assert!(h.transport().monitor_muted());
        b.lock().unwrap().cpal_uid = "dev-A".into();
        assert!(!reconcile_output_safety(h.transport(), &b, &event));
        assert!(!h.transport().monitor_muted());
    }

    /// When BOTH restore attempts fail after VPIO released the output, nothing is open:
    /// `rerouted()` is false by design (nothing open is not a reroute), the playhead's
    /// only clock — the output callback — is gone, and the Dart device watch re-opens
    /// the output ONLY on `EVENT_OUTPUT_LOST`. Staying silent here leaves playback dead
    /// for the rest of the session; the event is the self-heal's sole trigger, exactly
    /// as on SetOutput's double failure.
    #[test]
    fn a_double_failed_output_restore_raises_output_lost() {
        let event = AtomicU32::new(0);
        let (handle, resolved) =
            restore_output_or_flag::<()>(Some("dev-A"), &event, |_| Err("device gone".into()));
        assert!(handle.is_none());
        assert!(resolved.is_empty());
        assert_ne!(
            event.load(Ordering::Acquire) & EVENT_OUTPUT_LOST,
            0,
            "a double-failed restore must raise EVENT_OUTPUT_LOST so the self-heal runs"
        );
    }

    /// A restore that succeeds — on the chosen device or via the default fallback — must
    /// NOT raise the lost event: the fallback case is surfaced through the reroute path
    /// (binding vs. requested), not through the self-heal trigger.
    #[test]
    fn a_successful_output_restore_raises_nothing() {
        let event = AtomicU32::new(0);
        let (handle, resolved) = restore_output_or_flag(Some("dev-A"), &event, |uid| match uid {
            Some(_) => Err("device gone".into()),
            None => Ok(((), "default-dev".to_string())),
        });
        assert!(handle.is_some());
        assert_eq!(resolved, "default-dev");
        assert_eq!(
            event.load(Ordering::Acquire) & EVENT_OUTPUT_LOST,
            0,
            "a restore that opened a device must not trigger the self-heal"
        );
    }
}

#[cfg(test)]
mod cell_exclusion_tests {
    //! The input cell's exclusion, and the output resampler's behaviour once the cell retires it
    //! mid-callback. Both close the same hole: a stream teardown that failed to stop its
    //! callback, which the code has always known can happen.

    use super::{InputCell, OutResampler};
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::live::{spawn, RtCell};
    use crate::ring::spsc;
    use crate::sample::{Smp, CHANNELS, ENGINE_SR};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
    use std::sync::Arc;

    const BLOCK: usize = 128;

    fn input_cell() -> (Arc<InputCell>, crate::ring::Consumer<[Smp; CHANNELS]>) {
        let (mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(1 << 12);
        let (cap_prod, _c) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, _r) = spsc::<[Smp; CHANNELS]>(8);
        let cell = InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(1.0f64.to_bits())),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU32::new(0)),
        );
        cell.reconfigure(ENGINE_SR, 1);
        (cell, mic_cons)
    }

    /// The meter is written only by a callback, so a new input stream used to start on the
    /// previous session's last peak until its first callback arrived (hundreds of ms for a
    /// VPIO open) — and that stale peak led the next take's live waveform.
    #[test]
    fn a_new_input_stream_starts_its_meter_at_zero() {
        let level = Arc::new(AtomicU32::new(0));
        let (mic_prod, _mic) = spsc::<[Smp; CHANNELS]>(1 << 12);
        let (cap_prod, _c) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, _r) = spsc::<[Smp; CHANNELS]>(8);
        let cell = InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&level),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(1.0f64.to_bits())),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU32::new(0)),
        );
        cell.reconfigure(ENGINE_SR, 1);
        cell.feed_test(&[0.5f32; 256]);
        assert!(f32::from_bits(level.load(std::sync::atomic::Ordering::Relaxed)) > 0.4);

        cell.reconfigure(ENGINE_SR, 1); // the next session's stream opens
        assert_eq!(
            f32::from_bits(level.load(std::sync::atomic::Ordering::Relaxed)),
            0.0
        );
    }

    /// Two input callbacks would be two producers on rings that are single-producer by
    /// construction — corruption, not merely UB-by-contract. A retired one drops its samples.
    #[test]
    fn a_retired_input_callback_feeds_nothing() {
        let (cell, mut mic) = input_cell();
        let old = cell.claim();
        let new = cell.claim();

        assert!(!cell.try_feed(old, &vec![0.5f32; 256]));
        assert!(mic.pop().is_none(), "a retired callback must feed nothing");

        assert!(cell.try_feed(new, &vec![0.5f32; 256]));
        assert!(mic.pop().is_some());
    }

    /// `claim` retires older callbacks but cannot evict one already INSIDE `feed`, and a teardown
    /// whose stop failed leaves exactly that zombie running. `reconfigure` therefore has to take
    /// the same single-accessor flag rather than trust a host-thread invariant that a failed stop
    /// has already broken — otherwise it writes `&mut InputState` alongside the feed.
    #[test]
    fn reconfigure_waits_for_a_callback_still_inside_the_cell() {
        use std::sync::atomic::Ordering;
        let (cell, _mic) = input_cell();
        // Stand in for a zombie callback mid-`feed`: it holds the flag, nothing else may enter.
        let held = cell.enter_blocking();
        assert!(
            !cell.try_feed(cell.claim(), &vec![0.5f32; 256]),
            "an occupied cell must deny even a current-epoch feed"
        );

        let done = Arc::new(AtomicBool::new(false));
        let (c2, d2) = (Arc::clone(&cell), Arc::clone(&done));
        let host = std::thread::spawn(move || {
            c2.reconfigure(44_100.0, 2);
            d2.store(true, Ordering::Release);
        });
        // Give the host thread room to run: it must still be spinning, not through.
        for _ in 0..1000 {
            std::hint::spin_loop();
        }
        assert!(
            !done.load(Ordering::Acquire),
            "reconfigure must not write the state under an in-flight feed"
        );

        drop(held);
        host.join().unwrap();
        assert!(done.load(Ordering::Acquire));
        // And it left the cell usable rather than holding the flag.
        assert!(cell.try_feed(cell.claim(), &vec![0.5f32; 256]));
    }

    /// The exclusion under contention, not just in a staged two-thread window: a zombie
    /// callback feeding as fast as it can while the host claims and reconfigures over and
    /// over. Any admitted feed must have exclusive `&mut InputState`, so a `reconfigure`
    /// that wrote alongside one would show up here as a torn `step`/`in_channels` pair (or,
    /// under a sanitizer, as the data race it is).
    #[test]
    fn feeds_and_reconfigures_never_overlap_under_contention() {
        use std::sync::atomic::Ordering;
        let (cell, _mic) = input_cell();
        let stop = Arc::new(AtomicBool::new(false));

        let feeders: Vec<_> = (0..3)
            .map(|_| {
                let c = Arc::clone(&cell);
                let s = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let buf = vec![0.25f32; 128];
                    let mut admitted = 0u64;
                    while !s.load(Ordering::Relaxed) {
                        // Whatever generation is current right now — a real callback holds a
                        // fixed one, but this way some calls are admitted and some retired,
                        // which is the interleaving that matters.
                        let epoch = c.owner.load(Ordering::Acquire);
                        if epoch != 0 && c.try_feed(epoch, &buf) {
                            admitted += 1;
                        }
                    }
                    admitted
                })
            })
            .collect();

        for i in 0..2_000 {
            let epoch = cell.claim();
            assert!(epoch > 0);
            // Alternate the layout so a torn write would leave an impossible pair.
            let (rate, ch) = if i % 2 == 0 {
                (44_100.0, 1)
            } else {
                (96_000.0, 2)
            };
            cell.reconfigure(rate, ch);
            // SAFETY: the host thread is the only reconfigurer, and it is between claims —
            // reading the two fields it just wrote is the assertion, not a second accessor.
            let g = cell.enter_blocking();
            let st = unsafe { &*cell.state.get() };
            let expect_step = rate / crate::sample::ENGINE_SR;
            assert_eq!(st.in_channels, ch, "channels torn at iteration {i}");
            assert!(
                (st.step - expect_step).abs() < 1e-12,
                "step torn at iteration {i}: {} vs {expect_step}",
                st.step
            );
            drop(g);
        }

        stop.store(true, Ordering::Relaxed);
        for f in feeders {
            f.join().unwrap();
        }
        // And the cell is still usable, not left with the flag held.
        assert!(cell.try_feed(cell.claim(), &vec![0.5f32; 128]));
    }

    /// A failed teardown can leave a zombie callback owning the cell while the host
    /// prepares the NEXT stream. `reconfigure` must retire it BEFORE the new format
    /// applies: admitted past that point, its samples are decoded with the new
    /// step/channel count (e.g. a 24 kHz AEC-bypass unit's frames emitted 1:1 as
    /// 48 kHz) — the retiring claim inside the stream build comes milliseconds later.
    #[test]
    fn reconfigure_retires_the_zombie_before_the_new_format_applies() {
        let (cell, mut mic) = input_cell();
        let zombie = cell.claim();
        assert!(cell.try_feed(zombie, &vec![0.5f32; 64]));
        while mic.pop().is_some() {}

        // Host prepares a replacement stream: reconfigure retires the zombie and mints
        // the generation the new stream will feed as, all before the stream build.
        let new = cell.reconfigure(24_000.0, 2);
        assert!(
            !cell.try_feed(zombie, &vec![0.5f32; 64]),
            "a zombie from a failed teardown must not feed audio under the new format"
        );
        assert!(
            mic.pop().is_none(),
            "no zombie sample may reach the mic ring"
        );

        // The replacement stream feeds as the minted generation and is admitted.
        assert!(cell.try_feed(new, &vec![0.5f32; 64]));
        assert!(mic.pop().is_some());
    }

    fn tone_resampler(device_rate: f64) -> (OutResampler, Arc<RtCell>) {
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: Vec::<Edge>::new(),
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        std::mem::forget(h);
        let cell = RtCell::new(rt);
        let epoch = cell.claim();
        (
            OutResampler::new(device_rate, BLOCK, Arc::clone(&cell), epoch),
            cell,
        )
    }

    /// The consumed cursor is advanced from FIFO read progress, so a retired resampler pushing
    /// silence into the FIFO would drive `consumed_total` past `rendered_total` and jump the
    /// reported playhead. It must abandon the callback whole instead.
    #[test]
    fn a_retired_resampler_outputs_silence_without_moving_the_cursor() {
        let (mut res, cell) = tone_resampler(44_100.0);
        let frames = 512;
        let mut out = vec![0.0; frames * CHANNELS];
        res.process(&mut out, frames);
        let consumed = cell.transport().consumed_frames();

        // A replacement stream claims the cell; this resampler is now a zombie.
        let _new = cell.claim();
        let mut out2 = vec![1.0; frames * CHANNELS];
        res.process(&mut out2, frames);

        assert!(
            out2.iter().all(|&s| s == 0.0),
            "a retired callback must output silence, not stale FIFO content"
        );
        assert_eq!(
            cell.transport().consumed_frames(),
            consumed,
            "and must not advance the consumed cursor"
        );
    }

    /// The healthy path is untouched: the owner renders and the cursor tracks delivery.
    #[test]
    fn the_current_resampler_still_renders_and_advances() {
        let (mut res, cell) = tone_resampler(44_100.0);
        let frames = 512;
        let mut out = vec![0.0; frames * CHANNELS];
        res.process(&mut out, frames);
        assert!(cell.transport().consumed_frames() > 0);
        // Delivery can never outrun production; the resampler holds a FIFO between them.
        assert!(
            cell.transport().consumed_frames() <= cell.transport().rendered_frames_for_test(),
            "consumed must never outrun rendered"
        );
    }
}
