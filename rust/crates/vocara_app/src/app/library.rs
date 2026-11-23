//! Opening the library at launch: the seam for `Persistence.open` and the
//! outcomes `main.dart` handled, and the "Reveal Library Folder" action.
//!
//! PERSISTENCE SEAM: `vocara_store`'s `Persistence` is not ported yet, so the
//! open goes through an [`OpenLibrary`] function returning a
//! [`LibraryOpen`] — the same outcomes Dart's `Persistence.open` had (the
//! opened library, or `NewerLibraryException`, `LibraryInUseException`, or
//! any other failure), with the `onWaitingForLibrary` callback. The app runs
//! with [`open_library_stub`], which always succeeds, until the store port
//! provides the real open.

use std::path::PathBuf;
use std::sync::Arc;

use super::environment::AppEnvironment;

/// The opened library (Dart's `Persistence`: the database, the asset pool,
/// both repositories and the remote-applied stream).
///
/// PLACEHOLDER: an empty token until `vocara_store::db::persistence` lands;
/// it then becomes (or wraps) the opened `Persistence`.
#[derive(Debug)]
pub struct Library {
    _private: (),
}

impl Library {
    /// The placeholder library [`open_library_stub`] opens.
    pub fn stub() -> Self {
        Library { _private: () }
    }
}

/// How opening the library went.
#[derive(Debug, Clone)]
pub enum LibraryOpen {
    /// Opened: the app starts.
    Opened(Arc<Library>),
    /// `NewerLibraryException`: the library was written by a newer Vocara.
    /// The text is the refusal, for the diagnostics log.
    NewerLibrary(String),
    /// `LibraryInUseException`: another Vocara runs on it and this one would
    /// have to upgrade it.
    LibraryInUse(String),
    /// Any other failure: the error as text (shown as the detail, `'$e'`) and
    /// whatever else the log should get (Dart logged the stack too).
    Failed { detail: String, log: String },
}

/// Opens the library. `on_waiting` is called (from the opening thread) when
/// another Vocara is still starting up on it and this one waits for it
/// (`onWaitingForLibrary`). Blocks until the library is open or refused.
pub type OpenLibrary = fn(on_waiting: &mut dyn FnMut()) -> LibraryOpen;

/// The stand-in open: always succeeds at once.
pub fn open_library_stub(_on_waiting: &mut dyn FnMut()) -> LibraryOpen {
    LibraryOpen::Opened(Arc::new(Library::stub()))
}

/// The app's library folder: the application support directory per bundle
/// id (`~/Library/Application Support/com.rutatang.vocara[.dev|.test]` on
/// macOS), which holds the database and the asset pool.
///
/// LOCAL STAND-IN for `getApplicationSupportDirectory()` until the store
/// port provides the library's location.
pub fn library_folder(env: AppEnvironment) -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(env.bundle_id()))
}

/// Opens the library folder in the file manager (Finder), for backups or
/// support, and waits for the opener (`Process.run`). Best-effort: the fatal
/// window stays up either way. Blocking: run it off the UI thread.
pub fn reveal_library_folder(env: AppEnvironment) {
    let Some(dir) = library_folder(env) else {
        return;
    };
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(dir).status();
}
