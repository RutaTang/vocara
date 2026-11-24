//! Copies one audio file out of the app to a place the user picks (port of
//! `lib/data/services/audio_exporter.dart`): the guarantee that the user's
//! audio is never trapped in the app's private container. Shared by the song
//! screen (takes, clips, backings) and the technique page (examples), so both
//! refuse and say the same things.
//!
//! One export at a time per exporter; every outcome but a cancelled save
//! panel comes back as a message to show.
//!
//! [`AudioExporter::export`] blocks its (worker) thread for the save panel
//! and the copy. The save panel itself is the app's: Dart's default picker
//! (`systemSaveLocation`, file_selector's save panel) is a GUI call, so
//! `vocara_app` passes its own (the save panel of [`crate::file_panels`],
//! run on the main thread for the worker) to [`AudioExporter::new`].

use std::error::Error;
use std::fs;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use vocara_model::util::dart_path;
use vocara_model::util::export_copy::export_copy;
use vocara_store::db::AssetStore;

use crate::audio_file_service::AudioFiles;
use crate::recording_finalizer::RecordingFinalizer;

/// Why the save panel could not be shown.
pub type PickError = Box<dyn Error + Send + Sync>;

/// Resolves where an export is written, given the suggested file name: the
/// chosen path, or `None` when the user cancelled.
type SaveLocationPicker = Box<dyn Fn(&str) -> Result<Option<String>, PickError> + Send + Sync>;

/// Copies a source onto the picked destination (see [`AudioExporter::with_copy`]).
type CopyFn = Box<dyn Fn(&Path, &str) -> io::Result<()> + Send + Sync>;

/// Exports audio files (see the module doc).
pub struct AudioExporter {
    audio_files: Arc<dyn AudioFiles>,

    /// Resolves where an export is written (the platform save panel in the
    /// app); injected so tests can drive an export without one.
    pick_save_location: SaveLocationPicker,

    finalizer: Arc<RecordingFinalizer>,

    /// The copy (`export_copy`; a test seam).
    copy: CopyFn,

    /// Whether an export (its save panel or its copy) is under way.
    exporting: AtomicBool,
}

impl std::fmt::Debug for AudioExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioExporter")
            .field("exporting", &self.exporting())
            .finish_non_exhaustive()
    }
}

/// Releases the one-at-a-time claim however the export ends.
struct Claim<'a>(&'a AtomicBool);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl AudioExporter {
    /// An exporter over [audio_files] (which says what the app's own storage
    /// is), writing where [pick_save_location] answers, and holding the
    /// app-wide [`RecordingFinalizer::instance`]'s quit while it copies.
    pub fn new(
        audio_files: Arc<dyn AudioFiles>,
        pick_save_location: impl Fn(&str) -> Result<Option<String>, PickError> + Send + Sync + 'static,
    ) -> Self {
        AudioExporter {
            audio_files,
            pick_save_location: Box::new(pick_save_location),
            finalizer: RecordingFinalizer::instance(),
            copy: Box::new(export_copy),
            exporting: AtomicBool::new(false),
        }
    }

    /// Holds [finalizer]'s quit instead of the app-wide one's.
    pub fn with_finalizer(mut self, finalizer: Arc<RecordingFinalizer>) -> Self {
        self.finalizer = finalizer;
        self
    }

    /// Replaces the copy (`export_copy`) with [copy] — a test seam, where
    /// Dart's tests watched the quit held across the copy's awaits.
    pub fn with_copy(
        mut self,
        copy: impl Fn(&Path, &str) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.copy = Box::new(copy);
        self
    }

    /// `"{title} - {item name}{source extension}"` (a practice and its take,
    /// a technique and its example), with path-hostile characters flattened
    /// so the suggestion is a valid file name. The source extension is kept
    /// (an imported mp3 exports as .mp3; recordings are .wav). A file name
    /// holds 255 bytes, and neither name is bounded: the suggestion the copy
    /// then failed on is cut to fit, the title first (the take's name says
    /// which file it is), and never the [suffix] after the take's name (cut
    /// with it, two clips of a take suggested one name). Dart's default
    /// [suffix] is `""`.
    pub fn file_name(title: &str, item_name: &str, source_path: &str, suffix: &str) -> String {
        let ext = dart_path::extension(source_path);
        let flat = |s: &str| s.replace(['/', '\\', ':'], "-");
        let name = fit_bytes(&flat(item_name), 120usize.saturating_sub(suffix.len()));
        let tail = format!(" - {name}{suffix}");
        let base = fit_bytes(&flat(title), 200usize.saturating_sub(tail.len())) + &tail;
        format!("{base}{}", if ext.is_empty() { ".wav" } else { &ext })
    }

