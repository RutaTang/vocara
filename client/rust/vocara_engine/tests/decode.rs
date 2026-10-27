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

#[test]
fn an_mp3_starts_where_other_players_start_it() {
    // 1 s at 44.1 kHz, silent until a tone at 0.5 s, as LAME encodes it: its encoder delay
    // and padding were decoded as audio, the tone landing 25 ms late (a technique example's
    // analysis trailed its own playback) and the file 46 ms long.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/onset_0500ms.mp3"
    );
    let (frames, rate) = decode_file(path, 0.0, 0, 0).unwrap();
    assert_eq!(rate, 44_100);
    assert_eq!(frames.len(), 44_100, "the padding is trimmed too");
    let onset = frames.iter().position(|f| f[0].abs() > 0.09).unwrap();
    assert!(
        onset.abs_diff(22_050) < 44,
        "the tone starts at frame {onset}"
    );
}

/// 2 s of a 440 Hz mono 16-bit tone at 44.1 kHz.
fn tone_i16() -> Vec<i16> {
    (0..88_200)
        .map(|i| {
            let t = i as f64 / 44_100.0;
            ((2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.5 * i16::MAX as f64) as i16
        })
        .collect()
}

/// Writes [bytes] to a fresh file named [name] in the temp dir and returns its path.
fn temp_file(name: &str, bytes: &[u8]) -> String {
    let path = std::env::temp_dir().join(format!("{}-{name}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn an_aiff_bounce_decodes() {
    // GarageBand's and Logic's default bounce. The import offered .aif/.aiff, and the decoder
    // was built without them: every one failed with "could not be converted for playback".
    let samples = tone_i16();
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
    let mut comm = Vec::new();
    comm.extend_from_slice(&1u16.to_be_bytes()); // channels
    comm.extend_from_slice(&(samples.len() as u32).to_be_bytes()); // frames
    comm.extend_from_slice(&16u16.to_be_bytes()); // bits
    comm.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]); // 44100, 80-bit float
    let mut body = b"AIFF".to_vec();
    body.extend_from_slice(b"COMM");
    body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    body.extend_from_slice(&comm);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&(data.len() as u32 + 8).to_be_bytes());
    body.extend_from_slice(&[0; 8]); // offset, block size
    body.extend_from_slice(&data);
    let mut file = b"FORM".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_be_bytes());
    file.extend_from_slice(&body);
    let path = temp_file("tone.aiff", &file);

    let decoded = decode_file(&path, 0.0, 0, 0);
    let _ = std::fs::remove_file(&path);
    let (frames, rate) = decoded.expect("an AIFF decodes");
    assert_eq!(rate, 44_100);
    assert!((frames.len() as f64 / 44_100.0 - 2.0).abs() < 0.05);
    assert!((tone_hz(&frames, rate) - 440.0).abs() < 10.0);
}

#[test]
fn a_caf_file_decodes() {
    // Core Audio Format, offered by the import as .caf and refused by the decoder alike.
    let samples = tone_i16();
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut file = b"caff".to_vec();
    file.extend_from_slice(&1u16.to_be_bytes()); // version
    file.extend_from_slice(&0u16.to_be_bytes()); // flags
    file.extend_from_slice(b"desc");
    file.extend_from_slice(&32i64.to_be_bytes());
    file.extend_from_slice(&44_100f64.to_be_bytes());
    file.extend_from_slice(b"lpcm");
    file.extend_from_slice(&2u32.to_be_bytes()); // little-endian integer samples
    file.extend_from_slice(&2u32.to_be_bytes()); // bytes per packet
    file.extend_from_slice(&1u32.to_be_bytes()); // frames per packet
    file.extend_from_slice(&1u32.to_be_bytes()); // channels
    file.extend_from_slice(&16u32.to_be_bytes()); // bits
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(data.len() as i64 + 4).to_be_bytes());
    file.extend_from_slice(&0u32.to_be_bytes()); // edit count
    file.extend_from_slice(&data);
    let path = temp_file("tone.caf", &file);

    let decoded = decode_file(&path, 0.0, 0, 0);
    let _ = std::fs::remove_file(&path);
    let (frames, rate) = decoded.expect("a CAF decodes");
    assert_eq!(rate, 44_100);
    assert!((frames.len() as f64 / 44_100.0 - 2.0).abs() < 0.05);
    assert!((tone_hz(&frames, rate) - 440.0).abs() < 10.0);
}
