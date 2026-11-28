//! The decode policy the Dart services layered on the engine's decode calls:
//! the duration and memory caps, the typed refusals with their user-facing
//! messages, and the playback, analysis and probe helpers (the top level of
//! `graph_audio_playback_service.dart`, which `audio_edit_service.dart` and
//! `media_analysis_service.dart` imported from there).

use std::fs;
use std::time::Duration;

use super::{decode, decode_mono, probe_duration_ms, DecodeError, Pcm, ProbeError};
use super::{VE_DECODE_IO, VE_DECODE_RATE_UNSUPPORTED, VE_DECODE_TOO_BIG, VE_DECODE_TOO_LONG};

/// Hard duration cap for the PLAYBACK and import decode path. Committed tracks
/// live in memory as full-length engine-rate stereo f32 (~1.4 GB an hour, with
/// a ~2x transient while decoding), so an arbitrary-length import is an OOM,
/// not a feature. The cap is enforced natively BEFORE the track allocates (the
/// decode's `max_frames`), and is deliberately far above any song-practice use
/// so only runaway inputs (a DJ set, a podcast rip) hit it. The engine's splice
/// mirrors it (`MAX_PLAYBACK_SECS`): the two must move together.
pub const MAX_PLAYBACK_DURATION: Duration = Duration::from_secs(60 * 60);

/// The ABSOLUTE source-frame memory budget for playback staging: the bound the
/// native allocation actually obeys (duration and rate ceilings alone still
/// multiply to gigabytes inside both limits; one stereo frame is 8 bytes in the
/// decode buffer). 60 minutes at a 96 kHz source, about 346 M frames or 2.8 GB:
/// whole-track residency is the playback design's documented cost, and this
/// bounds what a single import can demand of it (a 60-minute 192 kHz file would
/// have doubled it). The engine's splice mirrors it (`MAX_SPLICE_SRC_FRAMES`).
pub const MAX_PLAYBACK_SRC_FRAMES: u64 = 346_000_000;

/// The analysis decode's absolute source-frame budget: about 33 M frames, or
/// 264 MB, a 10-minute take at up to ~55 kHz source, shorter at higher rates.
/// The analysis targets vocal takes recorded at 48 kHz, so this refuses only
/// exotic hi-rate media.
pub const MAX_ANALYSIS_SRC_FRAMES: u64 = 33_000_000;

/// The rate the analysis decode resamples to unless asked otherwise (Dart's
/// `decodeMediaToMono` default).
pub const ANALYSIS_DECODE_RATE: u32 = 44_100;

/// A decode refusal the user is told about: the Dart `MediaTooLongException`,
/// `MediaUnsupportedException` and `MediaReadException`. The messages are
/// user-presentable (and, by the UI copy rule, never join sentences with a
/// dash).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// Longer than the decode path's duration cap ([`MAX_PLAYBACK_DURATION`]
    /// for playback and import, the caller's cap for analysis).
    #[error("This file is too long. The limit is {} minutes.", .max_duration.as_secs() / 60)]
    TooLong {
        /// The cap the media exceeded.
        max_duration: Duration,
    },
    /// Refused by a MEMORY budget or the supported-rate envelope rather than
    /// the duration cap: a short but extremely dense file (e.g. 384 kHz),
    /// which "too long" would misdiagnose.
    #[error("{0}")]
    Unsupported(String),
    /// The file could not be read (the disk, a file being replaced or synced
    /// in): unlike the other refusals, the same file can decode on another
    /// try. A kind of [`MediaError::Unsupported`] in Dart (a subclass); see
    /// [`MediaError::is_unsupported`].
    #[error("The file could not be read. Check the disk and try again.")]
    Read,
    /// No engine to decode with: the Dart path where the native library could
    /// not be loaded and the call threw something that was none of the above.
    #[error("The audio engine is not available.")]
    EngineUnavailable,
}

/// The too-dense refusal's message.
const TOO_DENSE: &str =
    "This file is too dense to process. Sample rates this high are not supported.";

/// The unsupported-rate refusal's message.
const UNSUPPORTED_RATE: &str = "This file uses an unsupported sample rate.";

