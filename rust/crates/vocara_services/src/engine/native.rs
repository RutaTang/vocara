//! The native engine: [`EngineBindings`] over `vocara_engine::ffi` (macOS,
//! `device` feature).

use std::ffi::{c_char, c_int};
use std::ptr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use vocara_engine::ffi::{self, VeEngine};

use super::{c_chars_to_string, c_string, EngineBindings, EngineError, UID_CAP};

/// Samples per frame in the buffers the engine reads and fills.
const CHANNELS: usize = vocara_engine::sample::CHANNELS;

/// The longest engine diagnostics line read in one piece (longer ones are cut).
const DIAGNOSTIC_CAP: usize = 1024;

/// The engine pointer `ve_create` returned, or null once destroyed.
struct Handle(*mut VeEngine);

// SAFETY: the engine is built to be driven by one control thread at a time,
// not by a fixed one: nothing on its control side depends on the calling
// thread's identity, its thread-bound parts (the `!Send` CoreAudio streams,
// the VoiceProcessingIO unit) live on the audio-host thread it owns and are
// reached only by message, and the retired graphs it holds raw pointers to
// are its own to free from whichever thread reaps. The `Mutex` around the
// handle keeps every call exclusive, so at most one thread is ever inside it.
unsafe impl Send for Handle {}

/// The native audio engine (`ve_create` .. `ve_destroy`), one instance.
///
/// Every call locks the one mutex around the engine pointer, then calls the C
/// ABI on it: the ABI makes each call an exclusive `&mut VeEngine`, so calls
/// are serialized, as the Dart side serialized them on one isolate (see the
/// module docs on threading). A destroyed engine answers every call as an
/// unavailable one, without reaching the C ABI.
pub struct NativeEngine {
    engine: Mutex<Handle>,
}

impl NativeEngine {
    /// Creates the engine (`ve_create`): the graph at the engine rate, and the
    /// audio-host thread, which opens the default output and resamples to its
    /// native rate at the I/O boundary. [`EngineError::Unavailable`] when that
    /// fails (no output device opened).
    pub fn create() -> Result<NativeEngine, EngineError> {
        let e = ffi::ve_create();
        if e.is_null() {
            return Err(EngineError::Unavailable);
        }
        Ok(NativeEngine {
            engine: Mutex::new(Handle(e)),
        })
    }

    /// The lock; a panic in another holder (none reach the C ABI: it catches
    /// its own) cannot have left the pointer half-written, so a poisoned lock
    /// is taken as is.
    fn lock(&self) -> MutexGuard<'_, Handle> {
        self.engine.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `f` on the live engine, or `unavailable` once destroyed.
    fn read<T>(&self, unavailable: T, f: impl FnOnce(*mut VeEngine) -> T) -> T {
        let h = self.lock();
        if h.0.is_null() {
            unavailable
        } else {
            f(h.0)
        }
    }

    /// A call answering 0 for done and a negative code for refused.
    fn status(
        &self,
        call: &'static str,
        f: impl FnOnce(*mut VeEngine) -> c_int,
    ) -> Result<(), EngineError> {
        match self.read(None, |e| Some(f(e))) {
            None => Err(EngineError::Unavailable),
            Some(0) => Ok(()),
            Some(code) => Err(EngineError::Refused { call, code }),
        }
    }

    /// A call answering an index (>= 0) or a negative code for refused.
    fn index(
        &self,
        call: &'static str,
        f: impl FnOnce(*mut VeEngine) -> c_int,
    ) -> Result<i32, EngineError> {
        match self.read(None, |e| Some(f(e))) {
            None => Err(EngineError::Unavailable),
            Some(i) if i >= 0 => Ok(i),
            Some(code) => Err(EngineError::Refused { call, code }),
        }
    }

    /// Reads a uid the engine writes into a caller-owned buffer of [`UID_CAP`]
    /// chars ("" once destroyed).
    fn uid(&self, read: extern "C" fn(*mut VeEngine, *mut c_char, usize) -> usize) -> String {
        self.read(String::new(), |e| {
            let mut buf: [c_char; UID_CAP] = [0; UID_CAP];
            // SAFETY: `buf` holds UID_CAP chars, the cap passed; the engine
            // writes at most that many, NUL-terminated and zero-padded.
            read(e, buf.as_mut_ptr(), UID_CAP);
            c_chars_to_string(&buf)
        })
    }
}

