//! The stateless file calls: decode, probe and splice. They touch no engine
//! instance and are safe on any thread, at once (the Dart side ran each in a
//! background isolate).

#[cfg(feature = "device")]
use super::pcm::Layout;
use super::pcm::Pcm;
use super::{
    VE_DECODE_ERR, VE_DECODE_IO, VE_DECODE_OK, VE_DECODE_RATE_UNSUPPORTED, VE_DECODE_TOO_BIG,
    VE_DECODE_TOO_LONG,
};

#[cfg(feature = "device")]
use super::c_string;
#[cfg(feature = "device")]
use vocara_engine::ffi;

/// Why a decode or a splice produced nothing: a `ve_decode` status, typed.
/// The taxonomy matters for diagnosis: every refusal used to surface as "too
/// long", so a short 384 kHz file and a failing disk both read as an
/// over-duration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// No engine to decode with (a build without the `device` feature; the
    /// Dart lookup of the symbol threw).
    #[error("the audio engine is not available")]
    Unavailable,
    /// `VE_DECODE_ERR`: an unreadable or unsupported file, or anything else
    /// the engine names no cause for.
    #[error("the file could not be decoded")]
    Failed,
    /// `VE_DECODE_TOO_LONG`: over the caller's duration budget, refused before
    /// the full track allocated.
    #[error("the audio is over the duration budget")]
    TooLong,
    /// `VE_DECODE_TOO_BIG`: over a memory budget without being over-duration.
    #[error("the audio is over the memory budget")]
    TooBig,
    /// `VE_DECODE_RATE_UNSUPPORTED`: a source rate outside [8 kHz, 384 kHz].
    #[error("the audio's sample rate is not supported")]
    RateUnsupported,
    /// `VE_DECODE_IO`: the file could not be read (the disk, not the format);
    /// the same file can decode on another try.
    #[error("the file could not be read")]
    Io,
}

impl DecodeError {
    /// The refusal a `ve_decode` status (or a negative `ve_punch_wav` result)
    /// names; `None` for [`VE_DECODE_OK`]. A status the ABI does not define
    /// reads as the generic failure.
    pub fn from_status(status: i32) -> Option<DecodeError> {
        match status {
            VE_DECODE_OK => None,
            VE_DECODE_TOO_LONG => Some(DecodeError::TooLong),
            VE_DECODE_TOO_BIG => Some(DecodeError::TooBig),
            VE_DECODE_RATE_UNSUPPORTED => Some(DecodeError::RateUnsupported),
            VE_DECODE_IO => Some(DecodeError::Io),
            _ => Some(DecodeError::Failed),
        }
    }

    /// The status this refusal crosses the ABI as. [`DecodeError::Unavailable`],
    /// which no engine reports, reads as the generic [`VE_DECODE_ERR`].
    pub fn status(self) -> i32 {
        match self {
            DecodeError::Unavailable | DecodeError::Failed => VE_DECODE_ERR,
            DecodeError::TooLong => VE_DECODE_TOO_LONG,
            DecodeError::TooBig => VE_DECODE_TOO_BIG,
            DecodeError::RateUnsupported => VE_DECODE_RATE_UNSUPPORTED,
            DecodeError::Io => VE_DECODE_IO,
        }
    }
}

/// Why `ve_probe_duration_ms` declared no length it could give.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    /// No engine to probe with.
    #[error("the audio engine is not available")]
    Unavailable,
    /// The probe's -2: the file could not be opened or probed at all.
    #[error("the file could not be probed")]
    Unreadable,
}

/// Decodes `path` (wav/mp3/m4a/aac/flac/ogg/caf/aiff, or a video's audio) and
/// resamples it to `rate`: interleaved stereo, mono sources duplicated
/// (`ve_decode`). Stateless.
///
/// `max_frames` (at `rate`; 0 = unbounded) caps the decoded DURATION before
/// the full track allocates: a too-long file is refused up front when its
/// container declares its length, or the moment the budget is crossed
/// mid-decode, never after the allocation. `max_src_frames` (0 = unbounded) is
/// the caller's ABSOLUTE memory budget in SOURCE-rate frames (8 bytes each in
/// the decode buffer): duration and rate ceilings alone still multiply to
/// gigabytes inside both limits; this is the bound the allocation obeys.
pub fn decode(
    path: &str,
    rate: u32,
    max_frames: u64,
    max_src_frames: u64,
) -> Result<Pcm, DecodeError> {
    #[cfg(feature = "device")]
    {
        decode_native(
            path,
            rate,
            max_frames,
            max_src_frames,
            Layout::InterleavedStereo,
        )
    }
    #[cfg(not(feature = "device"))]
    {
        let _ = (path, rate, max_frames, max_src_frames);
        Err(DecodeError::Unavailable)
    }
}

/// [`decode`], but the result is MONO, one sample per frame, the L/R average
/// (`ve_decode_mono`). The analysis path wants mono, and building it from an
/// interleaved stereo buffer kept both alive at once (12 bytes per frame at
/// peak); the engine downmixes inside the stereo allocation instead.
pub fn decode_mono(
    path: &str,
    rate: u32,
    max_frames: u64,
    max_src_frames: u64,
) -> Result<Pcm, DecodeError> {
    #[cfg(feature = "device")]
    {
        decode_native(path, rate, max_frames, max_src_frames, Layout::Mono)
    }
    #[cfg(not(feature = "device"))]
    {
        let _ = (path, rate, max_frames, max_src_frames);
        Err(DecodeError::Unavailable)
    }
}

