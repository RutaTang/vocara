//! The C ABI the Dart/Flutter side binds (via cbindgen → ffigen). POD only across the
//! boundary; every entry point validates inputs and wraps its body in `catch_unwind` so a
//! panic never unwinds across the ABI. Control calls are atomic writes / wait-free pushes;
//! the audio data plane is the cpal callback Dart never sees.
//!
//! The default P0 graph is one vocal track (full EQ → Comp → Gain → Pan chain + a shared
//! reverb send) fed by a ring; Dart decodes/feeds PCM via [`ve_push_pcm`] (or calls
//! [`ve_decode_file`] to get device-rate PCM ready to push).

// FFI entry points deliberately take raw pointers from C and validate them in-body.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::panic::{catch_unwind, AssertUnwindSafe};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::build::{self, build, BusSpec, ProjectSpec, TrackSpec};
use crate::device::{self, AudioHost, RecordWriter};
use crate::graph::NodeId;
use crate::live::{spawn, EngineHandle, RtCell};
use crate::ring::{spsc, Consumer};
use crate::sample::{Smp, CHANNELS, ENGINE_SR};
use std::os::raw::c_float;
use crate::source::{BufferSource, RingSource};

/// A track staged for the next [`ve_tracks_commit`].
struct PendingTrack {
    pcm: Arc<[Smp]>,
    start_frame: i64,
    gain: f32,
    muted: bool,
    /// Vocal tracks get the full EQ -> Comp insert chain + a reverb send; backing is dry.
    vocal: bool,
    /// Output routing: a staged bus index, or -1 for the master.
    bus: i32,
    /// Insert-chain order as FxKind tags (empty → default EQ → Comp).
    chain: Vec<u8>,
    /// Post-fader aux sends: (target bus index, level dB).
    sends: Vec<(i32, f32)>,
}

/// An FX bus staged for the next [`ve_tracks_commit`].
struct PendingBus {
    gain: f32,
    muted: bool,
    /// Output routing: another staged bus index, or -1 for the master.
    output: i32,
    /// Insert-chain order as FxKind tags (empty → default EQ → Comp).
    chain: Vec<u8>,
    /// Post-fader aux sends: (target bus index, level dB).
    sends: Vec<(i32, f32)>,
}

pub struct VeEngine {
    // Declared first so it drops first: dropping `host` stops the stream and joins the audio
    // thread (the RT side is gone) before `handle` reclaims any retired graphs.
    host: AudioHost,
    handle: EngineHandle,
    /// Device-event flags (e.g. output lost), set by the stream error callback; polled by Dart.
    device_event: Arc<AtomicU32>,
    /// UID of the active output device (system default at startup; updated on a successful switch).
    current_output_uid: String,
    /// UID of the selected input device ('' = system default), set on a successful switch.
    current_input_uid: String,
    /// Live-input peak level (f32 bits), written by the input callback; read by `ve_input_level`.
    input_level: Arc<AtomicU32>,
    /// Total input frames received (diagnostic for whether the mic is actually delivering data).
    input_frames_in: Arc<AtomicU64>,
    /// Whether audible monitoring is on (one of the reasons to keep the input stream open).
    monitoring: bool,
    /// Capture gate (analysis) — the input callback fans frames to the capture ring while set.
    cap_active: Arc<AtomicBool>,
    /// Recording gate — the input callback fans frames to the recording ring while set.
    rec_active: Arc<AtomicBool>,
    /// Capture-ring consumer drained by `ve_input_frames` (live analysis, single consumer = Dart).
    cap_cons: Consumer<[Smp; CHANNELS]>,
    /// Background WAV writer (owns the recording-ring consumer; writes only while recording).
    writer: RecordWriter,
    source_id: u32,
    // Decoded PCM cache (path -> device-rate interleaved stereo), so re-committing the graph
    // never re-decodes a file. Tracks being staged for the next commit, and the gain node id
    // per committed track (for live volume/mute without a rebuild).
    cache: std::collections::HashMap<String, Arc<[Smp]>>,
    pending: Vec<PendingTrack>,
    pending_buses: Vec<PendingBus>,
    track_gain_nodes: Vec<NodeId>,
    // Per committed track: the vocal compressor's gain-reduction meter (None for backing).
    gr_meters: Vec<Option<Arc<AtomicU32>>>,
    // Per committed FX bus: the bus compressor's gain-reduction meter.
    bus_gr_meters: Vec<Option<Arc<AtomicU32>>>,
}

const MAX_BLOCK: usize = 1024;
const RING_FRAMES: usize = 1 << 16; // ~1.4 s @ 48k of headroom
const MIC_RING_FRAMES: usize = 1 << 12; // 4096 frames ≈ 85 ms @ 48k (monitor jitter buffer)
const CAP_RING_FRAMES: usize = 1 << 13; // analysis capture (Dart polls ve_input_frames)
const REC_RING_FRAMES: usize = 1 << 14; // recording (drained by the writer thread)

/// Create the engine: build the P0 graph at the fixed `ENGINE_SR`, then spawn the audio-host
/// thread which opens the default output and starts rendering (resampling `ENGINE_SR` → the
/// device's native rate at the boundary). Returns null on any failure.
#[no_mangle]
pub extern "C" fn ve_create() -> *mut VeEngine {
    let built = catch_unwind(|| -> Result<*mut VeEngine, String> {
        let (prod, cons) = spsc::<[Smp; CHANNELS]>(RING_FRAMES);
        let track = TrackSpec::vocal(1, Box::new(RingSource::new(cons)));
        let desc = build(ProjectSpec::new(vec![track]));
        // The graph is device-independent: always compiled at ENGINE_SR.
        let graph = desc
            .compile(ENGINE_SR, MAX_BLOCK)
            .map_err(|e| format!("compile: {e:?}"))?;
        let source_id = build::source_id(1);
        let mut producers = HashMap::new();
        producers.insert(source_id, prod);
        // Input rings: the input callback (host thread) fans resampled mic frames out to the
        // monitor (RtProcessor), the analysis capture (Dart), and the recording writer.
        let (mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(MIC_RING_FRAMES);
        let (cap_prod, cap_cons) = spsc::<[Smp; CHANNELS]>(CAP_RING_FRAMES);
        let (rec_prod, rec_cons) = spsc::<[Smp; CHANNELS]>(REC_RING_FRAMES);
        let (handle, rt) = spawn(graph, producers, mic_cons, 1024);
        // The audio-host thread owns the !Send streams and the shared RtCell for the engine's
        // whole life; FFI threads only message it.
        let rt_cell = RtCell::new(rt);
        let device_event = Arc::new(AtomicU32::new(0));
        let input_level = Arc::new(AtomicU32::new(0));
        let input_frames_in = Arc::new(AtomicU64::new(0));
        let cap_active = Arc::new(AtomicBool::new(false));
        let rec_active = Arc::new(AtomicBool::new(false));
        let input_cell = device::InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::clone(&cap_active),
            Arc::clone(&rec_active),
            Arc::clone(&input_level),
            Arc::clone(&input_frames_in),
        );
        let writer = RecordWriter::spawn(rec_cons);
        let (host, current_output_uid) =
            device::spawn_audio_host(rt_cell, input_cell, Arc::clone(&device_event))?;
        Ok(Box::into_raw(Box::new(VeEngine {
            host,
            handle,
            device_event,
            current_output_uid,
            current_input_uid: String::new(),
            input_level,
            input_frames_in,
            monitoring: false,
            cap_active,
            rec_active,
            cap_cons,
            writer,
            source_id,
            cache: std::collections::HashMap::new(),
            pending: Vec::new(),
            pending_buses: Vec::new(),
            track_gain_nodes: Vec::new(),
            gr_meters: Vec::new(),
            bus_gr_meters: Vec::new(),
        })))
    });
    match built {
        Ok(Ok(p)) => p,
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn ve_destroy(e: *mut VeEngine) {
    if e.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(e));
    }));
}

