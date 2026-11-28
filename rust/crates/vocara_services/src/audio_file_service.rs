//! Audio files on disk (port of `lib/data/services/audio_file_service.dart`).
//!
//! Imported files are copied into the app sandbox (under
//! `audio/<practiceId>/imports/`) so a practice owns its media and never
//! loses a reference to an external file. Recordings are written straight
//! into `audio/<practiceId>/`.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use vocara_model::util::dart_path;
use vocara_model::util::export_copy::exports_into;
use vocara_model::util::id::{is_path_component_id, new_id};

use crate::local_storage_service::LocalStorageService;

/// Why an audio file operation was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum AudioFileError {
    /// An id that is not a well-formed path component (Dart's
    /// `ArgumentError`).
    #[error("not a well-formed id: {0}")]
    InvalidId(String),
    /// A refusal with the message the UI shows (Dart's `FormatException`).
    #[error("{0}")]
    Format(String),
    /// A file system failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, AudioFileError>;

/// Header facts of a canonical PCM WAV file — see [`probe_wav`].
/// [`data_bytes`] is the payload the header AND the file both cover: the
/// header's data-chunk size clamped to the bytes actually present after the
/// header (a hard-killed recording legitimately holds a
/// written-but-not-yet-patched tail beyond the last header patch; that tail
/// is not counted).
///
/// [`data_bytes`]: WavProbe::data_bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavProbe {
    pub channels: u32,
    pub sample_rate: u32,
    pub bits_per_sample: u32,
    pub data_bytes: u64,
}

impl WavProbe {
    /// Whether this is the shape the recording engine writes: mono, 32-bit
    /// float (16-bit PCM from pre-float versions), with a payload. Boot
    /// recovery adopts nothing else as a take, and nothing of this shape is
    /// deleted as "not a recording".
    pub fn is_recorder_shaped(&self) -> bool {
        self.channels == 1
            && (self.bits_per_sample == 16 || self.bits_per_sample == 32)
            && self.data_bytes > 0
    }
}

/// A recording's lane and timeline start, as its file name and sidecar say
/// (see [`recording_placement_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingPlacement {
    pub take_id: String,
    pub start_ms: i64,
}

/// What the song and technique screens need from the audio files (Dart
/// tests faked this class by overriding these methods).
pub trait AudioFiles: Send + Sync {
    /// See [`AudioFileService::import_audio`].
    fn import_audio(&self, practice_id: &str, source_path: &str) -> Result<String>;
    /// See [`AudioFileService::new_recording_path`].
    fn new_recording_path(
        &self,
        practice_id: &str,
        extension: &str,
        take_id: Option<&str>,
        start_ms: Option<i64>,
    ) -> Result<String>;
    /// See [`write_aligned_start_to`].
    fn write_aligned_start(&self, path: &str, start_ms: i64) -> bool;
    /// See [`AudioFileService::is_app_storage`].
    fn is_app_storage(&self, path: &str) -> bool;
    /// See [`AudioFileService::delete_audio`].
    fn delete_audio(&self, path: &str) -> Result<()>;
}

/// Owns audio files on disk (see the module doc).
pub struct AudioFileService {
    storage: Arc<LocalStorageService>,
}

/// Where imports stage: a subdirectory of the practice's staging tree, NOT
/// the tree itself. Boot recovery adopts every recorder-shaped WAV lying
/// DIRECTLY under `audio/<practiceId>/`, and an import of a mono 48 kHz
/// canonical WAV has exactly that shape (it stays the track's file until the
/// save pools it). A quit or crash before that save brought it back as a
/// phantom "(recovered)" take. One level down it is invisible to recovery,
/// which skips subdirectories, and the boot staging sweep still reclaims it
/// with the rest of `audio/` (the same rule edit outputs follow under
/// `edits/`).
pub const IMPORTS_SUBDIR: &str = "imports";

