//! Carries the library out of the App Sandbox container, once.
//!
//! Every release up to 0.3.0 ran in the App Sandbox, where `HOME` is the
//! app's container, so everything it kept went under the container's
//! application support folder:
//!
//! ```text
//! ~/Library/Containers/<bundle id>/Data/Library/Application Support/<bundle id>
//! ```
//!
//! — the library (`vocara.db` and its WAL, the asset pool, recordings and
//! imports, technique media, staging and trash), the settings documents, the
//! stems cache and the separation models, the diagnostics log, the device
//! node id and the library lock. Later releases are not sandboxed (they
//! replace their own bundle when they update, as Clew does, which a
//! sandboxed app cannot do), so the same code resolves to
//!
//! ```text
//! ~/Library/Application Support/<bundle id>
//! ```
//!
//! and an updated user would open an empty library there. [`settle`] runs
//! first thing in `main`, before anything resolves, opens or creates a store
//! or the diagnostics log, and moves that one folder across with a single
//! `rename`. On one volume (the home folder's) a rename is atomic: a crash
//! leaves the folder wholly at one place or the other, never half of it and
//! never two copies, and the next launch decides again from what it finds —
//! so a second launch, or a re-run, after a move is a no-op. The library
//! stores no absolute path (asset rows hold pool-relative ones; the
//! migrator's marker, the settings and the lock hold none), so the moved
//! folder opens as it was. Each flavor's bundle id (`com.rutatang.vocara`,
//! `.dev`, `.test`) has its own container and its own folder; the app
//! settles the one it runs as.
//!
//! What [`migrate`] finds decides. Below, A is the Application Support
//! folder and C the container's; "a library" is a folder holding the
//! database, the backup a schema migration interrupted mid-swap left, or the
//! migration's marker:
//!
//! | A | C | |
//! |---|---|---|
//! | a library | anything | A opens. C is left exactly as it is — never merged, never overwritten — and, when it holds a library, named for the app to tell the user ([`Outcome::BothHold`], [`library_left_behind`]); when it holds files but no library (logs, stems, models left once its library was deleted), only logged ([`Outcome::FilesLeftInContainer`]). |
//! | no library | nothing (missing, empty) | A opens ([`Outcome::NothingToMove`]): a fresh install, or one moved before. |
//! | nothing (missing, empty) | anything | C is moved to A ([`Outcome::Moved`]). |
//! | files, but no library | anything | C stays and opens ([`Why::UnsandboxedNotEmpty`]): nothing is merged into A or deleted from it. |
//! | unreadable | anything | C stays and opens ([`Why::UnsandboxedUnreadable`]). |
//! | no library | unreadable | C stays, and the open that then fails there says why ([`Why::ContainerUnreadable`]). |
//!
//! "Empty" counts Finder's `.DS_Store` as nothing: it is all a folder gains
//! from being looked at. When C stays ([`Outcome::StaysInContainer`]), the
//! whole app is rooted at it for that launch ([`app_support::root_at`]): the
//! library, the settings, the asset folders and the diagnostics log all open
//! there, as they did while sandboxed, so the user never meets an empty
//! library while theirs sits in the container. A later launch moves it if
//! it then can. Both hold a library when an older, sandboxed Vocara was
//! opened after a newer one had moved the library out (a copy left in
//! /Applications while the new one ran from its disk image): it starts a
//! fresh library in its container, and takes recorded there are in no
//! library the app opens. Or when someone ran an unsandboxed build of the
//! same flavor (a debug bundle, `cargo run`) beside a sandboxed release. The
//! unsandboxed one is what every build from now on keeps using, so it wins;
//! the container's is left as it is, and Home tells the user where it is.
//!
//! The move first takes the library's single-instance lock
//! ([`library_lock::hold_for_move`]). An older, sandboxed Vocara that is
//! still open holds it, and moving the folder from under it would split the
//! library in two (it would go on writing new files at the old paths). The
//! move waits a moment for the lock ([`Patience`]) — of two launches at the
//! same moment, one moves and the other then finds the library moved — and
//! past that, C stays for this launch ([`Why::InUse`]). The move may also
//! fail outright, onto another volume (`EXDEV`: a home folder split across
//! volumes by hand) or refused, and C stays ([`Why::MoveFailed`]): copying a
//! library that may hold gigabytes of audio, crash-safely, at launch, is not
//! worth that rare case.
//!
//! Only that one folder moves. The rest of the container is the system's and
//! AppKit's — preferences (the panels' last folders), saved window state,
//! caches, temporary files — none of it the user's work; the container
//! itself stays, for macOS to manage, holding no data once the folder is out.
//!
//! macOS 14 and later protect a sandboxed app's container ("App Data"):
//! another app reading it asks the user first. The app the container
//! belongs to — the same bundle id, signed by the same team — is expected to
//! read it without asking, sandboxed or not; the release workflow checks
//! that on every release by updating a real container made by the last
//! sandboxed release (`.github/scripts/launch-check.sh --update`). A
//! build signed otherwise (an ad-hoc local bundle) may be asked about, and
//! refused: a refusal is a look or a move that fails, which roots the app at
//! the container — its open then fails there with the system's reason, in
//! the fatal window with "Reveal Library Folder" — never an empty library.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vocara_model::config::app_support;
use vocara_model::config::environment::AppEnvironment;
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_store::db::durable_fs::fsync_dir;
use vocara_store::db::library_lock::{self, LibraryLock, MoveHold};
use vocara_store::db::safe_migrator::SafeMigrator;