/// Run `f` with a `&mut VeEngine` under panic protection, returning `default` on null/panic.
fn with_engine<T>(e: *mut VeEngine, default: T, f: impl FnOnce(&mut VeEngine) -> T) -> T {
    if e.is_null() {
        return default;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let eng = unsafe { &mut *e };
        f(eng)
    }))
    .unwrap_or(default)
}

/// Device descriptor across the FFI boundary. Fixed-size char arrays keep it POD (no Rust
/// ownership crosses the ABI); strings are NUL-terminated and truncated to fit.
#[repr(C)]
pub struct VeDeviceInfo {
    pub uid: [c_char; 128],
    pub name: [c_char; 128],
    pub rate: u32,
    pub is_default: u8,
    pub is_input: u8,
    pub is_bluetooth: u8,
}

/// Copy `s` (truncated) into a fixed C char buffer, NUL-terminated and zero-padded.
fn copy_cstr(dst: &mut [c_char], s: &str) {
    let max = dst.len().saturating_sub(1);
    let bytes = s.as_bytes();
    let n = bytes.len().min(max);
    for i in 0..n {
        dst[i] = bytes[i] as c_char;
    }
    for d in &mut dst[n..] {
        *d = 0;
    }
}

/// Fill the caller-allocated `out` (up to `cap`) with `devices`; returns the total count.
fn fill_device_info(
    out: *mut VeDeviceInfo,
    cap: usize,
    devices: &[device::DeviceDesc],
    is_input: u8,
) -> usize {
    if !out.is_null() {
        let n = devices.len().min(cap);
        let slice = unsafe { std::slice::from_raw_parts_mut(out, n) };
        for (dst, d) in slice.iter_mut().zip(devices.iter()) {
            copy_cstr(&mut dst.uid, &d.uid);
            copy_cstr(&mut dst.name, &d.name);
            dst.rate = d.rate;
            dst.is_default = d.is_default as u8;
            dst.is_input = is_input;
            dst.is_bluetooth = d.is_bluetooth as u8;
        }
    }
    devices.len()
}

/// Enumerate output devices into the caller-allocated `out` (up to `cap` entries). Returns the
/// total device count (may exceed `cap`; only `min(total, cap)` entries are written).
#[no_mangle]
pub extern "C" fn ve_enum_output_devices(out: *mut VeDeviceInfo, cap: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        fill_device_info(out, cap, &device::enum_output_devices(), 0)
    }))
    .unwrap_or(0)
}

/// Enumerate input devices into the caller-allocated `out` (up to `cap` entries). Returns the
/// total device count.
#[no_mangle]
pub extern "C" fn ve_enum_input_devices(out: *mut VeDeviceInfo, cap: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        fill_device_info(out, cap, &device::enum_input_devices(), 1)
    }))
    .unwrap_or(0)
}

/// Switch the active output device by UID. 0 = ok, negative = failure (the engine falls back to
/// the default output if the requested device can't be opened).
#[no_mangle]
pub extern "C" fn ve_set_output_device(e: *mut VeEngine, uid: *const c_char) -> c_int {
    with_engine(e, -1, |eng| {
        if uid.is_null() {
            return -1;
        }
        let s = match unsafe { CStr::from_ptr(uid) }.to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        };
        let code = eng.host.set_output_device(s);
        if code == 0 {
            eng.current_output_uid = s.to_string();
        }
        code
    })
}

/// Write the active output device's UID into `buf` (up to `cap`), NUL-terminated. Returns the
/// UID byte length (excluding the NUL).
#[no_mangle]
pub extern "C" fn ve_current_output_device(
    e: *mut VeEngine,
    buf: *mut c_char,
    cap: usize,
) -> usize {
    with_engine(e, 0, |eng| {
        if !buf.is_null() && cap > 0 {
            let dst = unsafe { std::slice::from_raw_parts_mut(buf, cap) };
            copy_cstr(dst, &eng.current_output_uid);
        }
        eng.current_output_uid.len()
    })
}

/// Poll and clear pending device events (bitmask; see `device::EVENT_*`). 0 = none.
#[no_mangle]
pub extern "C" fn ve_poll_device_event(e: *mut VeEngine) -> u32 {
    with_engine(e, 0, |eng| eng.device_event.swap(0, Ordering::AcqRel))
}

/// Select the input device by UID (reopens the input stream live if monitoring is active).
/// 0 = ok, negative = failure.
#[no_mangle]
pub extern "C" fn ve_set_input_device(e: *mut VeEngine, uid: *const c_char) -> c_int {
    with_engine(e, -1, |eng| {
        if uid.is_null() {
            return -1;
        }
        let s = match unsafe { CStr::from_ptr(uid) }.to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        };
        let code = eng.host.set_input_device(s);
        if code == 0 {
            eng.current_input_uid = s.to_string();
        }
        code
    })
}

/// Write the selected input device's UID into `buf` (up to `cap`). Returns its byte length.
#[no_mangle]
pub extern "C" fn ve_current_input_device(e: *mut VeEngine, buf: *mut c_char, cap: usize) -> usize {
    with_engine(e, 0, |eng| {
        if !buf.is_null() && cap > 0 {
            let dst = unsafe { std::slice::from_raw_parts_mut(buf, cap) };
            copy_cstr(dst, &eng.current_input_uid);
        }
        eng.current_input_uid.len()
    })
}

/// Open or close the input stream so it's running iff something needs it (monitoring, analysis
/// capture, or recording). Idempotent — `start_input`/`stop_input` no-op when already in state.
fn sync_input(eng: &VeEngine) {
    let want = eng.monitoring
        || eng.cap_active.load(Ordering::Relaxed)
        || eng.rec_active.load(Ordering::Relaxed);
    if want {
        eng.host.start_input();
    } else {
        eng.host.stop_input();
    }
}

/// Arm the live input (open the mic stream so the meter + recording work). The audible
/// hear-yourself level is NOT set here — it is controlled solely by the monitor-gain fader
/// (`ve_set_monitor_gain`), which defaults to 0 so a built-in speaker can't feed back. 0 = ok.
#[no_mangle]
pub extern "C" fn ve_start_monitor(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.monitoring = true;
        sync_input(eng);
        0
    })
}

