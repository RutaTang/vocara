//! The AVFoundation backend (macOS, `device` feature): an `AVPlayer` playing
//! one `AVPlayerItem` for the clip's file URL, and for video an
//! `AVPlayerItemVideoOutput` that hands out 32BGRA pixel buffers, converted
//! to RGBA for the UI. Everything is polled: no KVO, no notifications, no
//! blocks, so nothing calls back into Rust behind the UI's back.
//!
//! Main-thread-only: `AVPlayer` and `AVPlayerItem` are main-thread classes
//! (objc2 makes them `!Send` and asks for a `MainThreadMarker` to create
//! them), so [`AvMediaPlayer`] is `!Send` too, and is created, used and
//! dropped on the main thread — where iced runs `update` and `view`.
//!
//! Ownership: every object is a `Retained` (a +1 reference objc2 releases on
//! drop). `copyPixelBufferForItemTime:` is in the copy family, so objc2
//! takes the buffer it returns as already retained and releases it once:
//! no `CVBufferRelease` by hand. Each call that may produce autoreleased
//! temporaries (an error's description, a new item) runs in its own
//! autorelease pool, so polling at frame rate accumulates nothing whatever
//! pool the caller's run loop has.

use std::ptr;
use std::slice;
use std::sync::Arc;
use std::time::Duration;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVPlayer, AVPlayerItem, AVPlayerItemStatus, AVPlayerItemVideoOutput, AVPlayerStatus,
    AVPlayerTimeControlStatus,
};
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, kCVReturnSuccess,
    CVGetCurrentHostTime, CVGetHostClockFrequency, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetDataSize, CVPixelBufferGetExtendedPixels,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferIsPlanar, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::{
    bgra_to_rgba, cm_time_to_duration, duration_to_cm_time, map_state, MediaKind, MediaPlayer,
    MediaPlayerError, NativeItemStatus, NativeState, NativeTimeControl, PlayerState, Result,
    VideoFrame,
};

/// The clip that is open: its item, and the item's video output (video
/// players only).
struct Opened {
    item: Retained<AVPlayerItem>,
    output: Option<Retained<AVPlayerItemVideoOutput>>,
}

/// An `AVPlayer`-backed [`MediaPlayer`] (see the module docs).
pub(super) struct AvMediaPlayer {
    kind: MediaKind,
    mtm: MainThreadMarker,
    player: Retained<AVPlayer>,
    opened: Option<Opened>,
    frame: Option<VideoFrame>,
}

impl AvMediaPlayer {
    /// A player with nothing open. Fails off the main thread.
    pub(super) fn new(kind: MediaKind) -> Result<AvMediaPlayer> {
        let mtm = MainThreadMarker::new().ok_or(MediaPlayerError::NotMainThread)?;
        // SAFETY: `+[AVPlayer new]` has no preconditions; the marker proves
        // this is the main thread, which AVPlayer requires.
        let player = unsafe { AVPlayer::new(mtm) };
        Ok(AvMediaPlayer {
            kind,
            mtm,
            player,
            opened: None,
            frame: None,
        })
    }

    /// The video output's attributes: 32BGRA pixel buffers.
    fn pixel_buffer_attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
        // SAFETY: `kCVPixelBufferPixelFormatTypeKey` is an immutable CFString
        // constant CoreVideo exports for the process's lifetime; reading the
        // extern static has no other precondition.
        let key: &NSString = unsafe { kCVPixelBufferPixelFormatTypeKey }.as_ref();
        let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
        let value: &AnyObject = &format;
        NSDictionary::from_slices(&[key], &[value])
    }

    /// Reads the native state this poll maps.
    fn native(&self, item: &AVPlayerItem) -> NativeState {
        // SAFETY: plain property reads on live objects we hold a reference
        // to, on the main thread (the type is `!Send`, created there).
        unsafe {
            let item_status = match item.status() {
                AVPlayerItemStatus::ReadyToPlay => NativeItemStatus::ReadyToPlay,
                AVPlayerItemStatus::Failed => NativeItemStatus::Failed,
                _ => NativeItemStatus::Unknown,
            };
            let player_failed = self.player.status() == AVPlayerStatus::Failed;
            let error = item
                .error()
                .or_else(|| self.player.error())
                .map(|e| e.localizedDescription().to_string());
            let time_control = match self.player.timeControlStatus() {
                AVPlayerTimeControlStatus::Playing => NativeTimeControl::Playing,
                AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate => {
                    NativeTimeControl::Waiting
                }
                _ => NativeTimeControl::Paused,
            };
            NativeState {
                item_status,
                player_failed,
                error,
                time_control,
                rate: self.player.rate(),
                current_time: duration_of(self.player.currentTime()),
                duration: duration_of(item.duration()),
            }
        }
    }
}