use crate::local_storage_service::default_application_support_dir;
use crate::persistence::DB_FILE_NAME;

/// The entries that make a folder a library (see the module doc): the
/// database, the database a schema migration's interrupted swap left as its
/// backup, the migration's marker.
const LIBRARY_ENTRIES: [&str; 3] = [
    DB_FILE_NAME,
    SafeMigrator::BAK_NAME,
    SafeMigrator::MARKER_NAME,
];

/// What Finder writes into a folder the user merely opens: not their data.
const FINDER_VIEW_SETTINGS: &str = ".DS_Store";

/// How many times a move that lost a race (the folder moved, or appeared,
/// between the look and the rename) looks again before C stays.
const RACE_RETRIES: u32 = 8;

/// The diagnostics log's area for what [`settle`] did.
const LOG_AREA: &str = "storage";

/// One bundle id's two folders (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locations {
    /// The App Sandbox container's application support folder (C), where a
    /// sandboxed release kept everything.
    pub container: PathBuf,
    /// The Application Support folder (A): the stores' default directory,
    /// where an unsandboxed build keeps everything.
    pub unsandboxed: PathBuf,
}

impl Locations {
    /// [bundle_id]'s two folders as macOS lays them out for a user whose
    /// home folder is [home] (what a sandboxed process's `HOME` pointed into,
    /// and what an unsandboxed one's is).
    pub fn under_home(home: &Path, bundle_id: &str) -> Locations {
        Locations {
            container: home
                .join("Library/Containers")
                .join(bundle_id)
                .join("Data/Library/Application Support")
                .join(bundle_id),
            unsandboxed: home.join("Library/Application Support").join(bundle_id),
        }
    }
}

/// [bundle_id]'s two folders on this machine: the container's under [home],
/// and the directory the stores default to
/// ([`default_application_support_dir`]), so that A is exactly where they
/// open (on macOS the same as [`Locations::under_home`]'s).
pub fn locations(home: &Path, bundle_id: &str) -> io::Result<Locations> {
    Ok(Locations {
        container: Locations::under_home(home, bundle_id).container,
        unsandboxed: default_application_support_dir(bundle_id)?,
    })
}

/// How long a move waits for a library another Vocara holds, and how often
/// it looks again meanwhile.
#[derive(Debug, Clone, Copy)]
pub struct Patience {
    pub wait: Duration,
    pub poll: Duration,
}

impl Default for Patience {
    /// The library lock's own: two seconds, a look every 100 ms.
    fn default() -> Self {
        Patience {
            wait: LibraryLock::DEFAULT_WAIT,
            poll: LibraryLock::DEFAULT_POLL,
        }
    }
}

/// What [`migrate`] found and did (see the module doc's table).
#[derive(Debug)]
pub enum Outcome {
    /// No library in the container — a fresh install, or one moved before —
    /// or A's library opens and the container holds nothing.
    NothingToMove,
    /// The container's folder is A now.
    Moved,
    /// A holds a library and opens; the container holds one too and is left
    /// exactly as it is.
    BothHold,
    /// A holds a library and opens; the container holds files but no
    /// library (what is left once someone deleted the library from it: the
    /// logs, the stems cache, the models), and is left as it is. Logged, but
    /// nothing the app tells the user about: there are no recordings in it
    /// to miss.
    FilesLeftInContainer,
    /// A holds a library and opens; whether the container holds anything
    /// could not be told.
    ContainerUnchecked(io::Error),
    /// The container's folder stays where it is this launch, and the app is
    /// rooted at it.
    StaysInContainer(Why),
}