/// Disarm the live input: close the mic stream if nothing else needs it. Leaves the monitor-gain
/// fader value untouched (the closed stream is silent regardless). 0 = ok.
#[no_mangle]
pub extern "C" fn ve_stop_monitor(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.monitoring = false;
        sync_input(eng);
        0
    })
}

/// Begin capturing input frames for analysis (`ve_input_frames`); opens the input stream. 0 = ok.
#[no_mangle]
pub extern "C" fn ve_start_capture(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.cap_active.store(true, Ordering::Release);
        sync_input(eng);
        0
    })
}

/// Stop capturing input frames; closes the input stream if nothing else needs it. 0 = ok.
#[no_mangle]
pub extern "C" fn ve_stop_capture(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.cap_active.store(false, Ordering::Release);
        sync_input(eng);
        0
    })
}

/// Pop up to `max_frames` captured interleaved-stereo frames (at 48 kHz) into `out`
/// (`max_frames * 2` floats). Returns frames written. Non-blocking; for live analysis.
#[no_mangle]
pub extern "C" fn ve_input_frames(e: *mut VeEngine, out: *mut c_float, max_frames: usize) -> usize {
    with_engine(e, 0, |eng| {
        if out.is_null() || max_frames == 0 {
            return 0;
        }
        let slice = unsafe { std::slice::from_raw_parts_mut(out, max_frames * CHANNELS) };
        let mut n = 0;
        while n < max_frames {
            match eng.cap_cons.pop() {
                Some(f) => {
                    slice[n * CHANNELS] = f[0];
                    slice[n * CHANNELS + 1] = f[1];
                    n += 1;
                }
                None => break,
            }
        }
        n
    })
}

/// Start recording the live input to a mono 48 kHz WAV at `path` (writer thread). Opens the
/// input stream. 0 = ok.
#[no_mangle]
pub extern "C" fn ve_start_input_recording(e: *mut VeEngine, path: *const c_char) -> c_int {
    with_engine(e, -1, |eng| {
        if path.is_null() {
            return -1;
        }
        let p = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return -1,
        };
        eng.writer.start(p);
        eng.rec_active.store(true, Ordering::Release);
        sync_input(eng);
        0
    })
}

/// Stop the input recording (finalizes the WAV); closes the input stream if nothing else needs
/// it. 0 = ok.
#[no_mangle]
pub extern "C" fn ve_stop_input_recording(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.rec_active.store(false, Ordering::Release);
        eng.writer.stop();
        sync_input(eng);
        0
    })
}

/// Set the live-input monitor gain (linear, 0 = muted). 0 = ok.
#[no_mangle]
pub extern "C" fn ve_set_monitor_gain(e: *mut VeEngine, gain: c_float) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.set_monitor_gain(gain);
        0
    })
}

/// Set the record/input gain (linear) — the mic-input fader's makeup gain applied to the captured
/// voice before recording/analysis. 0 = ok.
#[no_mangle]
pub extern "C" fn ve_set_input_gain(_e: *mut VeEngine, gain: c_float) -> c_int {
    #[cfg(target_vendor = "apple")]
    {
        crate::vpio::set_input_gain(gain);
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = gain;
    }
    0
}

/// Set the monitor reverb (room / damping / wet, each 0..1). 0 = ok.
#[no_mangle]
pub extern "C" fn ve_set_monitor_reverb(
    e: *mut VeEngine,
    room: c_float,
    damping: c_float,
    wet: c_float,
) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.set_monitor_reverb(room, damping, wet);
        0
    })
}

/// The current live-input peak level (0..1, linear) for a meter. 0 when not capturing.
#[no_mangle]
pub extern "C" fn ve_input_level(e: *mut VeEngine) -> c_float {
    with_engine(e, 0.0, |eng| {
        f32::from_bits(eng.input_level.load(Ordering::Relaxed))
    })
}

/// Diagnostic: total input frames the mic callback has received. 0 = the input stream isn't
/// delivering data (not open, or macOS is feeding silence due to a denied permission).
#[no_mangle]
pub extern "C" fn ve_input_frames_received(e: *mut VeEngine) -> u64 {
    with_engine(e, 0, |eng| eng.input_frames_in.load(Ordering::Relaxed))
}

/// Estimated record round-trip latency in frames @ ENGINE_SR (48 kHz): how far behind the engine
/// playhead the captured/recorded vocal lands (output IO + acoustic flight + input IO + AEC block).
/// The UI shifts recorded takes earlier by this so they sit on the beat the singer actually heard.
/// 0 until the mic has opened once, or on a platform without the VoiceProcessingIO path.
#[no_mangle]
pub extern "C" fn ve_record_latency_frames(_e: *mut VeEngine) -> u32 {
    #[cfg(target_vendor = "apple")]
    {
        crate::vpio::RECORD_LATENCY_FRAMES.load(Ordering::Relaxed)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        0
    }
}

#[no_mangle]
pub extern "C" fn ve_play(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.play();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_pause(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.pause();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_seek(e: *mut VeEngine, frame: i64) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.seek(frame);
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_loop(e: *mut VeEngine, enabled: c_int, start: i64, end: i64) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.set_loop(enabled != 0, start, end);
        0
    })
}

/// Queue a parameter change (coalesced; call [`ve_flush`] to publish to the audio thread).
#[no_mangle]
pub extern "C" fn ve_set_param(e: *mut VeEngine, node: u32, param: u16, value: f32) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.set_param(node, param, value);
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_flush(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.flush();
        0
    })
}

/// Feed `frames` interleaved-stereo f32 frames (`pcm` points at `frames * 2` floats) to the
/// source ring. Returns frames accepted (back-pressure: a short return means slow down).
#[no_mangle]
pub extern "C" fn ve_push_pcm(e: *mut VeEngine, pcm: *const f32, frames: usize) -> usize {
    if e.is_null() || pcm.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let eng = unsafe { &mut *e };
        // SAFETY: [f32; 2] is two contiguous f32; caller guarantees `frames * 2` floats.
        let slice = unsafe { std::slice::from_raw_parts(pcm as *const [Smp; CHANNELS], frames) };
        let src = eng.source_id;
        eng.handle.push_pcm(src, slice)
    }))
    .unwrap_or(0)
}

/// The current playhead in samples (atomic read).
#[no_mangle]
pub extern "C" fn ve_frame_pos(e: *mut VeEngine) -> i64 {
    with_engine(e, 0, |eng| eng.handle.frame())
}

/// The engine/device sample rate.
#[no_mangle]
pub extern "C" fn ve_sample_rate(e: *mut VeEngine) -> f64 {
    with_engine(e, 0.0, |eng| eng.handle.sample_rate())
}

/// The node id Dart should feed PCM to (the default track's source).
#[no_mangle]
pub extern "C" fn ve_source_id(e: *mut VeEngine) -> u32 {
    with_engine(e, 0, |eng| eng.source_id)
}

