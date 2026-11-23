//! The WAV codec: audio_wav_test.dart, spectrogram_test.dart's PCM16
//! round trip, and the reference-decoder halves of spectrogram_gate_test.dart
//! ("readWavMonoStreaming equals the reference decoder": the streaming
//! reader and the admission gate are spectrogram_service's, ported with it)
//! and audio_edit_service_test.dart (EXTENSIBLE and odd-fmt files the split
//! and merge produce; the edits themselves are audio_edit_service's).

use super::super::{read_wav_mono, write_wav_mono};
use super::support::assert_close;

/// A RIFF/WAVE builder for hand-made headers.
#[derive(Default)]
struct Riff(Vec<u8>);

impl Riff {
    fn str(&mut self, s: &str) -> &mut Self {
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
}

const PCM_SUBFORMAT_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// A canonical 16-bit PCM WAV of [samples] (spectrogram_test's `_wav`).
fn pcm16_wav(samples: &[i16], sr: u32) -> Vec<u8> {
    let data_size = samples.len() as u32 * 2;
    let mut b = Riff::default();
    b.str("RIFF")
        .u32(36 + data_size)
        .str("WAVE")
        .str("fmt ")
        .u32(16);
    b.u16(1).u16(1).u32(sr).u32(sr * 2).u16(2).u16(16);
    b.str("data").u32(data_size);
    for s in samples {
        b.bytes(&s.to_le_bytes());
    }
    b.0
}

/// A mono float32 WAV of raw little-endian [data] at 48 kHz.
fn float32_wav(data: &[u8]) -> Vec<u8> {
    let mut b = Riff::default();
    b.str("RIFF").u32(36 + data.len() as u32).str("WAVE");
    b.str("fmt ")
        .u32(16)
        .u16(3)
        .u16(1)
        .u32(48000)
        .u32(48000 * 4)
        .u16(4)
        .u16(32);
    b.str("data").u32(data.len() as u32).bytes(data);
    b.0
}

#[test]
fn write_wav_mono_read_wav_mono_round_trips_samples_and_sample_rate() {
    let sr = 44100;
    let samples: Vec<f64> = (0..256)
        .map(|i| 0.7 * (f64::from(i) / 255.0 * 2.0 - 1.0))
        .collect();
    let pcm = read_wav_mono(&write_wav_mono(&samples, sr)).expect("parses");
    assert_eq!(pcm.sample_rate, sr);
    assert_eq!(pcm.samples.len(), samples.len());
    for (i, (&got, &want)) in pcm.samples.iter().zip(&samples).enumerate() {
        assert_close(f64::from(got), want, 2e-4, &format!("sample {i}"));
    }
}

#[test]
fn concatenating_wav_parts_preserves_total_length_and_boundary_samples() {
    // Mirrors AudioEditService.concatWav: split into two halves, glue back.
    let sr = 16000;
    let a = vec![0.3f64; 1000];
    let b = vec![-0.4f64; 500];
    let pa = read_wav_mono(&write_wav_mono(&a, sr)).unwrap();
    let pb = read_wav_mono(&write_wav_mono(&b, sr)).unwrap();
    let merged: Vec<f32> = pa.samples.iter().chain(&pb.samples).copied().collect();
    let round = read_wav_mono(&write_wav_mono(&merged, sr)).unwrap();
    assert_eq!(round.samples.len(), 1500);
    assert_close(f64::from(round.samples[0]), 0.3, 2e-4, "first");
    assert_close(f64::from(round.samples[1499]), -0.4, 2e-4, "last");
}

#[test]
fn read_wav_mono_round_trips_16_bit_pcm() {
    let bytes = pcm16_wav(&[0, 16384, -16384, 32767, -32768, 0], 44100);
    let pcm = read_wav_mono(&bytes).expect("parses");
    assert_eq!(pcm.sample_rate, 44100);
    assert_eq!(pcm.samples.len(), 6);
    assert_close(f64::from(pcm.samples[3]), 32767.0 / 32768.0, 1e-4, "s[3]");
}

#[test]
fn write_wav_mono_clamps_to_the_16_bit_range_and_rounds_half_away_from_zero() {
    let bytes = write_wav_mono(&[1.5f64, -1.5, 0.5 / 32767.0, -0.5 / 32767.0], 8000);
    assert_eq!(bytes.len(), 44 + 8);
    let s = |i: usize| i16::from_le_bytes([bytes[44 + 2 * i], bytes[45 + 2 * i]]);
    assert_eq!((s(0), s(1), s(2), s(3)), (32767, -32768, 1, -1));
}

#[test]
fn the_reference_decoder_reads_mono_pcm16() {
    // spectrogram_gate_test: "mono PCM16".
    let samples: Vec<f64> = (0..500)
        .map(|i| (f64::from(i) / 25.0).sin() * 0.8)
        .collect();
    let pcm = read_wav_mono(&write_wav_mono(&samples, 8000)).expect("parses");
    assert_eq!(pcm.sample_rate, 8000);
    assert_eq!(pcm.samples.len(), 500);
    // Written as round(s·32767), read back over 32768.
    for (i, (&got, &s)) in pcm.samples.iter().zip(&samples).enumerate() {
        let want = ((s * 32767.0).round() / 32768.0) as f32;
        assert_eq!(got, want, "sample {i}");
    }
}

#[test]
fn the_reference_decoder_downmixes_an_extensible_stereo_pcm16_with_extra_chunks() {
    // spectrogram_gate_test: interleaved stereo with distinct channels, a
    // LIST chunk before fmt, so the chunk walk and the downmix averaging are
    // genuinely exercised.
    let mut data = Vec::new();
    let mut want = Vec::new();
    for i in 0..400 {
        let l = ((f64::from(i) / 10.0).sin() * 12000.0).round() as i16;
        let r = ((f64::from(i) / 10.0).cos() * 9000.0).round() as i16;
        data.extend_from_slice(&l.to_le_bytes());
        data.extend_from_slice(&r.to_le_bytes());
        want.push(((f64::from(l) / 32768.0 + f64::from(r) / 32768.0) / 2.0) as f32);
    }
    let mut b = Riff::default();
    b.str("RIFF")
        .u32(4 + 12 + (8 + 40) + (8 + data.len() as u32))
        .str("WAVE");
    b.str("LIST").u32(4).bytes(&[0; 4]);
    b.str("fmt ")
        .u32(40)
        .u16(0xFFFE)
        .u16(2)
        .u32(8000)
        .u32(8000 * 4)
        .u16(4)
        .u16(16);
    b.u16(22).u16(16).u32(3).u16(1).bytes(&PCM_SUBFORMAT_TAIL);
    b.str("data").u32(data.len() as u32).bytes(&data);
    let pcm = read_wav_mono(&b.0).expect("EXTENSIBLE PCM16 parses");
    assert_eq!(pcm.sample_rate, 8000);
    assert_eq!(pcm.samples, want);
}

#[test]
fn the_reference_decoder_scrubs_non_finite_float_samples_to_zero() {
    let mut data = Vec::new();
    for v in [
        0.5f32,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.25,
        0.125,
    ] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    let pcm = read_wav_mono(&float32_wav(&data)).expect("float32 parses");
    assert_eq!(pcm.samples, [0.5, 0.0, 0.0, 0.0, -0.25, 0.125]);
}

#[test]
fn the_reference_decoder_passes_float32_takes_through_over_unity_included() {
    let mut data = Vec::new();
    let mut want = Vec::new();
    for i in 0..300 {
        // Includes an over-unity peak, legal in float takes.
        let v = ((f64::from(i) / 7.0).sin() * 1.2) as f32;
        data.extend_from_slice(&v.to_le_bytes());
        want.push(v);
    }
    let pcm = read_wav_mono(&float32_wav(&data)).expect("float32 parses");
    assert_eq!(pcm.sample_rate, 48000);
    assert_eq!(pcm.samples, want);
}

#[test]
fn the_reference_decoder_refuses_what_it_cannot_decode() {
    // Too short, not RIFF/WAVE, no data chunk, and a format with no branch
    // (integer PCM at 64 bits).
    assert!(read_wav_mono(&[0; 43]).is_none());
    let mut not_wave = pcm16_wav(&[1, 2, 3], 8000);
    not_wave[8..12].copy_from_slice(b"AVI ");
    assert!(read_wav_mono(&not_wave).is_none());
    let mut b = Riff::default();
    b.str("RIFF").u32(36 + 80).str("WAVE");
    b.str("fmt ")
        .u32(16)
        .u16(1)
        .u16(1)
        .u32(48000)
        .u32(48000 * 8)
        .u16(8)
        .u16(64);
    b.str("data").u32(80).bytes(&[0; 80]);
    assert!(read_wav_mono(&b.0).is_none());
    let mut no_data = Riff::default();
    no_data.str("RIFF").u32(36).str("WAVE");
    no_data
        .str("fmt ")
        .u32(16)
        .u16(1)
        .u16(1)
        .u32(8000)
        .u32(16000)
        .u16(2)
        .u16(16);
    no_data.str("junk").u32(8).bytes(&[0; 8]);
    assert!(read_wav_mono(&no_data.0).is_none());
}

#[test]
fn read_wav_mono_accepts_extensible_pcm16() {
    // audio_edit_service_test: AVFoundation can write WAVs tagged 0xFFFE
    // rather than plain PCM.
    let sr = 8000u32;
    let samples: Vec<i16> = (0..sr).map(|i| ((i % 200) * 100) as i16 - 10000).collect();
    let mut data = Vec::new();
    for s in &samples {
        data.extend_from_slice(&s.to_le_bytes());
    }
    let mut b = Riff::default();
    b.str("RIFF")
        .u32(4 + (8 + 40) + (8 + data.len() as u32))
        .str("WAVE");
    b.str("fmt ")
        .u32(40)
        .u16(0xFFFE)
        .u16(1)
        .u32(sr)
        .u32(sr * 2)
        .u16(2)
        .u16(16);
    b.u16(22).u16(16).u32(0).u16(1).bytes(&PCM_SUBFORMAT_TAIL);
    b.str("data").u32(data.len() as u32).bytes(&data);
    let pcm = read_wav_mono(&b.0).expect("readWavMono must accept EXTENSIBLE PCM16");
    assert_eq!(pcm.samples.len(), sr as usize);
    for (i, (&got, &want)) in pcm.samples.iter().zip(&samples).enumerate() {
        assert_eq!(
            (f64::from(got) * 32768.0).round() as i16,
            want,
            "sample {i}"
        );
    }
}

#[test]
fn read_wav_mono_walks_a_word_aligned_odd_fmt_chunk() {
    // audio_edit_service_test: an odd-size fmt body is followed by a pad
    // byte; the data chunk sits at the word-aligned offset.
    let samples: Vec<i16> = (0..1000).collect();
    let mut data = Vec::new();
    for s in &samples {
        data.extend_from_slice(&s.to_le_bytes());
    }
    let mut b = Riff::default();
    b.str("RIFF")
        .u32(4 + (8 + 17 + 1) + (8 + data.len() as u32))
        .str("WAVE");
    b.str("fmt ")
        .u32(17)
        .u16(1)
        .u16(1)
        .u32(1000)
        .u32(2000)
        .u16(2)
        .u16(16);
    b.bytes(&[0, 0]); // the 17th fmt byte, then the word-alignment pad
    b.str("data").u32(data.len() as u32).bytes(&data);
    assert_eq!(&b.0[38..42], b"data");
    let pcm = read_wav_mono(&b.0).expect("the odd-fmt WAV parses");
    assert_eq!(pcm.samples.len(), 1000);
    assert_eq!((f64::from(pcm.samples[250]) * 32768.0).round() as i16, 250);
}
