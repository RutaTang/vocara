//! The C ABI the Dart/Flutter side binds (via cbindgen → ffigen). POD only across the
//! boundary; every entry point validates inputs and wraps its body in `catch_unwind` so a
//! panic never unwinds across the ABI. Control calls are atomic writes / wait-free pushes;
//! the audio data plane is the cpal callback Dart never sees.
//!
//! The default P0 graph is one vocal track (full EQ → Comp → Gain → Pan chain + a shared
//! reverb send) fed by a silent ring; the first [`ve_tracks_commit`] replaces it with the
//! real project graph.

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

/// (size, mtime) of a cache entry's source file, captured when the PCM was stored. Size+mtime
/// is the standard freshness heuristic: a same-size rewrite inside one mtime tick can evade it,
/// but every real re-render/re-record changes at least one of the two.
type FileStamp = (u64, std::time::SystemTime);

/// Stat `path` for freshness comparison. `None` when the file is unreadable (missing, perms).
fn file_stamp(path: &str) -> Option<FileStamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// A decoded-PCM cache entry: the samples plus the stage-recency stamp used for the
/// bounded LRU eviction at commit (see [`cache_retain`]).
struct CachedPcm {
    pcm: Arc<[Smp]>,
    /// `VeEngine::cache_seq` at the last time this path was staged (higher = more recent).
    staged_seq: u64,
    /// Source-file stamp at store time, so a path hit can detect the file changing on disk
    /// underneath the cache (see [`cache_lookup_fresh`]). `None` when the file could not be
    /// statted at store time.
    stamp: Option<FileStamp>,
}