/// A `CMTime` as a [`Duration`] (`None` while indefinite, invalid, infinite,
/// or with a timescale that is not positive).
fn duration_of(t: CMTime) -> Option<Duration> {
    cm_time_to_duration(t.value, t.timescale, t.flags.0)
}

/// A `CMTime` for a [`Duration`].
fn cm_time(d: Duration) -> CMTime {
    let (value, timescale) = duration_to_cm_time(d);
    CMTime {
        value,
        timescale,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

/// `kCMTimeZero`, built rather than read from the extern static.
const CM_TIME_ZERO: CMTime = CMTime {
    value: 0,
    timescale: 1,
    flags: CMTimeFlags::Valid,
    epoch: 0,
};

/// Unlocks a pixel buffer's base address when dropped.
struct BaseAddressLock<'a>(&'a CVPixelBuffer);

impl Drop for BaseAddressLock<'_> {
    fn drop(&mut self) {
        // SAFETY: paired with the successful read-only lock in
        // `read_frame`, with the same flags, on a buffer still alive (we
        // borrow it).
        unsafe {
            CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly);
        }
    }
}

/// Copies a 32BGRA pixel buffer out as an RGBA frame: `(width, height,
/// rgba)`. `None` for any buffer that is not what was asked for.
fn read_frame(buffer: &CVPixelBuffer) -> Option<(u32, u32, Vec<u8>)> {
    if CVPixelBufferIsPlanar(buffer)
        || CVPixelBufferGetPixelFormatType(buffer) != kCVPixelFormatType_32BGRA
    {
        return None;
    }
    // SAFETY: `buffer` is a live pixel buffer we hold a reference to; a
    // read-only lock makes its memory CPU-readable until the matching unlock
    // (the guard below).
    if unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) }
        != kCVReturnSuccess
    {
        return None;
    }
    let _lock = BaseAddressLock(buffer);
    let base = CVPixelBufferGetBaseAddress(buffer);
    if base.is_null() {
        return None;
    }
    let (width, height) = (
        CVPixelBufferGetWidth(buffer),
        CVPixelBufferGetHeight(buffer),
    );
    let bytes_per_row = CVPixelBufferGetBytesPerRow(buffer);
    if width == 0 || height == 0 {
        return None;
    }
    // Read only what the geometry covers (the last row need not carry its
    // padding), and only if the buffer holds that much past the base
    // address. The base address is pixel (0, 0), which extended (padding)
    // pixels would put past the start of the data: count them in, so the
    // bound holds wherever the base address sits.
    let len = bytes_per_row
        .checked_mul(height - 1)?
        .checked_add(width.checked_mul(4)?)?;
    let (mut left, mut right, mut top, mut bottom) = (0usize, 0usize, 0usize, 0usize);
    // SAFETY: four valid, writable `usize`s for the out-parameters, on a
    // live buffer.
    unsafe {
        CVPixelBufferGetExtendedPixels(buffer, &mut left, &mut right, &mut top, &mut bottom);
    }
    let lead = top
        .checked_mul(bytes_per_row)?
        .checked_add(left.checked_mul(4)?)?;
    if CVPixelBufferGetDataSize(buffer) < lead.checked_add(len)? {
        return None;
    }
    // SAFETY: `base` is the non-null base address of a buffer locked above
    // (unlocked only when `_lock` drops, after this slice's last use), and
    // the buffer holds at least `len` bytes from it (checked against its
    // data size, extended pixels included). The bytes are only read.
    let src = unsafe { slice::from_raw_parts(base.cast::<u8>(), len) };
    let rgba = bgra_to_rgba(src, width, height, bytes_per_row)?;
    Some((
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
        rgba,
    ))
}