impl Drop for NativeEngine {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Splits (target, level) pairs into the C ABI's parallel arrays.
fn split_sends(sends: &[(i32, f32)]) -> (Vec<i32>, Vec<f32>) {
    sends.iter().copied().unzip()
}

#[deny(clippy::missing_trait_methods)]
#[allow(clippy::too_many_arguments)]
impl EngineBindings for NativeEngine {
    fn destroy(&self) {
        let mut h = self.lock();
        let e = std::mem::replace(&mut h.0, ptr::null_mut());
        if !e.is_null() {
            // SAFETY: `e` is the pointer ve_create returned, nulled above under
            // the lock, so it is destroyed exactly once and no call can be
            // inside the engine meanwhile (each holds the lock throughout).
            ffi::ve_destroy(e);
        }
    }

    fn set_output_device(&self, uid: &str) -> Result<(), EngineError> {
        let cuid = c_string(uid);
        // SAFETY: a NUL-terminated string alive for the call.
        self.status("ve_set_output_device", |e| {
            ffi::ve_set_output_device(e, cuid.as_ptr())
        })
    }

    fn current_output_device(&self) -> String {
        self.uid(ffi::ve_current_output_device)
    }

    fn actual_output_device(&self) -> String {
        self.uid(ffi::ve_actual_output_device)
    }

    fn poll_device_event(&self, mask: u32) -> u32 {
        self.read(0, |e| ffi::ve_poll_device_event(e, mask))
    }

    fn stream_xruns(&self) -> u64 {
        self.read(0, |e| ffi::ve_stream_xruns(e))
    }

    fn commit_degraded_mask(&self) -> u64 {
        self.read(0, |e| ffi::ve_commit_degraded_mask(e))
    }

    fn drift_ppm(&self) -> f64 {
        self.read(0.0, |e| ffi::ve_drift_ppm(e))
    }

    fn set_input_device(&self, uid: &str) -> Result<(), EngineError> {
        let cuid = c_string(uid);
        // SAFETY: a NUL-terminated string alive for the call.
        self.status("ve_set_input_device", |e| {
            ffi::ve_set_input_device(e, cuid.as_ptr())
        })
    }

    fn current_input_device(&self) -> String {
        self.uid(ffi::ve_current_input_device)
    }

    fn actual_input_device(&self) -> String {
        self.uid(ffi::ve_actual_input_device)
    }

    fn start_monitor(&self) -> Result<(), EngineError> {
        self.status("ve_start_monitor", |e| ffi::ve_start_monitor(e))
    }

    fn stop_monitor(&self) -> Result<(), EngineError> {
        self.status("ve_stop_monitor", |e| ffi::ve_stop_monitor(e))
    }

    fn set_monitor_gain(&self, gain: f32) -> Result<(), EngineError> {
        self.status("ve_set_monitor_gain", |e| ffi::ve_set_monitor_gain(e, gain))
    }

    fn set_input_gain(&self, gain: f32) -> Result<(), EngineError> {
        self.status("ve_set_input_gain", |e| ffi::ve_set_input_gain(e, gain))
    }

    fn set_echo_free(&self, uid: &str) -> Result<(), EngineError> {
        let cuid = c_string(uid);
        // SAFETY: a NUL-terminated string alive for the call ("" clears).
        self.status("ve_set_echo_free", |e| {
            ffi::ve_set_echo_free(e, cuid.as_ptr())
        })
    }

    fn set_input_channel(&self, mode: u32) -> Result<(), EngineError> {
        self.status("ve_set_input_channel", |e| {
            ffi::ve_set_input_channel(e, mode)
        })
    }

    fn set_monitor_reverb(&self, room: f32, damping: f32, wet: f32) -> Result<(), EngineError> {
        self.status("ve_set_monitor_reverb", |e| {
            ffi::ve_set_monitor_reverb(e, room, damping, wet)
        })
    }

    fn input_level(&self) -> f32 {
        self.read(0.0, |e| ffi::ve_input_level(e))
    }

    fn input_frames_received(&self) -> u64 {
        self.read(0, |e| ffi::ve_input_frames_received(e))
    }

    fn input_stream_state(&self) -> u64 {
        self.read(0, |e| ffi::ve_input_stream_state(e))
    }

    fn record_latency_frames(&self) -> u32 {
        self.read(0, |e| ffi::ve_record_latency_frames(e))
    }