/// A track staged for the next [`ve_tracks_commit`].
struct PendingTrack {
    /// Cache key (the source file path), so the commit can evict cache entries whose
    /// paths are not in the newly-committed graph.
    path: String,
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
    /// Frames the recording ring dropped during the CURRENT take (reset on each start).
    /// Nonzero means the take on disk has silent holes — surfaced via
    /// `ve_record_dropped_frames` so the UI never reports such a take as clean.
    rec_dropped: Arc<AtomicU64>,
    /// Frames written into the recording ring for the CURRENT take (reset on each start):
    /// the per-take recorded-progress ack behind `ve_record_written_frames`.
    rec_written: Arc<AtomicU64>,
    /// Desired input state cell: (intent_seq << 1) | want_input, written by the control
    /// thread (sync_input) and read by the audio host at CONVERGENCE time — the host acts on
    /// the cell's current value, never on a queued command's historical payload, which is
    /// what linearizes "monitoring off, then REC" into a single kept-open stream.
    input_intent: Arc<AtomicU64>,
    /// The intent sequence the host last CONVERGED to (published after acting). The ready
    /// handshake requires applied >= the sequence of this take's arm, proving the host has
    /// acted on an intent at least as new as the arm.
    input_applied: Arc<AtomicU64>,
    /// Control-thread-only monotonic counter feeding `input_intent`'s sequence half.
    intent_seq: u64,
    /// Host-thread-authoritative input-stream state word: (generation << 1) | running.
    /// Set/bumped only by the audio-host thread — running set only after a successful
    /// stream start, cleared BEFORE any teardown joins the old callback, generation bumped
    /// on every start. One atomic so the ready handshake can snapshot it, check the frame
    /// counter, and snapshot again: an unchanged word with the running bit proves the same
    /// stream delivered those frames (see `ve_input_stream_state`).
    input_state: Arc<AtomicU64>,
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
    // Decoded PCM cache (path -> device-rate interleaved stereo), so re-committing the graph
    // never re-decodes a file. The committed graph pins every live track's samples via its
    // own Arcs, so the cache's true memory overhead is exactly the non-live entries; each
    // successful commit evicts those beyond CACHE_KEEP_BYTES (LRU by stage recency), so the
    // cache cannot grow unbounded across a session (deleted takes used to be retained until
    // the song closed). Tracks being staged for the next commit, and the gain node id per
    // committed track (for live volume/mute without a rebuild).
    cache: std::collections::HashMap<String, CachedPcm>,
    /// Monotonic stage counter stamping cache entries for the LRU eviction at commit.
    cache_seq: u64,
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
                                        // Recording ring (drained by the writer thread). 2^17 frames ≈ 2.7 s @ 48k (1 MiB): the
                                        // writer's ~1 s header-patch flush can stall multi-hundred ms on a slow or full disk, and
                                        // the old 2^14 (≈ 341 ms) headroom made such a stall silently drop take audio. Overflow is
                                        // still counted (`rec_dropped`) and surfaced — headroom reduces the odds, never the honesty.
const REC_RING_FRAMES: usize = 1 << 17;

/// Keep-alive budget for decoded PCM whose path is NOT in the committed graph. Live paths
/// are pinned by the graph's own Arcs regardless, so this bounds the cache's real overhead.
/// 256 MiB ≈ 11 minutes of 48 kHz stereo f32 (~2-3 typical songs): enough that the
/// reference ↔ accompaniment backing toggle and a just-separated song's original re-stage
/// instantly, while a long session (dozens of takes, some deleted) can no longer accumulate
/// gigabytes of dead PCM.
const CACHE_KEEP_BYTES: usize = 256 * 1024 * 1024;

/// Evict cached PCM for paths outside `live`, keeping the most recently staged non-live
/// entries within `keep_bytes` (LRU by stage recency). Live paths are always kept. Dropping
/// an entry only releases the map's Arc; samples still referenced by a retiring graph free
/// once the reaper drops that graph.
fn cache_retain(
    cache: &mut std::collections::HashMap<String, CachedPcm>,
    live: &std::collections::HashSet<&str>,
    keep_bytes: usize,
) {
    let mut idle: Vec<(String, u64, usize)> = cache
        .iter()
        .filter(|(path, _)| !live.contains(path.as_str()))
        .map(|(path, c)| {
            (
                path.clone(),
                c.staged_seq,
                c.pcm.len() * std::mem::size_of::<Smp>(),
            )
        })
        .collect();
    // Most recently staged first; evict every entry past the point the budget overflows.
    idle.sort_by_key(|e| std::cmp::Reverse(e.1));
    let mut used = 0usize;
    for (path, _, bytes) in idle {
        used = used.saturating_add(bytes);
        if used > keep_bytes {
            cache.remove(&path);
        }
    }
}

/// Store freshly decoded PCM in the cache under `path`, REPLACING any existing entry (the
/// file's content may have changed under a reused path — a cached Arc would serve the stale
/// audio), and return the shared samples for staging.
fn cache_store_pcm(
    cache: &mut std::collections::HashMap<String, CachedPcm>,
    path: &str,
    pcm: &[Smp],
    seq: u64,
) -> Arc<[Smp]> {
    let arc: Arc<[Smp]> = Arc::from(pcm);
    cache.insert(
        path.to_string(),
        CachedPcm {
            pcm: arc.clone(),
            staged_seq: seq,
            // Stamp the source file NOW (moments after it was decoded). A rewrite landing in
            // the decode→stat window would stamp newer metadata onto older samples; that
            // ms-scale race is accepted — the next rewrite is caught normally.
            stamp: file_stamp(path),
        },
    );
    arc
}

/// Look up `path` for staging and bump its LRU stamp — but only when the file on disk still
/// matches the [`FileStamp`] captured at store time. A changed stamp (or an entry stored
/// without one) returns `None`, turning the stale hit into a MISS: `ve_tracks_add` then
/// reports -1 and Dart decodes fresh, which re-stores the entry via [`cache_store_pcm`].
/// An UNREADABLE file (deleted/perms) still serves the cache: the staged audio may outlive
/// its source file, and a forced re-decode would only fail (pre-existing behavior).
fn cache_lookup_fresh(
    cache: &mut std::collections::HashMap<String, CachedPcm>,
    path: &str,
    seq: u64,
) -> Option<Arc<[Smp]>> {
    let c = cache.get_mut(path)?;
    match (c.stamp, file_stamp(path)) {
        (_, None) => {}                              // source gone: keep serving the cache
        (Some(stored), Some(now)) if stored == now => {} // verified fresh
        _ => return None,                            // changed on disk (or unverifiable): miss
    }
    c.staged_seq = seq;
    Some(c.pcm.clone())
}

/// Create the engine: build the P0 graph at the fixed `ENGINE_SR`, then spawn the audio-host
/// thread which opens the default output and starts rendering (resampling `ENGINE_SR` → the
/// device's native rate at the boundary). Returns null on any failure.
#[no_mangle]
pub extern "C" fn ve_create() -> *mut VeEngine {
    let built = catch_unwind(|| -> Result<*mut VeEngine, String> {
        // Baseline placeholder track: a silent ring (nothing feeds it), immediately replaced
        // by the first ve_tracks_commit.
        let (_prod, cons) = spsc::<[Smp; CHANNELS]>(RING_FRAMES);
        let track = TrackSpec::vocal(1, Box::new(RingSource::new(cons)));
        let desc = build(ProjectSpec::new(vec![track]));
        // The graph is device-independent: always compiled at ENGINE_SR.
        let graph = desc
            .compile(ENGINE_SR, MAX_BLOCK)
            .map_err(|e| format!("compile: {e:?}"))?;
        // Input rings: the input callback (host thread) fans resampled mic frames out to the
        // monitor (RtProcessor), the analysis capture (Dart), and the recording writer.
        let (mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(MIC_RING_FRAMES);
        let (cap_prod, cap_cons) = spsc::<[Smp; CHANNELS]>(CAP_RING_FRAMES);
        let (rec_prod, rec_cons) = spsc::<[Smp; CHANNELS]>(REC_RING_FRAMES);
        let (handle, rt) = spawn(graph, HashMap::new(), mic_cons, 1024);
        // The audio-host thread owns the !Send streams and the shared RtCell for the engine's
        // whole life; FFI threads only message it.
        let rt_cell = RtCell::new(rt);
        let device_event = Arc::new(AtomicU32::new(0));
        let input_level = Arc::new(AtomicU32::new(0));
        let input_frames_in = Arc::new(AtomicU64::new(0));
        let rec_dropped = Arc::new(AtomicU64::new(0));
        let rec_written = Arc::new(AtomicU64::new(0));
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
            Arc::clone(&rec_dropped),
            Arc::clone(&rec_written),
        );
        let writer = RecordWriter::spawn(rec_cons);
        // The host maintains the handle's renderer gate (stream up/down), so the control
        // thread can free retired graphs immediately while no stream callback runs (the
        // grace epoch never elapses without a callback advancing the block counter).
        let input_state = Arc::new(AtomicU64::new(0));
        let input_intent = Arc::new(AtomicU64::new(0));
        let input_applied = Arc::new(AtomicU64::new(0));
        let (host, current_output_uid) = device::spawn_audio_host(
            rt_cell,
            input_cell,
            Arc::clone(&device_event),
            handle.renderer_gate(),
            Arc::clone(&input_state),
            Arc::clone(&input_intent),
            Arc::clone(&input_applied),
        )?;
        Ok(Box::into_raw(Box::new(VeEngine {
            host,
            handle,
            device_event,
            current_output_uid,
            current_input_uid: String::new(),
            input_level,
            input_frames_in,
            rec_dropped,
            rec_written,
            input_intent,
            input_applied,
            intent_seq: 0,
            input_state,
            monitoring: false,
            cap_active,
            rec_active,
            cap_cons,
            writer,
            cache: std::collections::HashMap::new(),
            cache_seq: 0,
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

/// Publish the CURRENT desired input state (running iff something needs it: monitoring,
/// analysis capture, or recording) into the intent cell and nudge the host to converge.
/// The host reads the cell at convergence time, so a stale nudge can never execute a stale
/// teardown; `ve_input_applied_seq` reports the sequence the host last converged to.
fn sync_input(eng: &mut VeEngine) {
    let want = eng.monitoring
        || eng.cap_active.load(Ordering::Relaxed)
        || eng.rec_active.load(Ordering::Relaxed);
    eng.intent_seq += 1;
    eng.input_intent
        .store((eng.intent_seq << 1) | want as u64, Ordering::Release);
    eng.host.sync_input_nudge();
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

/// Start recording the live input to a mono 48 kHz WAV at `path` (writer thread). Blocks
/// briefly on the writer creating the file (file creation is not on the RT path). Opens the
/// input stream. 0 = ok; -1 = the file could not be created (disk full / permissions) —
/// nothing will be recorded.
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
        if !eng.writer.start(p) {
            return -1;
        }
        // A fresh take starts with clean counters: both belong to exactly one recording
        // (`ve_record_dropped_frames` after stop; `ve_record_written_frames` during it).
        eng.rec_dropped.store(0, Ordering::Release);
        eng.rec_written.store(0, Ordering::Release);
        eng.rec_active.store(true, Ordering::Release);
        sync_input(eng);
        0
    })
}

/// Stop the input recording (finalizes the WAV); closes the input stream if nothing else needs
/// it. 0 = ok; -2 = an IO error occurred during the recording (the WAV at the recorded path
/// is missing, damaged, or incomplete).
#[no_mangle]
pub extern "C" fn ve_stop_input_recording(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        // The input callback samples `rec_active` ONCE per buffer, so a callback already in
        // flight when the flag drops still pushes its whole slice. Stopping the writer
        // immediately would drain + finalize BEFORE that tail lands — the take would lose up
        // to one callback slice, and the frames would be discarded by the next take's
        // stale-ring sweep. Wait for two frame-counter advances (the in-flight callback plus
        // one boundary that provably observed the cleared flag) before finalizing; bail out
        // on a deadline so a dead input stream (device loss mid-take) can't hang the stop.
        let mut last = eng.input_frames_in.load(Ordering::Acquire);
        eng.rec_active.store(false, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        let mut advances = 0u32;
        while advances < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
            let now = eng.input_frames_in.load(Ordering::Acquire);
            if now != last {
                advances += 1;
                last = now;
            }
        }
        let clean = eng.writer.stop();
        sync_input(eng);
        if clean {
            0
        } else {
            -2
        }
    })
}

/// Frames the recording ring dropped during the current/latest take (reset at each recording
/// start). Nonzero means the take on disk is missing that many frames — the writer thread
/// stalled past the ring's headroom — so the UI must not present the take as gapless.
#[no_mangle]
pub extern "C" fn ve_record_dropped_frames(e: *mut VeEngine) -> u64 {
    with_engine(e, 0, |eng| eng.rec_dropped.load(Ordering::Acquire))
}

/// Frames written into the recording ring for the CURRENT take (engine-rate; reset at each
/// recording start) — the per-take recorded-progress ack. Nonzero PROVES audio is in this
/// take's ring: a callback that entered before the arm read the disarmed flag and never
/// pushed, so this can never count frames that are not in the WAV (unlike the global frame
/// counter, which a pre-arm or dying-stream callback also advances). The ready handshake
/// waits for it to leave zero; the pre-roll measurement reads it at play time — it IS the
/// take's file offset at that moment.
#[no_mangle]
pub extern "C" fn ve_record_written_frames(e: *mut VeEngine) -> u64 {
    with_engine(e, 0, |eng| eng.rec_written.load(Ordering::Acquire))
}

/// Set the live-input monitor gain (linear, 0 = muted). 0 = ok.
#[no_mangle]
pub extern "C" fn ve_set_monitor_gain(e: *mut VeEngine, gain: c_float) -> c_int {
    with_engine(e, -1, |eng| {
        eng.handle.set_monitor_gain(gain);
        0
    })
}

/// Set the record/input gain (linear) — the mic-input fader's gain applied to the captured voice
/// before recording/analysis. Applied as a pure linear scale (recordings are 32-bit float, so
/// levels past 0 dBFS are stored losslessly, never clipped or soft-limited). 0 = ok.
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

/// Diagnostic + pre-roll clock: total ENGINE-RATE (48 kHz, post-resample) frames the mic
/// callback has emitted. 0 = the input stream isn't delivering data (not open, or macOS is
/// feeding silence due to a denied permission). Engine-rate on purpose: callers convert this
/// to time by dividing by 48 000, which a raw device-rate count would skew.
#[no_mangle]
pub extern "C" fn ve_input_frames_received(e: *mut VeEngine) -> u64 {
    with_engine(e, 0, |eng| eng.input_frames_in.load(Ordering::Relaxed))
}

/// The sequence of the newest published input intent (the cell's high bits). Read right
/// after arming a recording, it is the arm's watermark for `ve_input_applied_seq`.
#[no_mangle]
pub extern "C" fn ve_input_intent_seq(e: *mut VeEngine) -> u64 {
    with_engine(e, 0, |eng| eng.input_intent.load(Ordering::Acquire) >> 1)
}

/// The intent sequence the audio host last CONVERGED to (published after acting on the
/// cell). `applied >= S` proves the host has acted on an intent at least as new as S — the
/// ordering half of the mic ready handshake; the recorded-progress half is
/// `ve_record_written_frames`.
#[no_mangle]
pub extern "C" fn ve_input_applied_seq(e: *mut VeEngine) -> u64 {
    with_engine(e, 0, |eng| eng.input_applied.load(Ordering::Acquire))
}

/// The host-thread-authoritative input-stream state word: `(generation << 1) | running`.
/// Running is set only after a successful stream start and cleared BEFORE any teardown
/// joins the old callback; the generation bumps on every start. Ready-handshake contract:
/// read this word, verify `ve_input_frames_received` advanced, read the word AGAIN — an
/// unchanged value with bit 0 set proves the SAME stream was up across the window, so the
/// frames cannot have come from a dying predecessor (a teardown clears the bit, a restart
/// changes the generation). 0 = no stream has ever run.
#[no_mangle]
pub extern "C" fn ve_input_stream_state(e: *mut VeEngine) -> u64 {
    with_engine(e, 0, |eng| eng.input_state.load(Ordering::Acquire))
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

/// The current playhead in samples (atomic read).
#[no_mangle]
pub extern "C" fn ve_frame_pos(e: *mut VeEngine) -> i64 {
    with_engine(e, 0, |eng| {
        // Reap retired graphs on Dart's hot playback poll: `publish_graph` can only free the
        // PREVIOUS generation (its own retiree is still inside the grace epoch), so without
        // this the last generation's decoded PCM stays resident until the next commit —
        // defeating the cache byte bound. Cheap (an atomic load; the list is usually empty)
        // and control-thread-safe (see `EngineHandle::reap`).
        eng.handle.reap();
        eng.handle.frame()
    })
}

/// The engine/device sample rate.
#[no_mangle]
pub extern "C" fn ve_sample_rate(e: *mut VeEngine) -> f64 {
    with_engine(e, 0.0, |eng| eng.handle.sample_rate())
}

// ---- Multi-track playback (one source per track, timeline-aligned) ----------------------

/// Begin staging a new track set; call [`ve_tracks_add`] per track, then [`ve_tracks_commit`].
#[no_mangle]
pub extern "C" fn ve_tracks_begin(e: *mut VeEngine) -> c_int {
    with_engine(e, -1, |eng| {
        // Collect graphs retired by the previous commit before staging the next set (see
        // the matching reap in `ve_frame_pos` for why prompt reaping matters).
        eng.handle.reap();
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
pub extern "C" fn ve_buses_add(e: *mut VeEngine, gain: f32, muted: c_int, output: c_int) -> c_int {
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
pub extern "C" fn ve_bus_chain(e: *mut VeEngine, bus: c_int, tags: *const u8, n: usize) -> c_int {
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

/// Stage one track from the decoded-PCM cache: place it at `start_ms` with linear `gain`
/// and `muted`. Returns the track index (>= 0), or -1 when `path` is not cached OR its file
/// changed on disk since it was cached (freshness is verified by size+mtime, so a take
/// re-rendered under a reused path can never re-stage stale audio) — the caller then decodes
/// OFF this thread ([`ve_decode`] on an isolate) and stages the samples via
/// [`ve_tracks_add_pcm`]. This function never decodes: it runs on the UI thread, and a cache
/// miss (first stage, or evicted at an earlier commit) must not block it on file I/O (the
/// freshness stat is one metadata syscall, not a decode).
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
        eng.cache_seq += 1;
        let seq = eng.cache_seq;
        // Freshness-checked lookup: a path whose file changed on disk since it was cached
        // (re-rendered/re-recorded under a reused name) counts as a MISS, so the caller
        // decodes the NEW content instead of silently re-staging the stale samples. The
        // stat runs on this (UI) thread, which is the same thread that would otherwise
        // decode — one metadata syscall is negligible next to that.
        let pcm = match cache_lookup_fresh(&mut eng.cache, path, seq) {
            Some(pcm) => pcm,
            None => return -1,
        };
        let start_frame = (start_ms as f64 * rate / 1000.0) as i64;
        let idx = eng.pending.len() as c_int;
        eng.pending.push(PendingTrack {
            path: path.to_string(),
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
        // The new graph is live: evict cached PCM for paths no longer in it (with a bounded
        // keep-alive for quick re-staging), so the cache can't grow unbounded across a
        // session. Only on success — a rejected topology keeps the OLD graph, whose paths
        // must stay cached.
        let live: std::collections::HashSet<&str> =
            eng.pending.iter().map(|p| p.path.as_str()).collect();
        cache_retain(&mut eng.cache, &live, CACHE_KEEP_BYTES);
        0
    })
}

/// Reinterpret a decoded stereo-frame Vec as its flat interleaved layout WITHOUT copying:
/// `[f32; CHANNELS]` and `CHANNELS` consecutive `f32`s share size and alignment, so only the
/// len/capacity bookkeeping changes. The decode paths' peak memory is measured in full-track
/// copies — this removes one.
fn flatten_stereo(mut v: Vec<[Smp; CHANNELS]>) -> Vec<Smp> {
    let ptr = v.as_mut_ptr() as *mut Smp;
    let len = v.len() * CHANNELS;
    let cap = v.capacity() * CHANNELS;
    std::mem::forget(v);
    // SAFETY: same allocation, identical layout (size cap*8, align 4) under both types;
    // the original Vec is forgotten so ownership transfers exactly once.
    unsafe { Vec::from_raw_parts(ptr, len, cap) }
}

/// Hard ceiling (seconds) for the decode paths that don't take a caller budget
/// (`ve_punch_wav`, `ve_decode_file`). Mirrors the Dart side's `maxPlaybackDuration`
/// (60 min) — the two must move together.
const MAX_PLAYBACK_SECS: f64 = 3600.0;

/// Highest output rate any decode/splice entry point accepts. Every real caller passes the
/// engine rate (48 kHz) or a common analysis rate; an absurd rate would multiply into every
/// frame-count ceiling above, so it is bounded before any arithmetic uses it.
const MAX_DECODE_RATE: u32 = 384_000;

/// `ve_decode` status: decoded successfully.
pub const VE_DECODE_OK: c_int = 0;
/// `ve_decode` status: open/probe/decode failure (unreadable or unsupported file).
pub const VE_DECODE_ERR: c_int = -1;
/// `ve_decode` status: the audio exceeds `max_frames` — refused before/while decoding so the
/// peak allocation stays bounded by the caller's budget, never by the file.
pub const VE_DECODE_TOO_LONG: c_int = -2;

/// Stateless decode (no engine handle) for a background isolate: decode `path` and resample
/// to `rate`, returning malloc'd interleaved-stereo f32. Free with [`ve_free_decoded`]. Safe
/// to call off the main thread (touches no shared engine state).
///
/// `max_frames` (at the OUTPUT `rate`; 0 = unbounded) caps the decoded duration up front:
/// the whole track materializes in memory here, so the caller's budget must be enforced
/// BEFORE the allocation happens, not checked after. `status` (nullable) receives
/// [`VE_DECODE_OK`] / [`VE_DECODE_ERR`] / [`VE_DECODE_TOO_LONG`] so a too-long file can
/// surface its own message instead of a generic decode failure.
#[no_mangle]
pub extern "C" fn ve_decode(
    path: *const c_char,
    rate: u32,
    max_frames: u64,
    status: *mut c_int,
) -> VePcm {
    let set_status = |code: c_int| {
        if !status.is_null() {
            unsafe { *status = code };
        }
    };
    set_status(VE_DECODE_ERR);
    if path.is_null() || rate == 0 || rate > MAX_DECODE_RATE {
        return VePcm::empty();
    }
    catch_unwind(AssertUnwindSafe(|| {
        let path = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return VePcm::empty(),
        };
        // The budget converts to seconds here and back to source-rate frames inside
        // decode_file (which knows the source rate); f64 keeps the round-trip exact enough
        // that the boundary error is < 1 frame.
        let max_secs = if max_frames == 0 {
            0.0
        } else {
            max_frames as f64 / rate as f64
        };
        let (frames, src_rate) = match device::decode_file(path, max_secs) {
            Ok(v) => v,
            Err(device::DecodeError::TooLong) => {
                set_status(VE_DECODE_TOO_LONG);
                return VePcm::empty();
            }
            Err(device::DecodeError::Other(_)) => return VePcm::empty(),
        };
        // Consuming resample: the identity case moves the buffer through un-copied, so the
        // peak here is 2 full-track buffers when resampling and 1 otherwise (down from 3).
        let resampled = device::resample_linear(frames, src_rate, rate);
        // Resampling can overshoot the budget by a rounding frame; clamp defensively.
        let n = resampled.len();
        if max_frames > 0 && n as u64 > max_frames {
            set_status(VE_DECODE_TOO_LONG);
            return VePcm::empty();
        }
        let flat = flatten_stereo(resampled);
        // into_boxed_slice guarantees len == capacity, which ve_free_decoded relies on. It
        // only realloc-copies when capacity overshoots len (growth-by-push decode with no
        // declared frame count); the exact-capacity paths shrink for free.
        let mut boxed = flat.into_boxed_slice();
        let ptr = boxed.as_mut_ptr();
        std::mem::forget(boxed);
        set_status(VE_DECODE_OK);
        VePcm {
            ptr,
            frames: n,
            rate,
        }
    }))
    .unwrap_or_else(|_| VePcm::empty())
}

/// Probe `path`'s DECLARED duration in milliseconds without decoding (container probe only)
/// — the import gate's cheap pre-check. Returns the duration in ms; -1 when the container
/// declares no frame count (caller falls back to the decode-time budget); -2 when the file
/// can't be opened/probed at all. Stateless; safe off the main thread.
#[no_mangle]
pub extern "C" fn ve_probe_duration_ms(path: *const c_char) -> i64 {
    if path.is_null() {
        return -2;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let path = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(p) => p,
            Err(_) => return -2,
        };
        match device::probe_duration_ms(path) {
            Ok(Some(ms)) => ms.min(i64::MAX as u64) as i64,
            Ok(None) => -1,
            Err(_) => -2,
        }
    }))
    .unwrap_or(-2)
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
/// `rate`), build `old[0..split] ++ new`, and write it to `out_path` as a 32-bit float WAV
/// (the recording/take format; legacy 16-bit inputs decode fine and re-encode as float).
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
    if old_path.is_null() || new_path.is_null() || out_path.is_null() {
        return -1;
    }
    if rate == 0 || rate > MAX_DECODE_RATE {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let cstr = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_str().ok();
        let (old_p, new_p, out_p) = match (cstr(old_path), cstr(new_path), cstr(out_path)) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => return -1,
        };
        // BUDGET BEFORE DECODE: the output is exactly split + new (keep + pad == split by
        // construction), so with DECLARED input durations the whole splice can be refused
        // before either full-track decode allocates. Containers without a declared length
        // fall through to the decode-time budgets below — bounded either way, but the
        // pre-check keeps a doomed splice from transiently holding two 60-minute tracks.
        let ceiling_pre = (MAX_PLAYBACK_SECS * rate as f64) as i128;
        let split_pre = (split_ms.max(0) as i128 * rate as i128) / 1000;
        if split_pre > ceiling_pre {
            return -1;
        }
        let declared_frames = |p: &str| -> Option<i128> {
            device::probe_duration_ms(p)
                .ok()
                .flatten()
                .map(|ms| (ms as i128 * rate as i128) / 1000)
        };
        if let Some(new_decl) = declared_frames(new_p) {
            if split_pre + new_decl > ceiling_pre {
                return -1;
            }
        }
        if let Some(old_decl) = declared_frames(old_p) {
            if old_decl > ceiling_pre {
                return -1;
            }
        }
        let decode = |p: &str| -> Option<Vec<[Smp; CHANNELS]>> {
            // Both inputs are normally takes this app recorded, but nothing enforces that,
            // so they get the same generous ceiling as the playback path (the Dart side's
            // maxPlaybackDuration, 60 min) instead of an unbounded decode.
            let (frames, src_rate) = device::decode_file(p, MAX_PLAYBACK_SECS).ok()?;
            Some(device::resample_linear(frames, src_rate, rate))
        };
        let (old_frames, new_frames) = match (decode(old_p), decode(new_p)) {
            (Some(a), Some(b)) => (a, b),
            _ => return -1,
        };

        // Checked all the way to the allocation: the old `as usize` cast of the i128 split
        // TRUNCATED a huge split_ms (wrapping to a small value — no overflow, but silently
        // wrong semantics), and unchecked adds/muls on attacker-influenced sizes are how
        // ceilings get bypassed. The ceiling itself is compared in i128, before any cast.
        let ceiling_frames = (MAX_PLAYBACK_SECS * rate as f64) as i128;
        let split_i128 = (split_ms.max(0) as i128 * rate as i128) / 1000;
        if split_i128 > ceiling_frames {
            return -1;
        }
        let split = split_i128 as usize;
        let keep = split.min(old_frames.len());
        // Silence frames when the cursor sits past old's end (preserves new's timing).
        let pad = split.saturating_sub(old_frames.len());
        // The OUTPUT is bounded too, not just the two input decodes: a runaway `split_ms`
        // would otherwise size an arbitrarily large silence pad straight into with_capacity.
        let total = match keep
            .checked_add(pad)
            .and_then(|t| t.checked_add(new_frames.len()))
        {
            Some(t) if (t as i128) <= ceiling_frames => t,
            _ => return -1,
        };
        let Some(flat_len) = total.checked_mul(CHANNELS) else {
            return -1;
        };

        let mut flat: Vec<f32> = Vec::with_capacity(flat_len);
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
        match crate::wav::write_wav_f32(out_p, &flat, CHANNELS as u16, rate) {
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
        eng.cache_seq += 1;
        let seq = eng.cache_seq;
        // The caller ALWAYS hands freshly decoded PCM here (Dart only reaches this after a
        // `ve_tracks_add` cache miss, and it just decoded the file), so store/replace the
        // cache entry unconditionally: serving a cached Arc on a path hit returned STALE
        // samples whenever the file's content changed under a reused path. Path-only cache
        // lookups stay in `ve_tracks_add`, which is documented cache-only.
        let slice = unsafe { std::slice::from_raw_parts(pcm, frames * CHANNELS) };
        let arc = cache_store_pcm(&mut eng.cache, path, slice, seq);
        let start_frame = (start_ms as f64 * rate / 1000.0) as i64;
        let idx = eng.pending.len() as c_int;
        eng.pending.push(PendingTrack {
            path: path.to_string(),
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
        eng.handle.set_param(
            build::MASTER_ID,
            crate::dsp::master::param::GAIN,
            gain.max(0.0),
        );
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

/// Decode `path` to interleaved-stereo f32 resampled to the engine's device rate. The caller
/// frees the returned buffer with [`ve_free_pcm`].
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
        // Same ceiling as the playback path — this legacy entry must not stay the one
        // unbounded decode door.
        let (frames, src_rate) = match device::decode_file(path, MAX_PLAYBACK_SECS) {
            Ok(v) => v,
            Err(_) => return VePcm::empty(),
        };
        let resampled = device::resample_linear(frames, src_rate, rate);
        let n = resampled.len();
        // Flatten [f32;2] -> Vec<f32> in place and hand ownership to the caller.
        let flat = flatten_stereo(resampled);
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

#[cfg(test)]
mod cache_tests {
    use super::{cache_lookup_fresh, cache_retain, cache_store_pcm, CachedPcm};
    use crate::sample::Smp;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    /// A cache entry of exactly `bytes` (Smp = f32, 4 bytes each) stamped with `seq`.
    fn entry(bytes: usize, seq: u64) -> CachedPcm {
        CachedPcm {
            pcm: Arc::from(vec![0.0 as Smp; bytes / std::mem::size_of::<Smp>()]),
            staged_seq: seq,
            stamp: None,
        }
    }

    #[test]
    fn live_paths_survive_even_with_zero_budget() {
        let mut cache = HashMap::new();
        cache.insert("a".to_string(), entry(1024, 1));
        cache.insert("b".to_string(), entry(1024, 2));
        let live: HashSet<&str> = ["a"].into_iter().collect();
        cache_retain(&mut cache, &live, 0);
        assert!(cache.contains_key("a"), "live entry must never be evicted");
        assert!(!cache.contains_key("b"), "non-live entry over budget must go");
    }

    #[test]
    fn recent_non_live_kept_within_budget_oldest_evicted() {
        let mut cache = HashMap::new();
        cache.insert("old".to_string(), entry(1024, 1));
        cache.insert("mid".to_string(), entry(1024, 2));
        cache.insert("new".to_string(), entry(1024, 3));
        let live: HashSet<&str> = HashSet::new();
        // Budget fits exactly two entries: the two most recently staged survive.
        cache_retain(&mut cache, &live, 2048);
        assert!(cache.contains_key("new"));
        assert!(cache.contains_key("mid"));
        assert!(!cache.contains_key("old"));
    }

    #[test]
    fn exact_budget_boundary_keeps_the_fitting_entry() {
        let mut cache = HashMap::new();
        cache.insert("a".to_string(), entry(1024, 1));
        let live: HashSet<&str> = HashSet::new();
        // used == keep_bytes is within budget (eviction requires used > keep_bytes).
        cache_retain(&mut cache, &live, 1024);
        assert!(cache.contains_key("a"));
        cache_retain(&mut cache, &live, 1023);
        assert!(!cache.contains_key("a"));
    }

    #[test]
    fn all_live_evicts_nothing() {
        let mut cache = HashMap::new();
        cache.insert("a".to_string(), entry(1024, 1));
        cache.insert("b".to_string(), entry(1024, 2));
        let live: HashSet<&str> = ["a", "b"].into_iter().collect();
        cache_retain(&mut cache, &live, 0);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn fresh_pcm_replaces_a_stale_entry_at_the_same_path() {
        // `ve_tracks_add_pcm` always receives freshly decoded samples: a path hit must serve
        // the NEW content, not the cached Arc (a file re-rendered/re-recorded under a reused
        // path used to keep playing its old audio).
        let mut cache = HashMap::new();
        let old = cache_store_pcm(&mut cache, "take.wav", &[0.1, 0.1, 0.2, 0.2], 1);
        assert_eq!(&old[..], &[0.1, 0.1, 0.2, 0.2]);
        let new = cache_store_pcm(&mut cache, "take.wav", &[0.9, 0.9], 2);
        assert_eq!(&new[..], &[0.9, 0.9], "the staged Arc must be the fresh samples");
        let cached = cache.get("take.wav").expect("path stays cached");
        assert_eq!(&cached.pcm[..], &[0.9, 0.9], "the cache must hold the fresh samples");
        assert_eq!(cached.staged_seq, 2, "the LRU stamp follows the re-stage");
    }

    #[test]
    fn overwritten_file_at_a_cached_path_is_a_miss() {
        // `ve_tracks_add` used to serve the cached Arc on ANY path hit: a take re-rendered
        // under a reused path silently kept playing its OLD audio (the dominant stale-PCM
        // flow — Dart only decodes after a miss). The freshness stamp turns a changed file
        // into a miss, forcing the fresh decode + re-store.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("vocara_cache_stamp_{}.bin", std::process::id()));
        let p = path.to_str().unwrap();
        std::fs::write(&path, [0u8; 8]).unwrap();
        let mut cache = HashMap::new();
        cache_store_pcm(&mut cache, p, &[0.1, 0.1], 1);
        let hit = cache_lookup_fresh(&mut cache, p, 2).expect("unchanged file must hit");
        assert_eq!(&hit[..], &[0.1, 0.1]);
        assert_eq!(cache.get(p).unwrap().staged_seq, 2, "a hit bumps the LRU stamp");
        // Overwrite with a different SIZE so the check cannot depend on mtime granularity.
        std::fs::write(&path, [0u8; 16]).unwrap();
        assert!(
            cache_lookup_fresh(&mut cache, p, 3).is_none(),
            "a file overwritten under a cached path must MISS"
        );
        // The fresh decode re-stores and hits again.
        cache_store_pcm(&mut cache, p, &[0.9, 0.9], 4);
        let fresh = cache_lookup_fresh(&mut cache, p, 5).expect("re-stored entry hits");
        assert_eq!(&fresh[..], &[0.9, 0.9]);
        // A DELETED source keeps serving the cache (staged audio may outlive its file; a
        // forced re-decode could only fail).
        std::fs::remove_file(&path).unwrap();
        assert!(
            cache_lookup_fresh(&mut cache, p, 6).is_some(),
            "an unreadable source file must not invalidate the cache"
        );
    }
}

#[cfg(test)]
mod punch_tests {
    //! The stateless punch splice's output ceiling: a runaway `split_ms` used to size an
    //! arbitrarily large silence pad straight into the output allocation.

    use std::ffi::CString;

    fn temp_wav(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vocara_punch_{}_{}.wav", std::process::id(), name))
    }

    #[test]
    fn a_runaway_split_pad_is_refused_and_a_normal_splice_still_works() {
        let old = temp_wav("old");
        let new = temp_wav("new");
        let out = temp_wav("out");
        crate::wav::write_wav_f32(&old, &[0.1; 96], 2, 48_000).unwrap();
        crate::wav::write_wav_f32(&new, &[0.2; 96], 2, 48_000).unwrap();
        let c = |p: &std::path::Path| CString::new(p.to_str().unwrap()).unwrap();
        let (c_old, c_new, c_out) = (c(&old), c(&new), c(&out));

        // A two-hour cursor offset would demand a two-hour silence pad: refused.
        let frames = super::ve_punch_wav(
            c_old.as_ptr(),
            c_new.as_ptr(),
            2 * 3600 * 1000,
            48_000,
            c_out.as_ptr(),
        );
        assert_eq!(frames, -1, "an over-ceiling output must be refused");

        // An absurd output rate is refused before any arithmetic uses it.
        let frames = super::ve_punch_wav(c_old.as_ptr(), c_new.as_ptr(), 1, 10_000_000, c_out.as_ptr());
        assert_eq!(frames, -1, "an over-ceiling rate must be refused");

        // A sane splice is unaffected: old's first 1 ms (48 frames) ++ new's 48 frames.
        let frames = super::ve_punch_wav(c_old.as_ptr(), c_new.as_ptr(), 1, 48_000, c_out.as_ptr());
        assert_eq!(frames, 96, "1 ms of old plus all of new");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::remove_file(&new);
        let _ = std::fs::remove_file(&out);
    }
}
