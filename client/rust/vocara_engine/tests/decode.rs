//! File decode against what the file holds, not only what its container says.
#![cfg(feature = "device")]

use vocara_engine::device::{decode_file, probe_duration_ms};

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
fn an_mp4_length_is_read_in_its_own_timescale() {
    // The media header counts in the track's timescale, not the sample rate's frames. This
    // file's is its 22.05 kHz core rate, so read as 44.1 kHz frames it probed at half its 2 s
    // and the import gate admitted a file twice the cap.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/he_aac_440hz.m4a"
    );
    let ms = probe_duration_ms(path).unwrap().unwrap();
    assert!(ms.abs_diff(2_000) < 100, "probed at {ms} ms");
}

#[test]
fn an_mp4_of_unknown_length_is_not_refused_as_too_long() {
    // 200 ms in 10 MHz ticks, its length written as all ones (unknown), as Smooth Streaming
    // writes it: read as frames, it was billions of hours, refused by the gate and the decode.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/unknown_length_200ms.mp4"
    );
    assert_eq!(probe_duration_ms(path), Ok(None));
    let (frames, rate) = decode_file(path, 1.0, 0, 0).unwrap();
    let secs = frames.len() as f64 / rate as f64;
    assert!((0.2..0.3).contains(&secs), "{secs:.3} s");
}

#[test]
fn a_surround_file_is_mixed_down_not_cut_to_its_front_pair() {
    // 5.1 with silent front channels: 440 Hz in the centre, 60 Hz in the LFE, 660 Hz in the
    // rear left, each at 0.5. Kept to its first two channels it decoded silent; mixed as
    // ffmpeg mixes it, the centre reaches both sides and the rear left its own, at -3 dB
    // (0.25 RMS each), and the LFE neither.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/surround_100ms.flac"
    );
    let (frames, _) = decode_file(path, 0.0, 0, 0).unwrap();
    let rms = |c: usize| {
        (frames.iter().map(|f| f64::from(f[c]).powi(2)).sum::<f64>() / frames.len() as f64).sqrt()
    };
    assert!(
        (rms(0) - 0.25 * 2f64.sqrt()).abs() < 0.01,
        "left {}",
        rms(0)
    );
    assert!((rms(1) - 0.25).abs() < 0.01, "right {}", rms(1));
}

#[test]
fn an_alac_cookie_wrapped_in_its_atom_headers_decodes() {
    // ffmpeg's CAF writer (and older encoders) put the ALAC config behind 'frma' and 'alac'
    // atom headers, which symphonia read as the config itself and refused as a newer version.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alac_100ms.caf");
    let (frames, rate) = decode_file(path, 0.0, 0, 0).unwrap();
    assert_eq!((frames.len(), rate), (4410, 44_100));
}

#[test]
fn an_aiff_ends_where_its_sound_does() {
    // symphonia reads 8 bytes past an AIFF's sound chunk. With an ID3 chunk after it, as iTunes
    // and taggers write one, those bytes were the tag's header, decoded as a click at the end:
    // here 4 frames, the first at 0.57.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/tagged_100ms.aiff"
    );
    let (frames, rate) = decode_file(path, 0.0, 0, 0).unwrap();
    assert_eq!((frames.len(), rate), (800, 8_000));
    assert!(
        frames.iter().all(|f| f[0].abs() <= 0.5),
        "a click past the sound"
    );
}

#[test]
fn a_caf_whose_sound_runs_to_its_end_decodes() {
    // Core Audio leaves a CAF's data size at -1 until the file is closed, so an interrupted
    // recording keeps it. symphonia read on from the sound's start as further chunks and
    // refused the file as malformed.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/unclosed_100ms.caf"
    );
    assert_eq!(probe_duration_ms(path), Ok(Some(100)));
    let (frames, rate) = decode_file(path, 0.0, 0, 0).unwrap();
    assert_eq!((frames.len(), rate), (800, 8_000));
}

