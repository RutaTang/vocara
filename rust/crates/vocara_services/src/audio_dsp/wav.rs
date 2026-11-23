//! Mono WAV encode/decode for the analysis paths.

use vocara_model::util::dart_num::round_to_i64;

/// Decoded mono PCM plus its sample rate.
///
/// Samples are held as f32: 16-bit PCM and float32 takes (every source our
/// own engine produces) are exactly representable, and the rare
/// float64/24/32-bit-integer WAV rounds to 24-bit mantissa precision — far
/// below any metric's resolution. The halved footprint of the ONE
/// full-length analysis buffer is the dominant analysis-memory term; all
/// windowed math downstream still runs in doubles.
#[derive(Clone, Debug, PartialEq)]
pub struct PcmAudio {
    pub samples: Vec<f32>,
    pub sample_rate: i64,
}

impl PcmAudio {
    pub fn new(samples: Vec<f32>, sample_rate: i64) -> Self {
        PcmAudio {
            samples,
            sample_rate,
        }
    }
}

/// Encodes mono [samples] (in [-1, 1]) as a 16-bit PCM WAV: each sample
/// scaled by 32767, rounded half away from zero and clamped to the 16-bit
/// range. (Dart threw on a NaN or infinite sample; here NaN writes 0 and an
/// infinity the rail it points at.)
pub fn write_wav_mono<S: Copy + Into<f64>>(samples: &[S], sample_rate: i64) -> Vec<u8> {
    let n = samples.len();
    let data_size = n as i64 * 2;
    let mut b = Vec::with_capacity(44 + n * 2);
    // ByteData.setUint32/16 keep the value's low bits.
    let u32_le = |b: &mut Vec<u8>, v: i64| b.extend_from_slice(&(v as u32).to_le_bytes());
    let u16_le = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
    b.extend_from_slice(b"RIFF");
    u32_le(&mut b, 36 + data_size);
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    u32_le(&mut b, 16);
    u16_le(&mut b, 1);
    u16_le(&mut b, 1);
    u32_le(&mut b, sample_rate);
    u32_le(&mut b, sample_rate * 2);
    u16_le(&mut b, 2);
    u16_le(&mut b, 16);
    b.extend_from_slice(b"data");
    u32_le(&mut b, data_size);
    for &s in samples {
        let v = round_to_i64(s.into() * 32767.0).clamp(-32768, 32767);
        b.extend_from_slice(&(v as i16).to_le_bytes());
    }
    b
}

/// Parses an uncompressed WAV into mono samples: 16-bit PCM (legacy takes
/// and imports) or 32-bit IEEE float (what the recording engine writes —
/// values may exceed [-1, 1] and are passed through verbatim), channels
/// averaged. WAVE_FORMAT_EXTENSIBLE headers are read through their
/// SubFormat. Returns `None` for anything else (e.g. compressed m4a/mp3).
pub fn read_wav_mono(bytes: &[u8]) -> Option<PcmAudio> {
    let len = bytes.len();
    if len < 44 {
        return None;
    }
    let tag = |o: usize| &bytes[o..o + 4];
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    if tag(0) != b"RIFF" || tag(8) != b"WAVE" {
        return None;
    }

    let (mut format, mut channels, mut sample_rate, mut bits) = (1u16, 1u16, 44100u32, 16u16);
    let mut data: Option<(usize, usize)> = None;

    let mut offset = 12usize;
    while offset + 8 <= len {
        let id = tag(offset);
        let size = u32_at(offset + 4) as usize;
        let body = offset + 8;
        if id == b"fmt " && body + 16 <= len {
            format = u16_at(body);
            channels = u16_at(body + 2);
            sample_rate = u32_at(body + 4);
            bits = u16_at(body + 14);
            // WAVE_FORMAT_EXTENSIBLE (e.g. AVFoundation recordings): the real
            // format tag lives in the SubFormat GUID's first two bytes.
            if format == 0xFFFE && body + 26 <= len {
                format = u16_at(body + 24);
            }
        } else if id == b"data" {
            data = Some((body, size));
            break;
        }
        offset = body + size + (size & 1); // chunks are word-aligned
    }

    let is_pcm16 = format == 1 && bits == 16;
    let is_float32 = format == 3 && bits == 32;
    let (data_offset, data_len) = data?;
    if (!is_pcm16 && !is_float32) || channels < 1 || sample_rate == 0 {
        return None;
    }

    let bytes_per_sample = if is_pcm16 { 2 } else { 4 };
    let channels = usize::from(channels);
    let total = data_len.min(len - data_offset) / bytes_per_sample;
    let frames = total / channels;
    let mut out = vec![0.0f32; frames];
    let mut p = data_offset;
    for o in &mut out {
        let mut sum = 0.0f64;
        for _ in 0..channels {
            if is_pcm16 {
                sum += f64::from(i16::from_le_bytes([bytes[p], bytes[p + 1]])) / 32768.0;
            } else {
                // Foreign float WAVs can carry NaN/inf samples; one would
                // poison every FFT/metric downstream. (Engine recordings are
                // already sanitized on write.) Read them as silence.
                let v = f64::from(f32::from_le_bytes([
                    bytes[p],
                    bytes[p + 1],
                    bytes[p + 2],
                    bytes[p + 3],
                ]));
                sum += if v.is_finite() { v } else { 0.0 };
            }
            p += bytes_per_sample;
        }
        *o = (sum / channels as f64) as f32;
        // Judge finiteness on the NARROWED value: a multi-channel sum of large
        // finite floats can overflow the f32 store even though `sum` itself
        // was finite in double precision.
        if !o.is_finite() {
            *o = 0.0;
        }
    }
    Some(PcmAudio::new(out, i64::from(sample_rate)))
}