    fn start_capture(&self) -> Result<(), EngineError> {
        self.status("ve_start_capture", |e| ffi::ve_start_capture(e))
    }

    fn stop_capture(&self) -> Result<(), EngineError> {
        self.status("ve_stop_capture", |e| ffi::ve_stop_capture(e))
    }

    fn input_frames(&self, out: &mut [f32]) -> usize {
        let max_frames = out.len() / CHANNELS;
        // SAFETY: the engine writes at most `max_frames * CHANNELS` floats,
        // which `out` (aligned, exclusively borrowed) holds.
        self.read(0, |e| ffi::ve_input_frames(e, out.as_mut_ptr(), max_frames))
    }

    fn start_input_recording(&self, path: &str) -> Result<(), EngineError> {
        let cpath = c_string(path);
        // SAFETY: a NUL-terminated string alive for the call.
        self.status("ve_start_input_recording", |e| {
            ffi::ve_start_input_recording(e, cpath.as_ptr())
        })
    }

    fn stop_input_recording(&self) -> Result<(), EngineError> {
        match self.status("ve_stop_input_recording", |e| {
            ffi::ve_stop_input_recording(e)
        }) {
            Err(EngineError::Refused { code: -2, .. }) => Err(EngineError::RecordingIo),
            r => r,
        }
    }

    fn record_dropped_frames(&self) -> u64 {
        self.read(0, |e| ffi::ve_record_dropped_frames(e))
    }

    fn record_written_frames(&self) -> u64 {
        self.read(0, |e| ffi::ve_record_written_frames(e))
    }

    fn input_intent_seq(&self) -> u64 {
        self.read(0, |e| ffi::ve_input_intent_seq(e))
    }

    fn input_applied_state(&self) -> u64 {
        self.read(0, |e| ffi::ve_input_applied_state(e))
    }

    fn reopen_input(&self, generation: u64) -> Result<(), EngineError> {
        self.status("ve_reopen_input", |e| ffi::ve_reopen_input(e, generation))
    }

    fn take_diagnostics(&self) -> Vec<String> {
        // The queue is process-wide (each line names its engine), so it is read whether or not
        // this engine is still alive, and without its lock.
        let mut lines = Vec::new();
        let mut buf: Vec<c_char> = vec![0; DIAGNOSTIC_CAP];
        // Bounded: a queue refilled as fast as it is read cannot hold the caller.
        for _ in 0..vocara_engine::diag::QUEUE_CAP + 1 {
            // SAFETY: `buf` holds DIAGNOSTIC_CAP chars, the cap passed; the engine writes at
            // most that many, NUL-terminated.
            let n = ffi::ve_take_diagnostic(buf.as_mut_ptr(), DIAGNOSTIC_CAP);
            if n == 0 {
                break;
            }
            lines.push(c_chars_to_string(&buf));
        }
        lines
    }

    fn play(&self) -> Result<(), EngineError> {
        self.status("ve_play", |e| ffi::ve_play(e))
    }

    fn pause(&self) -> Result<(), EngineError> {
        self.status("ve_pause", |e| ffi::ve_pause(e))
    }

    fn seek(&self, frame: i64) -> Result<(), EngineError> {
        self.status("ve_seek", |e| ffi::ve_seek(e, frame))
    }

    fn set_loop(&self, enabled: bool, start: i64, end: i64) -> Result<(), EngineError> {
        self.status("ve_set_loop", |e| {
            ffi::ve_set_loop(e, c_int::from(enabled), start, end)
        })
    }

    fn set_param(&self, node: u32, param: u16, value: f32) -> Result<(), EngineError> {
        self.status("ve_set_param", |e| ffi::ve_set_param(e, node, param, value))
    }

    fn flush(&self) -> Result<(), EngineError> {
        self.status("ve_flush", |e| ffi::ve_flush(e))
    }

    fn frame_pos(&self) -> i64 {
        self.read(0, |e| ffi::ve_frame_pos(e))
    }

    fn sample_rate(&self) -> f64 {
        self.read(0.0, |e| ffi::ve_sample_rate(e))
    }

    fn tracks_begin(&self) -> Result<(), EngineError> {
        self.status("ve_tracks_begin", |e| ffi::ve_tracks_begin(e))
    }