    /// Whether an export (its save panel or its copy) is under way.
    pub fn exporting(&self) -> bool {
        self.exporting.load(Ordering::SeqCst)
    }

    /// Copies [source_path] to a location the user picks, suggesting
    /// [suggested_name]. [noun] names what is exported in the messages
    /// ("take", "track", "example") and [medium] what it holds ("audio", or
    /// "video" for a video example; Dart's default is "audio"). Returns the
    /// outcome to show, or `None` when the user cancelled the save panel.
    pub fn export(
        &self,
        source_path: &str,
        suggested_name: &str,
        noun: &str,
        medium: &str,
    ) -> Option<String> {
        // One at a time: nothing showed a copy running (to a slow drive it runs
        // a while), and a second export to the same chosen name had two copies
        // writing one file, each then saying "Exported".
        const WAIT: &str = "Wait for the current export to finish.";
        if self.exporting() {
            return Some(WAIT.into());
        }
        // Bytes another device holds, not yet here: as edits of it say, not
        // "missing".
        if AssetStore::asset_id_of(source_path).is_some() {
            return Some(format!(
                "This {noun}'s {medium} isn't on this device yet, so it can't be exported."
            ));
        }
        // Claimed before the save panel opens, in one step with the check: a
        // double-click while a slow drive answered let both through, two panels
        // and two copies.
        if self
            .exporting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Some(WAIT.into());
        }
        let _claim = Claim(&self.exporting);
        let missing = format!("The {medium} file for this {noun} is missing.");
        if source_path.is_empty() || !file_exists(source_path) {
            return Some(missing);
        }
        self.export_picked(source_path, suggested_name, noun, medium, missing)
    }

    /// The save panel and the copy of [`AudioExporter::export`].
    fn export_picked(
        &self,
        source: &str,
        suggested_name: &str,
        noun: &str,
        medium: &str,
        missing: String,
    ) -> Option<String> {
        // Any failure to show the panel, a panic in the GUI call included (Dart
        // caught everything it threw).
        let picked = catch_unwind(AssertUnwindSafe(|| {
            (self.pick_save_location)(suggested_name)
        }));
        let dest = match picked {
            Ok(Ok(Some(dest))) => dest,
            Ok(Ok(None)) => return None, // user cancelled
            Ok(Err(_)) | Err(_) => return Some("Could not open the save dialog.".into()),
        };
        // Again, after the panel: it can stay open indefinitely, and the file can
        // go meanwhile (a sync removed it, a failed import's revert deleted its
        // copy) — the copy then failed as if the destination had.
        if !file_exists(source) {
            return Some(missing);
        }
        // Onto its own file (the save panel can browse into the app's storage
        // and confirm Replace): the copy truncated it to nothing, the audio
        // gone, and said "Exported".
        if Self::same_file(source, &dest) {
            return Some(format!(
                "That is the {noun}'s own {medium} file. Export it somewhere else."
            ));
        }
        // Into the app's storage: another take's audio, or the library itself.
        if self.audio_files.is_app_storage(&dest) {
            return Some("That folder holds Vocara's own files. Export somewhere else.".into());
        }
        // Overwrite is intentional: the save panel already confirmed replacing.
        // Held: a quit mid-copy exited under it and left the destination cut
        // short (an old file there already replaced).
        if self
            .finalizer
            .hold(|| (self.copy)(Path::new(source), &dest))
            .is_err()
        {
            // Blamed on the destination only when the source is still there.
            return Some(if file_exists(source) {
                format!("Could not export the {medium} to that location.")
            } else {
                missing
            });
        }
        Some(format!("Exported to {dest}"))
    }

    /// Whether [a] and [b] are one file (a symlink or hard link to it too).
    pub fn same_file(a: &str, b: &str) -> bool {
        if !file_exists(b) {
            return false;
        }
        // Both resolved first: identical() judges a link itself, not what it
        // points to.
        match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => identical(&a, &b).unwrap_or(false),
            _ => false,
        }
    }
}

/// [s]'s longest leading run of whole characters within [max_bytes] of UTF-8.
fn fit_bytes(s: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    let mut bytes = 0;
    for c in s.chars() {
        bytes += c.len_utf8();
        if bytes > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

/// Dart's `File.exists`: anything but a directory, links followed.
fn file_exists(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| !m.is_dir())
}

/// Dart's `FileSystemEntity.identical`: one file system object (same device
/// and inode), links followed.
fn identical(a: &Path, b: &Path) -> io::Result<bool> {
    let ma = fs::metadata(a)?;
    let mb = fs::metadata(b)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (ma, mb);
        Ok(a == b)
    }
}
