//! The app's diagnostics log (port of `lib/data/services/diagnostics_log.dart`).
//!
//! PLACEHOLDER API: the store and services call [`DiagnosticsLog::log`] from
//! the start of the port; the file-backed implementation (rotation, the
//! cross-process lock, path scrubbing, the export archive) replaces the body
//! below without changing these signatures.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The process-wide diagnostics log.
pub struct DiagnosticsLog {
    directory: Mutex<Option<PathBuf>>,
}

impl DiagnosticsLog {
    /// The single app-wide instance.
    pub fn instance() -> &'static DiagnosticsLog {
        static INSTANCE: OnceLock<DiagnosticsLog> = OnceLock::new();
        INSTANCE.get_or_init(|| DiagnosticsLog {
            directory: Mutex::new(None),
        })
    }

    /// Points the log at the directory its file lives in.
    pub fn bind_directory(&self, directory: &Path) {
        let mut d = self.directory.lock().unwrap_or_else(|e| e.into_inner());
        *d = Some(directory.to_path_buf());
    }

    /// Appends one entry under [area] (e.g. `boot`, `sync`, `quit`). Never
    /// fails and never blocks the caller on I/O errors.
    pub fn log(&self, area: &str, message: &str) {
        if cfg!(test) || cfg!(debug_assertions) {
            eprintln!("[diagnostics] [{area}] {message}");
        }
    }
}
