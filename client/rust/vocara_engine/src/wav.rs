//! Minimal 16-bit PCM WAV writer for the offline render harness + golden-file fixtures,
//! plus a streaming [`WavStream`] for live recording (header sizes patched in place
//! periodically while streaming, and stamped exactly on finalize).

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// Write a 44-byte PCM WAV header. `data_bytes` may be a placeholder (patched on finalize).
fn write_header<W: Write>(
    w: &mut W,
    channels: u16,
    sample_rate: u32,
    data_bytes: u32,
) -> io::Result<()> {
    let bits: u16 = 16;
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

/// A streaming 16-bit PCM WAV writer: writes a placeholder header on `create`, appends samples
/// incrementally, patches the RIFF + data sizes IN PLACE about once per second of audio (see
/// [`WavStream::sync_header_if_due`]), and stamps the exact final sizes on `finalize`. Used by
/// the recording writer thread (never the audio thread).
///
/// The periodic patch is what makes a TRUE hard kill (force quit / crash / `kill -9`, where no
/// finalize ever runs) survivable: the file on disk is a valid WAV covering everything written
/// up to the last patch, so the boot-time staging sweep can recover the take instead of finding
/// a header that still claims zero data bytes.
pub struct WavStream {
    w: BufWriter<File>,
    /// Mono sample count. u64 so a multi-hour recording never overflows (a u32 wrapped at
    /// ~24.8 h / panicked in debug at ~12.4 h, corrupting the patched header sizes).
    samples: u64,
    /// Samples per second, kept to derive the ~1 s periodic header-patch cadence.
    sample_rate: u32,
    /// Value of [`samples`] at the last in-place header patch (0 = header still says empty).
    patched_at: u64,
}

impl WavStream {
    pub fn create<P: AsRef<Path>>(path: P, channels: u16, sample_rate: u32) -> io::Result<Self> {
        let mut w = BufWriter::new(File::create(path)?);
        write_header(&mut w, channels, sample_rate, 0)?;
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

    /// Append one f32 sample (range ~[-1, 1]) as 16-bit PCM.
    #[inline]
    pub fn write_sample(&mut self, s: f32) -> io::Result<()> {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        self.w.write_all(&v.to_le_bytes())?;
        self.samples += 1;
        Ok(())
    }

    /// The (riff_size, data_size) header fields covering `samples` 16-bit samples.
    /// 2 bytes per sample. The counter is u64 (no overflow); the WAV header fields
    /// are u32, so clamp to u32::MAX — a >4 GB stream isn't representable in plain
    /// RIFF/WAVE (it would need RF64), but the header still agrees with the bytes up to 4 GB.
    fn header_sizes(samples: u64) -> (u32, u32) {
        let data_bytes = samples.saturating_mul(2);
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
        self.w.seek(SeekFrom::Start(44 + self.samples.saturating_mul(2)))?;
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

/// Write interleaved f32 `samples` (range ~[-1, 1]) as a 16-bit PCM WAV.
pub fn write_wav_i16<P: AsRef<Path>>(
    path: P,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let bits: u16 = 16;
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_bytes = (samples.len() * 2) as u32;

    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;

    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}
