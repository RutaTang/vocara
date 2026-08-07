//! Hardware-in-the-loop verification: real CoreAudio output + real mic on THIS
//! machine, driving the exact FFI surface the app uses. Manual/interactive only
//! (`cargo run --example hw_verify --features device`); the first run may pop
//! the macOS microphone-permission prompt for the invoking terminal.
//!
//! RUN IN RELEASE: the debug build's DSP (our AEC in the VPIO input callback)
//! misses the 256-frame real-time deadline on this hardware, so VPIO skips
//! cycles and the whole duplex runs at ~0.45x — a harness artifact the release
//! build does not have (measured: debug ~21.5k fr/s, release 48.0k fr/s).
//!
//! What it verifies (the two items simulation cannot):
//!  1. STREAM HEALTH — a disk-streamed track playing through the real output
//!     callback keeps `ve_stream_xruns` at 0 (the feeder pump outpaces real
//!     device timing, not just the test harness's synchronous pump).
//!  2. DRIFT ESTIMATOR ON REAL CLOCKS — with the mic open and the transport
//!     playing, the 1 s poll ticks the estimator against two REAL streams.
//!     On a shared codec (built-in mic + speakers) the truth is ~0 ppm, so the
//!     estimator must sit in its deadband (exactly 0.0 ppm published) — the
//!     "no spurious correction on a healthy chain" half of calibration. An
//!     independent-clock device pair (e.g. USB mic) would show a stable
//!     nonzero ppm instead; the report prints whatever is measured.
//!  3. FREEZE — closing the mic stops the input counter; the published
//!     correction must hold bit-for-bit afterwards. (A transport PAUSE does
//!     NOT freeze it: the output keeps delivering silence, both clocks keep
//!     ticking, and the estimator keeps measuring — by design.)

use std::ffi::CString;
use std::time::{Duration, Instant};

use vocara_engine::ffi::*;
use vocara_engine::sample::ENGINE_SR;

const PLAY_SECS: u64 = 180;
const PAUSE_SECS: u64 = 10;

fn main() {
    // Quiet 440 Hz tone, canonical engine-rate f32 stereo → the streaming fast
    // path in ve_tracks_add. Long enough that playback never runs off the end.
    let secs = (PLAY_SECS + 30) as usize;
    let frames = secs * ENGINE_SR as usize;
    let mut pcm = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = (i as f32 * (2.0 * std::f32::consts::PI * 440.0 / ENGINE_SR as f32)).sin() * 0.02;
        pcm.push(v);
        pcm.push(v);
    }
    let wav = std::env::temp_dir().join(format!("vocara_hw_verify_{}.wav", std::process::id()));
    vocara_engine::wav::write_wav_f32(&wav, &pcm, 2, ENGINE_SR as u32).expect("write wav");
    drop(pcm);

    let e = ve_create();
    assert!(!e.is_null(), "ve_create failed (no output device?)");
    let mut uid = [0u8; 256];
    let n = ve_current_output_device(e, uid.as_mut_ptr() as *mut i8, uid.len());
    println!(
        "output device uid: {}",
        String::from_utf8_lossy(&uid[..n.min(uid.len())])
    );

    assert_eq!(ve_tracks_begin(e), 0);
    let cpath = CString::new(wav.to_str().unwrap()).unwrap();
    let idx = ve_tracks_add(e, cpath.as_ptr(), 0, 1.0, 0, 0, -1);
    assert!(idx >= 0, "tracks_add failed");
    assert_eq!(ve_tracks_commit(e), 0, "tracks_commit failed");

    // Open the real mic with the monitor path muted (counters run, no feedback).
    assert_eq!(ve_set_monitor_gain(e, 0.0), 0);
    assert_eq!(ve_start_monitor(e), 0);
    println!("mic opening (grant the permission prompt if one appears)…");

    assert_eq!(ve_play(e), 0);
    let t0 = Instant::now();
    let mut events: u32 = 0;
    let mut last_report = 0u64;
    while t0.elapsed() < Duration::from_secs(PLAY_SECS) {
        std::thread::sleep(Duration::from_secs(1));
        events |= ve_poll_device_event(e, u32::MAX); // also ticks the drift estimator
        let s = t0.elapsed().as_secs();
        if s / 10 > last_report {
            last_report = s / 10;
            println!(
                "t={s:>3}s  pos={:>8}  xruns={}  mic_frames={:>8}  drift={:+.2} ppm  level={:.3}",
                ve_frame_pos(e),
                ve_stream_xruns(e),
                ve_input_frames_received(e),
                ve_drift_ppm(e),
                ve_input_level(e),
            );
        }
    }

    let xruns = ve_stream_xruns(e);
    let pos = ve_frame_pos(e);
    let mic = ve_input_frames_received(e);
    let ppm_playing = ve_drift_ppm(e);

    // Freeze check: close the mic → the input counter stops → ticks stop
    // measuring → the published correction must hold exactly.
    assert_eq!(ve_pause(e), 0);
    assert_eq!(ve_stop_monitor(e), 0);
    // The first post-close tick may legitimately consume the final partial
    // second of mic frames; the freeze baseline is taken after it settles.
    for _ in 0..3 {
        std::thread::sleep(Duration::from_secs(1));
        events |= ve_poll_device_event(e, u32::MAX);
    }
    let ppm_frozen_base = ve_drift_ppm(e);
    for _ in 0..PAUSE_SECS {
        std::thread::sleep(Duration::from_secs(1));
        events |= ve_poll_device_event(e, u32::MAX);
    }
    let ppm_paused = ve_drift_ppm(e);

    ve_destroy(e);
    let _ = std::fs::remove_file(&wav);

    println!("\n== hw_verify report ==");
    let expect_pos = PLAY_SECS as i64 * ENGINE_SR as i64;
    let pos_ok = (pos - expect_pos).abs() < ENGINE_SR as i64 * 2;
    let mic_ok = mic > (PLAY_SECS - 5) * ENGINE_SR as u64;
    let freeze_ok = ppm_paused.to_bits() == ppm_frozen_base.to_bits();
    println!("playback advance : pos={pos} (expected ≈{expect_pos})  {}", ok(pos_ok));
    println!("stream underruns : {xruns}  {}", ok(xruns == 0));
    println!("mic delivery     : {mic} engine frames  {}", ok(mic_ok));
    println!("drift (playing)  : {ppm_playing:+.3} ppm (deadband = 0.0 on a shared codec)");
    println!("drift (mic off)  : {ppm_paused:+.3} ppm  freeze {}", ok(freeze_ok));
    println!("device events    : {events:#b}");
    let pass = pos_ok && xruns == 0 && mic_ok && freeze_ok;
    println!("overall          : {}", if pass { "PASS" } else { "FAIL" });
    std::process::exit(if pass { 0 } else { 1 });
}

fn ok(b: bool) -> &'static str {
    if b {
        "OK"
    } else {
        "FAIL"
    }
}
