//! Isolation probe for the hw_verify rate anomaly: measures the consumed-cursor
//! advance rate per wall second in three phases — cpal output alone, VPIO duplex
//! (mic open), and back to cpal — printing per-second deltas. Manual only:
//! `cargo run --example hw_probe --features device`.

use std::ffi::CString;
use std::time::{Duration, Instant};

use vocara_engine::ffi::*;
use vocara_engine::sample::ENGINE_SR;

fn phase(e: *mut VeEngine, name: &str, secs: u64) {
    let start_pos = ve_frame_pos(e);
    let t0 = Instant::now();
    let mut last = start_pos;
    for s in 1..=secs {
        std::thread::sleep(Duration::from_secs(1));
        let _ = ve_poll_device_event(e, u32::MAX);
        let pos = ve_frame_pos(e);
        println!("[{name}] t={s:>2}s  pos={pos:>8}  d={:>6}/s", pos - last);
        last = pos;
    }
    let dt = t0.elapsed().as_secs_f64();
    let rate = (last - start_pos) as f64 / dt;
    println!("[{name}] mean advance = {rate:.0} frames/s over {dt:.1}s (engine rate {ENGINE_SR})");
}

fn main() {
    let secs_per_phase: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(15);
    let frames = 300 * ENGINE_SR as usize;
    let mut pcm = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = (i as f32 * (2.0 * std::f32::consts::PI * 440.0 / ENGINE_SR as f32)).sin() * 0.02;
        pcm.push(v);
        pcm.push(v);
    }
    let wav = std::env::temp_dir().join(format!("vocara_hw_probe_{}.wav", std::process::id()));
    vocara_engine::wav::write_wav_f32(&wav, &pcm, 2, ENGINE_SR as u32).expect("write wav");
    drop(pcm);

    let e = ve_create();
    assert!(!e.is_null());
    assert_eq!(ve_tracks_begin(e), 0);
    let cpath = CString::new(wav.to_str().unwrap()).unwrap();
    assert!(ve_tracks_add(e, cpath.as_ptr(), 0, 1.0, 0, 0, -1) >= 0);
    assert_eq!(ve_tracks_commit(e), 0);
    assert_eq!(ve_play(e), 0);

    phase(e, "cpal-only", secs_per_phase);

    assert_eq!(ve_set_monitor_gain(e, 0.0), 0);
    assert_eq!(ve_start_monitor(e), 0);
    phase(e, "vpio-duplex", secs_per_phase);

    assert_eq!(ve_stop_monitor(e), 0);
    phase(e, "cpal-restored", secs_per_phase);

    ve_destroy(e);
    let _ = std::fs::remove_file(&wav);
}
