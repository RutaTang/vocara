//! Non-destructive clip edits on WAV files (port of
//! `lib/data/services/audio_edit_service.dart`): the head trim a take's
//! placement needs, split and merge as byte splices, and the engine's
//! re-encoding splice (punch-in) for everything a byte splice cannot join.
//!
//! The Dart service ran every file walk on a background isolate; here each
//! call blocks its caller, and the app runs it off the UI thread like every
//! other store and service call.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use vocara_model::util::dart_num::round_to_i64;
use vocara_model::util::dart_path;
use vocara_store::db::durable_fs;

use crate::audio_file_service::{
    copy_bounded, probe_wav, AudioFileError, AudioFiles, MAX_IMPORT_BYTES,
};
use crate::engine::media::{MediaError, MAX_PLAYBACK_DURATION};
use crate::engine::{self, ENGINE_RATE};

/// Why an edit was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum AudioEditError {
    /// A refusal with the message the UI shows (Dart's `FormatException`).
    #[error("{0}")]
    Format(String),
    /// The typed refusal a decode named (Dart's `MediaTooLongException`,
    /// `MediaUnsupportedException`, `MediaReadException`), or no engine to
    /// splice with.
    #[error(transparent)]
    Media(#[from] MediaError),
    /// A practice id that is not a well-formed path component (Dart's
    /// `ArgumentError` from `newRecordingPath`).
    #[error("not a well-formed id: {0}")]
    InvalidId(String),
    /// A file system failure (Dart's `FileSystemException`).
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<AudioFileError> for AudioEditError {
    fn from(e: AudioFileError) -> Self {
        match e {
            AudioFileError::InvalidId(id) => AudioEditError::InvalidId(id),
            AudioFileError::Format(message) => AudioEditError::Format(message),
            AudioFileError::Io(e) => AudioEditError::Io(e),
        }
    }
}

/// The edit service's result type.
pub type Result<T> = std::result::Result<T, AudioEditError>;

/// The two halves [`AudioEditService::split_wav`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPaths {
    pub first_path: String,
    pub second_path: String,
}

/// Parsed layout of a WAV file: the original `fmt ` chunk body (preserved so
/// the exact sample format is kept on re-emit) plus where the PCM data lives.
#[derive(Debug, Clone)]
struct WavLayout {
    fmt: Vec<u8>,
    sample_rate: i64,
    /// Bytes per frame (channels * bits/8).
    block_align: i64,
    data_start: i64,
    data_len: i64,
}

/// Little-endian u16 at [off] within a `fmt ` chunk body (-1 if out of range).
fn fmt_u16(f: &[u8], off: usize) -> i64 {
    if off + 1 < f.len() {
        i64::from(f[off]) | (i64::from(f[off + 1]) << 8)
    } else {
        -1
    }
}

/// The effective sample TYPE: for WAVE_FORMAT_EXTENSIBLE (0xFFFE) it's the
/// SubFormat GUID's leading format code at fmt offset 24; otherwise the format
/// tag at offset 0 (1 = integer PCM, 3 = IEEE float).
fn eff_fmt(f: &[u8]) -> i64 {
    let tag = fmt_u16(f, 0);
    if tag == 0xFFFE {
        fmt_u16(f, 24)
    } else {
        tag
    }
}

