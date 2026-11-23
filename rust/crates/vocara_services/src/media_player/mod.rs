//! The technique examples' inline media player: the seam the example players
//! (`technique_audio_player`, `technique_video_player`) drive, in place of the
//! media_kit `Player` (libmpv) the Dart widgets were built on.
//!
//! What the Dart widgets used of media_kit, and where it went:
//!
//! - `Player()` (through the `createExamplePlayer` seam) →
//!   [`MediaPlayerFactory::create`], which also says whether the clip is
//!   audio or video (a video player renders frames, as media_kit's
//!   `VideoController` attached to the player did);
//! - `player.open(Media(path), play: ...)` → [`MediaPlayer::open`];
//! - `play` / `pause` / `playOrPause` / `seek` → the methods of the same
//!   names;
//! - `stream.playing` / `stream.position` / `stream.duration` (and
//!   `completed`, `error`) → [`MediaPlayer::state`], a snapshot the UI POLLS
//!   on an iced subscription tick while a clip plays, instead of streams it
//!   listens to;
//! - the `Video` widget's texture → [`MediaPlayer::frame`], the latest frame
//!   as RGBA with a revision, so the UI uploads a frame only when it changed;
//! - `dispose()` → `Drop`.
//!
//! # Backends
//!
//! - **AVFoundation** (`avfoundation.rs`, macOS with the `device` feature):
//!   an `AVPlayer` over an `AVPlayerItem` for the file, plus an
//!   `AVPlayerItemVideoOutput` for video. AVFoundation ships with macOS, so
//!   the app bundles no media framework (no libmpv, no GStreamer).
//! - [`NullMediaPlayer`] (every other build): opening fails with
//!   [`MediaPlayerError::Unavailable`], which the example players show as the
//!   unavailable clip, exactly as Dart showed a clip whose file is not on this
//!   device.
//! - [`FakeMediaPlayers`] (tests): records what each player was asked to do,
//!   and whether it was dropped.
//!
//! # Threading and ownership
//!
//! A player is **main-thread-only** and not `Send`: `AVPlayer` is a
//! main-thread class, and creating one needs a `MainThreadMarker`. The UI
//! owns its players in its screen state and calls them from `update` (and
//! reads them in `view`), which iced runs on the main thread on macOS; a
//! player never goes into a `Task`, and the compiler holds to that (a
//! `Box<dyn MediaPlayer>` is not `Send`). [`MediaPlayerFactory::create`] off
//! the main thread fails with [`MediaPlayerError::NotMainThread`]. Dropping a
//! player (the page leaving, an example removed) stops and releases it, on
//! the thread that owns it.

#[cfg(all(feature = "device", target_os = "macos"))]
mod avfoundation;
mod fake;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub use fake::{FakeMediaPlayer, FakeMediaPlayers, FakePlayerHandle};

/// What a player plays: audio only, or video (frames as well as sound).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Audio,
    Video,
}

/// Why a player could not be made or could not open its clip.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaPlayerError {
    /// This build has no media playback (no `device` feature, or not macOS).
    #[error("media playback is not available in this build")]
    Unavailable,
    /// Players are main-thread-only and this is not the main thread.
    #[error("media players can only be created on the main thread")]
    NotMainThread,
    /// The clip could not be opened.
    #[error("cannot open {path}: {reason}")]
    Open { path: String, reason: String },
}

pub type Result<T> = std::result::Result<T, MediaPlayerError>;

/// A player's state when polled: what media_kit's `stream.playing`,
/// `position`, `duration`, `completed` and `error` last reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerState {
    /// Playing, or asked to play and waiting for data (media_kit reported
    /// `playing: true` from the moment `play` was called).
    pub playing: bool,
    /// Where playback is, never past [`PlayerState::duration`] once that is
    /// known.
    pub position: Duration,
    /// The clip's length; zero until known (media_kit's duration stream
    /// started at zero too).
    pub duration: Duration,
    /// Playback reached the end and stopped there.
    pub completed: bool,
    /// The clip is still opening (its duration is not known yet).
    pub loading: bool,
    /// The clip could not be played; the reason.
    pub error: Option<String>,
}

/// One decoded video frame.
#[derive(Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8 rows, top to bottom: `width * height * 4` bytes.
    /// Shared, so handing the frame to the UI copies no pixels.
    pub rgba: Arc<Vec<u8>>,
    /// Bumped each time a new frame replaces the previous one (the first
    /// frame is 1): the UI re-uploads only when it changed.
    pub revision: u64,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// An inline media player (see the module docs). Main-thread-only; dropping