impl AvMediaPlayer {
    /// [`MediaPlayer::open`]'s native part, inside an autorelease pool.
    fn open_in_pool(&mut self, path: &str) -> Result<Opened> {
        let url = NSURL::from_file_path(path).ok_or_else(|| MediaPlayerError::Open {
            path: path.to_string(),
            reason: "not a file path".to_string(),
        })?;
        // SAFETY: a valid file URL, on the main thread (the marker). A
        // missing or unreadable file is not undefined behaviour: the item
        // fails later, and its status and error say so.
        let item = unsafe { AVPlayerItem::playerItemWithURL(&url, self.mtm) };
        let output = match self.kind {
            MediaKind::Audio => None,
            MediaKind::Video => {
                let attributes = Self::pixel_buffer_attributes();
                // SAFETY: the attributes dictionary maps a CoreVideo
                // pixel-buffer key to an NSNumber, the types the initializer
                // documents; `alloc` gives it a fresh allocation.
                let output = unsafe {
                    AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                        AVPlayerItemVideoOutput::alloc(),
                        Some(&attributes),
                    )
                };
                // SAFETY: a new output not yet attached to any item (adding
                // one twice would raise), added to a new item, on the main
                // thread.
                unsafe { item.addOutput(&output) };
                Some(output)
            }
        };
        // SAFETY: the item is new, so not attached to another player (which
        // would raise); on the main thread.
        unsafe { self.player.replaceCurrentItemWithPlayerItem(Some(&item)) };
        Ok(Opened { item, output })
    }

    /// Fetches the frame due now, if the output has a new one, as RGBA.
    fn new_frame(output: &AVPlayerItemVideoOutput) -> Option<(u32, u32, Vec<u8>)> {
        let frequency = CVGetHostClockFrequency();
        if frequency.is_nan() || frequency <= 0.0 {
            return None;
        }
        // The item time due now, from the host clock (what
        // `CACurrentMediaTime()` reads).
        let now = CVGetCurrentHostTime() as f64 / frequency;
        // SAFETY: plain calls on a live output, on the main thread. The
        // copied buffer comes back retained (copy family) and is released
        // once, when `buffer` drops; a null display-time pointer is allowed.
        // An item time that is not valid yet (the item still loading) has no
        // new buffer.
        let buffer = unsafe {
            let item_time = output.itemTimeForHostTime(now);
            if !output.hasNewPixelBufferForItemTime(item_time) {
                return None;
            }
            output.copyPixelBufferForItemTime_itemTimeForDisplay(item_time, ptr::null_mut())
        };
        // A null buffer means "nothing new to show": the last frame stays.
        read_frame(&*buffer?)
    }
}

impl MediaPlayer for AvMediaPlayer {
    fn open(&mut self, path: &str, play: bool) -> Result<()> {
        let opened = autoreleasepool(|_| self.open_in_pool(path))?;
        // The previous clip's item and output (if any) are released here.
        self.opened = Some(opened);
        self.frame = None;
        if play {
            // SAFETY: on the main thread, with an item to play.
            unsafe { self.player.play() };
        }
        Ok(())
    }

    fn play(&mut self) {
        // media_kit replayed a completed clip from the start; an AVPlayer
        // at its item's end would stop again at once.
        if self.state().completed {
            self.seek(Duration::ZERO);
        }
        // SAFETY: on the main thread; with no item it does nothing.
        unsafe { self.player.play() };
    }

    fn pause(&mut self) {
        // SAFETY: on the main thread; pausing a paused player is a no-op.
        unsafe { self.player.pause() };
    }

    fn seek(&mut self, to: Duration) {
        // Exact (zero tolerance), as media_kit's absolute seek was.
        // SAFETY: valid CMTimes, on the main thread; AVPlayer clamps the
        // target to the item and ignores the seek with no item.
        unsafe {
            self.player.seekToTime_toleranceBefore_toleranceAfter(
                cm_time(to),
                CM_TIME_ZERO,
                CM_TIME_ZERO,
            )
        };
    }

    fn state(&self) -> PlayerState {
        match &self.opened {
            Some(opened) => autoreleasepool(|_| map_state(&self.native(&opened.item))),
            None => PlayerState::default(),
        }
    }

    fn frame(&mut self) -> Option<VideoFrame> {
        let output = self.opened.as_ref()?.output.as_ref()?;
        if let Some((width, height, rgba)) = autoreleasepool(|_| Self::new_frame(output)) {
            let revision = self.frame.as_ref().map_or(0, |f| f.revision) + 1;
            self.frame = Some(VideoFrame {
                width,
                height,
                rgba: Arc::new(rgba),
                revision,
            });
        }
        self.frame.clone()
    }
}

impl Drop for AvMediaPlayer {
    fn drop(&mut self) {
        // Stop at once and let go of the item (and its output), rather than
        // leave the sound to end whenever AVFoundation drops its own
        // references. Runs on the main thread: the type is `!Send`.
        // SAFETY: on the main thread; detaching the current item is allowed
        // at any time.
        unsafe {
            self.player.pause();
            self.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}