/// Decode `path` natively, build a **timeline-addressable** in-memory source (so seek and
/// loop work), hot-swap it into the graph, seek to 0, and play. Returns 0 on success, a
/// negative code on failure. This is the right path for files (backing tracks / takes);
/// `ve_push_pcm` streaming is for live input.
#[no_mangle]
pub extern "C" fn ve_load_file(e: *mut VeEngine, path: *const c_char) -> c_int {
    if e.is_null() || path.is_null() {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let eng = unsafe { &mut *e };
        let path = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return -1,
        };
        let rate = eng.handle.sample_rate();
        let (frames, src_rate) = match device::decode_file(path) {
            Ok(v) => v,
            Err(_) => return -2,
        };
        let resampled = device::resample_linear(&frames, src_rate, rate as u32);
        let mut flat: Vec<Smp> = Vec::with_capacity(resampled.len() * CHANNELS);
        for f in resampled {
            flat.push(f[0]);
            flat.push(f[1]);
        }
        let src = BufferSource::new(flat, 0);
        let desc = build(ProjectSpec::new(vec![TrackSpec::vocal(1, Box::new(src))]));
        let graph = match desc.compile(rate, MAX_BLOCK) {
            Ok(g) => g,
            Err(_) => return -3,
        };
        // Load only: hot-swap the graph but preserve transport state (frame/playing), so
        // sync() can reload a track without restarting playback. The caller seeks/plays.
        eng.handle.publish_graph(graph);
        0
    }))
    .unwrap_or(-1)
}

// ---- Multi-track playback (one source per track, timeline-aligned) ----------------------

/// Begin staging a new track set; call [`ve_tracks_add`] per track, then [`ve_tracks_commit`].
#[no_mangle]
pub extern "C" fn ve_tracks_begin(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.pending.clear();
        0
    })
}

/// Begin staging the FX buses for the next [`ve_tracks_commit`]. Call this (and
/// [`ve_buses_add`]) BEFORE staging tracks, so a track can route to a bus by index.
#[no_mangle]
pub extern "C" fn ve_buses_begin(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        eng.pending_buses.clear();
        0
    })
}

/// Stage one FX bus: linear `gain`, `muted`, and `output` routing (another staged
/// bus index, or -1 for the master). Returns the bus index (>= 0) or -1.
#[no_mangle]
pub extern "C" fn ve_buses_add(
    e: *mut VeEngine,
    gain: f32,
    muted: c_int,
    output: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        let idx = eng.pending_buses.len() as c_int;
        eng.pending_buses.push(PendingBus {
            gain,
            muted: muted != 0,
            output,
            chain: Vec::new(),
            sends: Vec::new(),
        });
        idx
    })
}

/// Sets a staged track's insert-chain order (FxKind tags, e.g. [0,1] = EQ→Comp).
/// Call after [`ve_tracks_add`] and before [`ve_tracks_commit`].
#[no_mangle]
pub extern "C" fn ve_track_chain(
    e: *mut VeEngine,
    track: c_int,
    tags: *const u8,
    n: usize,
) -> c_int {
    with_engine(e, -1, |eng| {
        let chain = if tags.is_null() || n == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(tags, n) }.to_vec()
        };
        match eng.pending.get_mut(track as usize) {
            Some(pt) => {
                pt.chain = chain;
                0
            }
            None => -1,
        }
    })
}

/// Sets a staged FX bus's insert-chain order (FxKind tags). Call after
/// [`ve_buses_add`] and before [`ve_tracks_commit`].
#[no_mangle]
pub extern "C" fn ve_bus_chain(
    e: *mut VeEngine,
    bus: c_int,
    tags: *const u8,
    n: usize,
) -> c_int {
    with_engine(e, -1, |eng| {
        let chain = if tags.is_null() || n == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(tags, n) }.to_vec()
        };
        match eng.pending_buses.get_mut(bus as usize) {
            Some(pb) => {
                pb.chain = chain;
                0
            }
            None => -1,
        }
    })
}

fn read_sends(targets: *const i32, levels_db: *const f32, n: usize) -> Vec<(i32, f32)> {
    if targets.is_null() || levels_db.is_null() || n == 0 {
        return Vec::new();
    }
    let n = n.min(build::MAX_SENDS);
    let t = unsafe { std::slice::from_raw_parts(targets, n) };
    let l = unsafe { std::slice::from_raw_parts(levels_db, n) };
    (0..n).map(|i| (t[i], l[i])).collect()
}

/// Stage a track's post-fader aux sends: parallel `targets`/`levels_db` of length
/// `n` (capped at MAX_SENDS). Call after [`ve_tracks_add`], before commit.
#[no_mangle]
pub extern "C" fn ve_track_sends(
    e: *mut VeEngine,
    track: c_int,
    targets: *const i32,
    levels_db: *const f32,
    n: usize,
) -> c_int {
    with_engine(e, -1, |eng| {
        let sends = read_sends(targets, levels_db, n);
        match eng.pending.get_mut(track as usize) {
            Some(pt) => {
                pt.sends = sends;
                0
            }
            None => -1,
        }
    })
}

/// Stage a bus's post-fader aux sends. Call after [`ve_buses_add`], before commit.
#[no_mangle]
pub extern "C" fn ve_bus_sends(
    e: *mut VeEngine,
    bus: c_int,
    targets: *const i32,
    levels_db: *const f32,
    n: usize,
) -> c_int {
    with_engine(e, -1, |eng| {
        let sends = read_sends(targets, levels_db, n);
        match eng.pending_buses.get_mut(bus as usize) {
            Some(pb) => {
                pb.sends = sends;
                0
            }
            None => -1,
        }
    })
}

/// Live aux-send level for a committed track's send `slot` (no rebuild).
#[no_mangle]
pub extern "C" fn ve_set_send_level(
    e: *mut VeEngine,
    track: c_int,
    slot: c_int,
    level_db: f32,
) -> c_int {
    with_engine(e, -1, |eng| {
        let node = build::track_send_id((track + 1) as u32, slot as usize);
        let g = crate::sample::db_to_lin(level_db);
        eng.handle
            .set_param(node, crate::dsp::master::param::GAIN, g);
        eng.handle.flush();
        0
    })
}

/// Live aux-send level for a committed bus's send `slot` (no rebuild).
#[no_mangle]
pub extern "C" fn ve_set_bus_send_level(
    e: *mut VeEngine,
    bus: c_int,
    slot: c_int,
    level_db: f32,
) -> c_int {
    with_engine(e, -1, |eng| {
        let node = build::bus_aux_send_id(bus as u32, slot as usize);
        let g = crate::sample::db_to_lin(level_db);
        eng.handle
            .set_param(node, crate::dsp::master::param::GAIN, g);
        eng.handle.flush();
        0
    })
}

