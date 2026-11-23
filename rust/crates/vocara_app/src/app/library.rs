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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use vocara_model::config::environment::AppEnvironment;
use vocara_model::util::id::new_id;
use vocara_store::db::asset_store::AssetStore;
use vocara_store::db::schema::AppDatabase;
use vocara_store::repositories::{
    SongPracticeRepository, SongPracticeRepositoryDrift, TechniqueRepository,
    TechniqueRepositoryDrift,
};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_writer::SyncWriter;

/// The opened library (Dart's `Persistence`: the database, the asset pool,
/// both repositories and the remote-applied stream).
///
/// INTERIM: until `vocara_services::persistence` is wired in, this is an
/// in-memory store (nothing reaches the disk) with the real repositories
/// over it, so every screen runs against the real data layer.
pub struct Library {
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
    /// A library over an in-memory database, its asset pool in a fresh
    /// temporary folder.
    pub fn in_memory() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let db = Arc::new(AppDatabase::open_in_memory().expect("in-memory database"));
        let clock = Arc::new(HlcClock::new(new_id()));
        let writer = Arc::new(SyncWriter::new(db.clone(), clock));
        let root = std::env::temp_dir().join(format!(
            "vocara-memory-library-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let assets = Arc::new(AssetStore::new(db.clone(), root, writer.clone()));
        Library {
            song_repo: Arc::new(SongPracticeRepositoryDrift::new(
                db.clone(),
                writer.clone(),
                assets.clone(),
            )),
            technique_repo: Arc::new(TechniqueRepositoryDrift::new(db, writer, assets)),
        }
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
