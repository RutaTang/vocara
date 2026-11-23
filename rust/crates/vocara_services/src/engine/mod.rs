//! The audio engine bridge: the port of `lib/data/ffi/vocara_engine_bindings.dart`,
//! the ffigen bindings to the engine's C ABI, together with the marshalling the
//! Dart services wrapped around them (the device lists, the uid readers, the
//! decode and probe helpers of `graph_audio_playback_service.dart`'s top level).
//!
//! The engine is the Rust crate `client/rust/vocara_engine`. Its C ABI
//! (`vocara_engine::ffi`, 92 `extern "C"` functions) and its `device` module
//! exist only with the `device` feature, which forwards to the engine's. The
//! Dart side loaded the engine as a framework and looked every symbol up by
//! name; here it is linked in and called directly, with no lookup to fail.
//!
//! # What it offers
//!
//! - [`EngineBindings`]: the calls on one engine instance (`ve_play`,
//!   `ve_tracks_add`, `ve_poll_device_event`, ...), as safe methods with typed
//!   results. It is the seam the playback, recording and monitor services
//!   drive and their tests fake, as the Dart tests faked
//!   `VocaraEngineBindings`. [`create`] makes the native one.
//! - The calls that need no engine: [`decode`], [`decode_mono`],
//!   [`probe_duration_ms`], [`punch_wav`] (stateless file work the Dart side
//!   ran on background isolates), and [`enumerate_output_devices`],
//!   [`enumerate_input_devices`], [`input_device_channel_count`] (host
//!   queries).
//! - [`media`]: the decode policy the Dart services layered on those calls:
//!   the duration and memory caps, the typed refusals and their messages, the
//!   playback, analysis and probe helpers.
//!
//! # Native calls
//!
//! Every C ABI entry point only wraps device code (symphonia, CoreAudio, the
//! live graph), so each is called through `vocara_engine::ffi`. The C ABI
//! declares them as safe `extern "C" fn`s that validate their pointers in the
//! body, so a call needs no `unsafe` block; every call that hands one a
//! pointer still says, in a `// SAFETY:` comment, why the pointer is valid for
//! what the call does with it. The engine items used without `device` are its
//! constants ([`ENGINE_RATE`], the input-channel modes).
//!
//! # Without the `device` feature
//!
//! Every call takes the path the Dart side took when the native library was
//! unavailable: [`create`] reports no engine (as `ve_create` returning NULL
//! did, after which every service call no-ops), the device lists are empty and
//! the channel count is 0 (Dart caught the failed lookup), and the decodes,
//! the probe and the splice fail with an `Unavailable` error (Dart's lookup
//! threw out of them).
//!
//! # Threading
//!
//! A `NativeEngine` is `Send + Sync`. Its calls are serialized by one mutex:
//! the C ABI turns every call into an exclusive `&mut VeEngine`, so two at
//! once would alias it, and the Dart side made them all from one isolate. A
//! call that blocks (starting or stopping a recording, a commit priming its
//! streams, a device switch) holds the lock for as long, so a poll made
//! meanwhile waits behind it, as Dart's position timer waited on the same
//! isolate. The lock is the happens-before edge: a call sees every effect of
//! the calls that returned before it.
//!
//! The stateless calls touch no engine instance and run on any thread, at
//! once if need be, as the Dart side ran them in isolates.
//!
//! # Callbacks
//!
//! There are none: the engine posts nothing to its host, and the Dart side
//! used no `NativePort`. Everything that changes on the audio side is pulled:
//! the playhead (`ve_frame_pos`, Dart's 16 ms timer while playing), the device
//! events (`ve_poll_device_event`, the song page's 1 s watch and the technique
//! page's 50 ms tick, each claiming only its own bits), the input meter and
//! the captured frames (`ve_input_level`, `ve_input_frames`). Rust consumers
//! poll the same calls from their own ticks, with the same ordering: a poll
//! reads the state as of the last call that returned.
//!
//! # Memory
//!
//! A `NativeEngine` owns its `VeEngine` and destroys it exactly once, at
//! [`EngineBindings::destroy`] or on drop; a call after that answers as an
//! unavailable engine instead of reaching freed memory. A [`Pcm`] owns a
//! decoded buffer and frees it exactly once, on drop, with the free call that
//! matches its layout (`ve_free_decoded` reconstructs `frames * 2` floats,
//! `ve_free_decoded_mono` `frames`; the wrong one is undefined behavior, not a
//! leak). Every buffer handed in is a Rust slice whose length the wrapper
//! derives the count from (`out.len() / 2` frames for `ve_input_frames`,
//! `pcm.len() / 2` for `ve_tracks_add_pcm`), so the engine never reads or
//! writes past it; slices of `f32`, `i32` and `u8` are aligned for the C
//! types by construction.