impl MediaError {
    /// Whether this is a Dart `MediaUnsupportedException`, which
    /// `MediaReadException` extends: what an `on MediaUnsupportedException`
    /// clause caught.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, MediaError::Unsupported(_) | MediaError::Read)
    }

    /// The refusal a typed decode failure surfaces as, against
    /// `duration_cap`: `None` for the generic failure, which callers treat as
    /// "not decodable" without a special message.
    pub fn from_decode(e: DecodeError, duration_cap: Duration) -> Option<MediaError> {
        match e {
            DecodeError::Unavailable => Some(MediaError::EngineUnavailable),
            e => decode_status_to_error(e.status(), duration_cap),
        }
    }
}

/// Maps a refusing `ve_decode` status (or a negative `ve_punch_wav` result) to
/// its typed error; `None` for OK and for the generic failure, which callers
/// treat as "not decodable" without a special message.
pub fn decode_status_to_error(status: i32, duration_cap: Duration) -> Option<MediaError> {
    match status {
        VE_DECODE_TOO_LONG => Some(MediaError::TooLong {
            max_duration: duration_cap,
        }),
        VE_DECODE_TOO_BIG => Some(MediaError::Unsupported(TOO_DENSE.into())),
        VE_DECODE_RATE_UNSUPPORTED => Some(MediaError::Unsupported(UNSUPPORTED_RATE.into())),
        VE_DECODE_IO => Some(MediaError::Read),
        _ => None,
    }
}

/// The frame count `cap` lasts at `rate` (Dart's
/// `cap.inMilliseconds * rate ~/ 1000`): the decode's `max_frames`.
pub fn cap_frames(cap: Duration, rate: u32) -> u64 {
    u64::try_from(cap.as_millis() * u128::from(rate) / 1000).unwrap_or(u64::MAX)
}

/// Whether `frames` mono frames at `rate` Hz exceed the `max_duration` cap.
pub fn exceeds_decode_cap(frames: usize, rate: u32, max_duration: Duration) -> bool {
    frames as u128 > u128::from(cap_frames(max_duration, rate))
}

/// The DECLARED duration of a media file, probed from the container without
/// decoding a packet: the import gate's cheap pre-check. `Ok(None)` when the
/// path is not a plain file, the container declares no length (the decode-time
/// budget still bounds it later), or it cannot be probed (the import fails on
/// its own terms). Blocking: it opens and reads the file, which waits on an
/// iCloud placeholder until it downloads, or on a slow volume; run it off the
/// UI thread (the import runs it on its worker).
///
/// `Err` only when there is no engine to probe with: the Dart probe threw
/// then, and its one caller admitted the import.
pub fn probe_media_duration(path: &str) -> Result<Option<Duration>, MediaError> {
    // A plain file only: opening a named pipe with no writer blocks forever
    // (dropped as audio, one froze the app when this ran on the UI thread).
    // The import itself refuses anything else (see
    // `AudioFileService::import_audio`).
    if !fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    match probe_duration_ms(path) {
        Ok(ms) => Ok(ms.map(Duration::from_millis)),
        Err(ProbeError::Unreadable) => Ok(None),
        Err(ProbeError::Unavailable) => Err(MediaError::EngineUnavailable),
    }
}

/// Decodes `path` for playback staging at `rate` (the engine's): interleaved
/// stereo within [`MAX_PLAYBACK_DURATION`] and [`MAX_PLAYBACK_SRC_FRAMES`],
/// refused natively before the full track allocates. `Ok(None)` where the Dart
/// decode handed back no buffer or an empty one (a generic failure), which the
/// stager skipped alike. Stateless; meant for a worker thread.
pub fn decode_for_playback(path: &str, rate: u32) -> Result<Option<Pcm>, MediaError> {
    let max_frames = cap_frames(MAX_PLAYBACK_DURATION, rate);
    nonempty(
        decode(path, rate, max_frames, MAX_PLAYBACK_SRC_FRAMES),
        MAX_PLAYBACK_DURATION,
    )
}

