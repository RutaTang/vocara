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
//!  2. MIC IN STEP WITH THE OUTPUT — the mic is duplex VPIO, which delivers the
//!     input and renders the output in one IO cycle, so the drift estimator is
//!     not run here (`MIC_ON_OUTPUT_CLOCK`; the correction stays exactly 1.0).
//!     That premise is what this checks on real hardware, whatever the devices
//!     (a USB mic with the built-in speakers included): sampled every second,
//!     the mic's engine frames less the playhead must not trend — the drift two
//!     clocks would show — by 10 ppm or more, and must stay within a few IO
//!     cycles end to end. Counters in whole cycles show slow drift as whole-
//!     cycle steps too (30 ppm is about one over the run), so a lost IO cycle
//!     reads as drift here (one mid-run, as 47 ppm): the report counts the
//!     steps, and a FAIL with one or two of them wants a longer rerun (or the
//!     underrun counts) to tell the two apart.
//!  3. NO CORRECTION — the published drift correction reads exactly 0.0 ppm,
//!     with the mic open and after it closes.

use std::ffi::CString;
use std::time::{Duration, Instant};

use vocara_engine::ffi::*;
use vocara_engine::sample::ENGINE_SR;

const PLAY_SECS: u64 = 180;
const PAUSE_SECS: u64 = 10;
/// VPIO's IO cycle (engine frames), as the engine asks for it.
const IO_CYCLE: f64 = 256.0;

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
    // (playhead, mic frames - playhead) every second once the mic has settled
    // in, for the in-step check.
    let mut apart_at: Vec<(f64, f64)> = Vec::new();
    while t0.elapsed() < Duration::from_secs(PLAY_SECS) {
        std::thread::sleep(Duration::from_secs(1));
        events |= ve_poll_device_event(e, u32::MAX);
        let s = t0.elapsed().as_secs();
        if s >= 10 {
            let (mic, pos) = (ve_input_frames_received(e), ve_frame_pos(e));
            apart_at.push((pos as f64, mic as f64 - pos as f64));
        }
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
    let (first, last) = (apart_at[0], apart_at[apart_at.len() - 1]);
    let apart = last.1 - first.1;
    let trend_ppm = slope(&apart_at) * 1e6;
    let steps = whole_cycle_steps(&apart_at, IO_CYCLE);

    assert_eq!(ve_pause(e), 0);
    assert_eq!(ve_stop_monitor(e), 0);
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
    // A few 256-frame IO cycles end to end: each read is up to a cycle off.
    let in_step_ok = apart.abs() <= 2048.0 && trend_ppm.abs() < 10.0;
    let none_ok = ppm_playing == 0.0 && ppm_paused == 0.0;
    println!(
        "playback advance : pos={pos} (expected ≈{expect_pos})  {}",
        ok(pos_ok)
    );
    println!("stream underruns : {xruns}  {}", ok(xruns == 0));
    println!("mic delivery     : {mic} engine frames  {}", ok(mic_ok));
    println!(
        "mic vs playhead  : {apart:+.0} frames apart over {:.0}, trending {trend_ppm:+.2} ppm \
         ({steps} whole-cycle steps: lost cycles or drift)  {}",
        last.0 - first.0,
        ok(in_step_ok)
    );
    println!(
        "drift correction : {ppm_playing:+.3} ppm playing, {ppm_paused:+.3} ppm mic off  {}",
        ok(none_ok)
    );
    println!("device events    : {events:#b}");
    let pass = pos_ok && xruns == 0 && mic_ok && in_step_ok && none_ok;
    println!("overall          : {}", if pass { "PASS" } else { "FAIL" });
    std::process::exit(if pass { 0 } else { 1 });
}

/// The whole-cycle steps in `(playhead, mic - playhead)` samples: ten samples make a
/// level, their lowest (a read lands up to a cycle above the true difference), and a
/// level moving by half a cycle or more from one to the next is a step.
fn whole_cycle_steps(samples: &[(f64, f64)], cycle: f64) -> usize {
    let levels: Vec<f64> = samples
        .chunks_exact(10)
        .map(|w| w.iter().map(|s| s.1).fold(f64::INFINITY, f64::min))
        .collect();
    levels
        .windows(2)
        .filter(|l| (l[1] - l[0]).abs() >= cycle / 2.0)
        .count()
}

/// The least-squares slope of `y` on `x`.
fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (mx, my) = points
        .iter()
        .fold((0.0, 0.0), |(sx, sy), &(x, y)| (sx + x / n, sy + y / n));
    let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(sxy, sxx), &(x, y)| {
        (sxy + (x - mx) * (y - my), sxx + (x - mx) * (x - mx))
    });
    if sxx > 0.0 {
        sxy / sxx
    } else {
        0.0
    }
}

fn ok(b: bool) -> &'static str {
    if b {
        "OK"
    } else {
        "FAIL"
    }
}