/// Hard ceiling on any single imported file. Derivation: the longest media
/// playback will ever decode is 60 minutes, and the fattest legal 60-minute
/// source the decode budgets admit (96 kHz stereo float32 WAV) is ~2.8 GB;
/// 4 GB clears that with headroom while refusing movie-sized junk BEFORE a
/// full copy lands in the sandbox.
pub const MAX_IMPORT_BYTES: u64 = 4 * 1000 * 1000 * 1000;

const TOO_LARGE: &str =
    "This file is too large to import. Media longer than an hour is not supported.";

impl AudioFileService {
    pub fn new(storage: Arc<LocalStorageService>) -> Self {
        AudioFileService { storage }
    }

    /// Last line of defense for ids used as path components (the sync wire
    /// door already enforces this shape): a separator or `..` would escape
    /// the audio tree.
    fn check_path_component(practice_id: &str) -> Result<()> {
        if is_path_component_id(practice_id) {
            Ok(())
        } else {
            Err(AudioFileError::InvalidId(practice_id.to_string()))
        }
    }

    fn audio_dir(&self, practice_id: &str, subdir: Option<&str>) -> Result<PathBuf> {
        Self::check_path_component(practice_id)?;
        let base = self.storage.asset_dir("audio")?;
        let dir = match subdir {
            None => base.join(practice_id),
            Some(s) => base.join(practice_id).join(s),
        };
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }
        Ok(dir)
    }

    /// Copies [source_path] into the practice's import staging directory (see
    /// [`IMPORTS_SUBDIR`]) and returns the new sandbox path. Refuses a source
    /// over [`MAX_IMPORT_BYTES`] — judged from a stat before any bytes move,
    /// and re-enforced during the copy itself (see [`copy_bounded`]).
    pub fn import_audio(&self, practice_id: &str, source_path: &str) -> Result<String> {
        // Order: pure id validation, then the source stat, then any mkdir — a
        // refusal must precede every filesystem side effect.
        Self::check_path_component(practice_id)?;
        // A plain file only: a pipe reports no length and held the copy open
        // for good, and a link to a device (/dev/zero) copied up to the cap.
        let meta = match fs::metadata(source_path) {
            Ok(m) if m.is_file() => m,
            _ => return Err(AudioFileError::Format("The file couldn't be read.".into())),
        };
        if meta.len() == 0 {
            return Err(AudioFileError::Format("This file is empty.".into()));
        }
        if meta.len() > MAX_IMPORT_BYTES {
            return Err(AudioFileError::Format(TOO_LARGE.into()));
        }
        let dir = self.audio_dir(practice_id, Some(IMPORTS_SUBDIR))?;
        let ext = dart_path::extension(source_path);
        let dest = dir.join(format!("{}{ext}", new_id()));
        let dest = dest.to_string_lossy().into_owned();
        copy_bounded(source_path, &dest, MAX_IMPORT_BYTES)?;
        Ok(dest)
    }

    /// Returns a fresh path inside the sandbox for a new recording. The
    /// recorder writes directly to this path.
    ///
    /// A take records into lane [take_id] from [start_ms] on the timeline,
    /// and the name carries both (see [`recording_placement_of`]): after a
    /// crash, boot recovery found neither, so it added the take as a new lane
    /// at the timeline's start, and the empty lane REC had made for it stayed
    /// behind.
    pub fn new_recording_path(
        &self,
        practice_id: &str,
        extension: &str,
        take_id: Option<&str>,
        start_ms: Option<i64>,
    ) -> Result<String> {
        let dir = self.audio_dir(practice_id, None)?;
        // Only an id of the minted shape goes into a file name.
        let placed = match (take_id, start_ms) {
            (Some(t), Some(s)) if is_path_component_id(t) && s >= 0 => format!("~{t}~{s}"),
            _ => String::new(),
        };
        Ok(dir
            .join(format!("{}{placed}.{extension}", new_id()))
            .to_string_lossy()
            .into_owned())
    }

    /// Whether an export onto [path] would write inside the app's own
    /// storage (the folder holding the recordings, the take pool and the
    /// library database), links followed: it overwrote another take's audio,
    /// or the library itself.
    pub fn is_app_storage(&self, path: &str) -> bool {
        let Ok(audio) = self.storage.asset_path("audio") else {
            return false; // no storage to resolve (the copy then says what failed)
        };
        let root = dart_path::dirname(&audio.to_string_lossy());
        exports_into(&root, path)
    }

    /// Deletes a per-practice working audio file. REFUSES content-addressed
    /// pool files (under `assets/`, shared by content hash across clips) and
    /// `asset://` references: those are owned by the AssetStore and reclaimed
    /// by its hash-refcount GC. Deleting a pooled file by path would wipe the
    /// audio of every other clip that dedups to the same hash.
    pub fn delete_audio(&self, path: &str) -> Result<()> {
        if path.starts_with("asset://") {
            return Ok(());
        }
        // Path-only guard: a delete must never create the pool directory as a
        // side effect.
        let pool = self.storage.asset_path("assets")?;
        if dart_path::is_within(&pool.to_string_lossy(), path) {
            return Ok(());
        }
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            // Already gone (or a concurrent cleanup won): deletion achieved.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl AudioFiles for AudioFileService {
    fn import_audio(&self, practice_id: &str, source_path: &str) -> Result<String> {
        AudioFileService::import_audio(self, practice_id, source_path)
    }
    fn new_recording_path(
        &self,
        practice_id: &str,
        extension: &str,
        take_id: Option<&str>,
        start_ms: Option<i64>,
    ) -> Result<String> {
        AudioFileService::new_recording_path(self, practice_id, extension, take_id, start_ms)
    }
    fn write_aligned_start(&self, path: &str, start_ms: i64) -> bool {
        write_aligned_start_to(path, start_ms).wait()
    }
    fn is_app_storage(&self, path: &str) -> bool {
        AudioFileService::is_app_storage(self, path)
    }
    fn delete_audio(&self, path: &str) -> Result<()> {
        AudioFileService::delete_audio(self, path)
    }
}