/// Stage one track: decode `path` (cached so re-commits don't re-decode), place it at
/// `start_ms` with linear `gain` and `muted`. Returns the track index (>= 0) or -1.
#[no_mangle]
pub extern "C" fn ve_tracks_add(
    e: *mut VeEngine,
    path: *const c_char,
    start_ms: i64,
    gain: f32,
    muted: c_int,
    is_vocal: c_int,
    bus: c_int,
) -> c_int {
    if e.is_null() || path.is_null() {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let eng = unsafe { &mut *e };
        let path = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return -1,
        };
        let rate = eng.handle.sample_rate();
        let pcm = match eng.cache.get(path) {
            Some(c) => c.clone(),
            None => {
                let (frames, src_rate) = match device::decode_file(path) {
                    Ok(v) => v,
                    Err(_) => return -1,
                };
                let resampled = device::resample_linear(&frames, src_rate, rate as u32);
                let mut flat: Vec<Smp> = Vec::with_capacity(resampled.len() * CHANNELS);
                for f in resampled {
                    flat.push(f[0]);
                    flat.push(f[1]);
                }
                let arc: Arc<[Smp]> = flat.into();
                eng.cache.insert(path.to_string(), arc.clone());
                arc
            }
        };
        let start_frame = (start_ms as f64 * rate / 1000.0) as i64;
        let idx = eng.pending.len() as c_int;
        eng.pending.push(PendingTrack {
            pcm,
            start_frame,
            gain,
            muted: muted != 0,
            vocal: is_vocal != 0,
            bus,
            chain: Vec::new(),
            sends: Vec::new(),
        });
        idx
    }))
    .unwrap_or(-1)
}

/// Build the multi-source graph from the staged tracks and hot-swap it in (transport state
/// preserved). Each track: Source(@start) -> Gain -> Pan -> Master. Returns 0 / negative.
#[no_mangle]
pub extern "C" fn ve_tracks_commit(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        let rate = eng.handle.sample_rate();

        // FX buses first, so a track can route into one by index. Each bus gets a
        // gain-reduction meter; effect params are pushed after commit.
        let mut bus_specs: Vec<BusSpec> = Vec::with_capacity(eng.pending_buses.len());
        let mut bus_meters: Vec<Option<Arc<AtomicU32>>> =
            Vec::with_capacity(eng.pending_buses.len());
        let bus_count = eng.pending_buses.len() as i32;
        for (i, pb) in eng.pending_buses.iter().enumerate() {
            let meter = Some(Arc::new(AtomicU32::new(0)));
            bus_meters.push(meter.clone());
            // Only allow routing to a valid OTHER bus; anything else → master.
            let output = if pb.output >= 0 && pb.output < bus_count && pb.output != i as i32 {
                Some(pb.output as u32)
            } else {
                None
            };
            let chain = if pb.chain.is_empty() {
                vec![0, 1]
            } else {
                pb.chain.clone()
            };
            // Valid sends to other buses (never to self), level dB → linear.
            let sends: Vec<(u32, f32)> = pb
                .sends
                .iter()
                .filter(|(t, _)| *t >= 0 && *t < bus_count && *t != i as i32)
                .map(|&(t, db)| (t as u32, crate::sample::db_to_lin(db)))
                .collect();
            bus_specs.push(BusSpec {
                id: i as u32,
                gain: if pb.muted { 0.0 } else { pb.gain },
                send_db: -120.0,
                output,
                chain,
                sends,
                comp_meter: meter,
            });
        }

        let mut specs: Vec<TrackSpec> = Vec::with_capacity(eng.pending.len());
        let mut meters: Vec<Option<Arc<AtomicU32>>> = Vec::with_capacity(eng.pending.len());
        for (i, pt) in eng.pending.iter().enumerate() {
            // Vocal tracks get EQ -> Comp + a reverb send (effect params pushed after commit)
            // and a gain-reduction meter; backing is dry.
            let meter = if pt.vocal {
                Some(Arc::new(AtomicU32::new(0)))
            } else {
                None
            };
            meters.push(meter.clone());
            // Backing never routes to a bus; a vocal routes only to a valid bus.
            let bus = if pt.vocal && pt.bus >= 0 && pt.bus < bus_count {
                Some(pt.bus as u32)
            } else {
                None
            };
            let chain = if !pt.vocal {
                Vec::new()
            } else if pt.chain.is_empty() {
                vec![0, 1]
            } else {
                pt.chain.clone()
            };
            // Vocal sends to valid buses, level dB → linear (backing has none).
            let sends: Vec<(u32, f32)> = if pt.vocal {
                pt.sends
                    .iter()
                    .filter(|(t, _)| *t >= 0 && *t < bus_count)
                    .map(|&(t, db)| (t as u32, crate::sample::db_to_lin(db)))
                    .collect()
            } else {
                Vec::new()
            };
            specs.push(TrackSpec {
                id: (i + 1) as u32,
                source: Box::new(BufferSource::from_arc(pt.pcm.clone(), pt.start_frame)),
                vocal: pt.vocal,
                gain: if pt.muted { 0.0 } else { pt.gain },
                pan: 0.0,
                send_db: -120.0,
                bus,
                chain,
                sends,
                comp_meter: meter,
            });
        }
        let n = specs.len();
        let mut project = ProjectSpec::new(specs);
        project.buses = bus_specs;
        let desc = build(project);
        let graph = match desc.compile(rate, MAX_BLOCK) {
            Ok(g) => g,
            Err(_) => return -1,
        };
        eng.handle.publish_graph(graph);
        eng.track_gain_nodes = (0..n).map(|i| build::gain_id((i + 1) as u32)).collect();
        eng.gr_meters = meters;
        eng.bus_gr_meters = bus_meters;
        0
    })
}

/// Stateless decode (no engine handle) for a background isolate: decode `path` and resample
/// to `rate`, returning malloc'd interleaved-stereo f32. Free with [`ve_free_decoded`]. Safe
/// to call off the main thread (touches no shared engine state).
#[no_mangle]
pub extern "C" fn ve_decode(path: *const c_char, rate: u32) -> VePcm {
    if path.is_null() || rate == 0 {
        return VePcm::empty();
    }
    catch_unwind(AssertUnwindSafe(|| {
        let path = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return VePcm::empty(),
        };
        let (frames, src_rate) = match device::decode_file(path) {
            Ok(v) => v,
            Err(_) => return VePcm::empty(),
        };
        let resampled = device::resample_linear(&frames, src_rate, rate);
        let n = resampled.len();
        let mut flat: Vec<f32> = Vec::with_capacity(n * CHANNELS);
        for f in resampled {
            flat.push(f[0]);
            flat.push(f[1]);
        }
        let mut boxed = flat.into_boxed_slice();
        let ptr = boxed.as_mut_ptr();
        std::mem::forget(boxed);
        VePcm {
            ptr,
            frames: n,
            rate,
        }
    }))
    .unwrap_or_else(|_| VePcm::empty())
}