#[cfg(feature = "device")]
fn decode_native(
    path: &str,
    rate: u32,
    max_frames: u64,
    max_src_frames: u64,
    layout: Layout,
) -> Result<Pcm, DecodeError> {
    let cpath = c_string(path);
    let mut status: std::ffi::c_int = VE_DECODE_ERR;
    let call = match layout {
        Layout::InterleavedStereo => ffi::ve_decode,
        Layout::Mono => ffi::ve_decode_mono,
    };
    // SAFETY: `cpath` is a NUL-terminated string alive for the call, and
    // `status` a live, aligned c_int the engine writes once.
    let v = call(
        cpath.as_ptr(),
        rate,
        max_frames,
        max_src_frames,
        &mut status,
    );
    // Owned before anything else looks at it: whatever came back is freed
    // exactly once, a buffer that arrived with a failing status included.
    let pcm = Pcm::from_native(v, layout);
    match DecodeError::from_status(status) {
        Some(e) => Err(e),
        None => Ok(pcm.unwrap_or_else(|| match layout {
            Layout::InterleavedStereo => Pcm::interleaved_stereo(Vec::new(), rate),
            Layout::Mono => Pcm::mono(Vec::new(), rate),
        })),
    }
}

/// The DECLARED duration of `path` in milliseconds, probed from the container
/// without decoding a packet (`ve_probe_duration_ms`): the import gate's cheap
/// pre-check. `Ok(None)` when the container declares no length (the
/// decode-time budget still bounds the file later). Opens the file, so a
/// named pipe with no writer blocks it: see [`super::media::probe_media_duration`]
/// for the guarded call.
pub fn probe_duration_ms(path: &str) -> Result<Option<u64>, ProbeError> {
    #[cfg(feature = "device")]
    {
        let cpath = c_string(path);
        // SAFETY: `cpath` is a NUL-terminated string alive for the call.
        match ffi::ve_probe_duration_ms(cpath.as_ptr()) {
            ms if ms >= 0 => Ok(Some(ms.unsigned_abs())),
            -1 => Ok(None),
            _ => Err(ProbeError::Unreadable),
        }
    }
    #[cfg(not(feature = "device"))]
    {
        let _ = path;
        Err(ProbeError::Unavailable)
    }
}

/// Punch-in splice (`ve_punch_wav`): writes `old[0..split_ms] ++ new` to
/// `out_path` as a 32-bit float stereo WAV at `rate`. `split_ms` counts from
/// the START of old's audio; past old's end the gap is silence, so new keeps
/// its timeline place. The output is written to a temp file and renamed over
/// `out_path` only once complete, and is never one of the inputs. Returns the
/// output's frame count, or the refusal a decode named ([`DecodeError::Failed`]
/// when none did). Stateless.
pub fn punch_wav(
    old_path: &str,
    new_path: &str,
    split_ms: i64,
    rate: u32,
    out_path: &str,
) -> Result<u64, DecodeError> {
    #[cfg(feature = "device")]
    {
        let (old, new, out) = (c_string(old_path), c_string(new_path), c_string(out_path));
        // SAFETY: the three paths are NUL-terminated strings alive for the call.
        let n = ffi::ve_punch_wav(old.as_ptr(), new.as_ptr(), split_ms, rate, out.as_ptr());
        if n >= 0 {
            Ok(n.unsigned_abs())
        } else {
            let status = i32::try_from(n).unwrap_or(VE_DECODE_ERR);
            Err(DecodeError::from_status(status).unwrap_or(DecodeError::Failed))
        }
    }
    #[cfg(not(feature = "device"))]
    {
        let _ = (old_path, new_path, split_ms, rate, out_path);
        Err(DecodeError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_their_refusals_and_back() {
        assert_eq!(DecodeError::from_status(VE_DECODE_OK), None);
        for (status, e) in [
            (VE_DECODE_ERR, DecodeError::Failed),
            (VE_DECODE_TOO_LONG, DecodeError::TooLong),
            (VE_DECODE_TOO_BIG, DecodeError::TooBig),
            (VE_DECODE_RATE_UNSUPPORTED, DecodeError::RateUnsupported),
            (VE_DECODE_IO, DecodeError::Io),
        ] {
            assert_eq!(DecodeError::from_status(status), Some(e));
            assert_eq!(e.status(), status);
        }
        // A status the ABI does not name is the generic failure, never OK.
        assert_eq!(DecodeError::from_status(-9), Some(DecodeError::Failed));
        assert_eq!(DecodeError::from_status(7), Some(DecodeError::Failed));
        assert_eq!(DecodeError::Unavailable.status(), VE_DECODE_ERR);
    }

    #[cfg(not(feature = "device"))]
    #[test]
    fn without_the_device_feature_nothing_decodes_probes_or_splices() {
        assert_eq!(
            decode("/x.wav", 48_000, 0, 0).unwrap_err(),
            DecodeError::Unavailable
        );
        assert_eq!(
            decode_mono("/x.wav", 44_100, 0, 0).unwrap_err(),
            DecodeError::Unavailable
        );
        assert_eq!(
            probe_duration_ms("/x.wav").unwrap_err(),
            ProbeError::Unavailable
        );
        assert_eq!(
            punch_wav("/a.wav", "/b.wav", 0, 48_000, "/c.wav").unwrap_err(),
            DecodeError::Unavailable
        );
    }
}