fn u32_le(b: &[u8], at: usize) -> i64 {
    i64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

fn u16_le(b: &[u8], at: usize) -> i64 {
    i64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

/// Whether an effective format, rate, channel count, bit depth and frame
/// size are those of uncompressed samples whose `blockAlign` is a true PCM
/// frame: the admission rule every parser here shares.
///
/// Only uncompressed samples can be byte-spliced with PCM frame math. A
/// compressed RIFF payload (ADPCM / mp3-in-WAV / mu-law) parses the same
/// chunks, but its blockAlign is a codec block, not a PCM frame — splitting
/// it mid-block would produce silent nonsense. And an uncompressed
/// blockAlign is exactly channels * ceil(bits / 8); anything else is a lying
/// header whose frame math would shear every frame after the first cut.
fn admits_pcm_frames(fmt: &[u8], sample_rate: i64, block_align: i64) -> bool {
    let eff = eff_fmt(fmt);
    let channels = fmt_u16(fmt, 2);
    let bits = fmt_u16(fmt, 14);
    (eff == 1 || eff == 3)
        && sample_rate > 0
        && channels > 0
        && bits > 0
        && block_align == channels * ((bits + 7) / 8)
}

/// Scans a WAV's chunks generically (tolerating extra chunks and
/// WAVE_FORMAT_EXTENSIBLE). Returns `None` if it isn't an uncompressed WAV.
fn parse_wav(bytes: &[u8]) -> Option<WavLayout> {
    let len = bytes.len() as i64;
    if len < 12 {
        return None;
    }
    let tag = |o: i64| &bytes[o as usize..o as usize + 4];
    if tag(0) != b"RIFF" || tag(8) != b"WAVE" {
        return None;
    }

    let mut fmt: Option<Vec<u8>> = None;
    let (mut sample_rate, mut block_align, mut data_start, mut data_len) =
        (0i64, 0i64, -1i64, 0i64);
    let mut offset = 12i64;
    while offset + 8 <= len {
        let id = tag(offset);
        let size = u32_le(bytes, offset as usize + 4);
        let body = offset + 8;
        if id == b"fmt " && body + 16 <= len {
            let end = (body + size).min(len);
            fmt = Some(bytes[body as usize..end as usize].to_vec());
            sample_rate = u32_le(bytes, body as usize + 4);
            block_align = u16_le(bytes, body as usize + 12);
        } else if id == b"data" {
            data_start = body;
            data_len = size.min(len - body);
            break;
        }
        offset = body + size + (size & 1); // chunks are word-aligned
    }
    let fmt = fmt?;
    if data_start < 0 || block_align <= 0 || sample_rate <= 0 {
        return None;
    }
    // Only uncompressed samples, with a blockAlign that is a true PCM frame
    // (see admits_pcm_frames). A refusal routes such files to the callers'
    // "compressed audio" refusal.
    if !admits_pcm_frames(&fmt, sample_rate, block_align) {
        return None;
    }
    Some(WavLayout {
        fmt,
        sample_rate,
        block_align,
        data_start,
        data_len,
    })
}

/// Testable face of [`compatible_for_merge`]: parses two whole-file byte
/// buffers and answers whether they can be BYTE-spliced under the first's
/// header (false routes concat through the decode → re-encode path); `None`
/// when either buffer is not an uncompressed WAV.
pub fn wav_compatible_for_merge(a: &[u8], b: &[u8]) -> Option<bool> {
    let wa = parse_wav(a)?;
    let wb = parse_wav(b)?;
    Some(compatible_for_merge(&wa, &wb))
}

/// Whether two WAVs can be concatenated losslessly under ONE fmt header:
/// same sample rate, channel count, bit depth, effective sample type, and
/// (for EXTENSIBLE) valid bits + channel mask. The format TAG may differ
/// (plain PCM vs EXTENSIBLE of the same actual format).
fn compatible_for_merge(a: &WavLayout, b: &WavLayout) -> bool {
    // Comparing the effective format — not just rate/channels/bits — stops a
    // 32-bit int WAV and a 32-bit float WAV (same rate/channels/depth) from
    // merging into reinterpreted noise, while still allowing plain PCM vs
    // EXTENSIBLE-PCM of the same actual format (both resolve to 1).
    if a.sample_rate != b.sample_rate
        || fmt_u16(&a.fmt, 2) != fmt_u16(&b.fmt, 2)
        || fmt_u16(&a.fmt, 14) != fmt_u16(&b.fmt, 14)
        || eff_fmt(&a.fmt) != eff_fmt(&b.fmt)
    {
        return false;
    }
    // EXTENSIBLE extras: the merged file keeps A's header, so B's samples
    // would be reinterpreted under A's valid-bits (24-in-32 vs full 32 is
    // real corruption, not metadata) and rendered under A's channel mask.
    // A plain-fmt file implies validBits == bits and no mask claim.
    let valid_bits = |w: &WavLayout| {
        if w.fmt.len() >= 40 {
            fmt_u16(&w.fmt, 18)
        } else {
            fmt_u16(&w.fmt, 14)
        }
    };
    let mask = |w: &WavLayout| {
        if w.fmt.len() >= 40 {
            fmt_u16(&w.fmt, 20) | (fmt_u16(&w.fmt, 22) << 16)
        } else {
            0
        }
    };
    if valid_bits(a) != valid_bits(b) {
        return false;
    }
    // The mask is compared unconditionally, in both directions. `mask()`
    // already answers 0 for a plain fmt, which is exactly the spec's
    // "speaker positions unspecified" — so a plain stereo and an EXTENSIBLE
    // stereo carrying, say, Left+Centre are genuinely different claims about
    // the same bytes, and the splice would stamp A's claim over B's half.
    // Only comparing when BOTH sides were EXTENSIBLE let that through either
    // way round.
    if mask(a) != mask(b) {
        return false;
    }
    true
}

/// Up to [n] bytes from [file]'s current position (fewer at the end of the
/// file): Dart's `RandomAccessFile.readSync`.
fn read_up_to(file: &mut File, n: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(n.min(4 << 20) as usize);
    Read::by_ref(file).take(n).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Opens [path] for reading the way Dart's `File.openSync` does: a
/// directory is refused at the open, not at the first read.
fn open_for_read(path: &str) -> io::Result<File> {
    let file = File::open(path)?;
    if file.metadata()?.is_dir() {
        return Err(io::Error::from_raw_os_error(libc::EISDIR));
    }
    Ok(file)
}

/// Streamed parse of a WAV's header from an open [file]: the same admission
/// rules as [`parse_wav`], without loading the file (whole-file reads made a
/// long take cost gigabytes across split/concat buffers). `data_len` is
/// clamped to what the file physically holds, like [`parse_wav`]. A read
/// that fails is an `Err`, not a "not a WAV".
fn parse_wav_from_file(file: &mut File) -> io::Result<Option<WavLayout>> {
    let file_len = file.metadata()?.len() as i64;
    file.seek(SeekFrom::Start(0))?;
    let head = read_up_to(file, 12)?;
    if head.len() < 12 || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut fmt: Option<Vec<u8>> = None;
    let (mut data_start, mut data_len) = (-1i64, 0i64);
    let mut offset = 12i64;
    // Bounded walk, mirroring parse_wav's chunk tolerance (see the analysis
    // gate on why 64 covers every real-world metadata pile-up).
    for _ in 0..64 {
        if data_start >= 0 {
            break;
        }
        file.seek(SeekFrom::Start(offset as u64))?;
        let ch = read_up_to(file, 8)?;
        if ch.len() < 8 {
            break;
        }
        let size = u32_le(&ch, 4);
        if &ch[0..4] == b"fmt " && (16..=1024).contains(&size) {
            fmt = Some(read_up_to(file, size as u64)?);
        } else if &ch[0..4] == b"data" {
            data_start = offset + 8;
            data_len = size.min(file_len - data_start);
        }
        offset += 8 + size + (size & 1); // chunks are word-aligned
    }
    let Some(f) = fmt else {
        return Ok(None);
    };
    if f.len() < 16 || data_start < 0 || data_len < 0 {
        return Ok(None);
    }
    let sample_rate = u32_le(&f, 4);
    let block_align = u16_le(&f, 12);
    // Same admission rules as parse_wav: uncompressed samples only, and a
    // blockAlign that is a true PCM frame.
    if !admits_pcm_frames(&f, sample_rate, block_align) {
        return Ok(None);
    }
    Ok(Some(WavLayout {
        fmt: f,
        sample_rate,
        block_align,
        data_start,
        data_len,
    }))
}

/// Why [path] can't be edited when it doesn't parse as a WAV: [not_wav], or,
/// when there is no file at all (its bytes haven't reached this device, or
/// the file was removed), that — not a format it doesn't have.
fn cannot_edit(path: &str, not_wav: &str) -> AudioEditError {
    if Path::new(path).is_file() {
        AudioEditError::Format(not_wav.to_string())
    } else {
        AudioEditError::Format(
            "This take's audio file isn't on this device, so it can't be edited.".to_string(),
        )
    }
}

/// [`parse_wav_from_file`] on a path: opens, parses the header, closes.
/// Cheap (a handful of small reads), so callers may run it on any thread. A
/// path that cannot be opened is `None`; a read that fails is an `Err`.
fn parse_wav_at_path(path: &str) -> io::Result<Option<WavLayout>> {
    let Ok(mut file) = open_for_read(path) else {
        return Ok(None);
    };
    parse_wav_from_file(&mut file)
}

/// Patches the RIFF + data sizes written by [`write_wav_header`] down to
/// [actual_len] (whole-frame bytes, caller-aligned) and truncates the torn
/// tail. Used when a source turned out shorter than its header promised: the
/// up-front header describes the EXPECTED length, and publishing it
/// unpatched means a header claiming bytes the file does not hold.
fn patch_wav_sizes(out: &mut File, fmt: &[u8], actual_len: i64) -> io::Result<()> {
    // Data starts after RIFF(12) + fmt header(8) + fmt body + its
    // word-alignment pad — the exact layout write_wav_header emits.
    let fmt_len = fmt.len() as i64;
    let fmt_pad = fmt_len & 1;
    let data_start = 28 + fmt_len + fmt_pad;
    let pad = actual_len & 1;
    out.set_len((data_start + actual_len) as u64)?;
    if pad == 1 {
        out.seek(SeekFrom::Start((data_start + actual_len) as u64))?;
        out.write_all(&[0])?;
    }
    let mut u32_at = |off: i64, v: i64| -> io::Result<()> {
        out.seek(SeekFrom::Start(off as u64))?;
        // Dart's setUint32 keeps the value's low 32 bits.
        out.write_all(&(v as u32).to_le_bytes())
    };
    u32_at(4, 4 + (8 + fmt_len + fmt_pad) + (8 + actual_len + pad))?;
    u32_at(data_start - 4, actual_len)
}

/// A WAV header (RIFF + preserved fmt + data size).
///
/// RIFF chunks are word-aligned: an odd-length fmt body (the parsers admit
/// any size in [16, 1024]) must be followed by a pad byte, or every chunk
/// after it sits at an odd offset and the aligned walkers — this file's own
/// parsers and symphonia's — read garbage where the next chunk id should be.
fn wav_header(fmt: &[u8], data_len: i64) -> Vec<u8> {
    let fmt_len = fmt.len() as i64;
    let fmt_pad = fmt_len & 1;
    let pad = data_len & 1;
    let mut b = Vec::with_capacity(28 + fmt.len() + 1);
    // Dart's setUint32 keeps the value's low 32 bits.
    let u32 = |b: &mut Vec<u8>, v: i64| b.extend_from_slice(&(v as u32).to_le_bytes());
    b.extend_from_slice(b"RIFF");
    u32(&mut b, 4 + (8 + fmt_len + fmt_pad) + (8 + data_len + pad));
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    u32(&mut b, fmt_len);
    b.extend_from_slice(fmt);
    if fmt_pad == 1 {
        b.push(0);
    }
    b.extend_from_slice(b"data");
    u32(&mut b, data_len);
    b
}

/// Writes [`wav_header`] to [out].
fn write_wav_header(out: &mut File, fmt: &[u8], data_len: i64) -> io::Result<()> {
    out.write_all(&wav_header(fmt, data_len))
}

/// The copy chunk: 4 MiB at a time keeps the peak bounded regardless of the
/// take's length.
const COPY_CHUNK: i64 = 4 << 20;

/// Chunked byte-range copy (4 MiB at a time): the peak stays bounded no
/// matter how long the take is. Returns the bytes actually copied (a
/// truncated-on-disk source yields fewer; callers keep what exists, the same
/// tolerance the whole-file path had).
fn copy_range(src: &mut File, start: i64, len: i64, out: &mut File) -> io::Result<i64> {
    src.seek(SeekFrom::Start(start as u64))?;
    let mut remaining = len;
    let mut copied = 0i64;
    while remaining > 0 {
        let chunk = read_up_to(src, remaining.min(COPY_CHUNK) as u64)?;
        if chunk.is_empty() {
            break;
        }
        out.write_all(&chunk)?;
        copied += chunk.len() as i64;
        remaining -= chunk.len() as i64;
    }
    Ok(copied)
}

/// Opens (creating or truncating) [path] for writing, as Dart's
/// `FileMode.write` did.
fn create_for_write(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Wall-clock microseconds since the epoch (Dart's
/// `DateTime.now().microsecondsSinceEpoch`), for unique temp names.
fn micros_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

/// The frame count [ms] lasts at [rate] (Dart's `(ms / 1000 * rate).round()`).
fn frames_at(ms: i64, rate: i64) -> i64 {
    round_to_i64(ms as f64 / 1000.0 * rate as f64)
}

/// Cuts the first [trim_ms] of audio out of the uncompressed WAV at [path],
/// IN PLACE (temp file + atomic rename, so a crash mid-write leaves either
/// the old file or the new one — never a torn WAV). Frame-quantized;
/// preserves the exact sample format. Used by take placement when the
/// computed start would land before timeline zero: the pre-roll head that
/// precedes t=0 must be cut out of the FILE, otherwise clamping the position
/// silently discards the entire pre-roll/latency compensation. Fails with
/// [`AudioEditError::Format`] when [path] is not an uncompressed WAV
/// (callers treat that as best-effort and fall back to the un-trimmed take).
///
/// STREAMS the copy in bounded chunks: a long take is hundreds of megabytes,
/// and the old whole-file read → rebuild → write held two full copies.
pub fn trim_wav_head_in_place(path: &str, trim_ms: i64) -> Result<()> {
    if trim_ms <= 0 {
        return Ok(());
    }
    let tmp = trim_wav_head_to_temp(path, trim_ms)?;
    // The trimmed copy takes the name of the take's only copy (the save
    // pools it seconds later): its bytes go to disk before the rename gives
    // it that name, and the rename before the take is relied on. Unflushed,
    // a power cut in between could leave an empty file there, which boot
    // recovery takes for junk.
    let published = durable_fs::fsync_file(Path::new(&tmp)).and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = published {
        // Neither flushed nor in place: the take keeps its head, and the
        // copy doesn't linger beside it.
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    durable_fs::fsync_dir(Path::new(&dart_path::dirname(path)));
    Ok(())
}

/// The file work of [`trim_wav_head_in_place`]: the header walk (header
/// bytes only), then a chunked data copy into a temp file beside [path],
/// whose path it returns for the caller to make durable and rename.
fn trim_wav_head_to_temp(path: &str, trim_ms: i64) -> Result<String> {
    let no_wav = || AudioEditError::Format("Only uncompressed WAV takes can be trimmed.".into());
    let mut raf = open_for_read(path)?;
    // The bounded walk and the admission rules are parse_wav_from_file's
    // (Dart repeated them inline here, to the letter).
    let Some(w) = parse_wav_from_file(&mut raf)? else {
        return Err(no_wav());
    };
    let f = &w.fmt;
    let block_align = w.block_align;
    // Whole frames only (see concat_wav on why alignment matters), and never
    // cut past the end — a trim longer than the take leaves an empty WAV.
    let whole_len = (w.data_len / block_align) * block_align;
    let cut = frames_at(trim_ms, w.sample_rate)
        .saturating_mul(block_align)
        .clamp(0, whole_len);
    let new_len = whole_len - cut;

    // Unique temp name (pid + wall clock): the old fixed '<path>.trim' was
    // predictable, so a pre-placed entry at that exact name could redirect or
    // tear a concurrent/re-entrant trim. Failed attempts are deleted below.
    let tmp_path = format!("{path}.trim.{}.{}.tmp", std::process::id(), micros_now());
    let mut out = create_for_write(&tmp_path)?;
    let written = (|| -> io::Result<()> {
        out.write_all(&wav_header(f, new_len))?;
        // Chunked copy: 4 MiB at a time keeps the peak bounded regardless of
        // the take's length.
        raf.seek(SeekFrom::Start((w.data_start + cut) as u64))?;
        let mut remaining = new_len;
        while remaining > 0 {
            let chunk = read_up_to(&mut raf, remaining.min(COPY_CHUNK) as u64)?;
            if chunk.is_empty() {
                break; // truncated on disk: keep what exists
            }
            out.write_all(&chunk)?;
            remaining -= chunk.len() as i64;
        }
        if remaining == 0 {
            if new_len & 1 == 1 {
                out.write_all(&[0])?;
            }
        } else {
            // The source was truncated under us: keep whole frames and patch
            // both size fields so the header never claims bytes the file
            // does not hold (a header written up front describes the
            // EXPECTED length).
            patch_wav_sizes(
                &mut out,
                f,
                ((new_len - remaining) / block_align) * block_align,
            )?;
        }
        Ok(())
    })();
    drop(out);
    if let Err(e) = written {
        // A failed attempt must not linger: the .tmp suffix is outside every
        // recovery scan, but disk space and confusion are still real.
        let _ = fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(tmp_path)
}

/// Stands in for [`AudioEditService::punch_in`] (a test seam: the engine
/// does not splice in unit tests). Called with the practice id, the old and
/// new paths and the split point.
pub type PunchIn = Arc<dyn Fn(&str, &str, &str, i64) -> Result<Option<String>> + Send + Sync>;

/// The clip edits the song screen makes (Dart tests faked the service by
/// overriding these methods). See [`AudioEditService`].
pub trait AudioEdits: Send + Sync {
    /// See [`AudioEditService::split_wav`].
    fn split_wav(
        &self,
        practice_id: &str,
        source_path: &str,
        split_file_ms: i64,
    ) -> Result<SplitPaths>;
    /// See [`AudioEditService::concat_wav`].
    fn concat_wav(&self, practice_id: &str, first_path: &str, second_path: &str) -> Result<String>;
    /// See [`AudioEditService::ensure_streamable_wav`].
    fn ensure_streamable_wav(&self, practice_id: &str, path: &str) -> Result<String>;
    /// See [`AudioEditService::punch_in`].
    fn punch_in(
        &self,
        practice_id: &str,
        old_path: &str,
        new_path: &str,
        split_ms: i64,
    ) -> Result<Option<String>>;
    /// See [`AudioEditService::duplicate`].
    fn duplicate(&self, practice_id: &str, source_path: &str) -> Result<String>;
}

/// Non-destructive clip edits on WAV files. Split/merge slice and join the
/// raw PCM **bytes** (preserving the source's exact sample format —
/// 16/24/32-bit, integer or float, plain or WAVE_FORMAT_EXTENSIBLE), so any
/// uncompressed WAV works losslessly. Recordings are WAV, so takes can always
/// be edited.
pub struct AudioEditService {
    files: Arc<dyn AudioFiles>,
    punch: Option<PunchIn>,
}

impl std::fmt::Debug for AudioEditService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioEditService").finish_non_exhaustive()
    }
}

impl AudioEditService {
    /// The engine's working sample rate; punch-in output is written at this
    /// rate.
    pub const ENGINE_RATE: u32 = ENGINE_RATE;

    /// An edit service handing out paths through [files].
    pub fn new(files: Arc<dyn AudioFiles>) -> Self {
        AudioEditService { files, punch: None }
    }

    /// This service with [punch] standing in for the engine's splice (a test
    /// seam: Dart's tests overrode `punchIn`).
    pub fn with_punch_in(mut self, punch: PunchIn) -> Self {
        self.punch = Some(punch);
        self
    }

    /// A fresh output path under `audio/<practiceId>/edits/` — a sibling of
    /// the take staging tree, NOT the tree itself. Boot recovery
    /// (`Persistence::recover_staged_recordings`) adopts any orphan
    /// recording-shaped WAV lying DIRECTLY under `audio/<practiceId>/`, so
    /// writing edit outputs there means a kill between the file write and
    /// the save commit resurrects each output as a phantom "(recovered)"
    /// take next boot while the original clip is intact. Files under `edits/`
    /// are invisible to recovery (it skips subdirectories) but still
    /// reclaimed by the boot staging sweep, which trashes the whole `audio/`
    /// tree. The paths stay ordinary sandbox paths, so the save flow imports
    /// them into the pool exactly as before.
    fn new_edit_path(&self, practice_id: &str, extension: &str) -> Result<String> {
        let base = self
            .files
            .new_recording_path(practice_id, extension, None, None)?;
        let dir = dart_path::join(&dart_path::dirname(&base), "edits");
        fs::create_dir_all(&dir)?;
        Ok(dart_path::join(&dir, &dart_path::basename(&base)))
    }

    /// Splits the WAV at [split_file_ms] (offset from the file's start) into
    /// two new WAV files. Returns their paths. Fails if the source isn't a
    /// WAV.
    pub fn split_wav(
        &self,
        practice_id: &str,
        source_path: &str,
        split_file_ms: i64,
    ) -> Result<SplitPaths> {
        // Header-only parse: the whole-file read held a full copy of a long
        // take (plus both slices and both outputs — gigabytes for a legal
        // file); the split is a byte-range operation and streams as one.
        let Some(w) = parse_wav_at_path(source_path)? else {
            return Err(cannot_edit(
                source_path,
                "This take is compressed audio (e.g. mp3/m4a). Only recordings and WAV files \
                 can be split.",
            ));
        };
        // Work in WHOLE frames: a data chunk that isn't a frame multiple (a
        // header from a write cut short) would otherwise let the clamp below
        // land the cut INSIDE a frame, and the second half would then start
        // on the wrong channel — swapped stereo for the rest of the take.
        let usable = (w.data_len / w.block_align) * w.block_align;
        // Need at least one frame on each side. A 0-length / sub-2-frame take
        // (e.g. a recording quit mid-capture) would otherwise give clamp
        // bounds where lower > upper, which Dart's clamp threw on instead of
        // this message.
        if usable < 2 * w.block_align {
            return Err(AudioEditError::Format(
                "This take is too short to split.".into(),
            ));
        }
        let frame = frames_at(split_file_ms, w.sample_rate);
        let cut = frame
            .saturating_mul(w.block_align)
            .clamp(w.block_align, usable - w.block_align);

        let first_path = self.new_edit_path(practice_id, "wav")?;
        let second_path = self.new_edit_path(practice_id, "wav")?;
        let mut src = open_for_read(source_path)?;
        // Both halves are written to unique temps and published together at
        // the end: a failure mid-way must not leave one half sitting at a
        // final path masquerading as a finished edit.
        let nonce = format!("{}.{}", std::process::id(), micros_now());
        let tmp_first = format!("{first_path}.{nonce}.tmp");
        let tmp_second = format!("{second_path}.{nonce}.tmp");
        let mut first_published = false;
        let written = (|| -> io::Result<()> {
            for (tmp_path, start, len) in [
                (&tmp_first, w.data_start, cut),
                // `usable`, not data_len: a trailing partial frame is
                // dropped, not carried into the second half as a half-frame
                // of noise.
                (&tmp_second, w.data_start + cut, usable - cut),
            ] {
                let mut out = create_for_write(tmp_path)?;
                write_wav_header(&mut out, &w.fmt, len)?;
                let copied = copy_range(&mut src, start, len, &mut out)?;
                if copied == len {
                    if len & 1 == 1 {
                        out.write_all(&[0])?;
                    }
                } else {
                    // Truncated on disk: keep what exists, header kept
                    // honest.
                    patch_wav_sizes(&mut out, &w.fmt, (copied / w.block_align) * w.block_align)?;
                }
            }
            fs::rename(&tmp_first, &first_path)?;
            first_published = true;
            fs::rename(&tmp_second, &second_path)
        })();
        if let Err(e) = written {
            // A first half already published goes too: alone at its final
            // path, it read as a finished edit.
            let mut doomed = vec![tmp_first.as_str(), tmp_second.as_str()];
            if first_published {
                doomed.push(first_path.as_str());
            }
            for t in doomed {
                let _ = fs::remove_file(t);
            }
            return Err(e.into());
        }
        Ok(SplitPaths {
            first_path,
            second_path,
        })
    }

    /// Concatenates two WAVs end-to-end ([first_path] then [second_path])
    /// into one new WAV (inverse of [`AudioEditService::split_wav`]). Takes
    /// of one format are joined byte for byte; any other pair is re-encoded
    /// by the engine ([`AudioEditService::punch_in`]).
    pub fn concat_wav(
        &self,
        practice_id: &str,
        first_path: &str,
        second_path: &str,
    ) -> Result<String> {
        // Header-only parses; the join itself streams below (the whole-file
        // reads held both takes, both slices, and the merged buffer at once).
        let wa = parse_wav_at_path(first_path)?;
        let wb = parse_wav_at_path(second_path)?;
        const COMPRESSED: &str =
            "These takes are compressed audio. Only recordings and WAV files can be merged.";
        let Some(wa) = wa else {
            return Err(cannot_edit(first_path, COMPRESSED));
        };
        let Some(wb) = wb else {
            return Err(cannot_edit(second_path, COMPRESSED));
        };
        // The output reuses wa's fmt header for BOTH halves, so the takes
        // must share the fields that determine how the PCM bytes are read —
        // sample rate, channels, bit depth — otherwise the second take is
        // reinterpreted under the first's format (wrong rate or channel
        // deinterleave for its whole half). The format TAG may differ (a
        // plain-PCM recording vs an imported WAVE_FORMAT_EXTENSIBLE of the
        // same actual format), so compare those fields rather than
        // byte-equal fmt chunks.
        if !compatible_for_merge(&wa, &wb) {
            // Different formats can't be byte-spliced: a legacy 16-bit take
            // beside a 32-bit float recording, or a mono recording beside a
            // stereo import (or an earlier merge's output). Fall back to the
            // engine's decode → splice → re-encode path, which resamples both
            // to the engine rate and makes them stereo, with the split pinned
            // at old's full duration (ceil, so no old audio is dropped; at
            // most <1 ms of pad). Output is a stereo float WAV. Refusing a
            // rate or channel mismatch outright made Merge down fail for a
            // recorded take under an imported one.
            let frames = wa.data_len / wa.block_align;
            let old_ms = (frames * 1000 + wa.sample_rate - 1) / wa.sample_rate;
            let merged = match self.punch_in(practice_id, first_path, second_path, old_ms) {
                // The two together, not either take: "This file is too long"
                // named neither.
                Err(AudioEditError::Media(MediaError::TooLong { .. })) => {
                    return Err(AudioEditError::Format(format!(
                        "The merged take would be longer than {} minutes.",
                        MAX_PLAYBACK_DURATION.as_secs() / 60
                    )));
                }
                other => other?,
            };
            return merged.ok_or_else(|| {
                AudioEditError::Format(
                    "These takes have different audio formats and merging them by re-encoding \
                     failed."
                        .into(),
                )
            });
        }
        // Trim each half DOWN to a whole number of frames before joining. A
        // truncated or malformed source can report a data_len that isn't
        // block-aligned, which would make the second half begin mid-frame
        // (channel swap + one-sample skew for its entire span). A
        // correctly-finalized WAV is always frame-aligned, so this is a no-op
        // for every well-formed file. block_align > 0 is guaranteed by the
        // parse.
        let a_len = (wa.data_len / wa.block_align) * wa.block_align;
        let b_len = (wb.data_len / wb.block_align) * wb.block_align;
        // Classic RIFF sizes are u32: refuse a merge whose header could not
        // cover its own payload instead of writing one that silently lies
        // past 4 GiB. The budget counts both word-alignment pads too, since
        // the RIFF size the header carries is 20 + fmt (+ its pad) + data +
        // pad.
        let fmt_len = wa.fmt.len() as i64;
        if a_len + b_len + ((a_len + b_len) & 1) > 0xFFFF_FFFF - 20 - fmt_len - (fmt_len & 1) {
            return Err(AudioEditError::Format(
                "These takes are too long to merge into a single WAV file.".into(),
            ));
        }

        let path = self.new_edit_path(practice_id, "wav")?;
        // Unique temp + rename: the final path only ever holds a COMPLETE
        // merge.
        let tmp_path = format!("{path}.{}.{}.tmp", std::process::id(), micros_now());
        let mut out = create_for_write(&tmp_path)?;
        let written = (|| -> Result<()> {
            write_wav_header(&mut out, &wa.fmt, a_len + b_len)?;
            let mut copied_a = 0;
            let mut copied_b = 0;
            for (src, start, len, is_first) in [
                (first_path, wa.data_start, a_len, true),
                (second_path, wb.data_start, b_len, false),
            ] {
                let mut raf = open_for_read(src)?;
                let copied = copy_range(&mut raf, start, len, &mut out)?;
                if is_first {
                    copied_a = copied;
                } else {
                    copied_b = copied;
                }
            }
            if copied_a != a_len {
                // A shortfall in the FIRST half shifts every frame of the
                // second half across the seam — that is corruption, not
                // tolerance.
                return Err(AudioEditError::Format(
                    "The first take changed on disk while merging.".into(),
                ));
            }
            if copied_b == b_len {
                if (a_len + b_len) & 1 == 1 {
                    out.write_all(&[0])?;
                }
            } else {
                // A short tail is safe to keep: it starts on a frame
                // boundary.
                patch_wav_sizes(
                    &mut out,
                    &wa.fmt,
                    a_len + (copied_b / wa.block_align) * wa.block_align,
                )?;
            }
            Ok(())
        })();
        drop(out);
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp_path);
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp_path, &path) {
            // Unpublished, the whole merge would sit in edits/ until a boot
            // sweep.
            let _ = fs::remove_file(&tmp_path);
            return Err(e.into());
        }
        Ok(path)
    }

    /// Industry-standard import discipline: media becomes an
    /// engine-streamable WAV ONCE, at import — never decoded-in-full again at
    /// every project open. Returns [path] unchanged when the native feeder
    /// can stream it from disk as-is (see
    /// [`AudioEditService::is_engine_streamable_wav`]); otherwise transcodes
    /// it to an engine-rate float WAV (via the punch engine's streamed,
    /// transactional split-0 path — output streams to disk, lands
    /// atomically, and the decode budgets apply) and returns the NEW path.
    /// The caller owns deleting the original compressed copy.
    ///
    /// This is P0 of the disk-streaming migration (docs/audio-streaming-plan):
    /// once every track is engine-rate WAV, playback can move from full-RAM
    /// Arcs to file-backed feeders without a decoder in the loop.
    pub fn ensure_streamable_wav(&self, practice_id: &str, path: &str) -> Result<String> {
        if Self::is_engine_streamable_wav(path) {
            return Ok(path.to_string());
        }
        // split == 0 keeps nothing of old and never reads it (pinned by
        // test); the source rides in as `new`.
        let out = self.punch_in(practice_id, path, path, 0)?;
        out.ok_or_else(|| {
            AudioEditError::Format("This file could not be converted for playback.".into())
        })
    }

    /// Whether the native feeder (`WavTrackReader::open` in feed.rs) can
    /// stream this file from disk: canonical 44-byte header (16-byte `fmt `
    /// followed immediately by `data`), PCM16 or float32, 1-2 channels,
    /// engine rate. This MUST mirror the Rust admission exactly — the old
    /// check accepted ANY uncompressed WAV (44.1 kHz, EXTENSIBLE, PCM24,
    /// extra chunks), so those imports skipped the transcode, the feeder then
    /// refused them at commit, and the track silently fell back to the
    /// full-decode RAM path with none of the streaming migration's benefit.
    /// Pinned by mirrored test vectors here and in the engine's feed tests.
    pub fn is_engine_streamable_wav(path: &str) -> bool {
        match probe_wav(Path::new(path)) {
            // probe_wav fails on an unreadable file so the recovery path can
            // hold a crashed take back rather than trash it. Here the
            // question is only "can the feeder stream this?", and "we cannot
            // read it" answers no — the caller's transcode path reports a
            // proper failure of its own.
            Err(_) | Ok(None) => false,
            Ok(Some(probe)) => {
                probe.sample_rate == Self::ENGINE_RATE && probe.channels >= 1 && probe.channels <= 2
            }
        }
    }

    /// Punch-in splice: produces a new WAV = `old[0..split_ms] ++ new`,
    /// where [split_ms] is the cursor offset from the START of [old_path]'s
    /// audio. If the cursor lands past old's end the gap is silence-padded so
    /// [new_path] keeps its timeline position. Decode/splice/encode run in
    /// the engine, so any source format works (output is engine-rate WAV).
    /// Returns the new file's path, or `None` on failure. Fails with the
    /// typed refusal a decode names (too long, too dense, an unsupported
    /// rate, a read error): a file with no declared length past the hour was
    /// copied whole, then said only that it "could not be converted".
    pub fn punch_in(
        &self,
        practice_id: &str,
        old_path: &str,
        new_path: &str,
        split_ms: i64,
    ) -> Result<Option<String>> {
        if let Some(punch) = &self.punch {
            return punch(practice_id, old_path, new_path, split_ms);
        }
        let out_path = self.new_edit_path(practice_id, "wav")?;
        match engine::punch_wav(old_path, new_path, split_ms, Self::ENGINE_RATE, &out_path) {
            Ok(frames) if frames > 0 => Ok(Some(out_path)),
            Ok(_) => Ok(None),
            Err(e) => match MediaError::from_decode(e, MAX_PLAYBACK_DURATION) {
                Some(refused) => Err(refused.into()),
                None => Ok(None),
            },
        }
    }

    /// Copies an audio file (any format) to a fresh sandbox path, preserving
    /// its extension. Used by "Duplicate".
    pub fn duplicate(&self, practice_id: &str, source_path: &str) -> Result<String> {
        let ext = dart_path::extension(source_path).replacen('.', "", 1);
        // Duplicates would dedup to an already-referenced content hash (so
        // recovery skips them anyway), but they go through the edits dir
        // too: one rule for every edit output beats a per-operation safety
        // proof.
        let dest = self.new_edit_path(practice_id, if ext.is_empty() { "wav" } else { &ext })?;
        // Bounded, transactional copy (temp + rename, cleaned up on
        // failure): a half-copied duplicate must never sit at a final edits/
        // path.
        copy_bounded(source_path, &dest, MAX_IMPORT_BYTES)?;
        Ok(dest)
    }
}

impl AudioEdits for AudioEditService {
    fn split_wav(
        &self,
        practice_id: &str,
        source_path: &str,
        split_file_ms: i64,
    ) -> Result<SplitPaths> {
        AudioEditService::split_wav(self, practice_id, source_path, split_file_ms)
    }
    fn concat_wav(&self, practice_id: &str, first_path: &str, second_path: &str) -> Result<String> {
        AudioEditService::concat_wav(self, practice_id, first_path, second_path)
    }
    fn ensure_streamable_wav(&self, practice_id: &str, path: &str) -> Result<String> {
        AudioEditService::ensure_streamable_wav(self, practice_id, path)
    }
    fn punch_in(
        &self,
        practice_id: &str,
        old_path: &str,
        new_path: &str,
        split_ms: i64,
    ) -> Result<Option<String>> {
        AudioEditService::punch_in(self, practice_id, old_path, new_path, split_ms)
    }
    fn duplicate(&self, practice_id: &str, source_path: &str) -> Result<String> {
        AudioEditService::duplicate(self, practice_id, source_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fmt_u16_out_of_range_reads_minus_one() {
        assert_eq!(fmt_u16(&[1, 2], 0), 0x0201);
        assert_eq!(fmt_u16(&[1, 2], 1), -1);
        // A short EXTENSIBLE body has no SubFormat: not PCM or float.
        assert_eq!(eff_fmt(&[0xFE, 0xFF, 1, 0]), -1);
    }

    #[test]
    fn a_header_lays_out_an_odd_fmt_body_word_aligned() {
        let h = wav_header(&[0u8; 17], 3);
        assert_eq!(&h[0..4], b"RIFF");
        // RIFF(12) + fmt header(8) + fmt body(17) + pad(1) = 38.
        assert_eq!(&h[38..42], b"data");
        assert_eq!(u32_le(&h, 4), 4 + (8 + 17 + 1) + (8 + 3 + 1));
        assert_eq!(u32_le(&h, 42), 3);
    }
}
