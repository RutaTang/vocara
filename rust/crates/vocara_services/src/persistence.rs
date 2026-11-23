//! The library's boot and lifecycle (port of `lib/data/db/persistence.dart`):
//! opening the database under the single-instance lock and the safe
//! migrator, the device node id and the HLC seed, wiring the write
//! chokepoint, the asset pool and both repositories, recovering takes a hard
//! kill left in staging, the gated boot sweeps, attaching sync, the
//! user-invoked repair, and close.
//!
//! It lives in the services crate, not the store, because it drives services:
//! boot recovery analyzes takes ([`waveform_service`]), cuts their pre-roll
//! ([`audio_edit_service::trim_wav_head_in_place`]) and reads their
//! placement ([`audio_file_service`]).
//!
//! # Boot order ([`Persistence::open`])
//!
//! 1. The single-instance guard ([`LibraryLock`]): wait out another
//!    launch's boot, then try to own the library exclusively.
//! 2. Owner: finish or roll back a half-done migration. Non-owner: refuse a
//!    library whose migration is unfinished.
//! 3. Refuse a library written by a newer Vocara, untouched.
//! 4. Owner: upgrade a stale schema (copy, migrate, validate, swap).
//!    Non-owner: refuse one that needs it.
//! 5. Open the connection: the clock-timed busy wait, the oplog-insert
//!    notifier, then the store's own open (schema, pragmas, indexes).
//! 6. The device node id; the HLC clock seeded from every durable stamp.
//! 7. Wire the writer, the asset pool and the repositories.
//! 8. Owner: probe the tables the sweeps read, then recover staged takes;
//!    both succeeding opens the sweep gate.
//! 9. Owner: clear waveform scratch.
//! 10. Gate open: drop the pre-migration backup, then, under the write gate
//!     held exclusively, sweep staging, unreferenced assets, orphan pool
//!     files and the trash, and compact the oplog.
//! 11. Owner: hand the lock back to shared.
//!
//! Every call blocks its caller (Dart awaited them); the app runs them off
//! the UI thread.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_model::util::{dart_path, dart_string};
use vocara_store::db::durable_fs::{self, DurableFs, SystemDurableFs};
use vocara_store::db::library_lock::{FixedGuard, LibraryGuard, LibraryLock};
use vocara_store::db::rows::PracticeRow;
use vocara_store::db::safe_migrator::SafeMigrator;
use vocara_store::db::schema::{ALL_TABLES, SCHEMA_V2_SQL};
use vocara_store::db::write_gate::WriteGate;
use vocara_store::db::{AppDatabase, AssetStore, CURRENT_SCHEMA_VERSION};
use vocara_store::repositories::{
    SongPracticeRepository, SongPracticeRepositoryDrift, TechniqueRepositoryDrift,
};
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::oplog_drain::OplogDrain;
use vocara_store::sync::quarantine_store::QuarantineStore;
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_engine::{
    OplogInserts, PageCommitHook, RemoteApplied, SyncEngine, SyncEngineOptions, SyncEngineParts,
    SyncTimerFactory,
};
use vocara_store::sync::sync_row::SyncRowBuilder;
use vocara_store::sync::sync_state_store::SyncStateStore;
use vocara_store::sync::sync_transport::SyncTransport;
use vocara_store::sync::sync_writer::SyncWriter;
use vocara_store::Error;

use crate::audio_edit_service::trim_wav_head_in_place;
use crate::audio_file_service::{
    dart_int_try_parse, probe_wav, recording_placement_of, write_aligned_start_to, WavProbe,
};
use crate::local_storage_service::application_support_dir;
use crate::waveform_service::{self, WaveformError, WaveformResult, DEFAULT_TARGET_BUCKETS};

/// The audio analyzer used to rebuild a recovered take's waveform peaks and
/// duration — [`waveform_service::analyze`] in production, injectable so
/// tests don't need the engine's decoder. Must fail on undecodable audio.
pub type WavAnalyzer = Arc<dyn Fn(&str) -> Result<WaveformResult, WaveformError> + Send + Sync>;

/// The production analyzer: [`waveform_service::analyze`] at its default
/// bucket count (Dart's `WaveformService(...).analyze`).
pub fn default_analyzer() -> WavAnalyzer {
    Arc::new(|path: &str| waveform_service::analyze(path, DEFAULT_TARGET_BUCKETS))
}

/// Why [`Persistence::open`] refused or failed. Anything but the two
/// refusals is a fatal open, surfaced by the app in a dedicated error window.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The on-disk library's schema `user_version` is HIGHER than this
    /// build's [`CURRENT_SCHEMA_VERSION`]: the library was written by a newer
    /// Vocara. Opening it anyway would run this build against tables and
    /// columns it doesn't understand. Typed so the app can show a dedicated
    /// "please update Vocara" window instead of a generic failure (Dart's
    /// `NewerLibraryException`).
    #[error(
        "NewerLibraryException: the library schema is v{library_version} but this build \
         understands up to v{app_version}"
    )]
    NewerLibrary {
        /// The schema `user_version` found in the on-disk database.
        library_version: i64,
        /// The schema version this build understands.
        app_version: i64,
    },
    /// The library needs a schema upgrade (or the recovery of an unfinished
    /// one) but another Vocara process holds it ([`LibraryLock`]). The
    /// upgrade swaps the database file, which must never happen under a
    /// process that has it open, and the store cannot open the stale file
    /// without it (Dart's `LibraryInUseException`).
    #[error("LibraryInUseException: the library needs an upgrade but another Vocara is using it")]
    LibraryInUse,
    /// The store failed (the migrator, the database).
    #[error(transparent)]
    Store(#[from] Error),
    /// A file system failure (the device node id).
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What one [`Persistence::repair_library`] pass found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RepairReport {
    /// Cross-row integrity violations detected before repairing.
    pub violations_found: usize,
    /// How many of those the repair pass resolved.
    pub violations_repaired: usize,
    /// Unreferenced assets tombstoned plus orphaned pool files reclaimed.
    pub assets_swept: usize,
    /// Pooled files whose bytes no longer hash to the address they are
    /// stored under (see [`AssetStore::verify_pool_integrity`]). Reported,
    /// never touched: the file is still the user's recording.
    pub corrupt_pool_files: usize,
}

impl fmt::Display for RepairReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RepairReport(found: {}, repaired: {}, assetsSwept: {}, corruptPoolFiles: {})",
            self.violations_found,
            self.violations_repaired,
            self.assets_swept,
            self.corrupt_pool_files
        )
    }
}

/// How one staged WAV's boot recovery ended. [`RecoveryOutcome::Junk`] and
/// [`RecoveryOutcome::Failed`] are both "not recovered", but only junk lets
/// the staging sweep take the file: nothing may trash the only copy of a take
/// on the strength of a failure the environment caused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryOutcome {
    /// Attached to its practice as a new take; the staging copy is a
    /// leftover.
    Recovered,
    /// Provably not a recoverable take: not the shape the recorder writes,
    /// bytes that some save already committed, or a practice that no longer
    /// exists.
    Junk,
    /// The attempt could not complete — an unreadable file, a decoder
    /// failure, a store that reported an error. Held back for the next boot
    /// to retry.
    Failed,
}

/// A recovery attempt that threw (Dart: anything the per-file `catch`
/// caught).
#[derive(Debug, thiserror::Error)]
enum AttemptError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] Error),
    #[error(transparent)]
    Analyze(WaveformError),
}

impl AttemptError {
    /// The Dart runtime type the failure would have had, for diagnostics.
    fn kind(&self) -> &'static str {
        match self {
            AttemptError::Io(_) => "FileSystemException",
            AttemptError::Store(e) => store_error_kind(e),
            AttemptError::Analyze(_) => "WaveformError",
        }
    }
}

/// The Dart runtime type a store error would have had, for diagnostics
/// (Dart logged `e.runtimeType`).
fn store_error_kind(e: &Error) -> &'static str {
    match e {
        Error::Sqlite(_) => "SqliteException",
        Error::Io(_) => "FileSystemException",
        Error::InvalidData(_) => "InvalidDataException",
        Error::InvalidArgument(_) => "ArgumentError",
        Error::State(_) => "StateError",
        Error::Format(_) => "FormatException",
        Error::Migration { .. } => "MigrationException",
        Error::HlcDrift(_) => "HlcDriftException",
    }
}

fn log(message: &str) {
    DiagnosticsLog::instance().log("persistence", message);
}

/// Locks [m], carrying on past a poisoned lock: every value kept under one
/// here is a plain set or slot that a panic cannot leave half-written.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The library database's file name inside the library directory.
pub const DB_FILE_NAME: &str = "vocara.db";

/// The per-install device node id's file name inside the library directory.
pub const NODE_FILE_NAME: &str = "device_node";

/// How long a write waits for another connection's transaction to commit
/// before it fails as "database is locked".
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Installs [`LOCK_WAIT`] on the live library's connection as it opens. A
/// second running instance shares the library (see [`LibraryLock`]) through
/// its own connection, so a write meeting the other's transaction must wait
/// for it, not fail: failed, it was a save that reported an error and
/// reverted its edit.
///
/// Not `PRAGMA busy_timeout`: SQLite's own handler counts its retries rather
/// than timing them (and in the Dart VM a signal cut each of its sleeps short,
/// so its five seconds came to under a tenth of one). This one waits by the
/// clock. The wait blocks only the thread making the write.
fn wait_out_other_writers(conn: &mut Connection) -> rusqlite::Result<()> {
    let mut waited = Instant::now();
    conn.busy_handler(Some(move |count: i32| {
        if count == 0 {
            waited = Instant::now();
        }
        if waited.elapsed() >= LOCK_WAIT {
            return false;
        }
        std::thread::sleep(Duration::from_millis(if count < 10 { 2 } else { 20 }));
        true
    }))
}