/// it stops and releases it (media_kit's `dispose`).
pub trait MediaPlayer {
    /// Opens the clip at `path` (an absolute file path) at 0:00: playing when
    /// `play`, else paused, so its duration loads before the first play.
    fn open(&mut self, path: &str, play: bool) -> Result<()>;

    /// Starts (or resumes) playback; once the clip has completed, from the
    /// start again.
    fn play(&mut self);

    /// Pauses where it is.
    fn pause(&mut self);

    /// Pauses when playing, plays otherwise (media_kit's `playOrPause`).
    fn play_or_pause(&mut self) {
        if self.state().playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Moves playback to `to` (clamped to the clip by the player).
    fn seek(&mut self, to: Duration);

    /// The player's state now.
    fn state(&self) -> PlayerState;

    /// Video players: fetches the frame due now when there is a new one, and
    /// returns the latest frame (`None` until the first). Audio players:
    /// `None`.
    fn frame(&mut self) -> Option<VideoFrame> {
        None
    }
}

/// Builds the example players: Dart's `createExamplePlayer` seam, which
/// production never reassigned and the widget tests swapped for a fake.
/// The factory itself is `Send + Sync` (the app shares one); the players it
/// makes are main-thread-only.
pub trait MediaPlayerFactory: Send + Sync {
    /// A new, unopened player for a clip of `kind`. Call it on the main
    /// thread.
    fn create(&self, kind: MediaKind) -> Result<Box<dyn MediaPlayer>>;
}

/// The players this build has: AVFoundation on macOS with the `device`
/// feature, the [`NullMediaPlayer`] everywhere else.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemMediaPlayers;

impl MediaPlayerFactory for SystemMediaPlayers {
    fn create(&self, kind: MediaKind) -> Result<Box<dyn MediaPlayer>> {
        #[cfg(all(feature = "device", target_os = "macos"))]
        {
            Ok(Box::new(avfoundation::AvMediaPlayer::new(kind)?))
        }
        #[cfg(not(all(feature = "device", target_os = "macos")))]
        {
            let _ = kind;
            Ok(Box::new(NullMediaPlayer))
        }
    }
}

/// The player of a build without media playback: every clip is
/// unavailable.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullMediaPlayer;

impl MediaPlayer for NullMediaPlayer {
    fn open(&mut self, _path: &str, _play: bool) -> Result<()> {
        Err(MediaPlayerError::Unavailable)
    }

    fn play(&mut self) {}

    fn pause(&mut self) {}

    fn seek(&mut self, _to: Duration) {}

    fn state(&self) -> PlayerState {
        PlayerState {
            error: Some(MediaPlayerError::Unavailable.to_string()),
            ..PlayerState::default()
        }
    }
}

// ------------------------------------------------- the backend's pure parts
//
// What the AVFoundation backend computes from the native values, kept free
// of Objective-C types so it is unit-tested on every platform.

/// `kCMTimeFlags_Valid`.
pub const CM_TIME_FLAG_VALID: u32 = 1 << 0;
/// `kCMTimeFlags_PositiveInfinity`.
pub const CM_TIME_FLAG_POSITIVE_INFINITY: u32 = 1 << 2;
/// `kCMTimeFlags_NegativeInfinity`.
pub const CM_TIME_FLAG_NEGATIVE_INFINITY: u32 = 1 << 3;
/// `kCMTimeFlags_Indefinite` (an `AVPlayerItem`'s duration until it loads).
pub const CM_TIME_FLAG_INDEFINITE: u32 = 1 << 4;

/// The timescale [`duration_to_cm_time`] writes: nanoseconds, so a seek
/// loses nothing of the `Duration` it was given.
pub const CM_TIME_NANOSECOND_TIMESCALE: i32 = 1_000_000_000;

/// A `CMTime` (`value / timescale` seconds, with `flags`) as a [`Duration`]:
/// `None` when it is not a number of seconds — invalid, indefinite (a
/// duration still loading), infinite, or with a timescale that is not
/// positive (a zero timescale divides by zero). A time before zero reads as
/// zero. Exact to the nanosecond (rounded down), with no overflow for any
/// value.
pub fn cm_time_to_duration(value: i64, timescale: i32, flags: u32) -> Option<Duration> {
    let not_numeric =
        CM_TIME_FLAG_POSITIVE_INFINITY | CM_TIME_FLAG_NEGATIVE_INFINITY | CM_TIME_FLAG_INDEFINITE;
    if flags & CM_TIME_FLAG_VALID == 0 || flags & not_numeric != 0 || timescale <= 0 {
        return None;
    }
    if value <= 0 {
        return Some(Duration::ZERO);
    }
    let (value, timescale) = (value.unsigned_abs(), u64::from(timescale.unsigned_abs()));
    let secs = value / timescale;
    // rem < timescale <= i32::MAX, so rem * 1e9 fits in a u64.
    let nanos = (value % timescale) * 1_000_000_000 / timescale;
    Some(Duration::new(secs, nanos as u32))
}

/// A [`Duration`] as a `CMTime`'s `(value, timescale)` at
/// [`CM_TIME_NANOSECOND_TIMESCALE`] (saturating past ~292 years).
pub fn duration_to_cm_time(d: Duration) -> (i64, i32) {
    let nanos = i64::try_from(d.as_nanos()).unwrap_or(i64::MAX);
    (nanos, CM_TIME_NANOSECOND_TIMESCALE)
}

/// Converts a locked `kCVPixelFormatType_32BGRA` pixel buffer — `height`
/// rows of `width` pixels, each row starting `bytes_per_row` bytes after the
/// previous (CoreVideo pads rows for alignment, so it may be more than
/// `width * 4`) — into tightly packed RGBA. Alpha is set opaque: a video
/// frame has no transparency, and the fourth byte of a decoded frame is not
/// guaranteed to be 0xFF.
///
/// `None` when the geometry does not fit the bytes (rows narrower than the
/// width, a buffer shorter than its last row, an overflow) or is empty.
pub fn bgra_to_rgba(
    src: &[u8],
    width: usize,
    height: usize,
    bytes_per_row: usize,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row = width.checked_mul(4)?;
    if bytes_per_row < row {
        return None;
    }
    // The last row need not carry its padding.
    let needed = bytes_per_row.checked_mul(height - 1)?.checked_add(row)?;
    if src.len() < needed {
        return None;
    }
    let mut out = Vec::with_capacity(row.checked_mul(height)?);
    for y in 0..height {
        let start = y * bytes_per_row;
        for px in src[start..start + row].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
        }
    }
    Some(out)
}