/// Streams [source_path] to [dest_path] with [max_bytes] enforced DURING the
/// copy — the pre-copy stat is advisory only, since a source still being
/// written (a download, an export in progress) can grow past it after the
/// check. The bytes land in a uniquely named sibling temp first and reach
/// [dest_path] only by rename, so no failure mode leaves a half file at the
/// final path: in `audio/<practiceId>/` a half-copied canonical WAV is
/// exactly what boot recovery would resurrect as a phantom take.
pub fn copy_bounded(source_path: &str, dest_path: &str, max_bytes: u64) -> Result<()> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let tmp = PathBuf::from(format!(
        "{dest_path}.import.{}.{micros}.tmp",
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        let mut src = fs::File::open(source_path)?;
        let mut out = fs::File::create(&tmp)?;
        let mut copied = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            copied += n as u64;
            if copied > max_bytes {
                return Err(AudioFileError::Format(TOO_LARGE.into()));
            }
            out.write_all(&buf[..n])?;
        }
        out.flush()?;
        drop(out);
        fs::rename(&tmp, dest_path)?;
        Ok(())
    })();
    if result.is_err() && tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The lane and timeline position a recording at [path] was made for, as
/// [`AudioFileService::new_recording_path`] named it — or, once the take's
/// backing started, as [`write_aligned_start_to`] put it beside it; `None`
/// for any other name (one from a build before it did, or not a recording).
/// The start is negative when the take's audio begins before the timeline
/// does: that much of the file's head is still to be cut.
pub fn recording_placement_of(path: &str) -> Option<RecordingPlacement> {
    let base = dart_path::basename_without_extension(path);
    let parts: Vec<&str> = base.split('~').collect();
    if parts.len() != 3 || !is_path_component_id(parts[1]) {
        return None;
    }
    let start_ms = dart_int_try_parse(parts[2])?;
    if start_ms < 0 {
        return None;
    }
    // Unreadable: placed at REC instead, late by the arm and the latency.
    let aligned = fs::read_to_string(aligned_start_of(path))
        .ok()
        .and_then(|s| dart_int_try_parse(&s));
    Some(RecordingPlacement {
        take_id: parts[1].to_string(),
        start_ms: aligned.unwrap_or(start_ms),
    })
}