#[test]
fn a_wav_written_to_a_pipe_is_not_refused_as_too_long() {
    // Written where it could not seek back, a WAV keeps its data size at all ones: 4 GB, which
    // read as hours. 100 ms at 8 kHz, its length unknown rather than refused: 16-bit, and
    // A-law, whose header declares no bit depth.
    for name in ["pipe_100ms.wav", "pipe_alaw_100ms.wav"] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        assert_eq!(probe_duration_ms(&path), Ok(None), "{name}");
        let (frames, rate) = decode_file(&path, 1.0, 0, 0).unwrap();
        assert_eq!((frames.len(), rate), (800, 8_000), "{name}");
    }
}

#[test]
fn a_tracks_wav_keeps_its_fourth_track() {
    // A WAV naming no layout is given one by its channel count, an LFE fourth. Four recorder
    // tracks, a 440 Hz tone at 0.5 on the fourth only: left out as an LFE it decoded silent;
    // mixed as a centre it reaches both sides at -3 dB.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/four_track_100ms.wav"
    );
    let (frames, _) = decode_file(path, 0.0, 0, 0).unwrap();
    for side in 0..2 {
        let rms = (frames
            .iter()
            .map(|f| f64::from(f[side]).powi(2))
            .sum::<f64>()
            / frames.len() as f64)
            .sqrt();
        assert!((rms - 0.25).abs() < 0.01, "side {side}: {rms}");
    }
}

#[test]
fn a_short_aiff_count_cuts_no_sound() {
    // The decode trims an AIFF to its declared count only by the 8 bytes its reader overruns:
    // a count its writer left 100 frames short must not cut those frames.
    let bytes = with_u32_after("tagged_100ms.aiff", b"COMM", 0, 10, 700);
    let path = temp_file("short.aiff", &bytes);
    let decoded = decode_file(&path, 0.0, 0, 0);
    let _ = std::fs::remove_file(&path);
    assert!(decoded.unwrap().0.len() >= 800);
}

#[test]
fn the_duration_budget_holds_at_the_rate_it_decodes_at() {
    // A 1.5 s budget judged at the declared 44.1 kHz is about 66k frames, which the 44k frames
    // the core decodes to pass, though they hold the full 2 s. The file declaring its length
    // is refused before decoding; the fragmented copy declares none, so only the budget at
    // the decoded rate can refuse it.
    for name in ["he_aac_440hz.m4a", "he_aac_unknown_length.m4a"] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        assert!(
            matches!(
                decode_file(&path, 1.5, 0, 0),
                Err(vocara_engine::device::DecodeError::TooLong)
            ),
            "{name}"
        );
        assert!(decode_file(&path, 2.5, 0, 0).is_ok(), "{name}");
    }
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

#[test]
fn two_mp3s_joined_end_to_end_decode_whole() {
    // The joined file keeps the first one's header, whose frame count covers only the first:
    // trimmed to that count, the second was dropped without a word.
    let one = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/onset_0500ms.mp3"
    ))
    .unwrap();
    let path = temp_file("joined.mp3", &[one.as_slice(), one.as_slice()].concat());
    let (frames, _) = decode_file(&path, 0.0, 0, 0).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(frames.len() > 88_000, "{} frames", frames.len());
}

#[test]
fn an_adpcm_wav_decodes_to_its_declared_length() {
    // Voice recorders write IMA or Microsoft ADPCM WAVs: no decoder read them. ADPCM codes
    // whole blocks, the last padded, and the length counted in blocks made 100 ms at 8 kHz
    // last 255 ms; the 'fact' chunk declares the real 800 frames.
    for name in ["ima_adpcm_100ms.wav", "ms_adpcm_100ms.wav"] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let (frames, rate) = decode_file(&path, 0.0, 0, 0).unwrap();
        assert_eq!((frames.len(), rate), (800, 8_000), "{name}");
        let rms = (frames.iter().map(|f| f64::from(f[0]).powi(2)).sum::<f64>() / 800.0).sqrt();
        assert!((rms - 0.125 / 2f64.sqrt()).abs() < 0.005, "{name}: {rms}");
    }
}