/// Reads the schema `user_version` of the library database at [db] for the
/// diagnostics bundle (SafeMigrator's read-only read), or the type of what
/// went wrong.
fn read_schema_version(db: &Path) -> Result<i64, String> {
    SafeMigrator::new()
        .read_user_version(db)
        .map_err(|e| store_error_kind(&e).to_string())
}

/// Lets the diagnostics bundle read the library's schema version through
/// the migrator, as Dart's did (the model crate has no SQLite). Called by
/// [`Persistence::open`]; the app may call it earlier (a bundle exported
/// from the fatal-open window). Idempotent.
pub fn install_schema_version_reader() {
    DiagnosticsLog::set_schema_version_reader(read_schema_version);
}

/// The library directory for [bundle_id], created when missing: the
/// application support directory, as Flutter's
/// `getApplicationSupportDirectory()` returned (and created) it.
pub fn library_dir(bundle_id: &str) -> io::Result<PathBuf> {
    let dir = application_support_dir(bundle_id)?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Makes the single-instance guard for a library directory (a test seam:
/// the real lock cannot conflict within one process).
pub type LibraryLockFactory<'a> = Box<dyn FnOnce(&Path) -> Arc<dyn LibraryGuard> + 'a>;

/// What [`Persistence::open`] opens, and its seams.
pub struct OpenOptions<'a> {
    /// The library directory (Dart's `dir ?? getApplicationSupportDirectory()`;
    /// production passes [`library_dir`]).
    pub dir: PathBuf,
    /// The recovery waveform analyzer (None: [`default_analyzer`]).
    pub analyze: Option<WavAnalyzer>,
    /// The single-instance guard (None: [`LibraryLock::for_library_with`]
    /// with its default wait and poll).
    pub library_lock: Option<LibraryLockFactory<'a>>,
    /// Called once if another Vocara's boot keeps this launch waiting for
    /// its hold on the library (see [`LibraryLock`]): the app shows a window
    /// meanwhile. The wait has no deadline — going on without the hold left
    /// the work of this instance where that boot's sweeps could trash it.
    pub on_waiting_for_library: Option<&'a mut dyn FnMut()>,
}

impl<'a> OpenOptions<'a> {
    /// The library in [dir], with the production analyzer and lock.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        OpenOptions {
            dir: dir.into(),
            analyze: None,
            library_lock: None,
            on_waiting_for_library: None,
        }
    }

    /// These options with [analyze] as the recovery analyzer.
    pub fn analyzer(mut self, analyze: WavAnalyzer) -> Self {
        self.analyze = Some(analyze);
        self
    }

    /// These options with [guard] as the single-instance guard.
    pub fn guard(mut self, guard: Arc<dyn LibraryGuard>) -> Self {
        self.library_lock = Some(Box::new(move |_| guard));
        self
    }

    /// These options calling [on_waiting] if another launch's boot keeps
    /// this one waiting.
    pub fn on_waiting_for_library(mut self, on_waiting: &'a mut dyn FnMut()) -> Self {
        self.on_waiting_for_library = Some(on_waiting);
        self
    }
}

/// The seams of [`Persistence::attach_sync`] (Dart's optional arguments).
#[derive(Clone, Default)]
pub struct AttachSyncOptions {
    /// How the engine's debounce timers are minted (None: real timers).
    pub timers: Option<SyncTimerFactory>,
    /// Test-only: runs inside each pulled page's transaction.
    pub debug_before_page_commit: Option<PageCommitHook>,
}

/// The "remote changes were applied by sync" broadcast: every subscriber
/// gets each event sent after it subscribed (Dart's broadcast
/// `StreamController`); a dropped receiver is forgotten at the next event.
#[derive(Default)]
struct RemoteAppliedBroadcast {
    listeners: Mutex<Vec<mpsc::Sender<RemoteApplied>>>,
}

impl RemoteAppliedBroadcast {
    fn subscribe(&self) -> mpsc::Receiver<RemoteApplied> {
        let (tx, rx) = mpsc::channel();
        lock(&self.listeners).push(tx);
        rx
    }

    fn add(&self, event: &RemoteApplied) {
        lock(&self.listeners).retain(|tx| tx.send(event.clone()).is_ok());
    }
}

/// The wired-up persistence layer: one database, the write chokepoint, the
/// asset pool, and the two repositories built on them. Constructed ONCE at
/// app startup (the database file, the per-install device node id and the
/// asset pool all live under the library directory). `Send + Sync`; the app
/// shares it as `Arc<Persistence>`.
pub struct Persistence {
    db: Arc<AppDatabase>,
    writer: Arc<SyncWriter>,
    assets: Arc<AssetStore>,
    song_repo: Arc<SongPracticeRepositoryDrift>,
    tech_repo: Arc<TechniqueRepositoryDrift>,
    owns_library: bool,
    guard: Arc<dyn LibraryGuard>,
    oplog_inserts: Arc<OplogInserts>,
    remote_applied: Arc<RemoteAppliedBroadcast>,
    sync: Mutex<Option<Arc<SyncEngine>>>,
    recovery_retry_paths: Mutex<HashSet<String>>,
    pooled_staging_paths: Mutex<HashSet<String>>,
}

impl fmt::Debug for Persistence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Persistence")
            .field("root", &self.assets.root())
            .field("owns_library", &self.owns_library)
            .finish_non_exhaustive()
    }
}

impl Persistence {
    /// How long a repeatedly-failing staged recording is protected from the
    /// boot sweep before it is released to it (trash retention then still
    /// applies on top).
    pub const RECOVERY_RETRY_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    /// How many boots must have failed to recover a staged recording before
    /// [`Persistence::RECOVERY_RETRY_MAX_AGE`] can release it.
    pub const RECOVERY_RETRY_MIN_ATTEMPTS: i64 = 3;

    /// The dependency carry-forward [`Persistence::compact_oplog`] runs
    /// before it collapses history. Its correlated subqueries run once per
    /// surviving entry, so they must resolve through ix_oplog_entity (a test
    /// pins the plan). Only an entry that actually inherits something is
    /// written (see [`OplogDrain::ACK_CARRY_SQL`]: an UPDATE rewrites every
    /// row it matches, and this one used to match the whole surviving ledger
    /// on every boot).
    ///
    /// Like the ack's carry, it looks back no further than the row's latest
    /// tombstone (see [`OplogDrain::ACK_CARRY_SQL`]: the drain's collapse
    /// inherits nothing through a delete, and the Dart app's compaction,
    /// which did, could hold a revived row on its deleted predecessor's
    /// asset forever). `verification/VocaraVerify/Oplog.lean` proves the
    /// compaction keeps what a read resolves each row to.
    pub const COMPACT_CARRY_SQL: &'static str = "UPDATE oplog SET depends_on_asset_id = (\
         SELECT o.depends_on_asset_id FROM oplog o \
         WHERE o.entity_table = oplog.entity_table \
         AND o.entity_id = oplog.entity_id \
         AND o.depends_on_asset_id IS NOT NULL \
         AND o.seq >= (SELECT COALESCE(MAX(t.seq), 0) FROM oplog t \
         WHERE t.entity_table = oplog.entity_table \
         AND t.entity_id = oplog.entity_id \
         AND t.op = 'tombstone') \
         ORDER BY o.seq DESC LIMIT 1\
         ) WHERE depends_on_asset_id IS NULL AND op <> 'tombstone' \
         AND seq IN (\
         SELECT MAX(seq) FROM oplog GROUP BY entity_table, entity_id) \
         AND EXISTS (SELECT 1 FROM oplog o \
         WHERE o.entity_table = oplog.entity_table \
         AND o.entity_id = oplog.entity_id \
         AND o.depends_on_asset_id IS NOT NULL \
         AND o.seq >= (SELECT COALESCE(MAX(t.seq), 0) FROM oplog t \
         WHERE t.entity_table = oplog.entity_table \
         AND t.entity_id = oplog.entity_id \
         AND t.op = 'tombstone'))";

    // --- Accessors ----------------------------------------------------------

    /// The library database.
    pub fn db(&self) -> &Arc<AppDatabase> {
        &self.db
    }

    /// The write chokepoint (stamps and the oplog).
    pub fn writer(&self) -> &Arc<SyncWriter> {
        &self.writer
    }

    /// The content-addressed asset pool (rooted at the library directory).
    pub fn assets(&self) -> &Arc<AssetStore> {
        &self.assets
    }

    /// The song practice repository.
    pub fn song_repo(&self) -> &Arc<SongPracticeRepositoryDrift> {
        &self.song_repo
    }

    /// The technique repository.
    pub fn tech_repo(&self) -> &Arc<TechniqueRepositoryDrift> {
        &self.tech_repo
    }

    /// Whether this instance booted as the library's owner: it had the
    /// single-instance lock ([`LibraryLock`]) exclusively, so no other
    /// Vocara process had the library open. Only then did
    /// [`Persistence::open`] run boot recovery and the boot sweeps: in a
    /// library another process has open, its in-flight recordings and
    /// imports look exactly like the leftovers those passes reclaim.
    pub fn owns_library(&self) -> bool {
        self.owns_library
    }

