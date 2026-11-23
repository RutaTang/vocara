//! The safe schema-migration protocol (port of
//! `lib/data/db/safe_migrator.dart`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags};

use super::durable_fs::{DurableFs, SystemDurableFs};
use crate::error::{Error, Result};

/// A migration step run on the COPY database (raw SQL). It must transform
/// the schema to the target version; the migrator sets `user_version` and
/// validates afterwards.
pub type MigrationStep = fn(&Connection) -> Result<()>;

/// Extra app-level validation run on the migrated copy (e.g. the
/// IntegrityChecker's invariants). Return a message to FAIL.
pub type CopyValidator<'a> = &'a dyn Fn(&Connection) -> Option<String>;

/// The registered schema upgrade steps, keyed by the version each upgrades
/// FROM (step N transforms a vN copy into a v(N+1) schema). Steps are raw
/// SQL on the migration COPY and must produce EXACTLY the schema a new
/// library gets for the target version (the tests compare column for column
/// against a freshly created database).
pub fn registered_migration_steps() -> BTreeMap<i64, MigrationStep> {
    BTreeMap::from([(1, migrate_v1_to_v2 as MigrationStep)])
}

/// v1 -> v2: adds the three LOCAL-ONLY sync bookkeeping tables. Additive
/// only — no existing table is touched, so user data passes through
/// unchanged.
///
/// These tables deliberately carry NO SyncCols: they are per-device engine
/// state, never journaled to the oplog and excluded from every synced-table
/// enumeration (the sync_quarantine stamp column is named remote_hlc_packed,
/// not hlc_packed, so the boot clock seed's hlc_packed scan and the synced
/// table registry's structural scan both skip it — see `rows`).
fn migrate_v1_to_v2(copy: &Connection) -> Result<()> {
    copy.execute_batch(
        "CREATE TABLE IF NOT EXISTS \"sync_state\" (\
         \"key\" TEXT NOT NULL, \
         \"value\" TEXT NOT NULL, \
         PRIMARY KEY (\"key\"))",
    )?;
    copy.execute_batch(
        "CREATE TABLE IF NOT EXISTS \"sync_quarantine\" (\
         \"tbl\" TEXT NOT NULL, \
         \"id\" TEXT NOT NULL, \
         \"remote_hlc_packed\" TEXT NOT NULL, \
         \"schema_v\" INTEGER NOT NULL, \
         \"payload\" TEXT NOT NULL, \
         \"reason\" TEXT NOT NULL, \
         \"quarantined_at\" INTEGER NOT NULL, \
         PRIMARY KEY (\"tbl\", \"id\"))",
    )?;
    copy.execute_batch(
        "CREATE TABLE IF NOT EXISTS \"asset_sync\" (\
         \"hash\" TEXT NOT NULL, \
         \"uploaded_at\" INTEGER NOT NULL, \
         PRIMARY KEY (\"hash\"))",
    )?;
    Ok(())
}

/// Upgrades a SQLite database SAFELY: it never edits the live file in place.
/// Instead it makes a transactionally-consistent COPY (`VACUUM INTO`), runs
/// the migration on the copy, validates it, and only then atomically swaps
/// the copy in. On ANY failure the original file is left byte-for-byte
/// untouched and an [`Error::Migration`] is returned, so a bad migration can
/// never corrupt data.
///
/// A fsync'd `migration.json` marker records the state so a crash mid-swap
/// can be recovered to a consistent file on the next launch
/// ([`SafeMigrator::recover_if_needed`]). Its format is Dart's, byte for
/// byte: `{"state":"<state>","to":<target>}`.
///
/// The caller MUST ensure no other connection holds the live file open (the
/// app quiesces its database first); the migrator opens the file itself.
pub struct SafeMigrator {
    fs: Arc<dyn DurableFs>,
}

impl Default for SafeMigrator {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SafeMigrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SafeMigrator").finish_non_exhaustive()
    }
}

