//! Opening the library at launch: the seam for `Persistence.open` and the
//! outcomes `main.dart` handled, and the "Reveal Library Folder" action.
//!
//! The open goes through an [`OpenLibrary`] function returning a
//! [`LibraryOpen`] — the outcomes Dart's `Persistence.open` had (the opened
//! library, or `NewerLibraryException`, `LibraryInUseException`, or any
//! other failure), with the `onWaitingForLibrary` callback. The app runs
//! [`open_library`] (`vocara_services::persistence`); tests run
//! [`open_library_stub`], an in-memory library.

use std::path::PathBuf;
use std::sync::Arc;

use vocara_model::config::environment::AppEnvironment;
use vocara_services::local_storage_service::application_support_dir;
use vocara_services::persistence::{library_dir, OpenError, OpenOptions, Persistence};
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};

/// The opened library: Dart's `Persistence` (the database, the asset pool,
/// both repositories and the remote-applied stream), with its repositories
/// as the trait objects the screens read.
pub struct Library {
    persistence: Persistence,
    /// `persistence.songRepo`.
    pub song_repo: Arc<dyn SongPracticeRepository>,
    /// `persistence.techRepo`.
    pub technique_repo: Arc<dyn TechniqueRepository>,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library").finish_non_exhaustive()
    }
}

impl Library {
    /// The library over an opened [`Persistence`].
    pub fn new(persistence: Persistence) -> Self {
        let song_repo: Arc<dyn SongPracticeRepository> = persistence.song_repo().clone();
        let technique_repo: Arc<dyn TechniqueRepository> = persistence.tech_repo().clone();
        Library {
            persistence,
            song_repo,
            technique_repo,
        }
    }

    /// The persistence layer itself, for the maintenance surfaces
    /// (Settings' "Repair library", the remote-applied stream).
    pub fn persistence(&self) -> &Persistence {
        &self.persistence
    }

    /// A library over an in-memory database (`Persistence.memory`), its
    /// asset pool in a fresh temporary folder: what the app's tests open.
    pub fn in_memory() -> Self {
        Library::new(Persistence::memory().expect("in-memory library"))
    }

    /// The library [`open_library_stub`] opens.
    pub fn stub() -> Self {
        Self::in_memory()
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

/// The stand-in open: an in-memory library, at once (tests).
pub fn open_library_stub(_on_waiting: &mut dyn FnMut()) -> LibraryOpen {
    LibraryOpen::Opened(Arc::new(Library::stub()))
}

/// The app's open (`Persistence.open` in `main.dart`): the library in this
/// environment's application support directory, with `on_waiting` called
/// when another Vocara is still starting up on it.
pub fn open_library(on_waiting: &mut dyn FnMut()) -> LibraryOpen {
    let env = AppEnvironment::current();
    let failed = |e: &dyn std::fmt::Display, log: String| LibraryOpen::Failed {
        detail: e.to_string(),
        log,
    };
    let dir = match library_dir(env.bundle_id()) {
        Ok(dir) => dir,
        Err(e) => return failed(&e, format!("{e:?}")),
    };
    match Persistence::open(OpenOptions::new(dir).on_waiting_for_library(on_waiting)) {
        Ok(persistence) => LibraryOpen::Opened(Arc::new(Library::new(persistence))),
        Err(e @ OpenError::NewerLibrary { .. }) => LibraryOpen::NewerLibrary(e.to_string()),
        Err(e @ OpenError::LibraryInUse) => LibraryOpen::LibraryInUse(e.to_string()),
        Err(e) => failed(&e, format!("{e:?}")),
    }
}

/// The app's library folder: the application support directory per bundle
/// id (`~/Library/Application Support/com.rutatang.vocara[.dev|.test]` on
/// macOS), which holds the database and the asset pool — the folder
/// [`open_library`] opens.
pub fn library_folder(env: AppEnvironment) -> Option<PathBuf> {
    application_support_dir(env.bundle_id()).ok()
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