/// Free a buffer returned by [`ve_decode`].
#[no_mangle]
pub extern "C" fn ve_free_decoded(ptr: *mut f32, frames: usize) {
    if ptr.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let len = frames * CHANNELS;
        drop(Vec::from_raw_parts(ptr, len, len));
    }));
}

/// Punch-in splice: decode `old_path` and `new_path` (resampled to interleaved-stereo at
/// `rate`), build `old[0..split] ++ new`, and write it to `out_path` as a 16-bit PCM WAV.
/// `split_ms` is measured from the START of old's audio; if it lands past old's end, the gap
/// is padded with silence so the new audio keeps its timeline position. Returns the output
/// frame count, or -1 on error. Stateless — safe to call on a background isolate.
#[no_mangle]
pub extern "C" fn ve_punch_wav(
    old_path: *const c_char,
    new_path: *const c_char,
    split_ms: i64,
    rate: u32,
    out_path: *const c_char,
) -> i64 {
    if old_path.is_null() || new_path.is_null() || out_path.is_null() || rate == 0 {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let cstr = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_str().ok();
        let (old_p, new_p, out_p) = match (cstr(old_path), cstr(new_path), cstr(out_path)) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => return -1,
        };
        let decode = |p: &str| -> Option<Vec<[Smp; CHANNELS]>> {
            let (frames, src_rate) = device::decode_file(p).ok()?;
            Some(device::resample_linear(&frames, src_rate, rate))
        };
        let (old_frames, new_frames) = match (decode(old_p), decode(new_p)) {
            (Some(a), Some(b)) => (a, b),
            _ => return -1,
        };

        let split = ((split_ms.max(0) as i128 * rate as i128) / 1000) as usize;
        let keep = split.min(old_frames.len());
        // Silence frames when the cursor sits past old's end (preserves new's timing).
        let pad = split.saturating_sub(old_frames.len());

        let mut flat: Vec<f32> = Vec::with_capacity((keep + pad + new_frames.len()) * CHANNELS);
        for f in &old_frames[..keep] {
            flat.push(f[0]);
            flat.push(f[1]);
        }
        flat.extend(std::iter::repeat_n(0.0, pad * CHANNELS));
        for f in &new_frames {
            flat.push(f[0]);
            flat.push(f[1]);
        }

        let out_frames = (flat.len() / CHANNELS) as i64;
        match crate::wav::write_wav_i16(out_p, &flat, CHANNELS as u16, rate) {
            Ok(_) => out_frames,
            Err(_) => -1,
        }
    }))
    .unwrap_or(-1)
}

/// Stage a track from already-decoded PCM (from [`ve_decode`]), caching it under `path`. The
/// engine copies the samples, so the caller frees its buffer afterward. `pcm` is `frames*2`
/// interleaved f32 at the device rate.
#[no_mangle]
pub extern "C" fn ve_tracks_add_pcm(
    e: *mut VeEngine,
    path: *const c_char,
    pcm: *const f32,
    frames: usize,
    start_ms: i64,
    gain: f32,
    muted: c_int,
    is_vocal: c_int,
    bus: c_int,
) -> c_int {
    if e.is_null() || path.is_null() || pcm.is_null() {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let eng = unsafe { &mut *e };
        let path = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return -1,
        };
        let rate = eng.handle.sample_rate();
        let arc = match eng.cache.get(path) {
            Some(c) => c.clone(),
            None => {
                let slice = unsafe { std::slice::from_raw_parts(pcm, frames * CHANNELS) };
                let arc: Arc<[Smp]> = Arc::from(slice);
                eng.cache.insert(path.to_string(), arc.clone());
                arc
            }
        };
        let start_frame = (start_ms as f64 * rate / 1000.0) as i64;
        let idx = eng.pending.len() as c_int;
        eng.pending.push(PendingTrack {
            pcm: arc,
            start_frame,
            gain,
            muted: muted != 0,
            vocal: is_vocal != 0,
            bus,
            chain: Vec::new(),
            sends: Vec::new(),
        });
        idx
    }))
    .unwrap_or(-1)
}

/// Live volume/mute for a committed track (no rebuild) — A/B toggle, mute/solo, sliders.
#[no_mangle]
pub extern "C" fn ve_set_track_volume(
    e: *mut VeEngine,
    index: c_int,
    gain: f32,
    muted: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        let node = match eng.track_gain_nodes.get(index as usize) {
            Some(&n) => n,
            None => return -1,
        };
        let g = if muted != 0 { 0.0 } else { gain };
        eng.handle.set_param(node, crate::dsp::master::param::GAIN, g);
        eng.handle.flush();
        0
    })
}

/// Live stereo pan for a committed track ([-1, 1]; 0 = center). Equal-power.
#[no_mangle]
pub extern "C" fn ve_set_pan(e: *mut VeEngine, index: c_int, pan: f32) -> c_int {
    with_engine(e, -1, |eng| {
        let node = build::pan_id((index + 1) as u32);
        eng.handle
            .set_param(node, crate::dsp::gain_pan::param::PAN, pan);
        eng.handle.flush();
        0
    })
}

/// Set the master bus output gain (linear, 1.0 = unity), feeding the master
/// soft-clip — the final level the whole mix passes through.
#[no_mangle]
pub extern "C" fn ve_set_master(e: *mut VeEngine, gain: f32) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle
            .set_param(build::MASTER_ID, crate::dsp::master::param::GAIN, gain.max(0.0));
        eng.handle.flush();
        0
    })
}

// ---- File decode (so Dart hands a path and feeds the returned device-rate PCM) ----------

/// Decoded PCM handed back to Dart. `ptr` is `frames * 2` interleaved f32; free with
/// [`ve_free_pcm`].
#[repr(C)]
pub struct VePcm {
    pub ptr: *mut f32,
    pub frames: usize,
    pub rate: u32,
}

impl VePcm {
    fn empty() -> Self {
        VePcm {
            ptr: std::ptr::null_mut(),
            frames: 0,
            rate: 0,
        }
    }
}

/// Decode `path` to interleaved-stereo f32 resampled to the engine's device rate. Dart then
/// feeds it via [`ve_push_pcm`] (e.g. from a feeder isolate) and frees it with [`ve_free_pcm`].
#[no_mangle]
pub extern "C" fn ve_decode_file(e: *mut VeEngine, path: *const c_char) -> VePcm {
    if e.is_null() || path.is_null() {
        return VePcm::empty();
    }
    catch_unwind(AssertUnwindSafe(|| {
        let eng = unsafe { &mut *e };
        let path = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return VePcm::empty(),
        };
        let rate = eng.handle.sample_rate() as u32;
        let (frames, src_rate) = match device::decode_file(path) {
            Ok(v) => v,
            Err(_) => return VePcm::empty(),
        };
        let resampled = device::resample_linear(&frames, src_rate, rate);
        let n = resampled.len();
        // Flatten [f32;2] -> Vec<f32> and hand ownership to the caller.
        let mut flat: Vec<f32> = Vec::with_capacity(n * CHANNELS);
        for f in resampled {
            flat.push(f[0]);
            flat.push(f[1]);
        }
        let mut boxed = flat.into_boxed_slice();
        let ptr = boxed.as_mut_ptr();
        std::mem::forget(boxed);
        VePcm {
            ptr,
            frames: n,
            rate,
        }
    }))
    .unwrap_or_else(|_| VePcm::empty())
}