    /// The app-wide store write gate: saves/imports run under its shared
    /// side, [`Persistence::repair_library`] and the boot sweeps take it
    /// exclusively — see [`WriteGate`].
    pub fn write_gate(&self) -> &Arc<WriteGate> {
        self.assets.gate()
    }

    /// The notifier of committed oplog inserts on this library's connection:
    /// what [`SyncEngine::start`] subscribes the debounced push to.
    pub fn oplog_inserts(&self) -> &Arc<OplogInserts> {
        &self.oplog_inserts
    }

    /// "Remote changes were applied by sync" notifications for the UI layer:
    /// one event per committed pull page that changed rows, sent strictly
    /// AFTER that page's transaction (and the follow-up integrity repair)
    /// committed — never from inside an open transaction. View models listen
    /// to refresh what they display; skipping a reload is always safe
    /// because three-way saves merge against base hashes (freshness, not
    /// safety). Each subscriber gets the events sent after it subscribed.
    pub fn subscribe_remote_applied(&self) -> mpsc::Receiver<RemoteApplied> {
        self.remote_applied.subscribe()
    }

    /// The sync engine, once [`Persistence::attach_sync`] wired one (None
    /// until then — P0 has no production transport; tests and the future
    /// server transport attach).
    pub fn sync(&self) -> Option<Arc<SyncEngine>> {
        lock(&self.sync).clone()
    }

    /// Staging files whose recovery attempt FAILED this boot — exempted
    /// from the boot staging sweep so the next boot's recovery retries them.
    /// Repopulated by every [`Persistence::recover_staged_recordings`] run.
    pub fn recovery_retry_paths(&self) -> HashSet<String> {
        lock(&self.recovery_retry_paths).clone()
    }

    /// Staging files whose bytes the pool provably holds: a saved row
    /// references their hash and a pool file of that hash is on disk. The
    /// boot sweep deletes these instead of trashing them. A graceful stop
    /// leaves every take of the session in staging after its save pooled
    /// it, so the trash otherwise kept a second copy of each recording for
    /// 30 days. Repopulated by every
    /// [`Persistence::recover_staged_recordings`] run.
    pub fn pooled_staging_paths(&self) -> HashSet<String> {
        lock(&self.pooled_staging_paths).clone()
    }

    // --- Sync -----------------------------------------------------------------

    /// Builds and wires the [`SyncEngine`] over this store: the applier (with
    /// a persistent quarantine), the oplog drain, the row builder, the
    /// sync-state and integrity collaborators, and the remote-applied
    /// broadcast. The engine starts PASSIVE — callers pump it explicitly;
    /// production calls [`SyncEngine::start`] with
    /// [`Persistence::oplog_inserts`] to also subscribe the debounced push to
    /// committed oplog writes. Attach at most once per Persistence (a second
    /// call fails with [`Error::State`]).
    pub fn attach_sync(
        &self,
        transport: Arc<dyn SyncTransport>,
        options: AttachSyncOptions,
    ) -> Result<Arc<SyncEngine>, Error> {
        let mut slot = lock(&self.sync);
        if slot.is_some() {
            return Err(Error::State(
                "a SyncEngine is already attached to this store".to_string(),
            ));
        }
        // One builder shared by the engine (push projection) and the applier
        // (the FK sweep's park-instead-of-lose snapshot) — same schema, same
        // memoized table ranks.
        let builder = Arc::new(SyncRowBuilder::new(self.db.clone()));
        let broadcast = self.remote_applied.clone();
        let engine = Arc::new(SyncEngine::new(
            SyncEngineParts {
                db: self.db.clone(),
                applier: Arc::new(SyncApplier::new(
                    self.db.clone(),
                    self.writer.clock().clone(),
                    builder.clone(),
                    Some(QuarantineStore::new(self.db.clone())),
                )),
                drain: OplogDrain::new(self.db.clone()),
                builder,
                state: SyncStateStore::new(self.db.clone()),
                integrity: Arc::new(IntegrityChecker::new(self.db.clone(), self.writer.clone())),
                transport,
            },
            SyncEngineOptions {
                on_remote_applied: Some(Arc::new(move |event: &RemoteApplied| {
                    broadcast.add(event)
                })),
                timers: options.timers,
                debug_before_page_commit: options.debug_before_page_commit,
                ..SyncEngineOptions::default()
            },
        ));
        *slot = Some(engine.clone());
        Ok(engine)
    }

    // --- Open -----------------------------------------------------------------