/// Decodes any engine-supported media file (wav/mp3/m4a/aac/video/...) to MONO
/// samples at `rate` for offline analysis. `Ok(None)` when the file cannot be
/// decoded or is empty. Stateless; meant for a worker thread.
///
/// `max_duration` bounds the decoded length for ANALYSIS callers: the whole
/// file materializes in memory (an hour of mono at 44.1 kHz is ~635 MB), so an
/// arbitrary-length input (a technique reference can be any media) would blow
/// memory. The cap is enforced NATIVELY, before the full-track buffer
/// allocates (a declared-too-long container is refused up front, an undeclared
/// one bails mid-decode), and exceeding it is [`MediaError::TooLong`]. `None`
/// falls back to [`MAX_PLAYBACK_DURATION`]: nothing decodes unbounded. The
/// memory budget is [`MAX_ANALYSIS_SRC_FRAMES`].
pub fn decode_media_to_mono(
    path: &str,
    rate: u32,
    max_duration: Option<Duration>,
) -> Result<Option<Pcm>, MediaError> {
    decode_mono_capped(
        path,
        rate,
        max_duration.unwrap_or(MAX_PLAYBACK_DURATION),
        MAX_ANALYSIS_SRC_FRAMES,
    )
}

/// [`decode_media_to_mono`] against an explicit duration cap and source-frame
/// budget (the waveform decodes imports within the playback budgets).
pub fn decode_mono_capped(
    path: &str,
    rate: u32,
    cap: Duration,
    max_src_frames: u64,
) -> Result<Option<Pcm>, MediaError> {
    // The MONO entry point: the downmix happens inside the native buffer, so an
    // interleaved stereo track and its mono reduction are never alive at the
    // same time (which cost 12 bytes per frame at peak, on top of everything
    // the analysis itself holds).
    let pcm = nonempty(
        decode_mono(path, rate, cap_frames(cap, rate), max_src_frames),
        cap,
    )?;
    if let Some(pcm) = &pcm {
        // Belt-and-braces re-check against the ACTUAL decode rate the engine
        // reports, so the cap stays a true duration bound even if the native
        // budget (computed at the REQUESTED rate) and the file's rate disagree.
        if exceeds_decode_cap(pcm.frames(), pcm.rate(), cap) {
            return Err(MediaError::TooLong { max_duration: cap });
        }
    }
    Ok(pcm)
}