    fn tracks_add(
        &self,
        path: &str,
        start_ms: i64,
        gain: f32,
        muted: bool,
        is_vocal: bool,
        bus: i32,
    ) -> Result<i32, EngineError> {
        let cpath = c_string(path);
        // SAFETY: a NUL-terminated string alive for the call.
        self.index("ve_tracks_add", |e| {
            ffi::ve_tracks_add(
                e,
                cpath.as_ptr(),
                start_ms,
                gain,
                c_int::from(muted),
                c_int::from(is_vocal),
                bus,
            )
        })
    }

    fn tracks_add_pcm(
        &self,
        path: &str,
        pcm: &[f32],
        start_ms: i64,
        gain: f32,
        muted: bool,
        is_vocal: bool,
        bus: i32,
    ) -> Result<i32, EngineError> {
        let cpath = c_string(path);
        let frames = pcm.len() / CHANNELS;
        // SAFETY: `cpath` is a NUL-terminated string alive for the call; the
        // engine copies `frames * CHANNELS` floats out of `pcm` (aligned,
        // non-null even when empty), which it holds, and keeps no pointer.
        self.index("ve_tracks_add_pcm", |e| {
            ffi::ve_tracks_add_pcm(
                e,
                cpath.as_ptr(),
                pcm.as_ptr(),
                frames,
                start_ms,
                gain,
                c_int::from(muted),
                c_int::from(is_vocal),
                bus,
            )
        })
    }

    fn tracks_commit(&self) -> Result<(), EngineError> {
        self.status("ve_tracks_commit", |e| ffi::ve_tracks_commit(e))
    }

    fn tracks_commit_staged(&self) -> Result<(), EngineError> {
        self.status("ve_tracks_commit_staged", |e| {
            ffi::ve_tracks_commit_staged(e)
        })
    }

    fn tracks_publish(&self) -> Result<(), EngineError> {
        self.status("ve_tracks_publish", |e| ffi::ve_tracks_publish(e))
    }

    fn set_track_volume(&self, index: i32, gain: f32, muted: bool) -> Result<(), EngineError> {
        self.status("ve_set_track_volume", |e| {
            ffi::ve_set_track_volume(e, index, gain, c_int::from(muted))
        })
    }

    fn set_pan(&self, index: i32, pan: f32) -> Result<(), EngineError> {
        self.status("ve_set_pan", |e| ffi::ve_set_pan(e, index, pan))
    }

    fn buses_begin(&self) -> Result<(), EngineError> {
        self.status("ve_buses_begin", |e| ffi::ve_buses_begin(e))
    }

    fn buses_add(&self, gain: f32, muted: bool, output: i32) -> Result<i32, EngineError> {
        self.index("ve_buses_add", |e| {
            ffi::ve_buses_add(e, gain, c_int::from(muted), output)
        })
    }

    fn track_chain(&self, track: i32, tags: &[u8]) -> Result<(), EngineError> {
        // SAFETY: the engine copies `tags.len()` bytes out of `tags`, which
        // holds them, and keeps no pointer.
        self.status("ve_track_chain", |e| {
            ffi::ve_track_chain(e, track, tags.as_ptr(), tags.len())
        })
    }

    fn track_lane(&self, track: i32, leader: i32) -> Result<(), EngineError> {
        self.status("ve_track_lane", |e| ffi::ve_track_lane(e, track, leader))
    }

    fn bus_chain(&self, bus: i32, tags: &[u8]) -> Result<(), EngineError> {
        // SAFETY: as for `track_chain`.
        self.status("ve_bus_chain", |e| {
            ffi::ve_bus_chain(e, bus, tags.as_ptr(), tags.len())
        })
    }

    fn track_sends(&self, track: i32, sends: &[(i32, f32)]) -> Result<(), EngineError> {
        let (targets, levels) = split_sends(sends);
        // SAFETY: the two arrays are parallel, `sends.len()` long each, and
        // outlive the call; the engine reads at most that many and keeps no
        // pointer.
        self.status("ve_track_sends", |e| {
            ffi::ve_track_sends(e, track, targets.as_ptr(), levels.as_ptr(), sends.len())
        })
    }

    fn bus_sends(&self, bus: i32, sends: &[(i32, f32)]) -> Result<(), EngineError> {
        let (targets, levels) = split_sends(sends);
        // SAFETY: as for `track_sends`.
        self.status("ve_bus_sends", |e| {
            ffi::ve_bus_sends(e, bus, targets.as_ptr(), levels.as_ptr(), sends.len())
        })
    }