    /// Opens the real on-disk store in [`OpenOptions::dir`]. First recovers
    /// any half-finished schema migration (a crash mid-swap) so the live
    /// file is always consistent before the store opens it. See the module
    /// doc for the whole boot order.
    ///
    /// Fails with [`OpenError::NewerLibrary`] when the library was written
    /// by a newer Vocara, and [`OpenError::LibraryInUse`] when it needs an
    /// upgrade while another Vocara process holds it; any other failure is
    /// a fatal open. A library another process holds otherwise opens
    /// normally, minus boot recovery and the boot sweeps (see
    /// [`Persistence::owns_library`]).
    pub fn open(options: OpenOptions<'_>) -> Result<Persistence, OpenError> {
        install_schema_version_reader();
        let OpenOptions {
            dir,
            analyze,
            library_lock,
            on_waiting_for_library,
        } = options;
        // The single-instance guard comes first: a half-finished migration's
        // marker may belong to another process that is migrating right now.
        // This instance holds the lock from here on (see [`LibraryLock`]); it
        // boots as the owner only if it took the lock exclusive, which no
        // process can while any other has the library open, and hands it
        // back to shared when the boot is done.
        let guard: Arc<dyn LibraryGuard> = match library_lock {
            Some(make) => make(&dir),
            None => LibraryLock::for_library_with(
                &dir,
                LibraryLock::DEFAULT_WAIT,
                LibraryLock::DEFAULT_POLL,
                on_waiting_for_library,
            ),
        };
        let owns_library = guard.try_exclusive();
        /// Hands the boot's exclusive hold back however the boot ends.
        struct Release(Arc<dyn LibraryGuard>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.release_exclusive();
            }
        }
        let _release = owns_library.then(|| Release(guard.clone()));
        Self::open_owned(&dir, analyze, guard, owns_library)
    }

    fn open_owned(
        dir: &Path,
        analyze: Option<WavAnalyzer>,
        guard: Arc<dyn LibraryGuard>,
        owns_library: bool,
    ) -> Result<Persistence, OpenError> {
        let db_path = dir.join(DB_FILE_NAME);
        let migrator = SafeMigrator::new();
        if owns_library {
            migrator.recover_if_needed(&db_path)?;
        } else {
            // A migration another process left unfinished (it died mid-swap)
            // is the owner's to recover; opened as it stands, a missing live
            // file became an empty library.
            if migrator.migration_pending(&db_path)? {
                return Err(OpenError::LibraryInUse);
            }
            log(
                "another Vocara process holds this library; skipping migration recovery, boot \
                 recovery and every boot sweep",
            );
        }
        // Refuse a library from a NEWER Vocara BEFORE building the store:
        // this build cannot understand (let alone migrate) a higher schema,
        // so nothing may touch it — not the store, not the sweeps. A
        // downgrade path does not exist by design; the on-disk file is left
        // byte-for-byte untouched.
        if db_path.is_file() {
            let on_disk = migrator.read_user_version(&db_path)?;
            if on_disk > CURRENT_SCHEMA_VERSION {
                return Err(OpenError::NewerLibrary {
                    library_version: on_disk,
                    app_version: CURRENT_SCHEMA_VERSION,
                });
            }
        }
        // Upgrade a stale on-disk schema BEFORE the store opens the file: the
        // store refuses an in-place upgrade (the copy/validate/swap protocol
        // owns upgrades). A fresh install (no file) is a no-op here — the
        // store's open builds the current schema directly.
        if owns_library {
            migrator.upgrade_if_needed(&db_path, CURRENT_SCHEMA_VERSION, None)?;
        } else if db_path.is_file() {
            let on_disk = migrator.read_user_version(&db_path)?;
            if on_disk != 0 && on_disk < CURRENT_SCHEMA_VERSION {
                return Err(OpenError::LibraryInUse);
            }
        }
        let mut conn = Connection::open(&db_path).map_err(Error::from)?;
        wait_out_other_writers(&mut conn).map_err(Error::from)?;
        // The notifier must be installed before the connection becomes the
        // store (SQLite's hooks need it exclusively); SyncEngine::start
        // subscribes to it.
        let inserts = OplogInserts::install(&mut conn)?;
        // As drift opened it: a failing open protocol (a table a derived
        // index needs is gone, an index the rows refuse) does not fail the
        // open, it poisons the store — every query fails, the read probe
        // below with it, so the boot goes on degraded with every destructive
        // sweep skipped.
        let (db, open_failure) = AppDatabase::from_connection_or_poisoned(conn)?;
        if let Some(e) = open_failure {
            log(&format!(
                "the library's open protocol failed ({}); every query will fail",
                store_error_kind(&e)
            ));
        }
        let db = Arc::new(db);
        let node = read_or_create_node(dir)?;
        // Restore the HLC clock's monotonicity across restarts BEFORE wiring
        // the writer (the startup sweeps below tick it), so a new local edit
        // can never stamp LOWER than one this install already pushed. See
        // seed_clock.
        let clock = seed_clock(&db, &node);
        let persistence = Self::wire(
            db.clone(),
            dir.to_path_buf(),
            &node,
            Some(clock),
            owns_library,
            guard,
            inserts,
        );
        // Recover hard-kill orphaned take recordings BEFORE the staging sweep
        // deletes them: a force quit / crash / kill -9 mid-record skips the
        // graceful stop + save, leaving the streamed WAV in staging as the
        // ONLY copy of the take (the engine keeps its header valid by
        // patching it in place ~1x/s while recording). Best-effort; never
        // blocks launch.
        //
        // GATE for every destructive sweep below: the sweeps derive their
        // keep-sets from the database, and sweep_staging clears the staging
        // tree outright — so they may only run once the database has
        // answered real reads AND recovery completed. If the probe or
        // recovery failed (an unreadable table, a failed staging scan), a
        // staged WAV may still be the ONLY copy of a take, and sweeping it
        // would be unrecoverable data loss. Skipping every sweep on a sick
        // boot merely leaks disk until a healthy boot; the sweeps themselves
        // remain best-effort and never block launch.
        //
        // And only in the process that holds the library (owns_library): in
        // another one's library, a staged WAV with no row is its take still
        // recording, not a crashed one.
        let mut sweep_gate_open = false;
        if owns_library {
            let analyze = analyze.unwrap_or_else(default_analyzer);
            match probe_db_readable(&db)
                .and_then(|()| persistence.recover_staged_recordings(&analyze))
            {
                Ok(_) => sweep_gate_open = true,
                Err(e) => log(&format!(
                    "boot health probe / staged-recording recovery failed ({}); skipping all \
                     destructive sweeps this boot",
                    store_error_kind(&e)
                )),
            }
        }
        // Waveform intermediates an analysis left when the app quit or was
        // killed mid-way. Derived scratch, never a copy of anything, so it
        // needs no gate; but only now, once recovery's own analyses (which
        // write there too) are done, and before any screen can start one.
        // And only as the owner: in a library another instance has open, a
        // .wave there is its analysis in flight (a take's stop-and-save among
        // them).
        if owns_library {
            waveform_service::sweep_scratch(dir);
        }
        if sweep_gate_open {
            // A migration's rollback copy has served its purpose once the
            // migrated database has answered real reads and recovery ran on
            // it. Kept on any boot that got less far.
            if let Err(e) = migrator.discard_backup(&db_path) {
                log(&format!(
                    "pre-migration backup not discarded ({})",
                    store_error_kind(&e)
                ));
            }
            // The whole boot maintenance pass holds the write gate
            // exclusively. Nothing user-facing is running yet, so this is
            // uncontended today; it exists so the sweeps and a save can
            // structurally never interleave (the same discipline
            // repair_library follows on a live store).
            persistence
                .write_gate()
                .exclusive(|| persistence.boot_maintenance());
        }
        Ok(persistence)
    }

    /// The boot maintenance pass [`Persistence::open`] runs under the write
    /// gate once the sweep gate is open. Each step is best-effort: a failure
    /// is logged and never blocks launch.
    fn boot_maintenance(&self) {
        // Reclaim staging leftovers: after a save, a recording/import has
        // been copied into the content-addressed pool and its working file
        // under audio/ or technique_media/ is an orphan (nothing persistent
        // references it — clips and media reference pool assets). At startup
        // nothing is in flight, so the staging tree is safe to clear. Files
        // whose recovery ATTEMPT failed are kept for a retry next boot (see
        // recover_staged_recordings) instead of being funneled to trash by
        // one environment-wide analyzer failure.
        self.assets
            .sweep_staging(&self.recovery_retry_paths(), &self.pooled_staging_paths());
        // Reclaim pooled bytes for assets no live clip/media/practice
        // references any more (removed takes, replaced accompaniment, deleted
        // examples). Derives the refcount from a query, so it's safe to run
        // at startup when nothing is in flight.
        if let Err(e) = self
            .assets
            .sweep_unreferenced(AssetStore::DEFAULT_SWEEP_MIN_AGE)
        {
            log(&format!(
                "sweepUnreferenced failed ({})",
                store_error_kind(&e)
            ));
        }
        // Reclaim pool files left with no asset row at all (a crash between
        // the pool write and the asset-row commit).
        if let Err(e) = self.assets.sweep_orphan_pool_files() {
            log(&format!(
                "sweepOrphanPoolFiles failed ({})",
                store_error_kind(&e)
            ));
        }
        // The sweeps above park reclaimed files in the asset trash rather
        // than unlinking them; age trash out ONLY after a fully healthy boot,
        // so a sick boot can never shorten the recovery window.
        if let Err(e) = self.assets.prune_trash(AssetStore::DEFAULT_TRASH_RETENTION) {
            log(&format!("pruneTrash failed ({})", store_error_kind(&e)));
        }
        // Compact the durability ledger: production attaches no sync
        // transport today, so nothing ever acks (deletes) oplog rows — every
        // save appends and the ledger only grows. Safe under the drain
        // contract (see compact_oplog).
        if let Err(e) = self.compact_oplog() {
            log(&format!("compactOplog failed ({})", store_error_kind(&e)));
        }
    }

    /// A synchronous in-memory instance (in-memory SQLite) for tests and the
    /// app's own tests: its asset pool lives in a fresh temp directory the
    /// caller removes (`vocara_mem…`). No recovery, no sweeps, an unseeded
    /// clock, and a guard that always owns the library.
    pub fn memory() -> Result<Persistence, Error> {
        let dir = std::env::temp_dir().join(format!("vocara_mem{}", new_id()));
        fs::create_dir_all(&dir)?;
        let mut conn = Connection::open_in_memory()?;
        let inserts = OplogInserts::install(&mut conn)?;
        let db = Arc::new(AppDatabase::from_connection(conn)?);
        Ok(Self::wire(
            db,
            dir,
            &new_id(),
            None,
            true,
            Arc::new(FixedGuard::new(true)),
            inserts,
        ))
    }

    fn wire(
        db: Arc<AppDatabase>,
        root: PathBuf,
        node: &str,
        clock: Option<HlcClock>,
        owns_library: bool,
        guard: Arc<dyn LibraryGuard>,
        oplog_inserts: Arc<OplogInserts>,
    ) -> Persistence {
        let clock = Arc::new(clock.unwrap_or_else(|| HlcClock::new(node)));
        let writer = Arc::new(SyncWriter::new(db.clone(), clock));
        let assets = Arc::new(AssetStore::new(db.clone(), root, writer.clone()));
        Persistence {
            song_repo: Arc::new(SongPracticeRepositoryDrift::new(
                db.clone(),
                writer.clone(),
                assets.clone(),
            )),
            tech_repo: Arc::new(TechniqueRepositoryDrift::new(
                db.clone(),
                writer.clone(),
                assets.clone(),
            )),
            db,
            writer,
            assets,
            owns_library,
            guard,
            oplog_inserts,
            remote_applied: Arc::default(),
            sync: Mutex::new(None),
            recovery_retry_paths: Mutex::new(HashSet::new()),
            pooled_staging_paths: Mutex::new(HashSet::new()),
        }
    }

    /// Closes the library: disposes the sync engine (if one was attached),
    /// drops this layer's handles, and closes the database connection,
    /// reporting a close failure (Dart's `db.close()`). The connection is
    /// only closed here when nothing else still holds the database (an
    /// engine or repository handle kept elsewhere); otherwise this fails
    /// with [`Error::State`] and the connection closes when the last holder
    /// drops it.
    pub fn close(self) -> Result<(), Error> {
        let Persistence {
            db,
            writer,
            assets,
            song_repo,
            tech_repo,
            sync,
            oplog_inserts,
            ..
        } = self;
        if let Some(engine) = sync.into_inner().unwrap_or_else(PoisonError::into_inner) {
            engine.dispose();
        }
        drop((writer, assets, song_repo, tech_repo, oplog_inserts));
        match Arc::try_unwrap(db) {
            Ok(db) => db.close(),
            Err(_) => Err(Error::State(
                "the library database is still held elsewhere; it closes when the last holder \
                 drops it"
                    .to_string(),
            )),
        }
    }

    // --- Repair ---------------------------------------------------------------

    /// One user-invoked "repair library" pass: runs the cross-row integrity
    /// repair to a fixpoint, then reclaims unreferenced assets and orphaned
    /// pool files. Repairs journal through the normal write chokepoint like
    /// any edit. Safe on a live store because the whole pass holds the write
    /// gate exclusively: an in-flight save (import committed, clip/media rows
    /// not yet) finishes first, and no new save starts mid-pass — so the GC
    /// can never tombstone an asset a save just imported, or trash its
    /// in-flight pool file. Meant for a diagnostics/settings action, never
    /// the boot path (boot has its own gated sweeps in
    /// [`Persistence::open`]).
    ///
    /// With sync attached, the pass also runs in the engine's serial
    /// section: between two pages of a pull run the store can hold a
    /// half-delivered aggregate (a live child whose parent's revive is on the
    /// next page), and a repair judged against it would journal derived
    /// intent that reaches every device. That section is taken FIRST and the
    /// gate inside it: held the other way round, the gate's exclusive side
    /// stayed held for as long as the pull run took to finish, and every
    /// save, import and take stalled behind it. Neither order deadlocks —
    /// the engine never takes the write gate, and nothing holding the gate
    /// waits on a pump.
    ///
    /// The pool's content check ([`AssetStore::verify_pool_integrity`]) runs
    /// after both, outside them: it only reads, and inside the gate a
    /// re-hash of the whole pool held every save — the one that arms REC
    /// included — for as long as it took, minutes for a large library.
    pub fn repair_library(&self) -> Result<RepairReport, Error> {
        let repaired = match self.sync() {
            None => self.repair_pass()?,
            Some(sync) => sync.run_exclusive(|| self.repair_pass())?,
        };
        // After the sweeps, so files on their way out are not re-hashed; one
        // a later sweep takes mid-pass reads as unreadable, not corrupt.
        // Reports only — see AssetStore::verify_pool_integrity for why
        // nothing is touched.
        let corrupt = self.assets.verify_pool_integrity()?;
        for path in &corrupt {
            log(&format!(
                "pooled file does not match its content address: {path}"
            ));
        }
        let report = RepairReport {
            corrupt_pool_files: corrupt.len(),
            ..repaired
        };
        log(&format!("repairLibrary: {report}"));
        Ok(report)
    }

    fn repair_pass(&self) -> Result<RepairReport, Error> {
        self.write_gate().exclusive(|| {
            let checker = IntegrityChecker::new(self.db.clone(), self.writer.clone());
            let found = checker.check()?;
            // One transaction around the whole repair fixpoint, exactly like
            // the sync engine's end-of-pull repair: each individual repair is
            // a row UPDATE plus its oplog entry, and the invariant "a row
            // mutation and its journal entry commit in the SAME transaction"
            // must hold here too — two autocommit statements would let a
            // crash between them tombstone/heal a synced row with no journal
            // entry, so the change would never propagate.
            let unrepaired = self.db.transaction(|| checker.repair())?;
            // A cascade can repair MORE than the initial scan saw; the
            // report never claims a negative remainder.
            let repaired = found.len().saturating_sub(unrepaired.len());
            // Reclaiming needs the library to this process alone, asked NOW,
            // not at boot: another instance's in-flight import is an
            // unreferenced asset, a pool file with no row or a `.part` in the
            // pool's staging until its save commits. Without exclusive,
            // reclaim nothing.
            let reclaim = self.guard.try_exclusive();
            let swept = if reclaim {
                let swept = self
                    .assets
                    .sweep_unreferenced(AssetStore::DEFAULT_SWEEP_MIN_AGE)
                    .and_then(|tombstoned| Ok(tombstoned + self.assets.sweep_orphan_pool_files()?));
                self.guard.release_exclusive();
                swept?
            } else {
                0
            };
            Ok(RepairReport {
                violations_found: found.len(),
                violations_repaired: repaired,
                assets_swept: swept,
                corrupt_pool_files: 0,
            })
        })
    }

    /// Compacts the oplog: for every (entity_table, entity_id), keeps only
    /// the LATEST journaled op and deletes the superseded history. Returns
    /// the number of rows deleted.
    ///
    /// This preserves the drain contract exactly: the ledger's only
    /// consumer, [`OplogDrain::read_pending`], already collapses each
    /// (table, id) to its latest op before pushing (the pusher uploads
    /// CURRENT row state, so intermediate ops are redundant by design — a
    /// trailing tombstone is the latest op and survives). Two details are
    /// handled deliberately:
    ///
    ///  * The asset gate: the drain's collapse keeps the latest NON-null
    ///    depends_on_asset_id across a row's entries (a content-only update
    ///    must not shed its insert's dependency), so before deleting we
    ///    carry that value onto the surviving row in the same transaction.
    ///    [`OplogDrain::ack`] carries it the same way for the same reason —
    ///    any deletion of oplog history owes the survivors that inheritance.
    ///    Not to a TOMBSTONE though: a delete references no bytes, and
    ///    gating one on a since-collected asset holds it — and, through the
    ///    ack watermark, the whole ledger behind it — forever. The subquery
    ///    needs no upper seq bound here because the target is already each
    ///    entity's MAX(seq), so nothing can follow it (unlike the ack, which
    ///    repairs rows that still have newer entries above them).
    ///  * synced = 1 rows cannot exist: nothing ever sets the flag — the ack
    ///    path hard-DELETES instead — so the SQL needs no synced filter;
    ///    keep-latest-per-entity alone is exact. If a future change ever
    ///    starts marking rows synced, revisit this predicate.
    ///
    /// Run from the gated boot maintenance pass (under the write gate
    /// exclusive, nothing in flight); one transaction so a crash can never
    /// leave a dependency carried but its history half-deleted, or vice
    /// versa.
    pub fn compact_oplog(&self) -> Result<usize, Error> {
        self.db.transaction(|| {
            self.db.execute(Self::COMPACT_CARRY_SQL, [])?;
            self.db.execute(
                "DELETE FROM oplog WHERE seq NOT IN (\
                 SELECT MAX(seq) FROM oplog GROUP BY entity_table, entity_id)",
                [],
            )
        })
    }

    // --- Boot recovery --------------------------------------------------------

    /// Boot-time recovery of hard-kill orphaned take recordings. Returns the
    /// number of takes recovered.
    ///
    /// Recordings stream to `audio/<practiceId>/<id>.wav` (the parent
    /// directory name IS the practice id — see
    /// `AudioFileService::new_recording_path`) and are copied into the
    /// content-addressed pool by the save at the end of a graceful stop, so
    /// after a save the staging file is a reclaimable leftover. A TRUE hard
    /// kill (force quit / crash / kill -9) skips stop + save entirely: the
    /// staging file is then the ONLY copy of the take. The engine's
    /// recording writer patches the WAV header in place about once per
    /// second, so that file is a valid WAV covering the take up to the last
    /// patch — re-attach it to its practice as a new vocal take, mirroring
    /// the normal stop-recording finalize (waveform analysis, clip creation,
    /// save).
    ///
    /// Must run BEFORE [`AssetStore::sweep_staging`]: whatever this pass
    /// does not recover (already-saved leftovers, empty/unparseable files,
    /// non-recording audio, files of deleted practices) is deleted by the
    /// sweep exactly as before.
    ///
    /// Crash-safety / idempotency (this recovery can itself be hard-killed):
    /// the per-file order is (1) pool import + take/clip row commit — both
    /// inside the repository's save, rows in one transaction — then (2)
    /// delete the staging file. A file is only recovered when NO clip /
    /// technique-media / separation reference — live OR tombstoned — carries
    /// its content hash (bytes that match anything EVER saved are either a
    /// normal already-pooled leftover or a deliberately deleted take; see
    /// `referenced_content_hashes`), so:
    ///  * killed before the rows commit → nothing references the hash yet;
    ///    the next boot retries the whole recovery (the pool import dedups by
    ///    hash, so a half-done import is reused, never duplicated);
    ///  * killed between the rows commit and the file delete → the new clip
    ///    row references the hash; the next boot SKIPS the file and the sweep
    ///    deletes it — the take can never be attached twice.
    ///
    /// Fails (closing [`Persistence::open`]'s sweep gate) only when the pass
    /// as a whole cannot run: the referenced hashes cannot be read, or the
    /// staging tree cannot be listed. A single file's failure holds that
    /// file back instead.
    pub fn recover_staged_recordings(&self, analyze: &WavAnalyzer) -> Result<usize, Error> {
        lock(&self.recovery_retry_paths).clear();
        lock(&self.pooled_staging_paths).clear();
        let staging = self.assets.root().join("audio");
        if !staging.is_dir() {
            return Ok(0);
        }

        // Content hashes referenced by any clip / technique-media /
        // separation row, live or tombstoned — see referenced_content_hashes
        // for why the tombstoned ones count. A staging file whose bytes match
        // one was already saved into the pool (or deliberately deleted) and
        // must NOT become a second take. Note the set covers only rows a save
        // COMPLETED for: an asset row alone (a crash between the pool import
        // and the row commit) does not mark its file as saved, so that take
        // is still recovered on the retry.
        //
        // GROWS as this pass runs: each recovery adds its own hash, so two
        // staging files with identical bytes cannot both become takes in one
        // boot.
        let mut referenced = self.referenced_content_hashes()?;

        let mut candidates: Vec<(String, PathBuf)> = Vec::new();
        for dir in fs::read_dir(&staging)? {
            let dir = dir?.path();
            if !dir.is_dir() {
                continue;
            }
            let practice_id = dart_path::basename(&dir.to_string_lossy());
            // Files directly in the practice folder only: imports and edit
            // outputs stage in subdirectories precisely so this scan never
            // adopts them.
            for f in fs::read_dir(&dir)? {
                let f = f?.path();
                if !f.is_file()
                    || dart_path::extension(&f.to_string_lossy()).to_lowercase() != ".wav"
                {
                    continue;
                }
                candidates.push((practice_id.clone(), f));
            }
        }
        // Oldest first (names start with a ULID, which sorts by time): two
        // takes that crashed into the same empty lane left it to the
        // directory's order which filled it, and the later take could take
        // the earlier's place.
        candidates.sort_by(|(_, a), (_, b)| {
            let (a, b) = (file_name(a), file_name(b));
            a.encode_utf16().cmp(b.encode_utf16())
        });
        let files: Vec<PathBuf> = candidates.iter().map(|(_, f)| f.clone()).collect();

        let mut recovered = 0;
        // Every staged take is hashed before it can be judged, and a
        // graceful stop leaves each of the last session's takes here for the
        // sweep, so this boot pass re-reads all of them before the window
        // shows. Hash them ahead of the sequential walk, a few background
        // threads at a time; the walk itself — and so every outcome — is
        // unchanged.
        std::thread::scope(|scope| {
            let hashes = StagedHashes::start(scope, &files, 4);
            for (i, (practice_id, f)) in candidates.iter().enumerate() {
                match self.recover_orphan_take(
                    f,
                    practice_id,
                    &mut referenced,
                    analyze,
                    (&hashes, i),
                ) {
                    Ok(RecoveryOutcome::Recovered) => recovered += 1,
                    // Provably not a recoverable take: leave it to the sweep.
                    Ok(RecoveryOutcome::Junk) => {}
                    Ok(RecoveryOutcome::Failed) => {
                        self.hold_back_for_retry(f, "the store reported a failure")
                    }
                    Err(e) => {
                        self.hold_back_for_retry(f, &format!("the attempt threw ({})", e.kind()))
                    }
                }
            }
            // The walk is done: the workers take no new file (each finishes
            // the one it is reading), and the scope joins them.
            hashes.stop();
        });
        Ok(recovered)
    }

    /// Adds [f] to the pooled staging paths when a non-empty pool file of
    /// [hash] exists. Anything less (no pooled copy found, a failed lookup)
    /// leaves the file to the trash as before: never an error, never a
    /// hold-back.
    fn note_pooled(&self, f: &Path, hash: &str) {
        // A failed lookup is unproven: the trash keeps it.
        let Ok(rels) = self.db.query(
            "SELECT local_rel_path AS rel FROM assets \
             WHERE content_hash = ? AND local_rel_path IS NOT NULL",
            [hash],
            |r| r.get::<_, String>(0),
        ) else {
            return;
        };
        for rel in rels {
            let pooled = self.assets.root().join(rel);
            if matches!(fs::metadata(&pooled), Ok(m) if m.is_file() && m.len() > 0) {
                lock(&self.pooled_staging_paths).insert(f.to_string_lossy().into_owned());
                return;
            }
        }
    }

    /// Exempts [f] from this boot's staging sweep so the next boot's
    /// recovery tries again.
    ///
    /// A failed recovery never blocks the other orphans or the launch, but a
    /// failure is an environmental symptom (decode error, disk error, a
    /// store that could not commit), not proof the file is junk — one
    /// systemic breakage would otherwise funnel EVERY orphan to trash this
    /// boot, with staging empty next boot so recovery never retries.
    /// Bounded: a file that has kept failing for over 7 days since its first
    /// failed attempt, and on at least
    /// [`Persistence::RECOVERY_RETRY_MIN_ATTEMPTS`] boots, goes to the sweep
    /// after all (the trash then still holds it for 30 days).
    fn hold_back_for_retry(&self, f: &Path, why: &str) {
        if still_retrying(f) {
            lock(&self.recovery_retry_paths).insert(f.to_string_lossy().into_owned());
        }
        log(&format!(
            "one staged recording failed to recover ({why}); exempted from this boot's sweep if \
             it has not kept failing too long"
        ));
    }

    /// Recovers one staged WAV as a new vocal take of [practice_id].
    /// [prefetched] is the prefetch and this file's place in it.
    ///
    /// The two non-recovered outcomes are deliberately NOT the same answer:
    /// only [`RecoveryOutcome::Junk`] releases the file to the staging sweep.
    /// The repository reports a store failure as an `Err` that this pass
    /// reads as [`RecoveryOutcome::Failed`], so a disk-full or
    /// unreadable-database boot does not look like "this file is not a take"
    /// and trash the only copy of a recording — the very case the retry
    /// hold-back exists for.
    fn recover_orphan_take(
        &self,
        file: &Path,
        practice_id: &str,
        referenced: &mut HashSet<String>,
        analyze: &WavAnalyzer,
        prefetched: (&StagedHashes<'_>, usize),
    ) -> Result<RecoveryOutcome, AttemptError> {
        let path = file.to_string_lossy().into_owned();
        // Only what the recording engine writes is a candidate: a
        // canonical-header MONO WAV — 32-bit float today, 16-bit PCM from
        // pre-float versions — with a non-empty payload. Imports and edit
        // outputs can have that shape, so they stage one level down
        // (`imports/`, `edits/`), which the scan above never enters:
        // misreviving one as a "take" would be worse than sweeping it. An
        // empty payload (killed before the first ~1 s header patch) carries
        // no audio worth keeping.
        //
        // An UNREADABLE file is not junk. `probe_wav` answers None only for
        // shape (too short / wrong layout / not PCM / gone) and FAILS on any
        // other I/O failure, so a permission denial or a hardware fault
        // leaves here as an error into the caller's retry hold-back.
        // Answering "not a recording" on the strength of a read that never
        // happened would sweep the only copy of a crashed take into the
        // trash, where the 30-day prune eventually deletes it — and no later
        // boot rescans the trash.
        if !is_recorder_shaped(probe_wav(file)?) {
            return Ok(RecoveryOutcome::Junk);
        }

        // Bytes already referenced by a completed save → a normal leftover,
        // not an orphaned take (this is also what makes a re-run after a
        // mid-recovery crash a no-op instead of a duplicate take). Asked
        // first by the hash the save recorded when it pooled this file: a
        // power cut soon after that save can bring the never-flushed staging
        // file back holding older bytes, whose own hash matches nothing (see
        // AssetStore::imported_hash_of).
        if let Some(imported) = AssetStore::imported_hash_of(file) {
            if referenced.contains(&imported) {
                self.note_pooled(file, &imported);
                return Ok(RecoveryOutcome::Junk);
            }
        }
        let (hashes, i) = prefetched;
        let hash = match hashes.of(i) {
            Some(hash) => hash,
            None => AssetStore::sha256_of_file(file)?,
        };
        if referenced.contains(&hash) {
            self.note_pooled(file, &hash);
            return Ok(RecoveryOutcome::Junk);
        }

        // The practice must still exist and be live (tombstones filtered);
        // an underivable id lands here too and the file is swept. Asked as
        // its own query first BECAUSE the repository's get collapses "gone"
        // and "the read failed" into one error, and only the former is proof
        // the file is junk — this select fails on a database failure and
        // holds the file back.
        if !self.practice_is_live(practice_id)? {
            return Ok(RecoveryOutcome::Junk);
        }
        // Loaded with its base, and saved against it as an editor saves: the
        // base-less save took the store's own rows as the base, so every row
        // this build re-encodes differently than it was stored (a field
        // another build wrote) read as a local edit. The practice row and
        // every take's and bus's chain were rewritten (that field stripped)
        // and journaled, though only the recovered take was new.
        let Ok(loaded) = self.song_repo.load_practice(practice_id) else {
            return Ok(RecoveryOutcome::Failed);
        };
        let practice = &loaded.aggregate;

        // Where the take was recorded, when its file name says (see
        // recording_placement_of): the lane it recorded into and where its
        // audio starts. (A take whose start sits before the timeline's start
        // keeps its head here: a stop would have cut it, and sits at 0.)
        let placed = recording_placement_of(&path);
        let into = placed
            .as_ref()
            .and_then(|p| practice.vocal_tracks.iter().position(|t| t.id == p.take_id));
        let mut start_ms = placed.as_ref().map_or(0, |p| p.start_ms);
        if start_ms < 0 {
            // Audio from before the timeline's start (a take from the top,
            // whose stop never ran to cut it): cut here, as the stop would
            // have, or it sat late by it. The start the cut leaves is noted
            // first (see write_aligned_start_to), so no later boot cuts it
            // twice.
            if write_aligned_start_to(&path, 0).wait() {
                // Kept whole on failure: placed at 0, late by the uncut head.
                let _ = trim_wav_head_in_place(&path, -start_ms);
            }
            start_ms = 0;
        }
        let analysis = analyze(&path).map_err(AttemptError::Analyze)?;
        let clip = AudioClip {
            id: new_id(),
            audio_file_path: path.clone(),
            duration_ms: analysis.duration_ms,
            start_ms,
            waveform_peaks: analysis.peaks,
        };
        let vocal_tracks: Vec<Arc<AudioTrack>> = match into {
            Some(i) if practice.vocal_tracks[i].clips.is_empty() => {
                // The empty take REC made for it: filled, where it was
                // recorded. Added as a lane of its own, that one stayed
                // behind empty. Only the clip is new: renamed, the lane's row
                // was rewritten and journaled as a local edit (see below).
                practice
                    .vocal_tracks
                    .iter()
                    .enumerate()
                    .map(|(j, t)| {
                        if j == i {
                            Arc::new(AudioTrack {
                                clips: vec![clip.clone()],
                                ..(**t).clone()
                            })
                        } else {
                            t.clone()
                        }
                    })
                    .collect()
            }
            _ => {
                // Same take construction as a graceful stop with nothing
                // armed: a fresh single-clip vocal take, in the
                // interpretation it was recorded in (a take over another's
                // audio stays a lane of its own here), at the timeline
                // origin when its position is unknown.
                let into = into.map(|i| &practice.vocal_tracks[i]);
                // Numbered among the takes of the interpretation it joins,
                // not of the one on screen.
                let number = match into.and_then(|t| t.version_id.clone()) {
                    Some(version) => SongPractice {
                        active_version_id: Some(version),
                        ..practice.clone()
                    }
                    .next_take_number(),
                    None => practice.next_take_number(),
                };
                let version_id = into
                    .and_then(|t| t.version_id.clone())
                    .or_else(|| practice.active_version().map(|v| v.id.clone()));
                let mut tracks = practice.vocal_tracks.clone();
                tracks.push(Arc::new(AudioTrack {
                    clips: vec![clip],
                    version_id,
                    ..AudioTrack::new(
                        new_id(),
                        AudioTrackKind::Vocal,
                        format!("Take {number} (recovered)"),
                        DateTime::now(),
                    )
                }));
                tracks
            }
        };
        // Only the take is new: the practice row is left as stored even when
        // the take runs past the stored timeline (the editor grows it in
        // memory on open, and nothing else reads it). Growing it here made
        // the row a local edit again: rewritten with fields another build
        // wrote stripped, and journaled at boot, before sync pulls, as a
        // whole-row write that would override what another device changed in
        // it meanwhile.
        let saved = self.song_repo.save_practice_with_base(
            &SongPractice {
                vocal_tracks,
                ..practice.clone()
            },
            &loaded.base_hashes,
        );
        if saved.is_err() {
            return Ok(RecoveryOutcome::Failed);
        }

        // These bytes are now referenced by a committed clip row, so the SAME
        // BOOT must treat a second file carrying them as an already-saved
        // leftover — the set was read once before the scan and would
        // otherwise still say "never saved". Two byte-identical staging WAVs
        // (two silent takes of equal length, or the same file left in two
        // practice folders) each became their own "(recovered)" take. Only
        // the next boot's re-read closed it.
        referenced.insert(hash);

        // The bytes are pooled and the rows are committed: the staging copy
        // is now an ordinary leftover. Delete it here; if the delete fails
        // the sweep (this boot) or the referenced-hash skip (next boot)
        // covers it.
        if fs::remove_file(file).is_err() {
            log(
                "recovered take staging copy not deleted (FileSystemException); the sweep covers \
                 it",
            );
        }
        Ok(RecoveryOutcome::Recovered)
    }

    /// Whether a LIVE (non-tombstoned) practice row with [id] exists. Fails
    /// on a database failure — the caller depends on that to tell "gone"
    /// apart from "could not be read".
    fn practice_is_live(&self, id: &str) -> Result<bool, Error> {
        let row = self.db.find::<PracticeRow>(id)?;
        Ok(matches!(row, Some(r) if r.deleted_at.is_none()))
    }

    /// Content hashes of every asset ANY referencing row — live or
    /// tombstoned — ever pointed at: bytes a save COMPLETED for at some
    /// point. Derived from [`AssetStore::ASSET_REF_COLUMNS`], the single
    /// source of truth for asset-referencing columns (guarded by a
    /// schema-derived test), so a future referencing column cannot be
    /// silently missed here while the GC honors it.
    ///
    /// Tombstoned references (and tombstoned assets) count on purpose: a
    /// graceful stop deliberately leaves the streamed staging WAV for the
    /// boot sweep, and delete_audio refuses pool paths — so when the user
    /// deletes that take in the same session, the staging copy survives
    /// while the clip row (and possibly the asset row) is only tombstoned.
    /// If this set covered live rows only, the next boot would adopt those
    /// bytes as a "(recovered)" take and resurrect the deleted recording.
    /// Bytes matching anything EVER saved are never adopted; they're left to
    /// the sweep. The crash-retry window is unaffected: a crash between the
    /// pool import and the clip-row commit leaves NO referencing row at all
    /// (live or tombstoned), so that take is still recovered.
    fn referenced_content_hashes(&self) -> Result<HashSet<String>, Error> {
        let union = AssetStore::ASSET_REF_COLUMNS
            .iter()
            .map(|r| {
                format!(
                    "SELECT {c} FROM {t} WHERE {c} IS NOT NULL",
                    c = r.column,
                    t = r.table
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ");
        let hashes = self.db.query(
            &format!("SELECT DISTINCT content_hash AS h FROM assets WHERE id IN ({union})"),
            [],
            |r| r.get::<_, rusqlite::types::Value>(0),
        )?;
        Ok(hashes
            .into_iter()
            .filter_map(|h| match h {
                rusqlite::types::Value::Text(h) => Some(h),
                _ => None,
            })
            .collect())
    }
}

/// [path]'s last component, as text.
fn file_name(path: &Path) -> String {
    dart_path::basename(&path.to_string_lossy())
}

/// The shape the recording engine writes (see
/// `Persistence::recover_orphan_take`).
fn is_recorder_shaped(probe: Option<WavProbe>) -> bool {
    matches!(probe, Some(p) if p.channels == 1
        && (p.bits_per_sample == 16 || p.bits_per_sample == 32)
        && p.data_bytes > 0)
}

/// Where a held-back staged recording keeps the time of its first failed
/// recovery (milliseconds since the epoch) and how many boots have failed it
/// since. The sweep keeps it with the file and takes it with the file.
fn first_failure_stamp(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.retry", path.to_string_lossy()))
}

/// Dart's `DateTime` range: ±8.64e15 ms around the epoch (a stamp beyond it
/// does not parse as a time).
const MAX_DART_MILLIS: i64 = 8_640_000_000_000_000;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Whether [f] is still held back for another attempt: it first failed to
/// recover less than [`Persistence::RECOVERY_RETRY_MAX_AGE`] ago, or has
/// failed on fewer than [`Persistence::RECOVERY_RETRY_MIN_ATTEMPTS`] boots,
/// counting this one. Measured from the file's modification time, a crash
/// left untouched for a week (the app not opened since) was released on its
/// first failed attempt; measured by time alone, on its second after another
/// week away.
///
/// A stamp that cannot be read or lies in the future starts over, and
/// anything that fails keeps the file: lingering is always safer than
/// trashing.
fn still_retrying(f: &Path) -> bool {
    let stamp = first_failure_stamp(f);
    let now = now_millis();
    let mut first: Option<i64> = None;
    let mut attempts = 0i64;
    // None yet (this is the first failure), or unreadable: start over.
    if let Ok(text) = fs::read_to_string(&stamp) {
        let parts: Vec<&str> = dart_string::trim(&text).split(' ').collect();
        if let Some(ms) = dart_int_try_parse(parts[0]) {
            if (-MAX_DART_MILLIS..=MAX_DART_MILLIS).contains(&ms) {
                first = Some(ms);
                attempts = if parts.len() > 1 {
                    dart_int_try_parse(parts[1]).unwrap_or(1)
                } else {
                    1
                };
            }
        }
    }
    let first = match first {
        Some(first) if first <= now => first,
        _ => {
            attempts = 0;
            now
        }
    };
    // Dart's int wraps; a wrapped count keeps the file, which is the safe
    // side.
    attempts = attempts.wrapping_add(1);
    let written = (|| -> io::Result<()> {
        let mut out = File::create(&stamp)?;
        out.write_all(format!("{first} {attempts}").as_bytes())?;
        out.sync_all()
    })();
    if written.is_err() {
        return true;
    }
    now.saturating_sub(first) < Persistence::RECOVERY_RETRY_MAX_AGE.as_millis() as i64
        || attempts < Persistence::RECOVERY_RETRY_MIN_ATTEMPTS
}

/// One real read against every table the boot sweeps derive their keep-sets
/// from. A miss (missing or corrupt table) fails, which closes the sweep
/// gate in [`Persistence::open`]: when reference derivation can't be
/// trusted, no destructive sweep may run.
fn probe_db_readable(db: &AppDatabase) -> Result<(), Error> {
    for table in ["practices", "tracks", "clips", "technique_media", "assets"] {
        db.query_row(&format!("SELECT COUNT(*) AS n FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })?;
    }
    Ok(())
}

/// Every table carrying an `hlc_packed` column (all synced tables + the
/// oplog), in declaration order — read off the schema, as Dart read it off
/// drift's table definitions. `sync_quarantine`'s stamp column is
/// deliberately named `remote_hlc_packed`: it also holds suspect far-future
/// stamps, which must never seed a clock.
fn hlc_tables() -> Vec<&'static str> {
    ALL_TABLES
        .iter()
        .copied()
        .filter(|t| {
            let head = format!("CREATE TABLE \"{t}\" (");
            SCHEMA_V2_SQL
                .lines()
                .any(|l| l.starts_with(&head) && l.contains("\"hlc_packed\""))
        })
        .collect()
}

/// Restores the [`HlcClock`]'s monotonicity across app restarts by seeding
/// it with the highest packed HLC ever persisted on this device.
///
/// [`HlcClock::tick`] guarantees a strictly-increasing stamp only relative
/// to its IN-MEMORY high-water mark. A fresh clock starts at wall 0, so
/// after a restart that follows a forward clock excursion (the wall had been
/// held ahead of real time only in memory) a new local edit would stamp a
/// LOWER hlc than ones this install already pushed to peers — and each
/// peer's LWW check silently DROPS that edit. Reseeding from the durable max
/// closes the gap: every synced row (and the oplog) carries hlc_packed — the
/// fixed-width, lexicographically ordered LWW key — so the global MAX is the
/// greatest stamp this install ever produced or adopted from a peer.
///
/// The tables are not the whole story, which is why the applier's durable
/// high-water mark ([`SyncStateStore::HLC_HIGH_WATER_KEY`]) is folded in as
/// well: a verified remote stamp whose record the FK sweep parked advanced
/// the live clock but left no row behind to be found here.
fn seed_clock(db: &Arc<AppDatabase>, node: &str) -> HlcClock {
    let mut max: Option<Hlc> = None;
    let mut consider = |hlc: Hlc| {
        if max.as_ref().is_none_or(|m| hlc > *m) {
            max = Some(hlc);
        }
    };
    // Seeding is a monotonicity safety net, not a launch precondition: one
    // malformed hlc_packed (or one unreadable table) must never brick
    // startup by failing open() before the window ever shows. Parse per
    // table so a bad value only skips ITS table — silent by design, like the
    // other best-effort startup sweeps. (Dart also guarded the enumeration
    // of the tables, which here is a read of the static schema and cannot
    // fail.)
    for table in hlc_tables() {
        let seeded = db
            .query_opt(
                &format!("SELECT MAX(hlc_packed) AS m FROM {table}"),
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .and_then(|packed| packed.flatten().map(|p| Hlc::parse(&p)).transpose());
        match seeded {
            Ok(Some(hlc)) => consider(hlc),
            Ok(None) => {}
            // Malformed value or unreadable table: keep scanning the others.
            Err(e) => log(&format!(
                "seedClock skipped {table} ({})",
                store_error_kind(&e)
            )),
        }
    }
    // The stamps that never became rows (see the doc above). Same
    // best-effort stance: a malformed or unreadable mark only costs its own
    // contribution.
    match SyncStateStore::new(db.clone())
        .hlc_high_water()
        .and_then(|packed| packed.map(|p| Hlc::parse(&p)).transpose())
    {
        Ok(Some(hlc)) => consider(hlc),
        Ok(None) => {}
        Err(e) => log(&format!(
            "seedClock skipped the durable high-water mark ({})",
            store_error_kind(&e)
        )),
    }
    let clock = HlcClock::new(node);
    match max {
        Some(max) => clock.with_initial(max),
        None => clock,
    }
}

/// Whether [s] has the shape of a valid device node id: exactly the 26
/// Crockford base32 characters a [`new_id`] ULID produces (I, L, O, U
/// excluded).
fn is_node_id_shape(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}

/// Reads (or mints + persists once) the stable per-install device node id —
/// the final HLC tiebreak, kept distinct from the data so reinstalling the
/// app is a new node.
///
/// Durability: the mint path writes temp → fsync → atomic rename → dir
/// fsync (the durable-fs idiom used by the asset pool and the migrator),
/// because a power cut mid-write could leave an empty or truncated file —
/// and the node id is this install's sync identity, which must never
/// silently change. Symmetrically, the read path treats an unreadable,
/// empty, or malformed existing file as ABSENT (mint fresh) rather than
/// adopting a corrupted identity.
fn read_or_create_node(dir: &Path) -> io::Result<String> {
    let f = dir.join(NODE_FILE_NAME);
    if f.is_file() {
        // Unreadable/undecodable (e.g. truncated to invalid UTF-8): treat as
        // absent — never let a corrupt file brick startup or leak in as an
        // id.
        let s = fs::read(&f)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| dart_string::trim(&s).to_string())
            .unwrap_or_default();
        if is_node_id_shape(&s) {
            return Ok(s);
        }
    }
    let node = new_id();
    let fs = SystemDurableFs;
    let tmp = PathBuf::from(format!("{}.tmp", f.to_string_lossy()));
    {
        let mut out = File::create(&tmp)?;
        out.write_all(node.as_bytes())?;
        out.sync_all()?;
    }
    fs.fsync_file(&tmp)?;
    fs.atomic_rename(&tmp, &f)?;
    durable_fs::fsync_dir(dir);
    Ok(node)
}

/// One staged file's prefetched hash.
enum Slot {
    /// No worker has taken it yet.
    Pending,
    /// A worker is hashing it (or the walk gave up waiting and hashes it
    /// itself).
    Claimed,
    /// Done: its sha256, or None when the walk must hash it itself.
    Done(Option<String>),
}

struct HashState {
    slots: Vec<Slot>,
    /// The next slot a worker takes, in walk order.
    next: usize,
    stopped: bool,
}

/// Boot recovery's staged-take hashes, computed AHEAD of its sequential walk
/// in up to `parallel` background threads, in walk order — so by the time
/// the walk reaches a file its hash is usually done, and the walk never
/// spends the read. Only recorder-shaped files are hashed (a cheap header
/// probe first), matching what the walk itself would hash.
///
/// Purely an accelerator: any failure here — a probe or read that fails, a
/// file that changed shape, a worker that never started — just answers
/// None, and the walk then probes and hashes the file itself exactly as
/// before, meeting the same error at the same point. A hash is a pure
/// function of the bytes, and nothing writes a staged file during boot, so a
/// prefetched hash IS the one the walk would have computed — and so is the
/// one its save's import already measured ([`AssetStore::known_hash_of`]),
/// which spares re-reading the session's takes.
struct StagedHashes<'a> {
    files: &'a [PathBuf],
    state: Mutex<HashState>,
    done: Condvar,
}

impl<'a> StagedHashes<'a> {
    /// Starts the workers on [files] inside [scope].
    fn start<'scope>(
        scope: &'scope std::thread::Scope<'scope, '_>,
        files: &'a [PathBuf],
        parallel: usize,
    ) -> Arc<StagedHashes<'a>>
    where
        'a: 'scope,
    {
        let this = Arc::new(StagedHashes {
            files,
            state: Mutex::new(HashState {
                slots: files.iter().map(|_| Slot::Pending).collect(),
                next: 0,
                stopped: false,
            }),
            done: Condvar::new(),
        });
        let processors = std::thread::available_parallelism().map_or(1, |n| n.get());
        let workers = parallel.min(processors).min(files.len());
        for _ in 0..workers {
            let worker = this.clone();
            // A worker that cannot start leaves its files to the walk.
            let _ = std::thread::Builder::new()
                .name("vocara-staged-hash".to_string())
                .spawn_scoped(scope, move || worker.work());
        }
        this
    }

    fn work(&self) {
        loop {
            let i = {
                let mut s = lock(&self.state);
                while s.next < s.slots.len() && !matches!(s.slots[s.next], Slot::Pending) {
                    s.next += 1;
                }
                if s.stopped || s.next >= s.slots.len() {
                    return;
                }
                let i = s.next;
                s.slots[i] = Slot::Claimed;
                s.next += 1;
                i
            };
            // Filled however the hash ends, a panic included, so the walk
            // never waits on a slot nobody will fill.
            let mut fill = Fill {
                hashes: self,
                i,
                hash: None,
            };
            fill.hash = Self::hash(&self.files[i]);
        }
    }

    /// The walk's own probe-then-hash, failures answering None (the walk
    /// meets them again itself).
    fn hash(f: &Path) -> Option<String> {
        if !is_recorder_shaped(probe_wav(f).ok()?) {
            return None;
        }
        AssetStore::known_hash_of(f).or_else(|| AssetStore::sha256_of_file(f).ok())
    }

    /// File [i]'s prefetched sha256, or None when the walk must hash it
    /// itself. Waits for a worker already hashing it; a file no worker has
    /// reached yet is left to the walk.
    fn of(&self, i: usize) -> Option<String> {
        let mut s = lock(&self.state);
        loop {
            match &mut s.slots[i] {
                Slot::Done(hash) => return hash.take(),
                slot @ Slot::Pending => {
                    *slot = Slot::Claimed;
                    return None;
                }
                Slot::Claimed => s = self.done.wait(s).unwrap_or_else(PoisonError::into_inner),
            }
        }
    }

    /// Ends the prefetch: workers take no new file.
    fn stop(&self) {
        lock(&self.state).stopped = true;
    }
}

/// Publishes a worker's hash into its slot when dropped.
struct Fill<'h, 'a> {
    hashes: &'h StagedHashes<'a>,
    i: usize,
    hash: Option<String>,
}

impl Drop for Fill<'_, '_> {
    fn drop(&mut self) {
        lock(&self.hashes.state).slots[self.i] = Slot::Done(self.hash.take());
        self.hashes.done.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_is_shared_across_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<Persistence>();
    }

    #[test]
    fn the_clock_seeds_from_every_synced_table_and_the_oplog_only() {
        assert_eq!(
            hlc_tables(),
            vec![
                "practices",
                "interpretations",
                "tracks",
                "clips",
                "practice_notes",
                "technique_markers",
                "fx_buses",
                "techniques",
                "technique_media",
                "assets",
                "oplog",
            ]
        );
    }

    #[test]
    fn a_node_id_is_exactly_a_minted_ulid() {
        assert!(is_node_id_shape(&new_id()));
        assert!(is_node_id_shape("01HZZZZZZZZZZZZZZZZZZZZZZZ"));
        assert!(!is_node_id_shape("01ABC"));
        assert!(!is_node_id_shape("01HZZZZZZZZZZZZZZZZZZZZZZI"));
        assert!(!is_node_id_shape("01hzzzzzzzzzzzzzzzzzzzzzzz"));
        assert!(!is_node_id_shape("01HZZZZZZZZZZZZZZZZZZZZZZZZ"));
    }

    #[test]
    fn the_prefetch_answers_each_file_once_and_leaves_unreached_ones_to_the_walk() {
        let dir = std::env::temp_dir().join(format!("vocara_staged_hashes{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        let files: Vec<PathBuf> = (0..6).map(|i| dir.join(format!("{i}.wav"))).collect();
        for f in &files {
            fs::write(f, b"not a recording").unwrap();
        }
        std::thread::scope(|scope| {
            let hashes = StagedHashes::start(scope, &files, 4);
            for i in 0..files.len() {
                // None: not recorder-shaped, so the walk judges it itself.
                assert_eq!(hashes.of(i), None);
            }
            hashes.stop();
        });
        std::thread::scope(|scope| {
            let hashes = StagedHashes::start(scope, &[], 4);
            hashes.stop();
        });
        fs::remove_dir_all(&dir).unwrap();
    }
}
