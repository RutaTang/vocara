//! Minimal WAV writers: a streaming [`WavStream`] (32-bit IEEE float — the recording format)
//! for live takes, plus one-shot f32/i16 writers for punch-in splices and the offline render
//! harness. The streaming header sizes are patched in place periodically while recording and
//! stamped exactly on finalize.
//!
//! # Why recordings are 32-bit float
//!
//! Industry practice for capture is "nothing baked in": with float samples the record gain is a
//! pure scale factor and levels past 0 dBFS are stored losslessly instead of being clipped or
//! soft-limited into the file (a tanh ceiling used to live in the capture path — irreversible
//! harmonic distortion on every loud note). Playback is protected by the master bus's safety
//! limiter, so an over-unity take tames at the speaker, not in the stored audio. The analyses
//! also read the undistorted waveform. Legacy 16-bit takes keep decoding everywhere (symphonia
//! and the Dart readers accept both).

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// WAVE format tags.
const FMT_PCM: u16 = 1;
const FMT_IEEE_FLOAT: u16 = 3;

/// Bytes per stored sample in the streaming (f32) format.
const F32_BYTES: u64 = 4;

/// Write a canonical 44-byte WAV header (16-byte `fmt ` immediately followed by `data`).
/// `data_bytes` may be a placeholder (patched on finalize). The same canonical layout is used
/// for both PCM16 and float32 — readers that require a `fact` chunk for non-PCM are not among
/// our consumers (symphonia, CoreAudio, ffmpeg and the in-app Dart parsers all accept this).
fn write_header<W: Write>(
    w: &mut W,
    format: u16,
    bits: u16,
    channels: u16,
    sample_rate: u32,
    data_bytes: u32,
) -> io::Result<()> {
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&format.to_le_bytes())?;
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

/// A streaming 32-bit float WAV writer: writes a placeholder header on `create`, appends
/// samples incrementally, patches the RIFF + data sizes IN PLACE about once per second of audio
/// (see [`WavStream::sync_header_if_due`]), and stamps the exact final sizes on `finalize`.
/// Used by the recording writer thread (never the audio thread).
///
/// The periodic patch is what makes a TRUE hard kill (force quit / crash / `kill -9`, where no
/// finalize ever runs) survivable: the file on disk is a valid WAV covering everything written
/// up to the last patch, so the boot-time staging sweep can recover the take instead of finding
/// a header that still claims zero data bytes.
pub struct WavStream {
    w: BufWriter<File>,
    /// Mono sample count. u64 so a multi-hour recording never overflows (a u32 wrapped at
    /// ~12.4 h of float samples, corrupting the patched header sizes).
    samples: u64,
    /// Samples per second, kept to derive the ~1 s periodic header-patch cadence.
    sample_rate: u32,
    /// Value of [`samples`] at the last in-place header patch (0 = header still says empty).
    patched_at: u64,
}

impl WavStream {
    pub fn create<P: AsRef<Path>>(path: P, channels: u16, sample_rate: u32) -> io::Result<Self> {
        Self::from_file(File::create(path)?, channels, sample_rate)
    }

    /// Like [`create`], but O_EXCL: fails when the path already exists and
    /// never follows a pre-placed symlink. The transactional temp-file writers
    /// use this — a symlink planted at a predictable temp name once redirected
    /// the create onto the splice's own INPUT, truncating it to a 44-byte
    /// header before it was read.
    pub fn create_new<P: AsRef<Path>>(
        path: P,
        channels: u16,
        sample_rate: u32,
    ) -> io::Result<Self> {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Self::from_file(f, channels, sample_rate)
    }

    fn from_file(f: File, channels: u16, sample_rate: u32) -> io::Result<Self> {
        let mut w = BufWriter::new(f);
        write_header(&mut w, FMT_IEEE_FLOAT, 32, channels, sample_rate, 0)?;
        // Hand the placeholder header to the OS right away: from creation on, the file
        // parses as a (so far empty) WAV even if the process is hard-killed before the
        // first periodic patch — never a 0-byte or headerless file.
        w.flush()?;
        Ok(WavStream {
            w,
            samples: 0,
            sample_rate,
            patched_at: 0,
        })
    }

    /// Append one f32 sample as 32-bit float. NOT clamped: float storage keeps levels past
    /// 0 dBFS losslessly (the whole point of the format). Non-finite values are stored as 0 so
    /// one bad sample can never poison downstream decoders/analyses.
    #[inline]
    pub fn write_sample(&mut self, s: f32) -> io::Result<()> {
        let v = if s.is_finite() { s } else { 0.0 };
        self.w.write_all(&v.to_le_bytes())?;
        self.samples += 1;
        Ok(())
    }