mod bindings;
mod decode;
#[cfg(all(test, feature = "device"))]
mod device_tests;
mod devices;
pub mod media;
#[cfg(feature = "device")]
mod native;
mod pcm;

use std::ffi::{c_char, CString};
use std::sync::Arc;

pub use bindings::EngineBindings;
pub use decode::{decode, decode_mono, probe_duration_ms, punch_wav, DecodeError, ProbeError};
pub use devices::{
    enumerate_input_devices, enumerate_output_devices, input_device_channel_count, DEVICE_ENUM_CAP,
    UID_CAP,
};
#[cfg(feature = "device")]
pub use native::NativeEngine;
pub use pcm::{Layout, Pcm};

/// The engine's fixed graph rate (`vocara_engine::sample::ENGINE_SR`): the
/// playhead, the record counters and the recording are in frames at it, and
/// it is the rate takes and transcoded imports are written at.
pub const ENGINE_RATE: u32 = 48_000;

/// `ve_set_input_channel` modes: Auto (the input carrying the mic: a mic on
/// either input of a 2-input interface records at full level), Input 1, and
/// Input 2 (input 1 on a device without one).
pub use vocara_engine::capture_layout::{INPUT_CHANNEL_1, INPUT_CHANNEL_2, INPUT_CHANNEL_AUTO};

/// `ve_decode` statuses, as the C header defines them (`VE_DECODE_*`). The
/// Dart side mirrored them by hand because ffigen binds only functions and
/// structs; here a device build checks them against the engine's own.
pub const VE_DECODE_OK: i32 = 0;
/// Open, probe or decode failure (an unreadable or unsupported file).
pub const VE_DECODE_ERR: i32 = -1;
/// Over the caller's duration budget.
pub const VE_DECODE_TOO_LONG: i32 = -2;
/// Over a memory budget without being over-duration (a short, very dense file).
pub const VE_DECODE_TOO_BIG: i32 = -3;
/// A source rate outside the supported envelope.
pub const VE_DECODE_RATE_UNSUPPORTED: i32 = -4;
/// The file could not be read (the disk, not the format).
pub const VE_DECODE_IO: i32 = -5;

/// `ve_poll_device_event` bits (the engine's `device::EVENT_*`).
pub mod events {
    /// The active output device was lost (stream error or disconnect).
    pub const OUTPUT_LOST: u32 = 1;
    /// The active input device was lost (stream error or disconnect).
    pub const INPUT_LOST: u32 = 2;
    /// The mic opened WITHOUT echo cancellation.
    pub const AEC_BYPASSED: u32 = 4;
    /// The selected input was unavailable; capture fell back to the system default.
    pub const INPUT_FELL_BACK: u32 = 8;
    /// The take reached the longest recording a WAV header can describe:
    /// capture past it is refused and the owner must finalize.
    pub const RECORD_LIMIT: u32 = 16;
    /// Playback is not on the selected output device; the engine mutes the
    /// monitor for as long as that holds.
    pub const OUTPUT_REROUTED: u32 = 32;
}

#[cfg(feature = "device")]
const _: () = {
    use vocara_engine::{device, ffi};
    assert!(VE_DECODE_OK == ffi::VE_DECODE_OK);
    assert!(VE_DECODE_ERR == ffi::VE_DECODE_ERR);
    assert!(VE_DECODE_TOO_LONG == ffi::VE_DECODE_TOO_LONG);
    assert!(VE_DECODE_TOO_BIG == ffi::VE_DECODE_TOO_BIG);
    assert!(VE_DECODE_RATE_UNSUPPORTED == ffi::VE_DECODE_RATE_UNSUPPORTED);
    assert!(VE_DECODE_IO == ffi::VE_DECODE_IO);
    assert!(events::OUTPUT_LOST == device::EVENT_OUTPUT_LOST);
    assert!(events::INPUT_LOST == device::EVENT_INPUT_LOST);
    assert!(events::AEC_BYPASSED == device::EVENT_AEC_BYPASSED);
    assert!(events::INPUT_FELL_BACK == device::EVENT_INPUT_FELL_BACK);
    assert!(events::RECORD_LIMIT == device::EVENT_RECORD_LIMIT);
    assert!(events::OUTPUT_REROUTED == device::EVENT_OUTPUT_REROUTED);
};

/// Why a call on an engine instance failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// No engine: the build has no `device` feature, `ve_create` returned
    /// NULL (no output device opened), or the engine was destroyed. Dart's
    /// `isAvailable == false`.
    #[error("the audio engine is not available")]
    Unavailable,
    /// The engine answered `call` with the failure `code` it documents for it
    /// (-1 for most: a null engine, a bad argument, a caught panic; for
    /// `ve_tracks_add`, a PCM-cache miss; for `ve_start_input_recording`, a
    /// file that could not be created).
    #[error("the audio engine refused {call} ({code})")]
    Refused { call: &'static str, code: i32 },
    /// `ve_stop_input_recording`'s -2: an I/O error during the recording, so
    /// the WAV on disk is missing, damaged or incomplete.
    #[error("the recording hit an I/O error; the take on disk is missing, damaged or incomplete")]
    RecordingIo,
}

