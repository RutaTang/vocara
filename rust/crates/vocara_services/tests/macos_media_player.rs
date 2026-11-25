//! The technique examples' player on a real Mac, with the `device` feature:
//! the AVFoundation backend (`media_player/avfoundation.rs`) as the app
//! makes it (`SystemMediaPlayers`), playing clips written here and now — a
//! WAV tone by the crate's own WAV writer (`audio_dsp::write_wav_mono`), and
//! a QuickTime movie of solid frames by AVFoundation's `AVAssetWriter`
//! (H.264, else Motion JPEG where no H.264 encoder is to be had) — so no
//! media file ships with the tests.
//!
//! AVFoundation's players are main-thread objects that settle their state
//! through the main run loop, so this test has no harness (`harness =
//! false` in Cargo.toml): `main` makes the players and runs the main run
//! loop between polls, as iced's loop runs between ticks.
//!
//! What it checks:
//! 1. the movie (no sound track: its clock is the host's, so it plays with or
//!    without an audio device) opens paused, loads its 3 s duration, and
//!    then: play advances the position; the frames arrive, 192×144 RGBA,
//!    with fresh revisions, red the first second, as written (BGRA read back
//!    in the right channel order); a steady play is not buffering; the
//!    buffered extent never runs past the duration nor behind the playhead
//!    once known; pause stops the position; an exact seek lands where asked,
//!    and the frames played from there are that second's (blue); playing to
//!    the end completes there, stopped; play after that starts over;
//! 2. the tone opens and loads its duration; volume and mute reach the
//!    `AVPlayer` itself (`AVPlayer.volume` is mpv's cube law of the volume
//!    set, clamped to 0–100; `AVPlayer.isMuted` the mute, the volume kept),
//!    read back through the backend's `platform_volume`; and, where the Mac
//!    has an audio output device, the same play, pause, seek and end as the
//!    movie's (an audio item needs a device to run its clock: without one
//!    the run says so and skips those).
//!
//! It plays a quiet tone for a few seconds. Elsewhere, or without
//! `device`, it prints a skip line.

#[cfg(not(all(target_os = "macos", feature = "device")))]
fn main() {
    println!(
        "macos_media_player: skipped (the AVFoundation player is macOS with the `device` feature)"
    );
}

#[cfg(all(target_os = "macos", feature = "device"))]
fn main() {
    avplayer::run();
}