    /// The (riff_size, data_size) header fields covering `samples` float samples.
    /// 4 bytes per sample. The counter is u64 (no overflow); the WAV header fields
    /// are u32, so clamp to u32::MAX — a >4 GB stream isn't representable in plain
    /// RIFF/WAVE (it would need RF64), but the header still agrees with the bytes up to 4 GB.
    fn header_sizes(samples: u64) -> (u32, u32) {
        let data_bytes = samples.saturating_mul(F32_BYTES);
        let data_size = data_bytes.min(u32::MAX as u64) as u32;
        let riff_size = (36u64 + data_bytes).min(u32::MAX as u64) as u32;
        (riff_size, data_size)
    }

    /// Patch the RIFF + data chunk sizes in place to cover every sample written so far, then
    /// return the write cursor to the end of the data chunk. `BufWriter`'s `Seek` impl flushes
    /// the buffered samples before moving the cursor, so the patched sizes never claim bytes
    /// that were not actually handed to the OS. No fsync: the target failure is a killed
    /// PROCESS (the page cache survives it), not power loss, so an fsync per patch would cost
    /// latency for nothing.
    fn sync_header(&mut self) -> io::Result<()> {
        let (riff_size, data_size) = Self::header_sizes(self.samples);
        self.w.seek(SeekFrom::Start(4))?;
        self.w.write_all(&riff_size.to_le_bytes())?;
        self.w.seek(SeekFrom::Start(40))?;
        self.w.write_all(&data_size.to_le_bytes())?;
        // Back to the end of the 44-byte header + data payload for the next append.
        self.w
            .seek(SeekFrom::Start(44 + self.samples.saturating_mul(F32_BYTES)))?;
        self.patched_at = self.samples;
        Ok(())
    }

    /// Patch the header in place (see [`sync_header`](Self::sync_header)) once at least ~1 s of
    /// audio accumulated since the last patch; a cheap counter compare otherwise. Called by the
    /// recording writer thread after each drain batch — never the RT callback — so a TRUE hard
    /// kill (force quit / crash / `kill -9`, where `finalize` never runs) still leaves a valid
    /// WAV covering the take up to the last patch. `finalize` keeps stamping the exact final
    /// sizes on every graceful path.
    pub fn sync_header_if_due(&mut self) -> io::Result<()> {
        if self.samples - self.patched_at >= u64::from(self.sample_rate.max(1)) {
            self.sync_header()?;
        }
        Ok(())
    }

    /// Flush + patch the exact final header sizes and close the file.
    pub fn finalize(self) -> io::Result<()> {
        let WavStream { w, samples, .. } = self;
        let mut f = w.into_inner().map_err(|e| e.into_error())?;
        let (riff_size, data_size) = Self::header_sizes(samples);
        f.seek(SeekFrom::Start(4))?;
        f.write_all(&riff_size.to_le_bytes())?;
        f.seek(SeekFrom::Start(40))?;
        f.write_all(&data_size.to_le_bytes())?;
        f.flush()
    }
}

/// Write interleaved f32 `samples` as a 32-bit float WAV (the recording/take format).
/// Non-finite samples are stored as 0; everything else verbatim (no clamp).
pub fn write_wav_f32<P: AsRef<Path>>(
    path: P,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let data_bytes = (samples.len() as u64).saturating_mul(F32_BYTES);
    write_header(
        &mut w,
        FMT_IEEE_FLOAT,
        32,
        channels,
        sample_rate,
        data_bytes.min(u32::MAX as u64) as u32,
    )?;
    for &s in samples {
        let v = if s.is_finite() { s } else { 0.0 };
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}

/// Write interleaved f32 `samples` (range ~[-1, 1]) as a 16-bit PCM WAV. Kept for the offline
/// render harness / golden fixtures, where a compact universally-readable file is the point.
pub fn write_wav_i16<P: AsRef<Path>>(
    path: P,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let data_bytes = (samples.len() * 2) as u32;
    write_header(&mut w, FMT_PCM, 16, channels, sample_rate, data_bytes)?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_new_refuses_a_pre_placed_symlink_and_leaves_its_target_intact() {
        // The transactional temp writers rely on this: a symlink planted at a
        // predictable temp name once redirected the create onto the splice's
        // own input, truncating a real take to a 44-byte header before it was
        // read. O_EXCL fails on ANY existing entry, symlink included, without
        // following it.
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let target = dir.join(format!("vocara_wav_symlink_target_{pid}.wav"));
        let link = dir.join(format!("vocara_wav_symlink_link_{pid}.tmp"));
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&link);
        write_wav_f32(&target, &[0.1; 96], 2, 48_000).unwrap();
        let before = std::fs::metadata(&target).unwrap().len();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(
            WavStream::create_new(&link, 2, 48_000).is_err(),
            "create_new must refuse a pre-existing (symlink) entry"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().len(),
            before,
            "the symlink's target must be byte-identical after the refusal"
        );

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
    }
}