fn dir_of(db_path: &Path) -> PathBuf {
    match db_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// `path` with [suffix] appended to its file name (Dart's `'$dbPath$s'`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Deletes [path] when it is a file (Dart's `File.exists` then `delete`).
fn delete_quietly(path: &Path) -> Result<()> {
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Dart's `\s` in a (non-unicode) RegExp: the ECMAScript white space and
/// line terminators.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// The marker's state: the first match of Dart's
/// `"state"\s*:\s*"(\w+)"`, or 'unknown'.
fn marker_state(json: &str) -> String {
    const KEY: &str = "\"state\"";
    let mut from = 0;
    while let Some(found) = json[from..].find(KEY) {
        let start = from + found;
        from = start + 1;
        let mut rest = json[start + KEY.len()..].chars().peekable();
        while rest.peek().is_some_and(|c| is_js_whitespace(*c)) {
            rest.next();
        }
        if rest.next() != Some(':') {
            continue;
        }
        while rest.peek().is_some_and(|c| is_js_whitespace(*c)) {
            rest.next();
        }
        if rest.next() != Some('"') {
            continue;
        }
        let mut word = String::new();
        while let Some(c) = rest.peek() {
            if c.is_ascii_alphanumeric() || *c == '_' {
                word.push(*c);
                rest.next();
            } else {
                break;
            }
        }
        if !word.is_empty() && rest.next() == Some('"') {
            return word;
        }
    }
    "unknown".to_string()
}

/// A path as SQL string-literal text (Dart's `_sqlLit`).
fn sql_lit(path: &Path) -> Result<String> {
    let s = path
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("not a UTF-8 path: {path:?}")))?;
    Ok(s.replace('\'', "''"))
}

impl SafeMigrator {
    /// The migration copy's file name.
    pub const COPY_NAME: &'static str = "vocara.migrating.db";
    /// The pre-migration rollback copy's file name.
    pub const BAK_NAME: &'static str = "vocara.pre-migrate.bak";
    /// The marker's file name.
    pub const MARKER_NAME: &'static str = "migration.json";

    /// A migrator on the real file system.
    pub fn new() -> Self {
        Self::with_fs(Arc::new(SystemDurableFs))
    }

    /// A migrator whose durability calls go to [fs].
    pub fn with_fs(fs: Arc<dyn DurableFs>) -> Self {
        SafeMigrator { fs }
    }

    /// Migrates the database at [db_path] to [target_version] with [step],
    /// then [validate] (if any) on the migrated copy. Fails with the
    /// original untouched.
    pub fn migrate(
        &self,
        db_path: &Path,
        target_version: i64,
        step: &dyn Fn(&Connection) -> Result<()>,
        validate: Option<CopyValidator<'_>>,
    ) -> Result<()> {
        let dir = dir_of(db_path);
        let copy_path = dir.join(Self::COPY_NAME);
        let bak_path = dir.join(Self::BAK_NAME);
        let marker = dir.join(Self::MARKER_NAME);

        // A stale copy from a crashed prior attempt is disposable.
        delete_quietly(&copy_path)?;
        self.write_marker(&marker, "copying", target_version)?;

        // 1. Checkpoint the live WAL, then VACUUM INTO a clean, consistent
        //    copy (one file, no -wal/-shm sidecars to coordinate).
        let src = Self::open_live(db_path)?;
        let copied = (|| -> Result<()> {
            src.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            src.execute_batch(&format!("VACUUM INTO '{}'", sql_lit(&copy_path)?))?;
            Ok(())
        })();
        drop(src);
        copied?;

        // 2. Run the migration on the COPY only, set user_version, validate.
        self.write_marker(&marker, "migrating", target_version)?;
        let copy = Connection::open(&copy_path)?;
        let migrated = (|| -> Result<()> {
            // Schema rebuilds (12-step table recreate) need FKs off
            // mid-migration.
            copy.execute_batch("PRAGMA foreign_keys = OFF")?;
            step(&copy)?;
            copy.pragma_update(None, "user_version", target_version)?;
            validate_core(&copy, target_version)?;
            if let Some(extra) = validate.and_then(|v| v(&copy)) {
                return Err(Error::migration(format!("app validation failed: {extra}")));
            }
            copy.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok(())
        })();
        if let Err(e) = migrated {
            drop(copy);
            delete_quietly(&copy_path)?;
            self.write_marker(&marker, "failed", target_version)?;
            return Err(match e {
                Error::Migration { .. } => e,
                other => Error::Migration {
                    message: "migration failed".to_string(),
                    cause: Some(Box::new(other)),
                },
            });
        }
        drop(copy);
        // SQLite's own syncs do not flush the drive's write cache (no
        // F_FULLFSYNC by default): force the migrated copy's BYTES to
        // permanent storage before it is renamed over the live db, or a
        // power cut right after the swap could leave a truncated/corrupt
        // file as the only database.
        self.fs.fsync_file(&copy_path)?;

        // 3. Atomic swap (marker first, then sidecar cleanup, then renames).
        self.write_marker(&marker, "swapping", target_version)?;
        for s in ["-wal", "-shm", "-journal"] {
            delete_quietly(&with_suffix(db_path, s))?;
            delete_quietly(&with_suffix(&copy_path, s))?;
        }
        delete_quietly(&bak_path)?;
        // Keep the pre-migration db as rollback.
        std::fs::rename(db_path, &bak_path)?;
        std::fs::rename(&copy_path, db_path)?;
        self.fs.fsync_dir(&dir);
        self.write_marker(&marker, "done", target_version)?;
        Ok(())
    }