/// Free PCM returned by [`ve_decode_file`].
#[no_mangle]
pub extern "C" fn ve_free_pcm(pcm: VePcm) {
    if pcm.ptr.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let len = pcm.frames * CHANNELS;
        drop(Vec::from_raw_parts(pcm.ptr, len, len));
    }));
}

// ---- Per-track effect parameters (vocal tracks) + the shared reverb bus -----------------
// `track` is the committed track index. Setting a param on a node a backing track doesn't
// have (EQ/comp/send) is a harmless no-op (the graph ignores unknown node ids).

/// Configure one EQ band: shape is fixed per slot; `freq` Hz, `q`, `gain_db`, `enabled`.
#[no_mangle]
pub extern "C" fn ve_set_eq_band(
    e: *mut VeEngine,
    track: c_int,
    band: c_int,
    freq: f32,
    q: f32,
    gain_db: f32,
    enabled: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::biquad::param as p;
        let node = build::eq_id((track + 1) as u32);
        let b = (band as u16) << p::FIELD_BITS;
        let h = &mut eng.handle;
        h.set_param(node, b | p::FREQ, freq);
        h.set_param(node, b | p::Q, q);
        h.set_param(node, b | p::GAIN_DB, gain_db);
        h.set_param(node, b | p::ENABLED, if enabled != 0 { 1.0 } else { 0.0 });
        h.flush();
        0
    })
}

/// Set the compressor: threshold/makeup dB, ratio (>=1), knee dB, attack/release ms, bypass.
#[no_mangle]
pub extern "C" fn ve_set_comp(
    e: *mut VeEngine,
    track: c_int,
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    attack_ms: f32,
    release_ms: f32,
    makeup_db: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::compressor::param as p;
        let node = build::comp_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::THRESHOLD_DB, threshold_db);
        h.set_param(node, p::RATIO, ratio);
        h.set_param(node, p::KNEE_DB, knee_db);
        h.set_param(node, p::ATTACK_MS, attack_ms);
        h.set_param(node, p::RELEASE_MS, release_ms);
        h.set_param(node, p::MAKEUP_DB, makeup_db);
        h.set_param(node, p::BYPASS, if bypass != 0 { 1.0 } else { 0.0 });
        h.flush();
        0
    })
}

/// Set a vocal track's reverb send level in dB (e.g. -120 = off).
#[no_mangle]
pub extern "C" fn ve_set_send(e: *mut VeEngine, track: c_int, send_db: f32) -> c_int {
    with_engine(e, -1, |eng| {
        let node = build::send_id((track + 1) as u32); // the send is a scalar gain node
        let g = crate::sample::db_to_lin(send_db);
        eng.handle.set_param(node, crate::dsp::master::param::GAIN, g);
        eng.handle.flush();
        0
    })
}

// ---- FX bus effect params (mirror the per-track setters, addressing bus nodes) ----------

/// Set one EQ band on an FX bus (0-based bus index). Same encoding as [`ve_set_eq_band`].
#[no_mangle]
pub extern "C" fn ve_set_bus_eq_band(
    e: *mut VeEngine,
    bus: c_int,
    band: c_int,
    freq: f32,
    q: f32,
    gain_db: f32,
    enabled: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::biquad::param as p;
        let node = build::bus_eq_id(bus as u32);
        let b = (band as u16) << p::FIELD_BITS;
        let h = &mut eng.handle;
        h.set_param(node, b | p::FREQ, freq);
        h.set_param(node, b | p::Q, q);
        h.set_param(node, b | p::GAIN_DB, gain_db);
        h.set_param(node, b | p::ENABLED, if enabled != 0 { 1.0 } else { 0.0 });
        h.flush();
        0
    })
}

/// Set an FX bus's compressor. Same encoding as [`ve_set_comp`].
#[no_mangle]
pub extern "C" fn ve_set_bus_comp(
    e: *mut VeEngine,
    bus: c_int,
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    attack_ms: f32,
    release_ms: f32,
    makeup_db: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::compressor::param as p;
        let node = build::bus_comp_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::THRESHOLD_DB, threshold_db);
        h.set_param(node, p::RATIO, ratio);
        h.set_param(node, p::KNEE_DB, knee_db);
        h.set_param(node, p::ATTACK_MS, attack_ms);
        h.set_param(node, p::RELEASE_MS, release_ms);
        h.set_param(node, p::MAKEUP_DB, makeup_db);
        h.set_param(node, p::BYPASS, if bypass != 0 { 1.0 } else { 0.0 });
        h.flush();
        0
    })
}

/// Set an FX bus's reverb send level in dB (-120 = off).
#[no_mangle]
pub extern "C" fn ve_set_bus_send(e: *mut VeEngine, bus: c_int, send_db: f32) -> c_int {
    with_engine(e, -1, |eng| {
        let node = build::bus_send_id(bus as u32);
        let g = crate::sample::db_to_lin(send_db);
        eng.handle.set_param(node, crate::dsp::master::param::GAIN, g);
        eng.handle.flush();
        0
    })
}

/// Live stereo pan for a committed FX bus ([-1, 1]; 0 = center).
#[no_mangle]
pub extern "C" fn ve_set_bus_pan(e: *mut VeEngine, bus: c_int, pan: f32) -> c_int {
    with_engine(e, -1, |eng| {
        let node = build::bus_pan_id(bus as u32);
        eng.handle
            .set_param(node, crate::dsp::gain_pan::param::PAN, pan);
        eng.handle.flush();
        0
    })
}

/// Live volume/mute for a committed FX bus (no rebuild) — its fader.
#[no_mangle]
pub extern "C" fn ve_set_bus_volume(
    e: *mut VeEngine,
    bus: c_int,
    gain: f32,
    muted: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        let node = build::bus_gain_id(bus as u32);
        let g = if muted != 0 { 0.0 } else { gain };
        eng.handle.set_param(node, crate::dsp::master::param::GAIN, g);
        eng.handle.flush();
        0
    })
}

/// The FX bus compressor's current gain reduction (dB, >= 0) — poll for a meter.
#[no_mangle]
pub extern "C" fn ve_bus_gr(e: *mut VeEngine, bus: c_int) -> f32 {
    with_engine(e, 0.0, |eng| match eng.bus_gr_meters.get(bus as usize) {
        Some(Some(m)) => f32::from_bits(m.load(Ordering::Relaxed)),
        _ => 0.0,
    })
}

// ---- Extra insert plugins (De-esser / Gate / Delay / Saturation / Doubler / Limiter) ----
// Each has a track setter (node id from the 1-based track index) and a bus setter (0-based).

macro_rules! bp {
    ($v:expr) => {
        if $v != 0 {
            1.0
        } else {
            0.0
        }
    };
}