/// A decode's outcome as the callers take it: a refusal typed against `cap`,
/// a generic failure or an empty buffer as `None`.
fn nonempty(decoded: Result<Pcm, DecodeError>, cap: Duration) -> Result<Option<Pcm>, MediaError> {
    match decoded {
        Ok(pcm) if !pcm.is_empty() => Ok(Some(pcm)),
        Ok(_) => Ok(None),
        Err(e) => MediaError::from_decode(e, cap).map_or(Ok(None), Err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::VE_DECODE_ERR;

    /// The analysis service's cap (`MediaAnalysisService.maxAnalysisDuration`).
    const CAP: Duration = Duration::from_secs(10 * 60);

    #[test]
    fn the_cap_trips_just_past_it_never_at_it() {
        let cap_frames = 10 * 60 * 44_100;
        assert!(!exceeds_decode_cap(cap_frames, 44_100, CAP));
        assert!(exceeds_decode_cap(cap_frames + 1, 44_100, CAP));
        assert!(!exceeds_decode_cap(0, 44_100, CAP));
    }

    #[test]
    fn the_cap_is_a_duration_bound_at_the_actual_decode_rate() {
        // 11 minutes of 22.05 kHz audio has fewer frames than 10 minutes of
        // 44.1 kHz audio, but must still trip the cap at its own rate.
        let frames = 11 * 60 * 22_050;
        assert!(exceeds_decode_cap(frames, 22_050, CAP));
        assert!(!exceeds_decode_cap(frames, 44_100, CAP));
    }

    #[test]
    fn too_long_carries_a_user_presentable_message() {
        let e = MediaError::TooLong { max_duration: CAP };
        assert!(e.to_string().contains("10 minutes"));
        // UI copy rule: sentences are never joined with dashes.
        assert!(!e.to_string().contains(" - "));
        assert!(!e.to_string().contains('\u{2014}'));
        assert_eq!(
            MediaError::TooLong {
                max_duration: MAX_PLAYBACK_DURATION
            }
            .to_string(),
            "This file is too long. The limit is 60 minutes."
        );
    }

    #[test]
    fn refusal_statuses_map_to_distinct_user_presentable_diagnoses() {
        // Every refusal used to collapse into "too long": a short 384 kHz file
        // and a failing disk both read as an over-duration file.
        assert_eq!(
            decode_status_to_error(VE_DECODE_TOO_LONG, CAP),
            Some(MediaError::TooLong { max_duration: CAP })
        );
        let too_big = decode_status_to_error(VE_DECODE_TOO_BIG, CAP).unwrap();
        assert!(too_big.is_unsupported());
        assert!(!too_big.to_string().contains("too long"));
        let rate = decode_status_to_error(VE_DECODE_RATE_UNSUPPORTED, CAP).unwrap();
        assert!(rate.is_unsupported());
        assert!(rate.to_string().contains("sample rate"));
        let io = decode_status_to_error(VE_DECODE_IO, CAP).unwrap();
        assert!(io.is_unsupported());
        assert!(io.to_string().contains("read"));
        assert_eq!(io, MediaError::Read);
        assert_eq!(decode_status_to_error(0, CAP), None);
        assert_eq!(decode_status_to_error(VE_DECODE_ERR, CAP), None);
        // UI copy rule: sentences are never joined with dashes.
        for e in [too_big, rate, io] {
            assert!(!e.to_string().contains(" - "));
            assert!(!e.to_string().contains('\u{2014}'));
        }
        assert!(!MediaError::TooLong { max_duration: CAP }.is_unsupported());
    }

    #[test]
    fn a_typed_decode_failure_surfaces_through_the_same_map() {
        assert_eq!(
            MediaError::from_decode(DecodeError::TooLong, CAP),
            Some(MediaError::TooLong { max_duration: CAP })
        );
        assert_eq!(
            MediaError::from_decode(DecodeError::Io, CAP),
            Some(MediaError::Read)
        );
        assert_eq!(MediaError::from_decode(DecodeError::Failed, CAP), None);
        assert_eq!(
            MediaError::from_decode(DecodeError::Unavailable, CAP),
            Some(MediaError::EngineUnavailable)
        );
    }

    #[test]
    fn cap_frames_is_the_millisecond_budget_at_the_rate() {
        assert_eq!(cap_frames(MAX_PLAYBACK_DURATION, 48_000), 172_800_000);
        assert_eq!(cap_frames(CAP, 44_100), 26_460_000);
        // Whole milliseconds, as Dart's `inMilliseconds` counted.
        assert_eq!(cap_frames(Duration::from_micros(1_999), 1_000), 1);
    }

    #[test]
    fn outcomes_reach_callers_as_dart_handed_them_back() {
        let pcm = Pcm::mono(vec![0.1; 10], 1_000);
        assert_eq!(nonempty(Ok(pcm), CAP).unwrap().unwrap().frames(), 10);
        assert!(nonempty(Ok(Pcm::mono(Vec::new(), 1_000)), CAP)
            .unwrap()
            .is_none());
        assert!(nonempty(Err(DecodeError::Failed), CAP).unwrap().is_none());
        assert_eq!(
            nonempty(Err(DecodeError::TooBig), CAP).unwrap_err(),
            MediaError::Unsupported(TOO_DENSE.into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_never_opens_a_pipe_or_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("song.mp3");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: `c` is a NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // Opening a pipe with no writer would block this test forever.
        assert_eq!(probe_media_duration(&fifo.to_string_lossy()), Ok(None));
        assert_eq!(
            probe_media_duration(&tmp.path().to_string_lossy()),
            Ok(None)
        );
        let missing = tmp.path().join("gone.wav");
        assert_eq!(probe_media_duration(&missing.to_string_lossy()), Ok(None));
    }

    #[cfg(not(feature = "device"))]
    #[test]
    fn without_the_device_feature_the_decodes_report_no_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("take.wav");
        fs::write(&file, b"RIFF").unwrap();
        let path = file.to_string_lossy();
        assert_eq!(
            probe_media_duration(&path),
            Err(MediaError::EngineUnavailable)
        );
        assert_eq!(
            decode_for_playback(&path, 48_000).unwrap_err(),
            MediaError::EngineUnavailable
        );
        assert_eq!(
            decode_media_to_mono(&path, ANALYSIS_DECODE_RATE, Some(CAP)).unwrap_err(),
            MediaError::EngineUnavailable
        );
    }
}