/// Why the container's folder stays where it is.
#[derive(Debug)]
pub enum Why {
    /// Another Vocara (an older, sandboxed one still open) has the library
    /// open.
    InUse,
    /// Whether the container holds a library could not be told (macOS
    /// refusing a build it does not take for the container's owner, or plain
    /// permissions).
    ContainerUnreadable(io::Error),
    /// A could not be read.
    UnsandboxedUnreadable(io::Error),
    /// A holds files but no library: nothing is merged into it, and nothing
    /// deleted from it.
    UnsandboxedNotEmpty,
    /// The move itself failed (onto another volume, refused).
    MoveFailed(io::Error),
}

impl Outcome {
    /// The diagnostics entry for this outcome; `None` for the everyday one
    /// (nothing to move). Names the folders relative to the home folder,
    /// which the log's path scrubbing leaves as they are.
    pub fn describe(&self, bundle_id: &str) -> Option<String> {
        let container = format!("the App Sandbox container (Library/Containers/{bundle_id})");
        let unsandboxed = format!("Library/Application Support/{bundle_id}");
        Some(match self {
            Outcome::NothingToMove => return None,
            Outcome::Moved => {
                format!("moved the library out of {container} into {unsandboxed}")
            }
            Outcome::BothHold => format!(
                "both {unsandboxed} and {container} hold a library: {unsandboxed} opens, \
                 and the container's is left as it is (nothing merged, nothing overwritten)"
            ),
            Outcome::FilesLeftInContainer => format!(
                "{unsandboxed} opens; {container} holds files but no library, left as they are"
            ),
            Outcome::ContainerUnchecked(e) => {
                format!("{unsandboxed} opens; could not check {container} for a library ({e})")
            }
            Outcome::StaysInContainer(why) => {
                let why = match why {
                    Why::InUse => "another Vocara has it open; it moves at a launch when none does"
                        .to_string(),
                    Why::ContainerUnreadable(e) => format!("the container could not be read ({e})"),
                    Why::UnsandboxedUnreadable(e) => {
                        format!("{unsandboxed} could not be read ({e})")
                    }
                    Why::UnsandboxedNotEmpty => format!(
                        "{unsandboxed} holds files but no library, and nothing is merged into \
                         it or deleted from it"
                    ),
                    Why::MoveFailed(e) => format!("the move failed ({e})"),
                };
                format!(
                    "the library stays in {container} this launch, and every store opens it \
                     there: {why}"
                )
            }
        })
    }
}

/// What a folder holds, as far as the move is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// Missing, or empty but for Finder's view settings.
    Nothing,
    /// A library (see [`LIBRARY_ENTRIES`]).
    Library,
    /// Something, but no library (or a file where the folder should be).
    Files,
}

/// What [dir] holds; an error when it could not be told.
fn look(dir: &Path) -> io::Result<Holds> {
    let meta = match fs::metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Holds::Nothing),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return Ok(Holds::Files);
    }
    let mut holds = Holds::Nothing;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if LIBRARY_ENTRIES.iter().any(|library| name == *library) {
            return Ok(Holds::Library);
        }
        if name != FINDER_VIEW_SETTINGS {
            holds = Holds::Files;
        }
    }
    Ok(holds)
}

/// Decides, and moves the container's folder to A when it should (see the
/// module doc). Touches nothing but the two folders, and creates neither
/// unless it moves (A's parent then, when missing).
pub fn migrate(at: &Locations, patience: &Patience) -> Outcome {
    migrate_with(at, patience, &|from: &Path, to: &Path| fs::rename(from, to))
}