/// `AVPlayerItem.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeItemStatus {
    /// Still loading (`AVPlayerItemStatusUnknown`).
    Unknown,
    ReadyToPlay,
    Failed,
}

/// `AVPlayer.timeControlStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTimeControl {
    Paused,
    /// Asked to play, waiting for data (`...WaitingToPlayAtSpecifiedRate`).
    Waiting,
    Playing,
}

/// The native values a poll reads, before they become a [`PlayerState`].
#[derive(Debug, Clone, PartialEq)]
pub struct NativeState {
    pub item_status: NativeItemStatus,
    /// `AVPlayer.status == AVPlayerStatusFailed`.
    pub player_failed: bool,
    /// The item's (else the player's) error description.
    pub error: Option<String>,
    pub time_control: NativeTimeControl,
    /// `AVPlayer.rate`: non-zero from `play` until a pause or the end.
    pub rate: f32,
    /// `AVPlayer.currentTime` (see [`cm_time_to_duration`]).
    pub current_time: Option<Duration>,
    /// `AVPlayerItem.duration`: `None` while indefinite.
    pub duration: Option<Duration>,
}

/// How close to its end a stopped clip counts as completed: a scrub to the
/// end seeks to whole milliseconds (`inMilliseconds`, truncated), up to one
/// short of the true duration.
pub const END_TOLERANCE: Duration = Duration::from_millis(1);

/// Maps a poll's native values onto media_kit's state.
///
/// - `playing`: asked to play (a non-zero rate, or a time control that is
///   not paused) and not failed — media_kit said playing from `play` on, not
///   only once sound came out.
/// - `duration`: zero until known; `position` never past it.
/// - `completed`: stopped at the end (an `AVPlayer` stops there with its
///   rate back at zero).
/// - `loading`: the item has neither loaded nor failed yet.
/// - `error`: the item or the player failed.
pub fn map_state(native: &NativeState) -> PlayerState {
    let failed = native.item_status == NativeItemStatus::Failed || native.player_failed;
    let error = failed.then(|| {
        native
            .error
            .clone()
            .unwrap_or_else(|| "playback failed".to_string())
    });
    let playing =
        !failed && (native.rate != 0.0 || native.time_control != NativeTimeControl::Paused);
    let duration = native.duration.unwrap_or(Duration::ZERO);
    let mut position = native.current_time.unwrap_or(Duration::ZERO);
    if !duration.is_zero() && position > duration {
        position = duration;
    }
    let completed =
        !failed && !playing && !duration.is_zero() && position + END_TOLERANCE >= duration;
    PlayerState {
        playing,
        position,
        duration,
        completed,
        loading: !failed && native.item_status == NativeItemStatus::Unknown,
        error,
    }
}

#[cfg(test)]
mod tests;