    /// Opens a read-write connection to the LIVE database for the
    /// migrator's own writes to it, with `fullfsync` on, like the store's
    /// connection.
    ///
    /// The only such write is step 1's `wal_checkpoint(TRUNCATE)`: it copies
    /// the WAL's frames into the live file and then truncates the WAL. With
    /// a plain fsync the drive may keep the truncate and lose the page
    /// writes, and the live file (or its .bak) is then left torn with nothing
    /// to redo it from. The migration COPY needs no such connection: it is
    /// disposable until the swap and is flushed whole with F_FULLFSYNC
    /// before it is renamed in. [`SafeMigrator::read_user_version`] opens
    /// read-only and never writes.
    pub fn open_live(db_path: &Path) -> Result<Connection> {
        let db = Connection::open(db_path)?;
        db.execute_batch("PRAGMA fullfsync = ON")?;
        Ok(db)
    }

    /// Whether a migration was left unfinished next to [db_path]: a marker
    /// in any state but 'done', or no live file while the migrated copy or
    /// the backup is there. Only [`SafeMigrator::recover_if_needed`] may
    /// touch such a library — opened as it stands, a missing live file
    /// became a new EMPTY database, and the next recovery then took that for
    /// the migrated one and dropped the backup.
    pub fn migration_pending(&self, db_path: &Path) -> Result<bool> {
        let dir = dir_of(db_path);
        let marker = dir.join(Self::MARKER_NAME);
        if marker.is_file() && marker_state(&std::fs::read_to_string(&marker)?) != "done" {
            return Ok(true);
        }
        if db_path.is_file() {
            return Ok(false);
        }
        Ok(dir.join(Self::COPY_NAME).is_file() || dir.join(Self::BAK_NAME).is_file())
    }

    /// Deletes the pre-migration rollback copy next to [db_path]. Returns
    /// whether one was deleted.
    ///
    /// The .bak is the way back while a freshly swapped-in database is still
    /// unproven. Call this only after the live database has answered real
    /// reads on a boot (Persistence runs it once its readability probe and
    /// staged-take recovery have both succeeded). Until then the copy stays.
    /// Never while a migration is unfinished: a marker in any state but
    /// 'done' belongs to [`SafeMigrator::recover_if_needed`], which may yet
    /// roll back to the .bak. ('done' is what a migration that completed
    /// during THIS boot leaves; the next boot's recovery clears it.)
    pub fn discard_backup(&self, db_path: &Path) -> Result<bool> {
        let dir = dir_of(db_path);
        let marker = dir.join(Self::MARKER_NAME);
        if marker.is_file() && marker_state(&std::fs::read_to_string(&marker)?) != "done" {
            return Ok(false);
        }
        let bak = dir.join(Self::BAK_NAME);
        if !bak.is_file() {
            return Ok(false);
        }
        std::fs::remove_file(&bak)?;
        Ok(true)
    }

    /// Brings the on-disk database at [db_path] up to [target_version] by
    /// running the registered steps (or [steps]) ONE version at a time, each
    /// through the full copy/migrate/validate/swap protocol (so a crash
    /// between two steps leaves a consistent, recoverable intermediate
    /// version).
    ///
    /// Deliberate no-ops, chosen as the safest behavior:
    ///  * the file does not exist — a fresh install; the store's open builds
    ///    the current schema directly (opening it here would CREATE an empty
    ///    file);
    ///  * `user_version` is 0 — the file exists but no schema was ever
    ///    created in it (an empty just-created file); the store's open
    ///    handles it;
    ///  * `user_version` >= [target_version] — already current (a NEWER
    ///    on-disk version is left untouched too: never modify data from a
    ///    newer app; the store's own version check refuses to open it).
    ///
    /// Call AFTER [`SafeMigrator::recover_if_needed`] and BEFORE the store
    /// opens the file.
    pub fn upgrade_if_needed(
        &self,
        db_path: &Path,
        target_version: i64,
        steps: Option<&BTreeMap<i64, MigrationStep>>,
    ) -> Result<()> {
        if !db_path.is_file() {
            return Ok(());
        }
        let registered;
        let registry = match steps {
            Some(steps) => steps,
            None => {
                registered = registered_migration_steps();
                &registered
            }
        };
        let mut current = self.read_user_version(db_path)?;
        if current == 0 {
            return Ok(());
        }
        while current < target_version {
            let Some(step) = registry.get(&current) else {
                return Err(Error::migration(format!(
                    "no registered migration step from v{current} (target v{target_version})"
                )));
            };
            self.migrate(db_path, current + 1, step, None)?;
            let next = self.read_user_version(db_path)?;
            if next <= current {
                // Defensive: a step that fails to advance must not spin
                // forever.
                return Err(Error::migration(format!(
                    "migration step from v{current} left user_version at {next}"
                )));
            }
            current = next;
        }
        Ok(())
    }