/// Dart's `int.tryParse`: surrounding whitespace, an optional sign, decimal
/// digits or a `0x` hex prefix, within 64 bits.
pub(crate) fn dart_int_try_parse(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let magnitude = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        i128::from_str_radix(hex, 16).ok()?
    } else {
        if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        body.parse::<i128>().ok()?
    };
    let v = if neg { -magnitude } else { magnitude };
    i64::try_from(v).ok()
}

/// The sidecar [`write_aligned_start_to`] writes; the boot sweep keeps it
/// with its recording while that is held back, and clears it with it.
pub fn aligned_start_of(path: &str) -> String {
    format!("{path}.place")
}

/// A queued aligned-start write; [`PlaceWrite::wait`] answers whether it
/// landed.
pub struct PlaceWrite(mpsc::Receiver<bool>);

impl PlaceWrite {
    /// Blocks until the write has landed (or failed).
    pub fn wait(self) -> bool {
        self.0.recv().unwrap_or(false)
    }
}

/// Records beside the take at [path] where its audio starts on the timeline:
/// once its backing has started, the playhead at REC less the file's head
/// recorded before the backing rolled and the input latency — where a stop
/// would place it; and from the stop on, where it did. The name holds only
/// the playhead at REC, so a take recovered by it alone sat late by both.
///
/// A negative start says the file's head is still to be cut. Whoever cuts it
/// writes the start it leaves FIRST, and cuts only once that write is on
/// disk: the other way round, a crash in between left a cut file still saying
/// to cut, and the next recovery cut it again.
///
/// Writes land in CALL order, each whole: the call queues the write before
/// it returns (the start's write is not waited on, and on a slow disk the
/// stop's ran alongside it, both truncating one file: it was left `-512` or
/// `0512`). Best effort: [`PlaceWrite::wait`] answers whether it landed.
pub fn write_aligned_start_to(path: &str, start_ms: i64) -> PlaceWrite {
    let (tx, rx) = mpsc::channel();
    place_writer().push(aligned_start_of(path), start_ms, tx);
    PlaceWrite(rx)
}

/// One writer thread, fed in call order, so writes to a sidecar land in the
/// order they were asked for.
struct PlaceWriter {
    queue: Mutex<VecDeque<(String, i64, mpsc::Sender<bool>)>>,
    wake: std::sync::Condvar,
}

impl PlaceWriter {
    fn push(&self, place: String, start_ms: i64, done: mpsc::Sender<bool>) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back((place, start_ms, done));
        self.wake.notify_one();
    }
}

fn place_writer() -> &'static Arc<PlaceWriter> {
    static WRITER: OnceLock<Arc<PlaceWriter>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let w = Arc::new(PlaceWriter {
            queue: Mutex::new(VecDeque::new()),
            wake: std::sync::Condvar::new(),
        });
        let worker = w.clone();
        std::thread::Builder::new()
            .name("vocara-place-writer".into())
            .spawn(move || loop {
                let next = {
                    let mut q = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if let Some(item) = q.pop_front() {
                            break item;
                        }
                        q = worker.wake.wait(q).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let (place, start_ms, done) = next;
                let _ = done.send(write_place(&place, start_ms));
            })
            .expect("spawn the aligned-start writer");
        w
    })
}

/// Writes [start_ms] to [place] through a temp file and a rename, so a crash
/// mid-write leaves the previous value rather than a cut-off one. Recovery
/// falls back to the playhead at REC when this fails.
fn write_place(place: &str, start_ms: i64) -> bool {
    let tmp = format!("{place}.tmp");
    let write = || -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(start_ms.to_string().as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, place)
    };
    write().is_ok()
}