/// [`migrate`] with the rename as a seam (tests: a move refused, a move
/// another launch made first).
fn migrate_with(
    at: &Locations,
    patience: &Patience,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Outcome {
    let stays = Outcome::StaysInContainer;
    let deadline = Instant::now() + patience.wait;
    let mut races = 0;
    loop {
        let unsandboxed = look(&at.unsandboxed);
        if let Ok(Holds::Library) = unsandboxed {
            // A's library opens, whatever the container holds.
            return match look(&at.container) {
                Ok(Holds::Nothing) => Outcome::NothingToMove,
                Ok(Holds::Library) => Outcome::BothHold,
                Ok(Holds::Files) => Outcome::FilesLeftInContainer,
                Err(e) => Outcome::ContainerUnchecked(e),
            };
        }
        match look(&at.container) {
            Ok(Holds::Nothing) => return Outcome::NothingToMove,
            Ok(_) => {}
            Err(e) => return stays(Why::ContainerUnreadable(e)),
        }
        match unsandboxed {
            Ok(Holds::Nothing) => {}
            Ok(_) => return stays(Why::UnsandboxedNotEmpty),
            Err(e) => return stays(Why::UnsandboxedUnreadable(e)),
        }
        // Held until this iteration ends: across the rename, and dropped
        // (closing the file lets the lock go) before a look again.
        let _hold = match library_lock::hold_for_move(&at.container) {
            MoveHold::InUse if Instant::now() < deadline => {
                std::thread::sleep(patience.poll);
                continue;
            }
            MoveHold::InUse => return stays(Why::InUse),
            MoveHold::Held(file) => Some(file),
            MoveHold::Unguarded => None,
        };
        match move_folder(at, rename) {
            Ok(()) => return Outcome::Moved,
            Err(e) if lost_a_race(&e) && races < RACE_RETRIES => races += 1,
            Err(e) => return stays(Why::MoveFailed(e)),
        }
    }
}

/// Moves the container's folder to A. An empty A in the way goes first (a
/// non-empty one by now fails, and the next look decides); A's parent is
/// made when missing.
fn move_folder(at: &Locations, rename: &dyn Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let ignore_missing = |result: io::Result<()>| match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    };
    ignore_missing(fs::remove_file(at.unsandboxed.join(FINDER_VIEW_SETTINGS)))?;
    ignore_missing(fs::remove_dir(&at.unsandboxed))?;
    if let Some(parent) = at.unsandboxed.parent() {
        fs::create_dir_all(parent)?;
    }
    rename(&at.container, &at.unsandboxed)?;
    // On permanent storage before the library is next written at A.
    for parent in [at.unsandboxed.parent(), at.container.parent()]
        .into_iter()
        .flatten()
    {
        fsync_dir(parent);
    }
    Ok(())
}

/// A move that failed because the folders changed under it: the container's
/// folder is gone (another launch moved it), or A has appeared.
fn lost_a_race(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
    )
}

/// [`migrate`] between [at] and what follows from it: [bundle_id]'s stores
/// rooted at the container when the library stays there
/// ([`app_support::root_at`]), and a diagnostics entry for anything but the
/// everyday nothing-to-move (buffered until the log is bound). Returns the
/// outcome.
pub fn settle_in(bundle_id: &str, at: &Locations, patience: &Patience) -> Outcome {
    let outcome = migrate(at, patience);
    if let Outcome::StaysInContainer(_) = outcome {
        app_support::root_at(bundle_id, at.container.clone());
    }
    if let Outcome::BothHold = outcome {
        *left_behind_lock() = Some(at.container.clone());
    }
    if let Some(entry) = outcome.describe(bundle_id) {
        DiagnosticsLog::instance().log(LOG_AREA, &entry);
    }
    outcome
}

/// The library this launch left in the App Sandbox container because the
/// unsandboxed folder held one too ([`Outcome::BothHold`]): an older,
/// sandboxed Vocara opened after a newer one had moved the library out
/// starts a fresh one there, and the takes recorded in it are in no
/// library this app opens. Nothing merges or moves it; the app says it is
/// there (Home asks once a run, with the folder to show).
pub fn library_left_behind() -> Option<PathBuf> {
    left_behind_lock().clone()
}

fn left_behind_lock() -> std::sync::MutexGuard<'static, Option<PathBuf>> {
    static LEFT_BEHIND: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    LEFT_BEHIND
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Settles where [env]'s data lives ([`settle_in`] for its bundle id, under
/// the user's home folder): the first thing `main` does, before anything
/// resolves the application support directory. macOS only — elsewhere
/// there are no App Sandbox containers, and it does nothing.
pub fn settle(env: AppEnvironment) {
    #[cfg(target_os = "macos")]
    {
        // A sandboxed process (a build signed with the sandbox entitlement)
        // has its HOME in its container already: the stores resolve inside
        // it, and there is nothing to carry anywhere.
        if std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some() {
            return;
        }
        let bundle_id = env.bundle_id();
        let Some(home) = dirs::home_dir() else {
            return;
        };
        match locations(&home, bundle_id) {
            Ok(at) => {
                settle_in(bundle_id, &at, &Patience::default());
            }
            Err(e) => DiagnosticsLog::instance().log(
                LOG_AREA,
                &format!("no Application Support folder to settle the library in ({e})"),
            ),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = env;
}

#[cfg(test)]
mod tests;