#[test]
fn a_chained_ogg_keeps_its_short_links() {
    // Two 0.3 s links at 8 kHz, 440 Hz then 1 kHz. Read seekable, symphonia skipped a link
    // whose sound all came in with its headers: only the 1 kHz one decoded.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/short_links.ogg"
    );
    let (frames, rate) = decode_file(path, 0.0, 0, 0).unwrap();
    assert_eq!((frames.len(), rate), (4_800, 8_000));
    let hz = |range: std::ops::Range<usize>| {
        let n = range.len();
        let crossings = range
            .skip(1)
            .filter(|&i| (frames[i - 1][0] < 0.0) != (frames[i][0] < 0.0))
            .count();
        crossings as f64 / 2.0 / (n as f64 / 8_000.0)
    };
    assert!((hz(400..2_000) - 440.0).abs() < 30.0, "{}", hz(400..2_000));
    assert!(
        (hz(2_800..4_400) - 1_000.0).abs() < 30.0,
        "{}",
        hz(2_800..4_400)
    );
}

#[test]
fn two_ogg_links_each_lose_only_their_own_edges() {
    // Chained end to end, as joined files are: the first link's end trim stayed mid-file
    // (the second link starting late) and was cut from the second link's end instead of
    // its own.
    let read = |name: &str| {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    };
    let joined = [read("link_2000ms.ogg"), read("link_1500ms.ogg")].concat();
    let path = temp_file("chain.ogg", &joined);
    let (frames, rate) = decode_file(&path, 0.0, 0, 0).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(rate, 44_100);
    assert!(
        frames.len().abs_diff(154_350) < 200,
        "{} frames",
        frames.len()
    );
}

#[test]
fn a_file_the_probe_admits_at_the_cap_decodes_under_it() {
    // 1 s once its encoder delay and padding are trimmed, 1.045 s with them: the probe
    // reported the trimmed length and the decode judged the untrimmed one, refusing as too
    // long what the import gate had admitted.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/onset_0500ms.mp3"
    );
    assert_eq!(probe_duration_ms(path).unwrap(), Some(1000));
    let (frames, _) = decode_file(path, 1.0, 0, 0).unwrap();
    assert_eq!(frames.len(), 44_100);
}

/// The Ogg page checksum of [bytes] (a page with its checksum field zeroed).
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut c: u32 = 0;
    for &b in bytes {
        c ^= (b as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
    }
    c
}

/// [ogg] with the granule position of its second-to-last page set to [granule] (and that
/// page's checksum redone), as a crafted file sets it.
fn with_granule_before_last(ogg: &[u8], granule: i64) -> Vec<u8> {
    let mut pages = Vec::new();
    let mut at = 0;
    while at < ogg.len() {
        assert_eq!(&ogg[at..at + 4], b"OggS");
        let segments = ogg[at + 26] as usize;
        let body: usize = ogg[at + 27..at + 27 + segments]
            .iter()
            .map(|&s| s as usize)
            .sum();
        pages.push((at, 27 + segments + body));
        at += 27 + segments + body;
    }
    let (page, len) = pages[pages.len() - 2];
    let mut out = ogg.to_vec();
    out[page + 6..page + 14].copy_from_slice(&granule.to_le_bytes());
    out[page + 22..page + 26].copy_from_slice(&[0; 4]);
    let sum = ogg_crc(&out[page..page + len]);
    out[page + 22..page + 26].copy_from_slice(&sum.to_le_bytes());
    out
}

