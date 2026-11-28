//! Hardware check for "the microphone works once, then never again": arms a recording on the
//! real mic several times in a row the way a take does — arm, wait for the take's own ring to
//! grow, stop — and reports each session's time to its first recorded frame, then prints the
//! engine's diagnostics lines (input syncs, the mic's negotiated format, failed stops). With
//! `--two-engines`, every other session runs on a second engine created while the first still
//! has its mic open (monitoring), as a page opened over another would: the newer one must get a
//! working mic. With `--monitor`, the first engine keeps monitoring on between sessions, so each
//! arm finds the stream kept. Manual only:
//! `cargo run --example hw_mic_sessions --features device -- [sessions] [--monitor] [--two-engines]`.

use std::ffi::{c_char, CString};
use std::time::{Duration, Instant};

use vocara_engine::ffi::*;

/// The controller's default ready timeout, and its reopen at half of it.
const READY: Duration = Duration::from_secs(5);

/// One session: arm, wait for the take's ring to grow (asking for one reopen at half the
/// timeout, as the recording controller does), stop. The time to the first frame, or `None`.
fn session(e: *mut VeEngine, n: u32) -> Option<Duration> {
    let wav = std::env::temp_dir().join(format!(
        "vocara_hw_mic_sessions_{}_{n}.wav",
        std::process::id()
    ));
    let path = CString::new(wav.to_str().unwrap()).unwrap();
    assert_eq!(
        ve_start_input_recording(e, path.as_ptr()),
        0,
        "the WAV opens"
    );
    let t0 = Instant::now();
    let mut reopened = false;
    let mut first = None;
    while t0.elapsed() < READY {
        if ve_record_written_frames(e) > 0 {
            first = Some(t0.elapsed());
            break;
        }
        if !reopened && t0.elapsed() >= READY / 2 {
            reopened = true;
            let generation = (ve_input_stream_state(e) >> 1) & 0xFF_FFFF;
            println!(
                "  session {n}: no frames at {:?}; reopening gen {generation}",
                t0.elapsed()
            );
            ve_reopen_input(e, generation);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // Record a little past the first frame, as a take would.
    std::thread::sleep(Duration::from_millis(500));
    let stop = ve_stop_input_recording(e);
    let _ = std::fs::remove_file(&wav);
    println!(
        "  session {n}: first frame {first:?}, {} frames written, stop {stop}",
        ve_record_written_frames(e)
    );
    first
}

fn dump_diagnostics() {
    let mut buf: Vec<c_char> = vec![0; 1024];
    loop {
        let n = ve_take_diagnostic(buf.as_mut_ptr(), buf.len());
        if n == 0 {
            break;
        }
        let bytes: Vec<u8> = buf[..n].iter().map(|&c| c as u8).collect();
        println!("    | {}", String::from_utf8_lossy(&bytes));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sessions: u32 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(5);
    let monitor = args.iter().any(|a| a == "--monitor");
    let two = args.iter().any(|a| a == "--two-engines");

    let a = ve_create();
    assert!(!a.is_null(), "ve_create failed (no output device?)");
    assert_eq!(ve_set_monitor_gain(a, 0.0), 0);
    if monitor || two {
        assert_eq!(ve_start_monitor(a), 0);
    }
    let mut results = Vec::new();
    for n in 1..=sessions {
        let on_second = two && n % 2 == 0;
        let r = if on_second {
            // A second engine while the first holds its mic open.
            let b = ve_create();
            assert!(!b.is_null(), "the second engine opens its output");
            assert_eq!(ve_set_monitor_gain(b, 0.0), 0);
            let r = session(b, n);
            ve_destroy(b);
            r
        } else {
            session(a, n)
        };
        dump_diagnostics();
        results.push((n, on_second, r));
        std::thread::sleep(Duration::from_millis(500));
    }
    if monitor || two {
        assert_eq!(ve_stop_monitor(a), 0);
    }
    ve_destroy(a);
    dump_diagnostics();

    println!("\n== hw_mic_sessions report ==");
    let mut all = true;
    for (n, second, r) in &results {
        let engine = if *second {
            "second engine"
        } else {
            "first engine"
        };
        match r {
            Some(t) => println!("session {n} ({engine}): delivered after {t:?}  OK"),
            None => {
                all = false;
                println!("session {n} ({engine}): NOTHING within {READY:?}  FAIL");
            }
        }
    }
    println!(
        "{}",
        if all {
            "every session delivered"
        } else {
            "a session did not deliver: see the diagnostics lines above"
        }
    );
}