    /// Reads the schema `user_version` of the SQLite file at [db_path]
    /// without ever creating or modifying it (read-only open). Fails if the
    /// file is not a readable SQLite database.
    ///
    /// Public because Persistence uses it to REFUSE a library written by a
    /// newer app (user_version above the code's schema) before building the
    /// store, and the diagnostics exporter includes it in bug-report
    /// bundles.
    pub fn read_user_version(&self, db_path: &Path) -> Result<i64> {
        let raw = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(raw.pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// Called at startup BEFORE opening the live db. If a crash left a
    /// half-done swap, finish or roll it back so the live file is always
    /// consistent.
    pub fn recover_if_needed(&self, db_path: &Path) -> Result<()> {
        let dir = dir_of(db_path);
        let marker = dir.join(Self::MARKER_NAME);
        if !marker.is_file() {
            return Ok(());
        }
        let state = marker_state(&std::fs::read_to_string(&marker)?);
        let copy_path = dir.join(Self::COPY_NAME);
        let bak_path = dir.join(Self::BAK_NAME);

        match state.as_str() {
            "swapping" => {
                // The two-rename swap (live->bak, copy->live) may have been
                // interrupted at ANY point, and with no durable directory
                // fsync the filesystem may have reordered the entry updates.
                // Recover deterministically from whichever file survived,
                // most-progressed first, so we NEVER come up with no
                // database (the old bug fell through to "nothing to do" when
                // both live and copy entries were lost, abandoning the only
                // data in .bak — a fresh empty db was then created).
                if db_path.is_file() {
                    // copy->live completed (or live was never moved): the
                    // live db is current.
                } else if copy_path.is_file() {
                    // live->bak done but copy->live not: promote the
                    // migrated copy. Flush its bytes first — the crash may
                    // have hit before migrate()'s own fsync, and promoting an
                    // unflushed copy reopens the window where power loss
                    // leaves a truncated live db.
                    self.fs.fsync_file(&copy_path)?;
                    std::fs::rename(&copy_path, db_path)?;
                } else if bak_path.is_file() {
                    // Both the live and copy directory entries were lost
                    // mid-swap: roll back to the pre-migration original so
                    // data is never lost. The migration simply re-runs next
                    // launch. (Idempotent across repeated crashes: a later
                    // boot re-enters this branch until the live file
                    // exists.)
                    std::fs::rename(&bak_path, db_path)?;
                }
                // else: no db, copy, or bak survived — unrecoverable FS
                // corruption. Durably land the rename (if any) BEFORE
                // dropping the marker, matching the forward-swap ordering
                // (fsync_dir then terminal marker); otherwise a double-fault
                // could delete the marker while the rename is still only in
                // the FS cache, abandoning the db in .bak with no marker to
                // recover from.
                self.fs.fsync_dir(&dir);
                std::fs::remove_file(&marker)?;
                self.fs.fsync_dir(&dir);
            }
            "copying" | "migrating" | "failed" => {
                // The live db was never touched; drop the disposable copy.
                delete_quietly(&copy_path)?;
                std::fs::remove_file(&marker)?;
            }
            _ => {
                // 'done', or a marker nobody can read.
                std::fs::remove_file(&marker)?;
            }
        }
        // The .bak is kept here on purpose: the migrated database has not
        // yet answered a real read. Persistence discards it
        // (discard_backup) once that boot's readability probe and recovery
        // have succeeded.
        Ok(())
    }

    fn write_marker(&self, marker: &Path, state: &str, target: i64) -> Result<()> {
        std::fs::write(marker, format!("{{\"state\":\"{state}\",\"to\":{target}}}"))?;
        self.fs.fsync_file(marker)?;
        // A newly CREATED marker is only durable once its directory entry is
        // also flushed (fsync_file covers the bytes, not the dirent).
        // Without this, a crash mid-swap could land the renames on disk with
        // NO marker, so recover_if_needed would see "nothing to do" while
        // the only database sits half-swapped (e.g. stranded in .bak).
        self.fs.fsync_dir(&dir_of(marker));
        Ok(())
    }
}

fn validate_core(copy: &Connection, target: i64) -> Result<()> {
    let integ: Vec<String> = {
        let mut stmt = copy.prepare("PRAGMA integrity_check")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if integ.first().map(String::as_str) != Some("ok") {
        return Err(Error::migration(format!("integrity_check: {integ:?}")));
    }
    // foreign_key_check is a standalone checker, independent of the FK
    // pragma.
    let fk = {
        let mut stmt = copy.prepare("PRAGMA foreign_key_check")?;
        let mut rows = stmt.query([])?;
        let mut n = 0;
        while rows.next()?.is_some() {
            n += 1;
        }
        n
    };
    if fk > 0 {
        return Err(Error::migration(format!(
            "foreign_key_check found {fk} violations"
        )));
    }
    let uv: i64 = copy.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if uv != target {
        return Err(Error::migration(format!(
            "user_version is {uv}, expected {target}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::rows::{PracticeRow, SyncStateRow};
    use crate::db::schema::{AppDatabase, CURRENT_SCHEMA_VERSION};
    use parking_lot::Mutex;

    struct Fixture {
        tmp: tempfile::TempDir,
        db_path: PathBuf,
    }

    fn pid() -> String {
        format!("{:0<26}", "P")
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("vocara.db");
        Fixture { tmp, db_path }
    }

    impl Fixture {
        fn path(&self, name: &str) -> PathBuf {
            self.tmp.path().join(name)
        }

        /// Creates a GENUINE v1 database file with one practice row, then
        /// closes it: the store creates v2 directly, so the v2 additions
        /// are stripped and user_version is reset to 1.
        fn seed_v1(&self) {
            let db = AppDatabase::open(&self.db_path).unwrap();
            db.insert(&PracticeRow {
                id: pid(),
                title: "keepme".to_string(),
                created_at: 1,
                updated_at: 1,
                hlc_wall: 1,
                hlc_ctr: 0,
                hlc_node: format!("{:0<26}", "N"),
                hlc_packed: "x".to_string(),
                ..Default::default()
            })
            .unwrap();
            db.close().unwrap();
            let raw = Connection::open(&self.db_path).unwrap();
            raw.execute_batch(
                "DROP TABLE sync_state; DROP TABLE sync_quarantine; \
                 DROP TABLE asset_sync; PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        fn user_version(&self) -> i64 {
            let raw = Connection::open(&self.db_path).unwrap();
            raw.pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap()
        }

        fn practice_count(&self) -> i64 {
            let raw = Connection::open(&self.db_path).unwrap();
            raw.query_row("SELECT COUNT(*) FROM practices", [], |r| r.get(0))
                .unwrap()
        }
    }

    fn add_column(name: &'static str) -> impl Fn(&Connection) -> Result<()> {
        move |copy| {
            copy.execute_batch(&format!("ALTER TABLE practices ADD COLUMN {name} TEXT"))?;
            Ok(())
        }
    }

    #[test]
    fn migrates_on_a_copy_swaps_in_keeps_data_and_bumps_version() {
        let f = fixture();
        f.seed_v1();
        SafeMigrator::new()
            .migrate(&f.db_path, 2, &add_column("test_col"), None)
            .unwrap();

        assert_eq!(f.user_version(), 2);
        let raw = Connection::open(&f.db_path).unwrap();
        let cols: Vec<String> = raw
            .prepare("PRAGMA table_info(practices)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .map(|c| c.unwrap())
            .collect();
        assert!(cols.contains(&"test_col".to_string()));
        let title: String = raw
            .query_row("SELECT title FROM practices WHERE id = ?", [pid()], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "keepme");

        // The pre-migration db is retained as a rollback copy, and the
        // marker is Dart's format.
        assert!(f.path(SafeMigrator::BAK_NAME).exists());
        assert_eq!(
            std::fs::read_to_string(f.path(SafeMigrator::MARKER_NAME)).unwrap(),
            r#"{"state":"done","to":2}"#
        );
    }

    #[test]
    fn a_failing_migration_leaves_the_original_untouched() {
        let f = fixture();
        f.seed_v1();
        let err = SafeMigrator::new()
            .migrate(
                &f.db_path,
                2,
                &|_| Err(Error::State("boom".to_string())),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Migration { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "MigrationException: migration failed (bad state: boom)"
        );

        assert_eq!(f.user_version(), 1); // never advanced
        assert_eq!(f.practice_count(), 1);
        // The disposable copy is gone.
        assert!(!f.path(SafeMigrator::COPY_NAME).exists());
        assert_eq!(
            std::fs::read_to_string(f.path(SafeMigrator::MARKER_NAME)).unwrap(),
            r#"{"state":"failed","to":2}"#
        );
    }

    #[test]
    fn app_validation_can_reject_a_structurally_valid_migration() {
        let f = fixture();
        f.seed_v1();
        let err = SafeMigrator::new()
            .migrate(
                &f.db_path,
                2,
                &add_column("x"),
                Some(&|_| Some("app says no".to_string())),
            )
            .unwrap_err();
        assert!(
            matches!(&err, Error::Migration { message, .. } if message == "app validation failed: app says no")
        );
        assert_eq!(f.user_version(), 1); // untouched
    }

    #[test]
    fn recovery_finishes_a_swap_interrupted_after_the_bak_rename() {
        let f = fixture();
        f.seed_v1();
        SafeMigrator::new()
            .migrate(&f.db_path, 2, &add_column("c"), None)
            .unwrap();
        // Re-stage "crashed mid-swap": the migrated db sits at the copy path
        // and the live path is empty.
        std::fs::rename(&f.db_path, f.path(SafeMigrator::COPY_NAME)).unwrap();
        std::fs::write(
            f.path(SafeMigrator::MARKER_NAME),
            r#"{"state":"swapping","to":2}"#,
        )
        .unwrap();

        SafeMigrator::new().recover_if_needed(&f.db_path).unwrap();

        assert!(f.db_path.exists());
        assert_eq!(f.user_version(), 2); // the staged copy became live
        assert!(!f.path(SafeMigrator::MARKER_NAME).exists());
    }

    #[test]
    fn recovery_drops_a_stale_copy_from_a_crash_before_the_swap() {
        let f = fixture();
        f.seed_v1();
        std::fs::write(f.path(SafeMigrator::COPY_NAME), "junk").unwrap();
        std::fs::write(
            f.path(SafeMigrator::MARKER_NAME),
            r#"{"state":"migrating","to":2}"#,
        )
        .unwrap();

        SafeMigrator::new().recover_if_needed(&f.db_path).unwrap();

        assert_eq!(f.user_version(), 1); // original intact
        assert!(!f.path(SafeMigrator::COPY_NAME).exists());
        assert!(!f.path(SafeMigrator::MARKER_NAME).exists());
    }

    #[test]
    fn recovery_rolls_back_to_the_bak_when_both_live_and_copy_were_lost() {
        let f = fixture();
        f.seed_v1();
        std::fs::rename(&f.db_path, f.path(SafeMigrator::BAK_NAME)).unwrap();
        std::fs::write(
            f.path(SafeMigrator::MARKER_NAME),
            r#"{"state":"swapping","to":2}"#,
        )
        .unwrap();

        SafeMigrator::new().recover_if_needed(&f.db_path).unwrap();

        // The original data is restored as the live db.
        assert!(f.db_path.exists());
        assert_eq!(f.user_version(), 1);
        assert_eq!(f.practice_count(), 1);
        assert!(!f.path(SafeMigrator::MARKER_NAME).exists());
    }

    #[test]
    fn recovery_is_a_no_op_when_there_is_no_marker() {
        let f = fixture();
        f.seed_v1();
        SafeMigrator::new().recover_if_needed(&f.db_path).unwrap();
        assert_eq!(f.user_version(), 1);
    }

    /// Records every durability call, tagged with enough directory state to
    /// assert ORDER relative to the swap renames.
    struct RecordingFs {
        dir: PathBuf,
        log: Mutex<Vec<String>>,
    }

    impl RecordingFs {
        fn new(dir: &Path) -> Arc<Self> {
            Arc::new(RecordingFs {
                dir: dir.to_path_buf(),
                log: Mutex::new(Vec::new()),
            })
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().clone()
        }
    }

    impl DurableFs for RecordingFs {
        fn fsync_file(&self, path: &Path) -> std::io::Result<()> {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name == SafeMigrator::MARKER_NAME {
                let state = marker_state(&std::fs::read_to_string(path)?);
                self.log.lock().push(format!("marker:{state}"));
            } else {
                let live = self.dir.join("vocara.db").exists();
                self.log
                    .lock()
                    .push(format!("file:{name}(liveExists={live})"));
            }
            crate::db::durable_fs::fsync_file(path)
        }

        fn fsync_dir(&self, path: &Path) {
            let bak = self.dir.join(SafeMigrator::BAK_NAME).exists();
            self.log.lock().push(format!("dir(bakExists={bak})"));
            crate::db::durable_fs::fsync_dir(path)
        }
    }

    fn index_from(log: &[String], entry: &str, from: usize) -> Option<usize> {
        log.iter()
            .skip(from)
            .position(|e| e == entry)
            .map(|i| i + from)
    }

    #[test]
    fn copy_bytes_and_swapping_marker_dirent_are_durable_before_the_swap_renames() {
        let f = fixture();
        f.seed_v1();
        let fs = RecordingFs::new(f.tmp.path());
        SafeMigrator::with_fs(fs.clone())
            .migrate(&f.db_path, 2, &add_column("c"), None)
            .unwrap();
        let log = fs.log();

        let copy_sync = index_from(
            &log,
            &format!("file:{}(liveExists=true)", SafeMigrator::COPY_NAME),
            0,
        )
        .unwrap_or_else(|| panic!("copy must be fsynced before the swap: {log:?}"));

        let swap_marker = index_from(&log, "marker:swapping", 0).unwrap();
        assert!(swap_marker > copy_sync);
        let pre_swap_dir_sync = index_from(&log, "dir(bakExists=false)", swap_marker)
            .unwrap_or_else(|| {
                panic!("the swapping marker dirent must be flushed before the renames: {log:?}")
            });
        assert!(pre_swap_dir_sync > swap_marker);
        assert!(index_from(&log, "dir(bakExists=true)", pre_swap_dir_sync).is_some());
    }

    /// PRAGMA table_info projected to the schema-relevant tuple per column.
    fn table_info(path: &Path, table: &str) -> Vec<(String, String, i64, Option<String>, i64)> {
        let raw = Connection::open(path).unwrap();
        let mut stmt = raw
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap();
        stmt.query_map([], |r| {
            Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .map(|c| c.unwrap())
        .collect()
    }

    #[test]
    fn upgrade_brings_a_v1_db_to_v2_matching_the_fresh_schema() {
        let f = fixture();
        f.seed_v1();
        SafeMigrator::new()
            .upgrade_if_needed(&f.db_path, CURRENT_SCHEMA_VERSION, None)
            .unwrap();
        assert_eq!(f.user_version(), CURRENT_SCHEMA_VERSION);

        let raw = Connection::open(&f.db_path).unwrap();
        let title: String = raw
            .query_row("SELECT title FROM practices WHERE id = ?", [pid()], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "keepme");
        drop(raw);

        // The migrated sync tables are column-for-column identical to what a
        // FRESH database gets.
        let fresh = f.path("fresh.db");
        AppDatabase::open(&fresh).unwrap().close().unwrap();
        for table in ["sync_state", "sync_quarantine", "asset_sync"] {
            assert_eq!(
                table_info(&f.db_path, table),
                table_info(&fresh, table),
                "{table} schema must match the fresh one"
            );
        }

        // The store opens the migrated file at the current version (its
        // in-place upgrade refusal must NOT fire) and the new tables work.
        let db = AppDatabase::open(&f.db_path).unwrap();
        let kept = db.select_all::<PracticeRow>().unwrap();
        assert_eq!(kept[0].title, "keepme");
        db.insert(&SyncStateRow {
            key: "k".to_string(),
            value: "v".to_string(),
        })
        .unwrap();
        assert_eq!(db.select_all::<SyncStateRow>().unwrap()[0].value, "v");
        db.close().unwrap();
    }

    #[test]
    fn upgrade_is_a_no_op_on_an_already_current_database() {
        let f = fixture();
        f.seed_v1();
        let m = SafeMigrator::new();
        m.upgrade_if_needed(&f.db_path, CURRENT_SCHEMA_VERSION, None)
            .unwrap();
        // Remove the rollback .bak so a second run would recreate it IF it
        // (wrongly) migrated again.
        std::fs::remove_file(f.path(SafeMigrator::BAK_NAME)).unwrap();

        m.upgrade_if_needed(&f.db_path, CURRENT_SCHEMA_VERSION, None)
            .unwrap();

        assert_eq!(f.user_version(), CURRENT_SCHEMA_VERSION);
        assert!(!f.path(SafeMigrator::BAK_NAME).exists());
    }

    #[test]
    fn upgrade_is_a_no_op_when_no_database_file_exists_yet() {
        let f = fixture();
        SafeMigrator::new()
            .upgrade_if_needed(&f.db_path, CURRENT_SCHEMA_VERSION, None)
            .unwrap();
        assert!(!f.db_path.exists()); // probing created nothing
    }

    #[test]
    fn upgrade_refuses_a_version_gap_with_no_registered_step() {
        let f = fixture();
        f.seed_v1();
        let err = SafeMigrator::new()
            .upgrade_if_needed(&f.db_path, CURRENT_SCHEMA_VERSION + 1, None)
            .unwrap_err();
        assert!(matches!(err, Error::Migration { .. }));
        // The v1->v2 step it DID have ran safely; the missing step ran
        // nothing.
        assert_eq!(f.user_version(), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn upgrade_runs_a_caller_supplied_step_map() {
        let f = fixture();
        f.seed_v1();
        let mut steps = BTreeMap::new();
        steps.insert(
            1,
            (|copy: &Connection| {
                copy.execute_batch("CREATE TABLE marker_of_the_custom_step (x)")?;
                Ok(())
            }) as MigrationStep,
        );
        SafeMigrator::new()
            .upgrade_if_needed(&f.db_path, 2, Some(&steps))
            .unwrap();
        assert_eq!(f.user_version(), 2);
        let raw = Connection::open(&f.db_path).unwrap();
        let n: i64 = raw
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'marker_of_the_custom_step'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn recovery_flushes_the_copy_bytes_before_promoting_it_to_live() {
        let f = fixture();
        f.seed_v1();
        SafeMigrator::new()
            .migrate(&f.db_path, 2, &add_column("c"), None)
            .unwrap();
        std::fs::rename(&f.db_path, f.path(SafeMigrator::COPY_NAME)).unwrap();
        std::fs::write(
            f.path(SafeMigrator::MARKER_NAME),
            r#"{"state":"swapping","to":2}"#,
        )
        .unwrap();

        let fs = RecordingFs::new(f.tmp.path());
        SafeMigrator::with_fs(fs.clone())
            .recover_if_needed(&f.db_path)
            .unwrap();

        assert!(fs.log().contains(&format!(
            "file:{}(liveExists=false)",
            SafeMigrator::COPY_NAME
        )));
        assert_eq!(f.user_version(), 2);
    }

    #[test]
    fn the_migrators_own_connection_to_the_live_database_flushes_through() {
        // It checkpoints the live WAL into the file and truncates it before
        // the copy is taken; without fullfsync the truncate can outlive the
        // page writes on a power cut.
        let f = fixture();
        let live = SafeMigrator::open_live(&f.db_path).unwrap();
        let full: i64 = live
            .pragma_query_value(None, "fullfsync", |r| r.get(0))
            .unwrap();
        assert_eq!(full, 1);
    }

    #[test]
    fn the_backup_is_never_discarded_while_a_migration_marker_is_pending() {
        let f = fixture();
        std::fs::write(f.path(SafeMigrator::BAK_NAME), "rollback copy").unwrap();
        let marker = f.path(SafeMigrator::MARKER_NAME);
        std::fs::write(&marker, r#"{"state":"swapping","to":2}"#).unwrap();

        let m = SafeMigrator::new();
        assert!(!m.discard_backup(&f.db_path).unwrap());
        assert!(f.path(SafeMigrator::BAK_NAME).exists());

        // A migration that finished (this boot's own) does not hold it back.
        std::fs::write(&marker, r#"{"state":"done","to":2}"#).unwrap();
        assert!(m.discard_backup(&f.db_path).unwrap());
        assert!(!f.path(SafeMigrator::BAK_NAME).exists());
        assert!(!m.discard_backup(&f.db_path).unwrap(), "nothing left");
    }

    #[test]
    fn migration_pending_sees_an_unfinished_marker_or_a_missing_live_file() {
        let f = fixture();
        let m = SafeMigrator::new();
        assert!(!m.migration_pending(&f.db_path).unwrap(), "fresh install");
        f.seed_v1();
        assert!(!m.migration_pending(&f.db_path).unwrap());
        let marker = f.path(SafeMigrator::MARKER_NAME);
        std::fs::write(&marker, r#"{"state":"swapping","to":99}"#).unwrap();
        assert!(m.migration_pending(&f.db_path).unwrap());
        std::fs::write(&marker, r#"{"state":"done","to":2}"#).unwrap();
        assert!(!m.migration_pending(&f.db_path).unwrap());
        // The owner died mid-swap after moving the live file away.
        std::fs::remove_file(&marker).unwrap();
        std::fs::rename(&f.db_path, f.path(SafeMigrator::BAK_NAME)).unwrap();
        assert!(m.migration_pending(&f.db_path).unwrap());
    }

    #[test]
    fn read_user_version_never_creates_the_file() {
        let f = fixture();
        assert!(SafeMigrator::new().read_user_version(&f.db_path).is_err());
        assert!(!f.db_path.exists());
        f.seed_v1();
        assert_eq!(
            SafeMigrator::new().read_user_version(&f.db_path).unwrap(),
            1
        );
    }

    #[test]
    fn the_marker_state_reads_like_dart_s_regexp() {
        assert_eq!(marker_state(r#"{"state":"swapping","to":2}"#), "swapping");
        assert_eq!(marker_state(r#"{ "state" :  "done" }"#), "done");
        assert_eq!(marker_state("{\"state\"\n:\t\"a_b1\"}"), "a_b1");
        assert_eq!(marker_state(r#"{"state":"two words"}"#), "unknown");
        assert_eq!(marker_state(r#"{"state":""}"#), "unknown");
        assert_eq!(marker_state("garbage"), "unknown");
        assert_eq!(
            marker_state(r#"{"state":1,"x":{"state":"later"}}"#),
            "later",
            "the first MATCH, not the first key"
        );
    }
}