#[test]
fn a_crafted_end_trim_lifts_no_budget() {
    // A last page whose granule runs backwards reads as an end trim of billions of frames:
    // judged on the trimmed length, the decode's every budget rose by as much, and 2 s came
    // back from a decode capped at 1 s.
    let ogg = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/link_2000ms.ogg"
    ))
    .unwrap();
    let path = temp_file("crafted.ogg", &with_granule_before_last(&ogg, 1 << 31));
    let decoded = decode_file(&path, 1.0, 0, 0);
    let _ = std::fs::remove_file(&path);
    if let Ok((frames, _)) = decoded {
        assert!(
            frames.len() <= 44_100,
            "{} frames past a 1 s cap",
            frames.len()
        );
    }
}

/// [fixture] with the big-endian u32 `at` bytes past the first `tag` set to [value], as a
/// crafted file sets it.
fn with_u32_after(fixture: &str, tag: &[u8], nth: usize, at: usize, value: u32) -> Vec<u8> {
    let mut bytes = std::fs::read(format!(
        "{}/tests/fixtures/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let found = (0..bytes.len() - tag.len())
        .filter(|&i| &bytes[i..i + tag.len()] == tag)
        .nth(nth)
        .unwrap();
    bytes[found + at..found + at + 4].copy_from_slice(&value.to_be_bytes());
    bytes
}

#[test]
fn a_crafted_packet_size_is_refused_before_the_decoder_sizes_it() {
    // The decoder allocates the declared frames per packet (times the channels) as it is
    // built, before any budget applies: a version-2 LPCM sample entry or an ALAC cookie
    // declaring 4 billion took the allocation down, aborting the app.
    for (name, bytes) in [
        // constLPCMFramesPerAudioPacket: 64 bytes past the sample entry's type.
        (
            "lpcm.mov",
            with_u32_after("lpcm_96k_20ms.mov", b"lpcm", 0, 64, u32::MAX),
        ),
        // frameLength: the cookie's first field, past the nested atom's type and flags.
        (
            "alac.m4a",
            with_u32_after("alac_100ms.m4a", b"alac", 1, 8, u32::MAX),
        ),
    ] {
        let path = temp_file(name, &bytes);
        let decoded = decode_file(&path, 0.0, 0, 0);
        let _ = std::fs::remove_file(&path);
        assert!(
            matches!(decoded, Err(vocara_engine::device::DecodeError::TooBig)),
            "{name}: {:?}",
            decoded.map(|d| d.0.len())
        );
    }
    // Untouched, both decode.
    for name in ["lpcm_96k_20ms.mov", "alac_100ms.m4a"] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        assert!(decode_file(&path, 0.0, 0, 0).is_ok(), "{name}");
    }
}

#[test]
fn a_table_declaring_more_than_the_file_holds_is_refused_before_the_probe() {
    // The reader reserves a whole sample table from its entry count, during the probe and before
    // reading a single entry: 4 billion entries asked for 16 to 64 GB and aborted the app, the
    // import gate included, since it probes too.
    for (name, bytes) in [
        // Chunk offsets, and sample durations: the count follows the version and flags.
        (
            "stco.mov",
            with_u32_after("lpcm_96k_20ms.mov", b"stco", 0, 8, u32::MAX),
        ),
        (
            "stts.mov",
            with_u32_after("lpcm_96k_20ms.mov", b"stts", 0, 8, u32::MAX),
        ),
        // Sample sizes (a table here, with no constant size ahead of the count).
        (
            "stsz.m4a",
            with_u32_after("alac_100ms.m4a", b"stsz", 0, 12, u32::MAX),
        ),
        (
            "stsc.m4a",
            with_u32_after("alac_100ms.m4a", b"stsc", 0, 8, u32::MAX),
        ),
        // The encoder tag's data atom, read whole into a buffer of its declared size.
        (
            "tag.m4a",
            with_u32_after("alac_100ms.m4a", b"\xa9too", 0, 4, u32::MAX),
        ),
    ] {
        let path = temp_file(name, &bytes);
        let probed = probe_duration_ms(&path);
        let decoded = decode_file(&path, 0.0, 0, 0);
        let _ = std::fs::remove_file(&path);
        assert!(probed.is_err(), "{name}: probe {probed:?}");
        assert!(
            matches!(decoded, Err(vocara_engine::device::DecodeError::TooBig)),
            "{name}: {:?}",
            decoded.map(|d| d.0.len())
        );
    }
    // Untouched, both probe and decode.
    for name in ["lpcm_96k_20ms.mov", "alac_100ms.m4a"] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        assert!(probe_duration_ms(&path).is_ok(), "{name}");
        assert!(decode_file(&path, 0.0, 0, 0).is_ok(), "{name}");
    }
}