/// File access [`probe_wav_from`] reads through (tests inject faults).
pub trait ProbeSource {
    /// The file's length (a stat).
    fn length(&self) -> io::Result<u64>;
    /// The first [n] bytes (fewer if the file is shorter).
    fn read_head(&self, n: usize) -> io::Result<Vec<u8>>;
}

impl ProbeSource for Path {
    fn length(&self) -> io::Result<u64> {
        Ok(fs::metadata(self)?.len())
    }
    fn read_head(&self, n: usize) -> io::Result<Vec<u8>> {
        let f = fs::File::open(self)?;
        let mut head = Vec::with_capacity(n);
        f.take(n as u64).read_to_end(&mut head)?;
        Ok(head)
    }
}

/// Parses the header of [path] as a canonical uncompressed-PCM WAV (`RIFF` /
/// `WAVE` / 16-byte `fmt ` / `data`, the exact layout the recording engine
/// writes). Reads only the header, never the payload, so it is cheap even on
/// an hours-long take.
///
/// `Ok(None)` answers ONE question — "is this the recorder's shape?" — and
/// means too short, not that layout, not plain PCM, or gone. An unreadable
/// file is an `Err` instead, and that distinction is load-bearing: boot
/// recovery treats `None` as "provably not a recording, leave it for the
/// sweep", which trashes the file. A hardware or filesystem fault must not be
/// able to answer that question, or the only copy of a crashed take is swept
/// away on the strength of a read that never happened.
pub fn probe_wav(path: &Path) -> io::Result<Option<WavProbe>> {
    probe_wav_from(path)
}

/// [`probe_wav`] through any [`ProbeSource`].
pub fn probe_wav_from(file: &(impl ProbeSource + ?Sized)) -> io::Result<Option<WavProbe>> {
    const HEADER_LEN: usize = 44;
    let length = match file.length() {
        Ok(l) => l,
        // Vanished between the directory listing and here: nothing to
        // recover, and nothing to hold back for. Every other IO failure
        // propagates.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if length < HEADER_LEN as u64 {
        return Ok(None);
    }
    let head = match file.read_head(HEADER_LEN) {
        Ok(h) => h,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if head.len() < HEADER_LEN {
        return Ok(None);
    }
    let u16_at = |o: usize| u32::from(u16::from_le_bytes([head[o], head[o + 1]]));
    let u32_at = |o: usize| u32::from_le_bytes([head[o], head[o + 1], head[o + 2], head[o + 3]]);
    let tag = |o: usize| &head[o..o + 4];
    if tag(0) != b"RIFF" || tag(8) != b"WAVE" || tag(12) != b"fmt " {
        return Ok(None);
    }
    // Canonical layout only: a 16-byte fmt chunk immediately followed by the
    // data chunk. Foreign files with extensible fmt / extra chunks are simply
    // "not a recording" here. The engine records 32-bit float (format 3);
    // pre-float recordings were 16-bit PCM (format 1).
    if u32_at(16) != 16 {
        return Ok(None);
    }
    let format = u16_at(20);
    let channels = u16_at(22);
    let sample_rate = u32_at(24);
    let bits = u16_at(34);
    let is_pcm16 = format == 1 && bits == 16;
    let is_float32 = format == 3 && bits == 32;
    if !is_pcm16 && !is_float32 {
        return Ok(None);
    }
    if tag(36) != b"data" {
        return Ok(None);
    }
    if channels < 1 || sample_rate == 0 {
        return Ok(None);
    }
    let header_data_bytes = u64::from(u32_at(40));
    // The payload both the header and the file cover. min() tolerates the
    // hard-kill shape (real bytes past the last header patch) and rejects a
    // header that claims more than the file holds.
    let data_bytes = header_data_bytes.min(length - HEADER_LEN as u64);
    Ok(Some(WavProbe {
        channels,
        sample_rate,
        bits_per_sample: bits,
        data_bytes,
    }))
}
