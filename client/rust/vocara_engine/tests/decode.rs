//! File decode against what the file holds, not only what its container says.
#![cfg(feature = "device")]

use vocara_engine::device::decode_file;

/// Zero-crossing pitch of the left channel, skipping 0.25 s at each end (encoder priming).
fn tone_hz(frames: &[[f32; 2]], rate: u32) -> f64 {
    let (s, e) = (rate as usize / 4, frames.len() - rate as usize / 4);
    let crossings = (s + 1..e)
        .filter(|&i| (frames[i - 1][0] < 0.0) != (frames[i][0] < 0.0))
        .count();
    crossings as f64 / 2.0 / ((e - s) as f64 / rate as f64)
}

#[test]
fn an_he_aac_file_is_labelled_with_the_rate_it_decodes_at() {
    // 2 s of 440 Hz as HE-AAC: a 22.05 kHz AAC-LC core with SBR to 44.1 kHz, which is what the
    // sample entry declares (as Apple's and libfdk's encoders write it). symphonia has no SBR
    // and decodes the core at 22.05 kHz; labelled 44.1 kHz, the import played an octave up at
    // double speed, and was baked that way into the library copy.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/he_aac_440hz.m4a"
    );
    let (frames, rate) = decode_file(path, 0.0, 0, 0).unwrap();
    let secs = frames.len() as f64 / rate as f64;
    assert!((secs - 2.0).abs() < 0.1, "{secs:.2} s at {rate} Hz");
    let hz = tone_hz(&frames, rate);
    assert!((hz - 440.0).abs() < 10.0, "plays at {hz:.0} Hz");
}

#[test]
fn the_duration_budget_holds_at_the_rate_it_decodes_at() {
    // A 1.5 s budget judged at the declared 44.1 kHz is about 66k frames, which the 44k frames
    // the core decodes to pass, though they hold the full 2 s.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/he_aac_440hz.m4a"
    );
    assert!(decode_file(path, 1.5, 0, 0).is_err());
    assert!(decode_file(path, 2.5, 0, 0).is_ok());
}