#[test]
fn the_audio_track_is_read_when_a_video_track_comes_first() {
    // symphonia's default track is simply the first, which in an MP4 is usually its video: the
    // probe saw no length and the decode failed as an unsupported codec. 100 ms of ALAC behind
    // a 16x16 MPEG-4 frame, and behind an Opus track (a codec symphonia names but has no
    // decoder for here).
    for name in ["video_first_100ms.mp4", "opus_first_100ms.m4a"] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        assert_eq!(probe_duration_ms(&path), Ok(Some(100)), "{name}");
        let (frames, rate) = decode_file(&path, 0.0, 0, 0).unwrap();
        assert_eq!((frames.len(), rate), (4410, 44_100), "{name}");
    }
}

#[test]
fn a_sample_larger_than_the_file_is_refused() {
    // Each packet is read whole into a buffer of its declared size: a size of 4 billion took the
    // allocation down at the first packet, aborting the app. A constant size is screened before
    // the probe, a table entry before the decode.
    for (name, bytes, probes) in [
        // The constant size, ahead of the count.
        (
            "const.m4a",
            with_u32_after("alac_100ms.m4a", b"stsz", 0, 8, 0xFFFF_FFF0),
            false,
        ),
        // The table's first entry, past the count.
        (
            "entry.m4a",
            with_u32_after("alac_100ms.m4a", b"stsz", 0, 16, 0xFFFF_FFF0),
            true,
        ),
    ] {
        let path = temp_file(name, &bytes);
        let probed = probe_duration_ms(&path);
        let decoded = decode_file(&path, 0.0, 0, 0);
        let _ = std::fs::remove_file(&path);
        assert_eq!(probed.is_ok(), probes, "{name}: probe {probed:?}");
        assert!(
            matches!(decoded, Err(vocara_engine::device::DecodeError::TooBig)),
            "{name}: {:?}",
            decoded.map(|d| d.0.len())
        );
    }
}

#[test]
fn an_ogg_comment_longer_than_its_packet_is_refused() {
    // Each comment's buffer is reserved from its length before it is read. The fixture's one
    // comment (the encoder's) has its length 28 bytes into the comment packet, on page 2.
    let mut ogg = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/link_2000ms.ogg"
    ))
    .unwrap();
    let page = 58;
    let body = page + 27 + ogg[page + 26] as usize;
    ogg[body + 28..body + 32].copy_from_slice(&u32::MAX.to_le_bytes());
    let len = body - page
        + ogg[page + 27..body]
            .iter()
            .map(|&s| s as usize)
            .sum::<usize>();
    ogg[page + 22..page + 26].copy_from_slice(&[0; 4]);
    let sum = ogg_crc(&ogg[page..page + len]);
    ogg[page + 22..page + 26].copy_from_slice(&sum.to_le_bytes());
    let path = temp_file("comment.ogg", &ogg);
    let probed = probe_duration_ms(&path);
    let decoded = decode_file(&path, 0.0, 0, 0);
    let _ = std::fs::remove_file(&path);
    assert!(probed.is_err(), "probe {probed:?}");
    assert!(matches!(
        decoded,
        Err(vocara_engine::device::DecodeError::TooBig)
    ));
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
