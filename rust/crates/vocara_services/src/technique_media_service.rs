//! Technique example clips on disk (port of
//! `lib/data/services/technique_media_service.dart`).
//!
//! Imported files are copied into the app sandbox (under
//! `technique_media/<techniqueId>/`) so a technique owns its media and never
//! depends on the user's original file staying put. Mirrors
//! [`AudioFileService`](crate::audio_file_service::AudioFileService), and
//! shares its error type and its bounded copy.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vocara_model::util::dart_path;
use vocara_model::util::id::{is_path_component_id, new_id};
use vocara_store::db::AssetStore;

use crate::audio_file_service::{copy_bounded, AudioFileError, Result, MAX_IMPORT_BYTES};
use crate::local_storage_service::LocalStorageService;

/// What the technique screens need from the media files (Dart tests faked
/// this class by overriding these methods).
pub trait TechniqueMediaFiles: Send + Sync {
    /// See [`TechniqueMediaService::import_media`].
    fn import_media(&self, technique_id: &str, source_path: &str) -> Result<String>;
    /// See [`TechniqueMediaService::delete_media`].
    fn delete_media(&self, path: &str) -> Result<()>;
    /// See [`TechniqueMediaService::delete_technique_media`].
    fn delete_technique_media(&self, technique_id: &str) -> Result<()>;
}

/// Owns technique example clips on disk (see the module doc).
pub struct TechniqueMediaService {
    storage: Arc<LocalStorageService>,
}

impl TechniqueMediaService {
    pub fn new(storage: Arc<LocalStorageService>) -> Self {
        TechniqueMediaService { storage }
    }

    /// Last line of defense for ids used as path components (the wire door
    /// already enforces this shape): a separator or `..` in an id would
    /// escape the media tree — worst on the recursive delete below.
    fn check_path_component(technique_id: &str) -> Result<()> {
        if is_path_component_id(technique_id) {
            Ok(())
        } else {
            Err(AudioFileError::InvalidId(technique_id.to_string()))
        }
    }

    fn media_dir(&self, technique_id: &str) -> Result<PathBuf> {
        Self::check_path_component(technique_id)?;
        let base = self.storage.asset_dir("technique_media")?;
        let dir = base.join(technique_id);
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }
        Ok(dir)
    }

    /// Copies [source_path] into the technique's media directory and returns
    /// the new sandbox path (preserving the original extension). Refuses a
    /// source past [`MAX_IMPORT_BYTES`] — judged from a stat, before any
    /// bytes move (a full copy of an oversized file was pure disk burn for
    /// media that could never decode here).
    pub fn import_media(&self, technique_id: &str, source_path: &str) -> Result<String> {
        // Order: pure id validation, then the source stat, then any mkdir — a
        // refusal must precede every filesystem side effect.
        Self::check_path_component(technique_id)?;
        let length = file_length(source_path)?;
        // Empty: staged and saved, it was an example that could neither play nor
        // be analyzed, where the song screen refuses the same file.
        if length == 0 {
            return Err(AudioFileError::Format("This file is empty.".into()));
        }
        if length > MAX_IMPORT_BYTES {
            return Err(AudioFileError::Format(
                "This file is too large to import. Media longer than an hour is not supported."
                    .into(),
            ));
        }
        let dir = self.media_dir(technique_id)?;
        let ext = dart_path::extension(source_path);
        let dest = dir
            .join(format!("{}{ext}", new_id()))
            .to_string_lossy()
            .into_owned();
        // Bounded streaming copy via temp + rename: the stat above is advisory
        // (a still-growing source can pass it), and a half file must never sit
        // at the final path. See audio_file_service::copy_bounded.
        copy_bounded(source_path, &dest, MAX_IMPORT_BYTES)?;
        Ok(dest)
    }

    /// Deletes a single copied media file. REFUSES content-addressed pool
    /// files (under `assets/`, shared by content hash across techniques and
    /// clips) and `asset://` references: those are owned by the
    /// [`AssetStore`] and reclaimed only by its hash-refcount GC. Persisted
    /// examples hydrate to pool paths, so deleting one by path would wipe the
    /// audio of every other row that dedups to the same bytes. Same guards as
    /// `AudioFileService::delete_audio`. Silently ignores a missing file.
    pub fn delete_media(&self, path: &str) -> Result<()> {
        if AssetStore::asset_id_of(path).is_some() {
            return Ok(());
        }
        // Path-only guard: a delete must never create the pool directory as a
        // side effect (and must still resolve while the root is being torn down).
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

    /// Removes the whole media directory for a deleted technique (best
    /// effort).
    pub fn delete_technique_media(&self, technique_id: &str) -> Result<()> {
        Self::check_path_component(technique_id)?;
        let base = self.storage.asset_path("technique_media")?;
        match delete_recursive(&base.join(technique_id)) {
            Ok(()) => Ok(()),
            // Never had media, or it is already gone: deletion achieved.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl TechniqueMediaFiles for TechniqueMediaService {
    fn import_media(&self, technique_id: &str, source_path: &str) -> Result<String> {
        TechniqueMediaService::import_media(self, technique_id, source_path)
    }
    fn delete_media(&self, path: &str) -> Result<()> {
        TechniqueMediaService::delete_media(self, path)
    }
    fn delete_technique_media(&self, technique_id: &str) -> Result<()> {
        TechniqueMediaService::delete_technique_media(self, technique_id)
    }
}

/// Dart's `File.length`: the size a stat reports, links followed; a
/// directory is an error (EISDIR), anything else reports its size (a pipe's
/// is 0).
fn file_length(path: &str) -> io::Result<u64> {
    let meta = fs::metadata(path)?;
    if meta.is_dir() {
        return Err(io::Error::from_raw_os_error(libc::EISDIR));
    }
    Ok(meta.len())
}

/// Dart's `Directory.delete(recursive: true)`: whatever sits at [path] goes —
/// a directory with everything under it, or a file or a link (the link
/// itself, never what it names).
fn delete_recursive(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