/// Creates the native engine (`ve_create`): builds the graph at
/// [`ENGINE_RATE`] and spawns the audio-host thread, which opens the default
/// output. [`EngineError::Unavailable`] when no output device opens, or in a
/// build without the `device` feature; the app then stays usable, just silent.
pub fn create() -> Result<Arc<dyn EngineBindings>, EngineError> {
    #[cfg(feature = "device")]
    {
        NativeEngine::create().map(|e| Arc::new(e) as Arc<dyn EngineBindings>)
    }
    #[cfg(not(feature = "device"))]
    {
        Err(EngineError::Unavailable)
    }
}

/// `s` as the C side reads it: Dart's `toNativeUtf8` copied the string whole,
/// so an interior NUL ended it there for the engine. Cut at the first NUL the
/// same way (a `CString` cannot hold one).
#[cfg_attr(not(feature = "device"), allow(dead_code))]
fn c_string(s: &str) -> CString {
    let bytes = s.as_bytes();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// Reads a fixed-size, NUL-terminated C char buffer (a device uid or name)
/// into a string: the bytes up to the first NUL, or the whole buffer. Decoded
/// as UTF-8 with malformed sequences replaced, as Dart decoded with
/// `allowMalformed`: device names and uids are not guaranteed ASCII (an
/// AirPlay device's name, an aggregate device's user-chosen uid), and the
/// engine truncates them to the buffer at a BYTE boundary, which can cut a
/// multi-byte sequence. Strict decoding threw out of the 1 s device watch.
fn c_chars_to_string(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .map(|&c| u8::from_ne_bytes(c.to_ne_bytes()))
        .take_while(|&b| b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(bytes: &[u8]) -> Vec<c_char> {
        bytes
            .iter()
            .map(|&b| c_char::from_ne_bytes(b.to_ne_bytes()))
            .collect()
    }

    #[test]
    fn the_rate_and_the_input_modes_are_the_engines() {
        assert_eq!(f64::from(ENGINE_RATE), vocara_engine::sample::ENGINE_SR);
        assert_eq!(
            (INPUT_CHANNEL_AUTO, INPUT_CHANNEL_1, INPUT_CHANNEL_2),
            (0, 1, 2)
        );
    }

    #[test]
    fn decode_statuses_and_event_bits_match_the_header() {
        assert_eq!(
            [
                VE_DECODE_OK,
                VE_DECODE_ERR,
                VE_DECODE_TOO_LONG,
                VE_DECODE_TOO_BIG,
                VE_DECODE_RATE_UNSUPPORTED,
                VE_DECODE_IO
            ],
            [0, -1, -2, -3, -4, -5]
        );
        use events::*;
        assert_eq!(
            [
                OUTPUT_LOST,
                INPUT_LOST,
                AEC_BYPASSED,
                INPUT_FELL_BACK,
                RECORD_LIMIT,
                OUTPUT_REROUTED
            ],
            [1, 2, 4, 8, 16, 32]
        );
    }

    #[test]
    fn a_c_string_ends_where_the_c_side_reads_it() {
        assert_eq!(c_string("dev-1").as_bytes(), b"dev-1");
        assert_eq!(c_string("ab\0cd").as_bytes(), b"ab");
        assert_eq!(c_string("").as_bytes(), b"");
        assert_eq!(c_string("\u{e9}").as_bytes(), "\u{e9}".as_bytes());
    }

    #[test]
    fn a_char_buffer_reads_up_to_its_nul_as_lossy_utf8() {
        assert_eq!(c_chars_to_string(&chars(b"BuiltIn\0garbage")), "BuiltIn");
        // No NUL: the whole buffer.
        assert_eq!(c_chars_to_string(&chars(b"abc")), "abc");
        assert_eq!(c_chars_to_string(&chars(b"\0abc")), "");
        // Non-ASCII names decode as UTF-8 ...
        let name = "\u{5c0f}\u{660e}\u{7684} AirPods";
        let mut buf = name.as_bytes().to_vec();
        buf.push(0);
        assert_eq!(c_chars_to_string(&chars(&buf)), name);
        // ... and a sequence the buffer cut at a byte boundary is replaced,
        // never an error.
        let cut = &name.as_bytes()[..4];
        assert_eq!(c_chars_to_string(&chars(cut)), "\u{5c0f}\u{fffd}");
    }

    #[cfg(not(feature = "device"))]
    #[test]
    fn without_the_device_feature_there_is_no_engine() {
        assert!(matches!(create(), Err(EngineError::Unavailable)));
    }
}