#[cfg(all(target_os = "macos", feature = "device"))]
mod avplayer {
    use std::ffi::c_void;
    use std::path::Path;
    use std::process::exit;
    use std::ptr::{self, NonNull};
    use std::sync::Mutex;
    use std::thread;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::MainThreadMarker;
    use objc2_av_foundation::{
        AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
        AVAssetWriterStatus, AVFileTypeQuickTimeMovie, AVMediaTypeVideo, AVVideoCodecKey,
        AVVideoCodecTypeH264, AVVideoCodecTypeJPEG, AVVideoHeightKey, AVVideoWidthKey,
    };
    use objc2_core_foundation::{kCFRunLoopDefaultMode, CFRetained, CFRunLoop};
    use objc2_core_media::{CMTime, CMTimeFlags};
    use objc2_core_video::{
        kCVPixelFormatType_32BGRA, kCVReturnSuccess, CVPixelBuffer, CVPixelBufferCreate,
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    use objc2_foundation::{NSDictionary, NSNumber, NSProcessInfo, NSString, NSURL};
    use vocara_services::audio_dsp::write_wav_mono;
    use vocara_services::media_player::{
        clamp_volume, volume_gain, MediaKind, MediaPlayer, MediaPlayerFactory, PlayerState,
        SystemMediaPlayers, VideoFrame, FULL_VOLUME,
    };

    /// How long the whole run may take.
    const LIMIT: Duration = Duration::from_secs(240);
    /// How long a clip gets to load.
    const LOAD: Duration = Duration::from_secs(15);
    /// How long a seek gets to land, and a play to reach the end.
    const SETTLE: Duration = Duration::from_secs(6);
    /// The clips' length.
    const CLIP: Duration = Duration::from_secs(3);
    /// The movie's frame size (whole H.264 macroblocks: no cropped rows)
    /// and rate.
    const WIDTH: usize = 192;
    const HEIGHT: usize = 144;
    const FPS: i64 = 10;
    /// Each second of the movie's colour: red, green, blue (RGB).
    const COLORS: [[u8; 3]; 3] = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    /// How far a duration may be from the clip's, as read back.
    const DURATION_SLACK: Duration = Duration::from_millis(60);
    /// How far an exact seek may land from its target.
    const SEEK_SLACK: Duration = Duration::from_millis(20);
    /// How far a paused position may drift.
    const PAUSE_SLACK: Duration = Duration::from_millis(30);

    static STEP: Mutex<&str> = Mutex::new("start");

    fn step(name: &'static str) {
        *STEP.lock().unwrap_or_else(|e| e.into_inner()) = name;
        println!("macos_media_player: {name}");
    }

    fn fatal(what: &str) -> ! {
        eprintln!("macos_media_player: FAIL: {what}");
        exit(1);
    }

    fn note(what: impl AsRef<str>) {
        println!("  note: {}", what.as_ref());
    }

    #[derive(Default)]
    struct Checks {
        failed: Vec<String>,
    }

    impl Checks {
        fn check(&mut self, ok: bool, what: impl Into<String>) -> bool {
            let what = what.into();
            if ok {
                println!("  ok: {what}");
            } else {
                eprintln!("  FAIL: {what}");
                self.failed.push(what);
            }
            ok
        }
    }

    /// Runs the main run loop for `seconds` (AVFoundation settles its
    /// players' state on it).
    fn spin(seconds: f64) {
        let started = Instant::now();
        // SAFETY: CoreFoundation's immutable mode constant.
        let mode = unsafe { kCFRunLoopDefaultMode };
        CFRunLoop::run_in_mode(mode, seconds, false);
        // A loop with nothing to wait on returns at once: do not spin hot.
        if started.elapsed() < Duration::from_millis(2) {
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Runs the main run loop until `done`, at most `timeout`.
    fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            spin(0.02);
        }
    }

    fn pump(time: Duration) {
        pump_until(time, || false);
    }

    fn near(a: Duration, b: Duration, slack: Duration) -> bool {
        a.abs_diff(b) <= slack
    }

    // ------------------------------------------------------- the clips

    /// A `CMTime` of `value / timescale` seconds.
    fn cm_time(value: i64, timescale: i64) -> CMTime {
        CMTime {
            value,
            timescale: timescale as i32,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        }
    }

    /// A 32BGRA pixel buffer of the movie's size, filled with `rgb`.
    fn solid_frame(rgb: [u8; 3]) -> Result<CFRetained<CVPixelBuffer>, String> {
        let mut out: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: a valid out-pointer; default allocator and attributes.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                WIDTH,
                HEIGHT,
                kCVPixelFormatType_32BGRA,
                None,
                NonNull::from(&mut out),
            )
        };
        let Some(raw) = NonNull::new(out).filter(|_| status == kCVReturnSuccess) else {
            return Err(format!("CVPixelBufferCreate failed ({status})"));
        };
        // SAFETY: CVPixelBufferCreate returned a +1 buffer (create rule),
        // whose release the `CFRetained` takes over.
        let buffer = unsafe { CFRetained::from_raw(raw) };
        // SAFETY: a live buffer, locked for writing and unlocked below with
        // the same flags; its rows are `bytes_per_row` apart, `HEIGHT` of
        // them, each at least `WIDTH * 4` bytes of 32BGRA.
        unsafe {
            if CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0)) != kCVReturnSuccess
            {
                return Err("could not lock a new pixel buffer".to_string());
            }
            let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
            let row = CVPixelBufferGetBytesPerRow(&buffer);
            if !base.is_null() {
                let [r, g, b] = rgb;
                for y in 0..HEIGHT {
                    let line = std::slice::from_raw_parts_mut(base.add(y * row), WIDTH * 4);
                    for pixel in line.as_chunks_mut::<4>().0 {
                        *pixel = [b, g, r, 0xFF];
                    }
                }
            }
            CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
            if base.is_null() {
                return Err("a new pixel buffer has no base address".to_string());
            }
        }
        Ok(buffer)
    }

    /// The codecs the movie may be written with.
    #[derive(Debug, Clone, Copy)]
    enum Codec {
        H264,
        Jpeg,
    }

    /// Writes the 3 s movie: `FPS` frames a second, each second one of
    /// [`COLORS`], no sound. AVAssetWriter's `finishWriting` must not run
    /// on the main thread: call it from another.
    fn write_movie(path: &Path, codec: Codec) -> Result<(), String> {
        let _ = std::fs::remove_file(path);
        let url = NSURL::from_file_path(path).ok_or("not a file path")?;
        // SAFETY: immutable NSString constants AVFoundation exports.
        let (file_type, media, codec_key, width_key, height_key, codec_name) = unsafe {
            (
                AVFileTypeQuickTimeMovie.ok_or("no AVFileTypeQuickTimeMovie")?,
                AVMediaTypeVideo.ok_or("no AVMediaTypeVideo")?,
                AVVideoCodecKey.ok_or("no AVVideoCodecKey")?,
                AVVideoWidthKey.ok_or("no AVVideoWidthKey")?,
                AVVideoHeightKey.ok_or("no AVVideoHeightKey")?,
                match codec {
                    Codec::H264 => AVVideoCodecTypeH264,
                    Codec::Jpeg => AVVideoCodecTypeJPEG,
                }
                .ok_or("no such codec name")?,
            )
        };
        let width = NSNumber::new_i32(WIDTH as i32);
        let height = NSNumber::new_i32(HEIGHT as i32);
        let values: [&AnyObject; 3] = [codec_name, &width, &height];
        let settings: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_slices(&[codec_key, width_key, height_key], &values);
        // SAFETY: AVFoundation's documented use: a writer for a new file
        // URL and a declared file type, one video input with complete output
        // settings (codec, width, height), added before writing starts, fed
        // 32BGRA buffers of that size in increasing time order on this one
        // thread, then finished. Nothing here is a main-thread object.
        unsafe {
            let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type)
                .map_err(|e| e.localizedDescription().to_string())?;
            if !writer.canApplyOutputSettings_forMediaType(Some(&settings), media) {
                return Err(format!("the writer cannot encode {codec:?}"));
            }
            let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                media,
                Some(&settings),
            );
            input.setExpectsMediaDataInRealTime(false);
            let adaptor = AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(&input, None);
            if !writer.canAddInput(&input) {
                return Err("the writer takes no video input".to_string());
            }
            writer.addInput(&input);
            let failure = |what: &str| {
                let why = writer
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default();
                format!("{what} ({codec:?}): {why}")
            };
            if !writer.startWriting() {
                return Err(failure("startWriting failed"));
            }
            writer.startSessionAtSourceTime(cm_time(0, FPS));
            let frames = FPS * CLIP.as_secs() as i64;
            for i in 0..frames {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !input.isReadyForMoreMediaData() {
                    if Instant::now() > deadline || writer.status() == AVAssetWriterStatus::Failed {
                        return Err(failure("the input never took the next frame"));
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                let buffer = solid_frame(COLORS[(i / FPS) as usize % COLORS.len()])?;
                if !adaptor.appendPixelBuffer_withPresentationTime(&buffer, cm_time(i, FPS)) {
                    return Err(failure("appendPixelBuffer failed"));
                }
            }
            input.markAsFinished();
            writer.endSessionAtSourceTime(cm_time(frames, FPS));
            #[allow(deprecated)]
            let finished = writer.finishWriting();
            if !finished || writer.status() != AVAssetWriterStatus::Completed {
                return Err(failure("finishWriting failed"));
            }
        }
        Ok(())
    }

    /// The 3 s tone: 440 Hz, quiet, mono 44.1 kHz, 16-bit, written by the
    /// crate's WAV writer.
    fn write_tone(path: &Path) -> Result<(), String> {
        let rate = 44_100usize;
        let samples: Vec<f32> = (0..rate * CLIP.as_secs() as usize)
            .map(|n| 0.2 * (2.0 * std::f32::consts::PI * 440.0 * n as f32 / rate as f32).sin())
            .collect();
        std::fs::write(path, write_wav_mono(&samples, rate as i64)).map_err(|e| e.to_string())
    }

    // ------------------------------------------------- the audio device

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        /// `OSStatus AudioObjectGetPropertyData(AudioObjectID, const
        /// AudioObjectPropertyAddress *, UInt32, const void *, UInt32 *,
        /// void *)` (AudioHardware.h).
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    /// The system's default output device, if it has one.
    fn default_output_device() -> Option<u32> {
        let address = AudioObjectPropertyAddress {
            selector: u32::from_be_bytes(*b"dOut"),
            scope: u32::from_be_bytes(*b"glob"),
            element: 0,
        };
        let mut device = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the system object (1), a valid address, no qualifier, and
        // an out-buffer of the size given for the AudioObjectID it holds.
        let status = unsafe {
            AudioObjectGetPropertyData(
                1,
                &address,
                0,
                ptr::null(),
                &mut size,
                (&mut device as *mut u32).cast(),
            )
        };
        (status == 0 && device != 0).then_some(device)
    }

    // ------------------------------------------------------ the checks

    /// What a frame's middle pixel is, if one colour of [`COLORS`] clearly.
    fn color_of(frame: &VideoFrame) -> Option<usize> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let at = ((h / 2) * w + w / 2) * 4;
        let rgb = frame.rgba.get(at..at + 3)?;
        let (r, g, b) = (i32::from(rgb[0]), i32::from(rgb[1]), i32::from(rgb[2]));
        if r > g + 80 && r > b + 80 {
            Some(0)
        } else if g > r + 80 && g > b + 80 {
            Some(1)
        } else if b > r + 80 && b > g + 80 {
            Some(2)
        } else {
            None
        }
    }

    /// The second of the movie a position is in, unless it is within
    /// 200 ms of a change of colour (the frame shown may be either side: a
    /// frame lasts 100 ms, and a poll reads the position just after it).
    fn second_at(position: Duration) -> Option<usize> {
        let ms = position.as_millis() % 1000;
        (200..=800)
            .contains(&ms)
            .then(|| (position.as_secs() as usize).min(COLORS.len() - 1))
    }

    /// What a run of polls saw.
    #[derive(Debug, Default)]
    struct Seen {
        first: Option<Duration>,
        last: Option<PlayerState>,
        revisions: Vec<u64>,
        sizes_ok: bool,
        /// (position, colour expected, colour seen) where they differ.
        wrong_colors: Vec<(Duration, usize, Option<usize>)>,
        right_colors: usize,
        /// Buffered extents past the duration, or behind a playhead it
        /// once covered.
        bad_buffers: Vec<(Duration, Duration)>,
        errors: Vec<String>,
    }

    /// Polls `player` (state, and frames if it is a video player) for
    /// `time`, as the UI's tick does.
    fn watch(player: &mut dyn MediaPlayer, time: Duration) -> Seen {
        let mut seen = Seen {
            sizes_ok: true,
            ..Seen::default()
        };
        let mut revision = player.frame().map_or(0, |frame| frame.revision);
        let deadline = Instant::now() + time;
        while Instant::now() < deadline {
            spin(0.02);
            let frame = player.frame();
            let state = player.state();
            seen.first.get_or_insert(state.position);
            if let Some(error) = &state.error {
                seen.errors.push(error.clone());
            }
            if !state.duration.is_zero()
                && (state.buffer > state.duration
                    || (!state.buffer.is_zero() && state.buffer + PAUSE_SLACK < state.position))
            {
                seen.bad_buffers.push((state.position, state.buffer));
            }
            // Only a frame fetched by this poll is the one due now (an older
            // one stays the latest until the output has a new one).
            let fresh = frame.filter(|frame| frame.revision > revision);
            if let Some(frame) = fresh {
                revision = frame.revision;
                seen.revisions.push(frame.revision);
                seen.sizes_ok &= frame.width as usize == WIDTH
                    && frame.height as usize == HEIGHT
                    && frame.rgba.len() == WIDTH * HEIGHT * 4;
                if let Some(second) = second_at(state.position) {
                    match color_of(&frame) {
                        Some(color) if color == second => seen.right_colors += 1,
                        color => seen.wrong_colors.push((state.position, second, color)),
                    }
                }
            }
            seen.last = Some(state);
        }
        seen
    }

    /// Opens `path` paused and waits for it to load; fails the run if it
    /// cannot.
    fn open(checks: &mut Checks, player: &mut dyn MediaPlayer, path: &Path, what: &str) {
        let Some(path_str) = path.to_str() else {
            fatal("a clip path that is not UTF-8");
        };
        if let Err(e) = player.open(path_str, false) {
            fatal(&format!("the {what} did not open: {e}"));
        }
        let loaded = pump_until(LOAD, || !player.state().loading);
        let state = player.state();
        checks.check(
            loaded && state.error.is_none(),
            format!("the {what} loads paused, with no error ({state:?})"),
        );
        checks.check(
            near(state.duration, CLIP, DURATION_SLACK),
            format!("the {what}'s duration is its 3 s ({:?})", state.duration),
        );
        checks.check(
            state.position.is_zero() && !state.playing && !state.completed && !state.buffering,
            format!("the {what} sits at 0:00, not playing, buffering or completed ({state:?})"),
        );
    }

    /// Play, pause, seek and the end, on a clip whose clock runs; with
    /// `frames`, the frames and their colours too.
    fn transport(checks: &mut Checks, player: &mut dyn MediaPlayer, what: &str, frames: bool) {
        player.play();
        checks.check(
            player.state().playing,
            format!("the {what} plays from play on (media_kit's playing)"),
        );
        let seen = watch(player, Duration::from_millis(1600));
        let last = seen.last.clone().unwrap_or_default();
        checks.check(
            last.position >= Duration::from_millis(600) && last.playing,
            format!(
                "the {what}'s position advances while it plays ({:?} → {:?})",
                seen.first, last.position
            ),
        );
        checks.check(
            !last.buffering,
            format!("a steady play of the {what} is not buffering ({last:?})"),
        );
        checks.check(
            !last.buffer.is_zero() && last.buffer >= last.position && last.buffer <= last.duration,
            format!(
                "the {what}'s buffered extent covers the playhead, within the clip \
                 (buffer {:?}, position {:?}, duration {:?})",
                last.buffer, last.position, last.duration
            ),
        );
        checks.check(
            seen.bad_buffers.is_empty() && seen.errors.is_empty(),
            format!(
                "the {what}'s buffered extent stays sane, with no error \
                 (bad: {:?}, errors: {:?})",
                seen.bad_buffers, seen.errors
            ),
        );
        if frames {
            checks.check(
                seen.revisions.len() >= 5,
                format!(
                    "frames arrive while the {what} plays, each a new revision ({:?})",
                    seen.revisions
                ),
            );
            checks.check(
                seen.revisions.windows(2).all(|w| w[0] < w[1]),
                "the frames' revisions only go up",
            );
            checks.check(seen.sizes_ok, "every frame is 192×144 tightly packed RGBA");
            checks.check(
                seen.right_colors > 0 && seen.wrong_colors.is_empty(),
                format!(
                    "the frames are the colours written for their second, red then green \
                     ({} right; wrong: {:?})",
                    seen.right_colors, seen.wrong_colors
                ),
            );
        }

        player.pause();
        let paused = player.state();
        checks.check(
            !paused.playing && !paused.buffering,
            format!("pause stops the {what} ({paused:?})"),
        );
        pump(Duration::from_millis(300));
        let held = player.state().position;
        pump(Duration::from_millis(700));
        let still = player.state().position;
        checks.check(
            near(held, still, PAUSE_SLACK),
            format!("the paused {what}'s position holds ({held:?}, then {still:?})"),
        );

        let target = Duration::from_millis(2400);
        player.seek(target);
        let landed = pump_until(SETTLE, || near(player.state().position, target, SEEK_SLACK));
        let at = player.state().position;
        checks.check(
            landed,
            format!("an exact seek of the {what} lands at {target:?} ({at:?})"),
        );
        player.play();
        let seen = watch(player, Duration::from_millis(400));
        if frames {
            checks.check(
                seen.right_colors > 0 && seen.wrong_colors.is_empty(),
                format!(
                    "played on from the seek, the frames are the third second's, blue \
                     ({} right; wrong: {:?})",
                    seen.right_colors, seen.wrong_colors
                ),
            );
        }
        let ended = pump_until(SETTLE, || player.state().completed);
        let end = player.state();
        checks.check(
            ended && !end.playing && near(end.position, end.duration, Duration::from_millis(5)),
            format!("the {what} plays to its end and completes there, stopped ({end:?})"),
        );
        player.play();
        pump(Duration::from_millis(300));
        let again = player.state();
        checks.check(
            again.playing && again.position < Duration::from_secs(1) && !again.completed,
            format!("play after the end starts the {what} over ({again:?})"),
        );
        player.pause();
    }

    /// Volume and mute, read back from the `AVPlayer`.
    fn volume(checks: &mut Checks, player: &mut dyn MediaPlayer) {
        let fresh = player.platform_volume();
        checks.check(
            fresh == Some((1.0, false)) && player.state().volume == FULL_VOLUME,
            format!(
                "a new player is at full volume, unmuted (AVPlayer: {fresh:?}, state: {})",
                player.state().volume
            ),
        );
        for set in [50.0, 0.0, 100.0, 150.0, -5.0, 25.0, 10.0] {
            player.set_volume(set);
            let gain = volume_gain(set) as f32;
            let native = player.platform_volume();
            let reported = player.state().volume;
            checks.check(
                native.is_some_and(|(volume, muted)| (volume - gain).abs() < 1e-6 && !muted)
                    && reported == clamp_volume(set),
                format!(
                    "set_volume({set}) gives the AVPlayer volume {gain} (the cube of {}/100) \
                     and reports {} (AVPlayer: {native:?}, state: {reported})",
                    clamp_volume(set),
                    clamp_volume(set)
                ),
            );
        }
        let gain = volume_gain(10.0) as f32;
        let at = |native: Option<(f32, bool)>, muted: bool| {
            native.is_some_and(|(volume, m)| (volume - gain).abs() < 1e-6 && m == muted)
        };
        player.set_muted(true);
        let native = player.platform_volume();
        let state = player.state();
        checks.check(
            at(native, true) && state.muted && state.volume == 10.0,
            format!(
                "set_muted(true) mutes the AVPlayer and keeps the volume \
                 (AVPlayer: {native:?}, state: muted {} at {})",
                state.muted, state.volume
            ),
        );
        player.set_muted(false);
        let native = player.platform_volume();
        checks.check(
            at(native, false) && !player.state().muted,
            format!("set_muted(false) unmutes it, at the same volume (AVPlayer: {native:?})"),
        );
    }

    pub fn run() {
        let Some(_mtm) = MainThreadMarker::new() else {
            fatal("main is not on the main thread (harness = false?)");
        };
        println!(
            "macos_media_player: macOS {}",
            NSProcessInfo::processInfo().operatingSystemVersionString()
        );
        thread::spawn(|| {
            thread::sleep(LIMIT);
            let step = *STEP.lock().unwrap_or_else(|e| e.into_inner());
            eprintln!("macos_media_player: FAIL: timed out during: {step}");
            std::process::abort();
        });
        let mut checks = Checks::default();
        let dir =
            tempfile::tempdir().unwrap_or_else(|e| fatal(&format!("no temporary folder: {e}")));

        step("writing the clips");
        let tone = dir.path().join("tone.wav");
        write_tone(&tone).unwrap_or_else(|e| fatal(&format!("cannot write the tone: {e}")));
        let movie = dir.path().join("movie.mov");
        let writing = movie.clone();
        let written = thread::spawn(move || {
            write_movie(&writing, Codec::H264).or_else(|h264| {
                println!("  note: no H.264 movie ({h264}); writing Motion JPEG");
                write_movie(&writing, Codec::Jpeg)
            })
        })
        .join()
        .unwrap_or_else(|_| fatal("the movie writer panicked"));
        if let Err(e) = written {
            fatal(&format!("cannot write the test movie: {e}"));
        }

        let players = SystemMediaPlayers;

        step("the movie: open, play, pause, seek, end");
        let mut video = players
            .create(MediaKind::Video)
            .unwrap_or_else(|e| fatal(&format!("no video player: {e}")));
        open(&mut checks, &mut *video, &movie, "movie");
        // Quiet, whatever the Mac plays it on (it has no sound anyway).
        video.set_volume(10.0);
        transport(&mut checks, &mut *video, "movie", true);
        drop(video);

        step("the tone: open, volume and mute");
        let mut audio = players
            .create(MediaKind::Audio)
            .unwrap_or_else(|e| fatal(&format!("no audio player: {e}")));
        open(&mut checks, &mut *audio, &tone, "tone");
        volume(&mut checks, &mut *audio);

        step("the tone: play, pause, seek, end");
        match default_output_device() {
            Some(device) => {
                note(format!("default output device: {device}"));
                transport(&mut checks, &mut *audio, "tone", false);
            }
            None => {
                audio.play();
                let seen = watch(&mut *audio, Duration::from_millis(1500));
                note(format!(
                    "this Mac has no audio output device, so an audio item's clock may not \
                     run: the tone's transport is NOT checked here (after 1.5 s of play: {:?})",
                    seen.last
                ));
                audio.pause();
            }
        }
        drop(audio);

        drop(dir);
        if checks.failed.is_empty() {
            println!("macos_media_player: all checks passed");
            exit(0);
        }
        eprintln!(
            "macos_media_player: {} check(s) failed:",
            checks.failed.len()
        );
        for failure in &checks.failed {
            eprintln!("  - {failure}");
        }
        exit(1);
    }
}