#[no_mangle]
pub extern "C" fn ve_set_deesser(
    e: *mut VeEngine,
    track: c_int,
    freq: f32,
    threshold_db: f32,
    range_db: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::deesser::param as p;
        let node = build::deesser_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::FREQ, freq);
        h.set_param(node, p::THRESHOLD_DB, threshold_db);
        h.set_param(node, p::RANGE_DB, range_db);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_bus_deesser(
    e: *mut VeEngine,
    bus: c_int,
    freq: f32,
    threshold_db: f32,
    range_db: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::deesser::param as p;
        let node = build::bus_deesser_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::FREQ, freq);
        h.set_param(node, p::THRESHOLD_DB, threshold_db);
        h.set_param(node, p::RANGE_DB, range_db);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_gate(
    e: *mut VeEngine,
    track: c_int,
    threshold_db: f32,
    range_db: f32,
    attack_ms: f32,
    release_ms: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::gate::param as p;
        let node = build::gate_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::THRESHOLD_DB, threshold_db);
        h.set_param(node, p::RANGE_DB, range_db);
        h.set_param(node, p::ATTACK_MS, attack_ms);
        h.set_param(node, p::RELEASE_MS, release_ms);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_bus_gate(
    e: *mut VeEngine,
    bus: c_int,
    threshold_db: f32,
    range_db: f32,
    attack_ms: f32,
    release_ms: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::gate::param as p;
        let node = build::bus_gate_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::THRESHOLD_DB, threshold_db);
        h.set_param(node, p::RANGE_DB, range_db);
        h.set_param(node, p::ATTACK_MS, attack_ms);
        h.set_param(node, p::RELEASE_MS, release_ms);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_delay(
    e: *mut VeEngine,
    track: c_int,
    time_ms: f32,
    feedback: f32,
    mix: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::delay::param as p;
        let node = build::delay_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::TIME_MS, time_ms);
        h.set_param(node, p::FEEDBACK, feedback);
        h.set_param(node, p::MIX, mix);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_bus_delay(
    e: *mut VeEngine,
    bus: c_int,
    time_ms: f32,
    feedback: f32,
    mix: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::delay::param as p;
        let node = build::bus_delay_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::TIME_MS, time_ms);
        h.set_param(node, p::FEEDBACK, feedback);
        h.set_param(node, p::MIX, mix);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_saturation(
    e: *mut VeEngine,
    track: c_int,
    drive_db: f32,
    mix: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::saturation::param as p;
        let node = build::saturation_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::DRIVE_DB, drive_db);
        h.set_param(node, p::MIX, mix);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_bus_saturation(
    e: *mut VeEngine,
    bus: c_int,
    drive_db: f32,
    mix: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::saturation::param as p;
        let node = build::bus_saturation_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::DRIVE_DB, drive_db);
        h.set_param(node, p::MIX, mix);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_doubler(
    e: *mut VeEngine,
    track: c_int,
    delay_ms: f32,
    depth_ms: f32,
    rate_hz: f32,
    mix: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::doubler::param as p;
        let node = build::doubler_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::DELAY_MS, delay_ms);
        h.set_param(node, p::DEPTH_MS, depth_ms);
        h.set_param(node, p::RATE_HZ, rate_hz);
        h.set_param(node, p::MIX, mix);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_bus_doubler(
    e: *mut VeEngine,
    bus: c_int,
    delay_ms: f32,
    depth_ms: f32,
    rate_hz: f32,
    mix: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::doubler::param as p;
        let node = build::bus_doubler_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::DELAY_MS, delay_ms);
        h.set_param(node, p::DEPTH_MS, depth_ms);
        h.set_param(node, p::RATE_HZ, rate_hz);
        h.set_param(node, p::MIX, mix);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_limiter(
    e: *mut VeEngine,
    track: c_int,
    ceiling_db: f32,
    release_ms: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::limiter::param as p;
        let node = build::limiter_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::CEILING_DB, ceiling_db);
        h.set_param(node, p::RELEASE_MS, release_ms);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_bus_limiter(
    e: *mut VeEngine,
    bus: c_int,
    ceiling_db: f32,
    release_ms: f32,
    bypass: c_int,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::limiter::param as p;
        let node = build::bus_limiter_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::CEILING_DB, ceiling_db);
        h.set_param(node, p::RELEASE_MS, release_ms);
        h.set_param(node, p::BYPASS, bp!(bypass));
        h.flush();
        0
    })
}

/// Set a track's reverb INSERT (series): room/damping/wet/width, each 0..1.
/// `wet` 0 = off (full dry passes through). Dry stays at unity.
#[no_mangle]
pub extern "C" fn ve_set_reverb_insert(
    e: *mut VeEngine,
    track: c_int,
    room: f32,
    damping: f32,
    wet: f32,
    width: f32,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::reverb::param as p;
        let node = build::reverb_id((track + 1) as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::ROOM_SIZE, room);
        h.set_param(node, p::DAMPING, damping);
        h.set_param(node, p::WET, wet);
        h.set_param(node, p::WIDTH, width);
        h.flush();
        0
    })
}

#[no_mangle]
pub extern "C" fn ve_set_bus_reverb_insert(
    e: *mut VeEngine,
    bus: c_int,
    room: f32,
    damping: f32,
    wet: f32,
    width: f32,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::reverb::param as p;
        let node = build::bus_reverb_id(bus as u32);
        let h = &mut eng.handle;
        h.set_param(node, p::ROOM_SIZE, room);
        h.set_param(node, p::DAMPING, damping);
        h.set_param(node, p::WET, wet);
        h.set_param(node, p::WIDTH, width);
        h.flush();
        0
    })
}

/// Set the shared reverb bus: room/damping/wet/dry/width, each 0..1.
#[no_mangle]
pub extern "C" fn ve_set_reverb(
    e: *mut VeEngine,
    room: f32,
    damping: f32,
    wet: f32,
    dry: f32,
    width: f32,
) -> c_int {
    with_engine(e, -1, |eng| {
        use crate::dsp::reverb::param as p;
        let h = &mut eng.handle;
        h.set_param(build::REVERB_ID, p::ROOM_SIZE, room);
        h.set_param(build::REVERB_ID, p::DAMPING, damping);
        h.set_param(build::REVERB_ID, p::WET, wet);
        h.set_param(build::REVERB_ID, p::DRY, dry);
        h.set_param(build::REVERB_ID, p::WIDTH, width);
        h.flush();
        0
    })
}

/// The vocal compressor's current gain reduction (dB, >= 0) for a committed track — poll for
/// a meter. Returns 0 for backing tracks or when not compressing.
#[no_mangle]
pub extern "C" fn ve_track_gr(e: *mut VeEngine, track: c_int) -> f32 {
    with_engine(e, 0.0, |eng| match eng.gr_meters.get(track as usize) {
        Some(Some(m)) => f32::from_bits(m.load(Ordering::Relaxed)),
        _ => 0.0,
    })
}