    fn set_send_level(&self, track: i32, slot: i32, level_db: f32) -> Result<(), EngineError> {
        self.status("ve_set_send_level", |e| {
            ffi::ve_set_send_level(e, track, slot, level_db)
        })
    }

    fn set_bus_send_level(&self, bus: i32, slot: i32, level_db: f32) -> Result<(), EngineError> {
        self.status("ve_set_bus_send_level", |e| {
            ffi::ve_set_bus_send_level(e, bus, slot, level_db)
        })
    }

    fn set_master(&self, gain: f32) -> Result<(), EngineError> {
        self.status("ve_set_master", |e| ffi::ve_set_master(e, gain))
    }

    fn set_eq_band(
        &self,
        track: i32,
        band: i32,
        freq: f32,
        q: f32,
        gain_db: f32,
        enabled: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_eq_band", |e| {
            ffi::ve_set_eq_band(e, track, band, freq, q, gain_db, c_int::from(enabled))
        })
    }

    fn set_comp(
        &self,
        track: i32,
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_comp", |e| {
            ffi::ve_set_comp(
                e,
                track,
                threshold_db,
                ratio,
                knee_db,
                attack_ms,
                release_ms,
                makeup_db,
                c_int::from(bypass),
            )
        })
    }

    fn set_send(&self, track: i32, send_db: f32) -> Result<(), EngineError> {
        self.status("ve_set_send", |e| ffi::ve_set_send(e, track, send_db))
    }

    fn set_reverb(
        &self,
        room: f32,
        damping: f32,
        wet: f32,
        dry: f32,
        width: f32,
    ) -> Result<(), EngineError> {
        self.status("ve_set_reverb", |e| {
            ffi::ve_set_reverb(e, room, damping, wet, dry, width)
        })
    }

    fn track_gr(&self, track: i32) -> f32 {
        self.read(0.0, |e| ffi::ve_track_gr(e, track))
    }

    fn set_deesser(
        &self,
        track: i32,
        freq: f32,
        threshold_db: f32,
        range_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_deesser", |e| {
            ffi::ve_set_deesser(e, track, freq, threshold_db, range_db, c_int::from(bypass))
        })
    }

    fn set_gate(
        &self,
        track: i32,
        threshold_db: f32,
        range_db: f32,
        attack_ms: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_gate", |e| {
            ffi::ve_set_gate(
                e,
                track,
                threshold_db,
                range_db,
                attack_ms,
                release_ms,
                c_int::from(bypass),
            )
        })
    }

    fn set_delay(
        &self,
        track: i32,
        time_ms: f32,
        feedback: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_delay", |e| {
            ffi::ve_set_delay(e, track, time_ms, feedback, mix, c_int::from(bypass))
        })
    }

    fn set_saturation(
        &self,
        track: i32,
        drive_db: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_saturation", |e| {
            ffi::ve_set_saturation(e, track, drive_db, mix, c_int::from(bypass))
        })
    }

    fn set_doubler(
        &self,
        track: i32,
        delay_ms: f32,
        depth_ms: f32,
        rate_hz: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_doubler", |e| {
            ffi::ve_set_doubler(
                e,
                track,
                delay_ms,
                depth_ms,
                rate_hz,
                mix,
                c_int::from(bypass),
            )
        })
    }

    fn set_limiter(
        &self,
        track: i32,
        ceiling_db: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_limiter", |e| {
            ffi::ve_set_limiter(e, track, ceiling_db, release_ms, c_int::from(bypass))
        })
    }

    fn set_reverb_insert(
        &self,
        track: i32,
        room: f32,
        damping: f32,
        wet: f32,
        width: f32,
    ) -> Result<(), EngineError> {
        self.status("ve_set_reverb_insert", |e| {
            ffi::ve_set_reverb_insert(e, track, room, damping, wet, width)
        })
    }

    fn set_bus_eq_band(
        &self,
        bus: i32,
        band: i32,
        freq: f32,
        q: f32,
        gain_db: f32,
        enabled: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_eq_band", |e| {
            ffi::ve_set_bus_eq_band(e, bus, band, freq, q, gain_db, c_int::from(enabled))
        })
    }

    fn set_bus_comp(
        &self,
        bus: i32,
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_comp", |e| {
            ffi::ve_set_bus_comp(
                e,
                bus,
                threshold_db,
                ratio,
                knee_db,
                attack_ms,
                release_ms,
                makeup_db,
                c_int::from(bypass),
            )
        })
    }

    fn set_bus_send(&self, bus: i32, send_db: f32) -> Result<(), EngineError> {
        self.status("ve_set_bus_send", |e| ffi::ve_set_bus_send(e, bus, send_db))
    }

    fn set_bus_volume(&self, bus: i32, gain: f32, muted: bool) -> Result<(), EngineError> {
        self.status("ve_set_bus_volume", |e| {
            ffi::ve_set_bus_volume(e, bus, gain, c_int::from(muted))
        })
    }

    fn set_bus_pan(&self, bus: i32, pan: f32) -> Result<(), EngineError> {
        self.status("ve_set_bus_pan", |e| ffi::ve_set_bus_pan(e, bus, pan))
    }

    fn bus_gr(&self, bus: i32) -> f32 {
        self.read(0.0, |e| ffi::ve_bus_gr(e, bus))
    }

    fn set_bus_deesser(
        &self,
        bus: i32,
        freq: f32,
        threshold_db: f32,
        range_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_deesser", |e| {
            ffi::ve_set_bus_deesser(e, bus, freq, threshold_db, range_db, c_int::from(bypass))
        })
    }

    fn set_bus_gate(
        &self,
        bus: i32,
        threshold_db: f32,
        range_db: f32,
        attack_ms: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_gate", |e| {
            ffi::ve_set_bus_gate(
                e,
                bus,
                threshold_db,
                range_db,
                attack_ms,
                release_ms,
                c_int::from(bypass),
            )
        })
    }

    fn set_bus_delay(
        &self,
        bus: i32,
        time_ms: f32,
        feedback: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_delay", |e| {
            ffi::ve_set_bus_delay(e, bus, time_ms, feedback, mix, c_int::from(bypass))
        })
    }

    fn set_bus_saturation(
        &self,
        bus: i32,
        drive_db: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_saturation", |e| {
            ffi::ve_set_bus_saturation(e, bus, drive_db, mix, c_int::from(bypass))
        })
    }

    fn set_bus_doubler(
        &self,
        bus: i32,
        delay_ms: f32,
        depth_ms: f32,
        rate_hz: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_doubler", |e| {
            ffi::ve_set_bus_doubler(
                e,
                bus,
                delay_ms,
                depth_ms,
                rate_hz,
                mix,
                c_int::from(bypass),
            )
        })
    }

    fn set_bus_limiter(
        &self,
        bus: i32,
        ceiling_db: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_limiter", |e| {
            ffi::ve_set_bus_limiter(e, bus, ceiling_db, release_ms, c_int::from(bypass))
        })
    }

    fn set_bus_reverb_insert(
        &self,
        bus: i32,
        room: f32,
        damping: f32,
        wet: f32,
        width: f32,
    ) -> Result<(), EngineError> {
        self.status("ve_set_bus_reverb_insert", |e| {
            ffi::ve_set_bus_reverb_insert(e, bus, room, damping, wet, width)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_native_engine_crosses_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<NativeEngine>();
    }

    #[test]
    fn sends_split_into_parallel_arrays() {
        assert_eq!(
            split_sends(&[(0, -6.0), (-1, 0.0), (2, -120.0)]),
            (vec![0, -1, 2], vec![-6.0, 0.0, -120.0])
        );
        assert_eq!(split_sends(&[]), (vec![], vec![]));
    }

    /// A destroyed engine answers every call as an unavailable one, without
    /// reaching the C ABI (built here around a null handle: what `destroy`
    /// leaves, and what no `create` ever returns).
    #[test]
    fn a_destroyed_engine_answers_as_unavailable() {
        let e = NativeEngine {
            engine: Mutex::new(Handle(ptr::null_mut())),
        };
        assert_eq!(e.play(), Err(EngineError::Unavailable));
        assert_eq!(e.reopen_input(1), Err(EngineError::Unavailable));
        // The diagnostics queue is the process's, not the engine's: still readable.
        let _ = e.take_diagnostics();
        assert_eq!(
            e.tracks_add("/x.wav", 0, 1.0, false, true, -1),
            Err(EngineError::Unavailable)
        );
        assert_eq!(e.frame_pos(), 0);
        assert_eq!(e.sample_rate(), 0.0);
        assert_eq!(e.actual_output_device(), "");
        let mut out = [1.0f32; 8];
        assert_eq!(e.input_frames(&mut out), 0);
        assert_eq!(out, [1.0; 8]);
        e.destroy();
        e.destroy();
    }
}
