//! The SQLite working store (port of `lib/data/db/app_database.dart` and the
//! schema half of `lib/data/db/tables.dart`).
//!
//! Schema history: v1 greenfield; v2 adds the LOCAL-ONLY sync bookkeeping
//! tables (sync_state, sync_quarantine, asset_sync) — additive only, upgraded
//! on disk by the registered SafeMigrator step (see `safe_migrator`).
//!
//! The schema is `schema_v2.sql`, dumped from the drift-created database and
//! executed verbatim, so a library this crate creates is byte-for-byte the
//! schema the Flutter app creates (and the reverse).
//!
//! In-place upgrades are DELIBERATELY refused: the safe schema-migration
//! protocol upgrades a COPY of the database, validates it, and atomically
//! swaps it in, so by the time the store opens the live file its
//! `user_version` already equals [`CURRENT_SCHEMA_VERSION`] and an in-place
//! upgrade must never run. Meeting another version here means that protocol
//! was bypassed.
//!
//! # Concurrency and transactions
//!
//! One connection behind a mutex, as drift serialized its queries. The mutex
//! is RE-ENTRANT per thread and [`AppDatabase::transaction`] keeps it for the
//! whole transaction, which gives drift's zone-scoped transactions their Rust
//! form: every store call a transaction's closure makes on its thread — a row
//! write, `SyncWriter::journal`, `SyncStateStore::set`,
//! `QuarantineStore::put` — joins that transaction, a nested
//! [`AppDatabase::transaction`] becomes a SAVEPOINT, and any other thread
//! waits until the transaction ends. So a row write and its oplog entry
//! commit together, or not at all.

use std::cell::Cell;
use std::path::Path;

use parking_lot::ReentrantMutex;
use rusqlite::{Connection, OptionalExtension, Row};

use super::rows::{self, SyncedRow, TableRow};
use crate::error::{Error, Result};

/// The current code schema version; the SafeMigrator upgrade driver targets
/// this so persistence wiring can never drift out of step with the schema.
pub const CURRENT_SCHEMA_VERSION: i64 = 2;

/// The exact schema of a v2 library, dumped from drift (do not hand-edit).
pub const SCHEMA_V2_SQL: &str = include_str!("../schema_v2.sql");

/// Every table, in drift's declaration order (`AppDatabase.allTables`).
pub const ALL_TABLES: &[&str] = &[
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
    "recordings_inflight",
    "waveform_cache",
    "sync_state",
    "sync_quarantine",
    "asset_sync",
];

/// Indexes + partial-unique constraints that drift's table definitions
/// can't express directly. Re-run with IF NOT EXISTS on EVERY open (not only
/// at creation), so an index added in a newer build reaches existing
/// libraries without a schema-version bump or a SafeMigrator step.
const DERIVED_INDEXES: &[&str] = &[
    // Single-slot rule: at most one LIVE accompaniment and one LIVE
    // reference track per practice (two offline devices each importing a
    // backing must not produce two live lanes — slot-level LWW resolves the
    // rest).
    "CREATE UNIQUE INDEX IF NOT EXISTS ux_track_slot \
     ON tracks (practice_id, kind) \
     WHERE deleted_at IS NULL AND kind IN ('accompaniment', 'reference')",
    // Dedup helper (NON-unique on purpose: identical bytes under two asset
    // ULIDs across devices is the normal content-addressed steady state).
    "CREATE INDEX IF NOT EXISTS ix_assets_hash ON assets (content_hash)",
    // Hot reads: list practices by recency; load one practice's whole graph;
    // a technique's marker usage; draining the oplog.
    "CREATE INDEX IF NOT EXISTS ix_practices_updated \
     ON practices (deleted_at, updated_at)",
    "CREATE INDEX IF NOT EXISTS ix_interp_practice \
     ON interpretations (practice_id)",
    "CREATE INDEX IF NOT EXISTS ix_tracks_practice ON tracks (practice_id)",
    "CREATE INDEX IF NOT EXISTS ix_clips_track ON clips (track_id)",
    "CREATE INDEX IF NOT EXISTS ix_notes_track ON practice_notes (track_id)",
    "CREATE INDEX IF NOT EXISTS ix_markers_track \
     ON technique_markers (track_id)",
    "CREATE INDEX IF NOT EXISTS ix_markers_technique \
     ON technique_markers (technique_id)",
    "CREATE INDEX IF NOT EXISTS ix_buses_practice ON fx_buses (practice_id)",
    "CREATE INDEX IF NOT EXISTS ix_media_technique \
     ON technique_media (technique_id)",
    "CREATE INDEX IF NOT EXISTS ix_oplog_unsynced ON oplog (synced, seq)",
    // Boot HLC seeding runs SELECT MAX(hlc_packed) over every hlc-carrying
    // table; the synced tables are small, but the oplog gains a row per save
    // and is only trimmed by ack/compaction, so give its scan an index (MAX
    // resolves off the btree tail).
    "CREATE INDEX IF NOT EXISTS ix_oplog_hlc ON oplog (hlc_packed)",
    // A save resolves every clip's pool path back to its live owner by
    // local_rel_path. Asset rows are only ever tombstoned, so the table grows
    // with the library's whole history; unindexed, those lookups scan it
    // (100 per save: 55 ms of SQL at 10k assets, 172 ms at 30k).
    "CREATE INDEX IF NOT EXISTS ix_assets_rel ON assets (local_rel_path)",
    // Per-entity history: the dependency carry-forward in boot compaction
    // and in OplogDrain::ack correlates a subquery on (entity_table,
    // entity_id) for EVERY surviving entry, and the GC's pending-journal
    // guard probes the same pair per asset. Unindexed, each probe scans the
    // whole ledger — quadratic in a ledger nothing acks today (one entry per
    // row ever written): ~6 s at 10k entities, ~24 s at 20k, on every boot
    // before the window shows. seq rides along so the ORDER BY seq DESC
    // LIMIT 1 resolves off the index tail.
    "CREATE INDEX IF NOT EXISTS ix_oplog_entity \
     ON oplog (entity_table, entity_id, seq)",
];

/// The statements of [`SCHEMA_V2_SQL`] a new library is created with: every
/// line but the version comment and `sqlite_sequence`, which SQLite creates
/// itself with the first AUTOINCREMENT table (and refuses to be created by
/// hand).
fn schema_statements() -> impl Iterator<Item = &'static str> {
    SCHEMA_V2_SQL
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("--"))
        .filter(|l| !l.starts_with("CREATE TABLE sqlite_sequence"))
}

struct Inner {
    conn: Connection,
    /// How many transactions (the outermost plus its savepoints) are open.
    depth: Cell<u32>,
}

/// The library database: one SQLite connection, serialized like drift's.
/// `Send + Sync`; share it as `Arc<AppDatabase>`.
pub struct AppDatabase {
    inner: ReentrantMutex<Inner>,
}

impl std::fmt::Debug for AppDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppDatabase").finish_non_exhaustive()
    }
}

impl AppDatabase {
    /// The current code schema version (drift's `currentSchemaVersion`).
    pub const CURRENT_SCHEMA_VERSION: i64 = CURRENT_SCHEMA_VERSION;

    /// Opens (creating when missing) the library database at [path].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_setup(path, |_| Ok(()))
    }

    /// [`AppDatabase::open`] running [setup] on the raw connection first,
    /// before the schema is checked or created (drift's
    /// `NativeDatabase(setup:)`; Persistence installs its busy handler
    /// there).
    pub fn open_with_setup(
        path: impl AsRef<Path>,
        setup: impl FnOnce(&Connection) -> rusqlite::Result<()>,
    ) -> Result<Self> {
        let conn = Connection::open(path)?;
        setup(&conn)?;
        Self::from_connection(conn)
    }

    /// A fresh in-memory library (drift's `NativeDatabase.memory()`).
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Runs the open protocol on an already-open connection: create the
    /// schema on a new file, refuse any other version than the current one,
    /// then the per-open pragmas and the derived indexes.
    pub fn from_connection(conn: Connection) -> Result<Self> {
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            // A new (or empty, just-created) file: build the current schema.
            // One transaction, so a crash mid-way leaves user_version 0 and
            // the next open simply builds it again.
            conn.execute_batch("BEGIN")?;
            let created = (|| -> rusqlite::Result<()> {
                for statement in schema_statements() {
                    conn.execute_batch(statement)?;
                }
                for statement in DERIVED_INDEXES {
                    conn.execute_batch(statement)?;
                }
                conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)
            })();
            match created {
                Ok(()) => conn.execute_batch("COMMIT")?,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e.into());
                }
            }
        } else if version != CURRENT_SCHEMA_VERSION {
            // The live database is never opened at another version — the
            // copy/migrate/validate/swap protocol guarantees user_version ==
            // the code schema version first. Reaching here means that
            // protocol was bypassed.
            return Err(Error::State(format!(
                "In-place drift onUpgrade must not run (from {version} to \
                 {CURRENT_SCHEMA_VERSION}). The safe schema-migration protocol \
                 owns upgrades."
            )));
        }
        // Commit durability on macOS. SQLite's default sync there is a plain
        // fsync(), which hands the bytes to the drive and returns while they
        // still sit in its write cache (see durable_fs). A power cut can then
        // lose saves that already reported success, and because the drive is
        // free to reorder what it holds, it can also land a checkpoint's page
        // writes out of order with the WAL restart that follows them, which
        // corrupts the library. With fullfsync on, every sync SQLite issues,
        // checkpoints included, is an F_FULLFSYNC. That is the same standard
        // the asset pool already holds its files to before a row may
        // reference them. Per connection, so it is set on every open.
        // Elsewhere the flag is stored but has no effect.
        //
        // Cost: one drive flush per commit that changes pages. A save is one
        // write transaction (plus one per newly imported file), and a
        // transaction that changes nothing never syncs.
        conn.execute_batch("PRAGMA fullfsync = ON")?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        // Derived indexes are VERSIONLESS schema: re-applied idempotently on
        // every open (IF NOT EXISTS). Cheap when nothing is missing — SQLite
        // resolves each IF NOT EXISTS against the schema table without
        // touching data.
        for statement in DERIVED_INDEXES {
            conn.execute_batch(statement)?;
        }
        Ok(AppDatabase {
            inner: ReentrantMutex::new(Inner {
                conn,
                depth: Cell::new(0),
            }),
        })
    }

    /// Closes the connection (drift's `close()`), reporting a failure that
    /// dropping would swallow.
    pub fn close(self) -> Result<()> {
        let inner = self.inner.into_inner();
        inner.conn.close().map_err(|(_, e)| e.into())
    }

    /// Runs [f] with the connection, holding the database for its duration.
    /// Inside a transaction on this thread, [f] runs in that transaction.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        let guard = self.inner.lock();
        f(&guard.conn)
    }

    /// Whether a transaction is open on this database (on this thread: any
    /// other thread waits here until it ends).
    pub fn in_transaction(&self) -> bool {
        self.inner.lock().depth.get() > 0
    }

    /// Runs [f] in a transaction: committed when it returns `Ok`, rolled
    /// back when it returns `Err` (or panics). Every store call [f] makes on
    /// this thread joins it; a nested call becomes a SAVEPOINT, so an inner
    /// failure the outer closure handles rolls back only the inner part.
    pub fn transaction<T, E>(
        &self,
        f: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<Error>,
    {
        let guard = self.inner.lock();
        let inner: &Inner = &guard;
        let depth = inner.depth.get();
        let (begin, commit, rollback) = if depth == 0 {
            (
                "BEGIN TRANSACTION".to_string(),
                "COMMIT".to_string(),
                "ROLLBACK".to_string(),
            )
        } else {
            (
                format!("SAVEPOINT s{depth}"),
                format!("RELEASE s{depth}"),
                format!("ROLLBACK TO s{depth}; RELEASE s{depth}"),
            )
        };
        inner
            .conn
            .execute_batch(&begin)
            .map_err(|e| E::from(Error::from(e)))?;
        inner.depth.set(depth + 1);

        /// Rolls back if [f] unwinds.
        struct OnUnwind<'a> {
            inner: &'a Inner,
            depth: u32,
            rollback: &'a str,
            armed: bool,
        }
        impl Drop for OnUnwind<'_> {
            fn drop(&mut self) {
                if self.armed {
                    let _ = self.inner.conn.execute_batch(self.rollback);
                    self.inner.depth.set(self.depth);
                }
            }
        }
        let mut on_unwind = OnUnwind {
            inner,
            depth,
            rollback: &rollback,
            armed: true,
        };
        let result = f();
        on_unwind.armed = false;
        drop(on_unwind);
        inner.depth.set(depth);
        match result {
            Ok(value) => match inner.conn.execute_batch(&commit) {
                Ok(()) => Ok(value),
                Err(e) => {
                    let _ = inner.conn.execute_batch(&rollback);
                    Err(E::from(Error::from(e)))
                }
            },
            Err(e) => {
                // A failure SQLite already rolled back (a full disk, an I/O
                // error) leaves nothing to roll back: ignore that refusal.
                let _ = inner.conn.execute_batch(&rollback);
                Err(e)
            }
        }
    }

    /// Runs one statement (drift's `customStatement` / `customUpdate`).
    /// Returns the number of rows it changed.
    pub fn execute(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        self.with_conn(|c| Ok(c.prepare_cached(sql)?.execute(params)?))
    }

    /// Runs several `;`-separated statements without parameters.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.with_conn(|c| Ok(c.execute_batch(sql)?))
    }

    /// Every row [sql] returns, mapped by [map] (drift's `customSelect`).
    pub fn query<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare_cached(sql)?;
            let rows = stmt.query_map(params, map)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// The single row [sql] returns (an error when there is none).
    pub fn query_row<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<T> {
        self.with_conn(|c| Ok(c.prepare_cached(sql)?.query_row(params, map)?))
    }

    /// The first row [sql] returns, if any.
    pub fn query_opt<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Option<T>> {
        self.with_conn(|c| Ok(c.prepare_cached(sql)?.query_row(params, map).optional()?))
    }

    /// Inserts [row] (see [`rows::insert_row`]).
    pub fn insert<R: TableRow>(&self, row: &R) -> Result<()> {
        self.with_conn(|c| rows::insert_row(c, row))
    }

    /// Inserts or overwrites [row] (see [`rows::upsert_row`]).
    pub fn upsert<R: TableRow>(&self, row: &R) -> Result<()> {
        self.with_conn(|c| rows::upsert_row(c, row))
    }

    /// Overwrites the stored row with [row]'s key (see [`rows::update_row`]).
    pub fn update<R: TableRow>(&self, row: &R) -> Result<usize> {
        self.with_conn(|c| rows::update_row(c, row))
    }

    /// The rows matching [where_sql] (see [`rows::select_rows`]).
    pub fn select<R: TableRow>(
        &self,
        where_sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<R>> {
        self.with_conn(|c| rows::select_rows(c, where_sql, params))
    }

    /// Every row of [R]'s table (drift's `select(table).get()`).
    pub fn select_all<R: TableRow>(&self) -> Result<Vec<R>> {
        self.select("", [])
    }

    /// The synced row with id [id], live or tombstoned.
    pub fn find<R: SyncedRow>(&self, id: &str) -> Result<Option<R>> {
        self.with_conn(|c| rows::find_row(c, id))
    }

    /// Sets some columns of the row of [table] with id [id] (drift's
    /// `update(table)..where(id)..write(companion)`). A new `id` among
    /// [sets] gets the synced tables' length check. Returns the number of
    /// rows changed.
    pub fn update_by_id(
        &self,
        table: &str,
        id: &str,
        sets: &[(&str, &dyn rusqlite::types::ToSql)],
    ) -> Result<usize> {
        if sets.is_empty() {
            return Ok(0);
        }
        for (column, value) in sets {
            if *column == "id" && is_synced_table(table) {
                let new_id = match value.to_sql()? {
                    rusqlite::types::ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Text(t)) => {
                        String::from_utf8_lossy(t).into_owned()
                    }
                    rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Text(t)) => t,
                    _ => String::new(),
                };
                rows::check_id_length(table, &new_id)?;
            }
        }
        let assignments = sets
            .iter()
            .enumerate()
            .map(|(i, (column, _))| format!("\"{column}\" = ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE \"{table}\" SET {assignments} WHERE \"id\" = ?{}",
            sets.len() + 1
        );
        let mut params: Vec<&dyn rusqlite::types::ToSql> = sets.iter().map(|(_, v)| *v).collect();
        params.push(&id);
        self.with_conn(|c| Ok(c.prepare_cached(&sql)?.execute(&*params)?))
    }
}

/// Whether [table] is one of the synced tables (they carry the SyncCols).
pub fn is_synced_table(table: &str) -> bool {
    SYNCED_TABLES.contains(&table)
}

/// The synced tables, in drift's declaration order.
pub const SYNCED_TABLES: &[&str] = &[
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
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::rows::*;

    /// A 26-char ULID-shaped stub id for tests.
    fn id(seed: &str) -> String {
        format!("{seed:0<26}")[..26].to_string()
    }

    fn practice(seed: &str) -> PracticeRow {
        PracticeRow {
            id: id(seed),
            created_at: 1000,
            updated_at: 1000,
            hlc_wall: 1000,
            hlc_ctr: 0,
            hlc_node: id("NODE"),
            hlc_packed: format!("0000000001000-000000-{}", id("NODE")),
            ..Default::default()
        }
    }

    fn track(seed: &str, practice: &str, kind: &str, deleted_at: Option<i64>) -> TrackRow {
        TrackRow {
            id: id(seed),
            practice_id: id(practice),
            kind: kind.to_string(),
            created_at: 1000,
            updated_at: 1000,
            hlc_wall: 1000,
            hlc_ctr: 0,
            hlc_node: id("NODE"),
            hlc_packed: format!("0000000001000-000000-{}", id("NODE")),
            deleted_at,
            ..Default::default()
        }
    }

    fn is_sqlite_error(r: Result<()>) -> bool {
        matches!(r, Err(Error::Sqlite(_)))
    }

    #[test]
    fn schema_creates_and_a_practice_round_trips() {
        let db = AppDatabase::open_in_memory().unwrap();
        db.insert(&practice("P1")).unwrap();
        let rows = db.select_all::<PracticeRow>().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id("P1"));
        assert_eq!(rows[0].master_volume, 1.0); // default applied
    }

    #[test]
    fn check_rejects_an_invalid_track_kind() {
        let db = AppDatabase::open_in_memory().unwrap();
        db.insert(&practice("P1")).unwrap();
        assert!(is_sqlite_error(
            db.insert(&track("T1", "P1", "bogus", None))
        ));
    }

    #[test]
    fn foreign_key_is_enforced() {
        let db = AppDatabase::open_in_memory().unwrap();
        let clip = ClipRow {
            id: id("C1"),
            track_id: id("NOPE"),
            asset_id: id("NOPE"),
            created_at: 1000,
            updated_at: 1000,
            hlc_wall: 1000,
            hlc_ctr: 0,
            hlc_node: id("NODE"),
            hlc_packed: "x".to_string(),
            ..Default::default()
        };
        assert!(is_sqlite_error(db.insert(&clip)));
    }

    #[test]
    fn partial_unique_index_allows_one_live_accompaniment_per_practice() {
        let db = AppDatabase::open_in_memory().unwrap();
        db.insert(&practice("P1")).unwrap();
        db.insert(&track("T1", "P1", "accompaniment", None))
            .unwrap();
        // A second LIVE accompaniment for the same practice is rejected.
        assert!(is_sqlite_error(db.insert(&track(
            "T2",
            "P1",
            "accompaniment",
            None
        ))));
        // Tombstoning the live one frees the slot.
        db.update_by_id("tracks", &id("T1"), &[("deleted_at", &2000)])
            .unwrap();
        db.insert(&track("T4", "P1", "accompaniment", None))
            .unwrap();
        let live = db
            .select::<TrackRow>("kind = 'accompaniment' AND deleted_at IS NULL", [])
            .unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, id("T4"));
    }

    #[test]
    fn multiple_vocal_tracks_per_practice_are_allowed() {
        let db = AppDatabase::open_in_memory().unwrap();
        db.insert(&practice("P1")).unwrap();
        db.insert(&track("V1", "P1", "vocal", None)).unwrap();
        db.insert(&track("V2", "P1", "vocal", None)).unwrap();
        let vocals = db.select::<TrackRow>("kind = 'vocal'", []).unwrap();
        assert_eq!(vocals.len(), 2);
    }

    fn index_names(db: &AppDatabase) -> Vec<String> {
        db.query(
            "SELECT name FROM sqlite_master WHERE type = 'index' \
             AND name LIKE 'ix\\_%' ESCAPE '\\'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn derived_indexes_are_re_applied_on_every_open() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("library.db");

        let on_disk = AppDatabase::open(&file).unwrap();
        assert!(index_names(&on_disk).contains(&"ix_oplog_hlc".to_string()));
        // Simulate a library created by an OLDER build that never had it.
        on_disk.execute_batch("DROP INDEX ix_oplog_hlc").unwrap();
        on_disk.close().unwrap();

        let on_disk = AppDatabase::open(&file).unwrap();
        assert!(index_names(&on_disk).contains(&"ix_oplog_hlc".to_string()));
    }

    #[test]
    fn a_new_library_has_exactly_the_drift_schema() {
        let db = AppDatabase::open_in_memory().unwrap();
        let stored: Vec<String> = db
            .query(
                "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expected: Vec<String> = SCHEMA_V2_SQL
            .lines()
            .filter(|l| !l.starts_with("--") && !l.trim().is_empty())
            .map(|l| l.trim_end_matches(';').to_string())
            .collect();
        assert_eq!(stored, expected);
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn every_open_sets_the_per_connection_pragmas() {
        let dir = tempfile::tempdir().unwrap();
        let db = AppDatabase::open(dir.path().join("vocara.db")).unwrap();
        let pragma = |name: &str| -> String {
            db.query_row(&format!("PRAGMA {name}"), [], |r| {
                r.get::<_, rusqlite::types::Value>(0)
            })
            .map(|v| match v {
                rusqlite::types::Value::Integer(i) => i.to_string(),
                rusqlite::types::Value::Text(t) => t,
                other => format!("{other:?}"),
            })
            .unwrap()
        };
        assert_eq!(pragma("fullfsync"), "1");
        assert_eq!(pragma("foreign_keys"), "1");
        assert_eq!(pragma("journal_mode"), "wal");
        // The flag is per connection: the in-memory store wires the same
        // open.
        let memory = AppDatabase::open_in_memory().unwrap();
        let full: i64 = memory
            .query_row("PRAGMA fullfsync", [], |r| r.get(0))
            .unwrap();
        assert_eq!(full, 1);
    }

    #[test]
    fn an_in_place_upgrade_or_downgrade_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vocara.db");
        AppDatabase::open(&path).unwrap().close().unwrap();
        for version in [1, CURRENT_SCHEMA_VERSION + 1] {
            let raw = Connection::open(&path).unwrap();
            raw.pragma_update(None, "user_version", version).unwrap();
            raw.close().unwrap();
            let err = AppDatabase::open(&path).unwrap_err();
            assert!(
                matches!(&err, Error::State(m) if m.contains("onUpgrade must not run")),
                "{err}"
            );
            // Nothing was changed: the version is still the one on disk.
            let raw = Connection::open(&path).unwrap();
            let on_disk: i64 = raw
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap();
            assert_eq!(on_disk, version);
        }
    }

    #[test]
    fn ids_outside_26_to_36_characters_are_refused_on_every_write() {
        let db = AppDatabase::open_in_memory().unwrap();
        let mut short = practice("P1");
        short.id = "P".repeat(25);
        assert!(matches!(db.insert(&short), Err(Error::InvalidData(_))));
        assert!(matches!(db.upsert(&short), Err(Error::InvalidData(_))));
        assert!(matches!(db.update(&short), Err(Error::InvalidData(_))));
        let mut long = practice("P1");
        long.id = "P".repeat(37);
        assert!(matches!(db.insert(&long), Err(Error::InvalidData(_))));
        for len in [26, 36] {
            let mut ok = practice("P1");
            ok.id = "Q".repeat(len);
            db.insert(&ok).unwrap();
        }
        // Local-only tables have no such rule.
        db.insert(&SyncStateRow {
            key: "k".to_string(),
            value: "v".to_string(),
        })
        .unwrap();
        // A partial update that changes the id is checked too.
        assert!(matches!(
            db.update_by_id("practices", &"Q".repeat(26), &[("id", &"short")]),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn upsert_updates_in_place_without_deleting_the_row() {
        let db = AppDatabase::open_in_memory().unwrap();
        db.insert(&practice("P1")).unwrap();
        db.insert(&track("T1", "P1", "vocal", None)).unwrap();
        // INSERT OR REPLACE would delete the parent and trip the child's FK.
        let mut p = practice("P1");
        p.title = "renamed".to_string();
        db.upsert(&p).unwrap();
        let back: PracticeRow = db.find(&id("P1")).unwrap().unwrap();
        assert_eq!(back.title, "renamed");
        assert_eq!(
            db.find::<TrackRow>(&id("T1")).unwrap().unwrap().kind,
            "vocal"
        );
    }

    #[test]
    fn transactions_commit_together_and_roll_back_together() {
        let db = AppDatabase::open_in_memory().unwrap();
        let failed: Result<()> = db.transaction(|| {
            db.insert(&practice("P1"))?;
            db.insert(&SyncStateRow {
                key: "k".to_string(),
                value: "v".to_string(),
            })?;
            Err(Error::State("boom".to_string()))
        });
        assert!(failed.is_err());
        assert!(db.select_all::<PracticeRow>().unwrap().is_empty());
        assert!(db.select_all::<SyncStateRow>().unwrap().is_empty());
        assert!(!db.in_transaction());

        db.transaction(|| -> Result<()> {
            db.insert(&practice("P1"))?;
            // A nested transaction that fails rolls back only its own part.
            let inner: Result<()> = db.transaction(|| {
                db.insert(&practice("P2"))?;
                Err(Error::State("inner".to_string()))
            });
            assert!(inner.is_err());
            assert!(db.in_transaction());
            db.transaction(|| db.insert(&practice("P3")))
        })
        .unwrap();
        let ids: Vec<String> = db
            .select_all::<PracticeRow>()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec![id("P1"), id("P3")]);
    }

    #[test]
    fn a_panicking_transaction_rolls_back_and_leaves_the_database_usable() {
        let db = std::sync::Arc::new(AppDatabase::open_in_memory().unwrap());
        let db2 = db.clone();
        let outcome = std::thread::spawn(move || {
            let _: Result<()> = db2.transaction(|| {
                db2.insert(&practice("P1")).unwrap();
                panic!("crash mid-transaction");
            });
        })
        .join();
        assert!(outcome.is_err());
        assert!(db.select_all::<PracticeRow>().unwrap().is_empty());
        db.transaction(|| db.insert(&practice("P2"))).unwrap();
        assert_eq!(db.select_all::<PracticeRow>().unwrap().len(), 1);
    }

    #[test]
    fn another_thread_waits_for_an_open_transaction() {
        use std::sync::mpsc;
        use std::time::Duration;
        let db = std::sync::Arc::new(AppDatabase::open_in_memory().unwrap());
        let (started_tx, started_rx) = mpsc::channel();
        let writer = {
            let db = db.clone();
            std::thread::spawn(move || {
                db.transaction(|| -> Result<()> {
                    db.insert(&practice("P1"))?;
                    started_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                    db.insert(&practice("P2"))
                })
                .unwrap();
            })
        };
        started_rx.recv().unwrap();
        // Blocks until the transaction commits: never sees half of it.
        let seen = db.select_all::<PracticeRow>().unwrap().len();
        writer.join().unwrap();
        assert_eq!(seen, 2);
    }

    #[test]
    fn oplog_rows_get_their_sequence_from_sqlite() {
        let db = AppDatabase::open_in_memory().unwrap();
        let entry = OplogRow {
            entity_table: "practices".to_string(),
            entity_id: id("P1"),
            op: "insert".to_string(),
            hlc_packed: "h".to_string(),
            payload_hash: "p".to_string(),
            created_at: 1,
            ..Default::default()
        };
        let first = db.with_conn(|c| entry.insert_new(c)).unwrap();
        let second = db.with_conn(|c| entry.insert_new(c)).unwrap();
        assert_eq!((first, second), (1, 2));
        let rows = db.select_all::<OplogRow>().unwrap();
        assert_eq!(rows[1].seq, 2);
        assert!(!rows[1].synced);
    }

    #[test]
    fn every_row_struct_names_exactly_its_tables_columns() {
        let db = AppDatabase::open_in_memory().unwrap();
        fn check<R: TableRow>(db: &AppDatabase) {
            let columns: Vec<String> = db
                .query(&format!("PRAGMA table_info(\"{}\")", R::TABLE), [], |r| {
                    r.get(1)
                })
                .unwrap();
            assert_eq!(columns, R::COLUMNS, "{}", R::TABLE);
            let pk: Vec<(i64, String)> = db
                .query(&format!("PRAGMA table_info(\"{}\")", R::TABLE), [], |r| {
                    Ok((r.get(5)?, r.get(1)?))
                })
                .unwrap();
            let mut pk: Vec<_> = pk.into_iter().filter(|(k, _)| *k > 0).collect();
            pk.sort();
            let pk: Vec<String> = pk.into_iter().map(|(_, c)| c).collect();
            assert_eq!(pk, R::PRIMARY_KEY, "{}", R::TABLE);
        }
        check::<PracticeRow>(&db);
        check::<InterpretationRow>(&db);
        check::<TrackRow>(&db);
        check::<ClipRow>(&db);
        check::<PracticeNoteRow>(&db);
        check::<TechniqueMarkerRow>(&db);
        check::<FxBusRow>(&db);
        check::<TechniqueRow>(&db);
        check::<TechniqueMediaRow>(&db);
        check::<AssetRow>(&db);
        check::<OplogRow>(&db);
        check::<RecordingInflightRow>(&db);
        check::<WaveformCacheRow>(&db);
        check::<SyncStateRow>(&db);
        check::<SyncQuarantineRow>(&db);
        check::<AssetSyncRow>(&db);
        let tables: Vec<String> = db
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name <> 'sqlite_sequence'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut tables = tables;
        tables.sort();
        let mut all: Vec<&str> = ALL_TABLES.to_vec();
        all.sort();
        assert_eq!(tables, all);
    }

    #[test]
    fn defaults_match_the_ddl_defaults() {
        let db = AppDatabase::open_in_memory().unwrap();
        // Insert through raw SQL naming only the required columns: SQLite
        // fills in the DDL defaults, which must equal the struct defaults.
        db.execute(
            "INSERT INTO practices (id, created_at, updated_at, hlc_wall, hlc_ctr, \
             hlc_node, hlc_packed) VALUES (?1, 0, 0, 0, 0, '', '')",
            [id("P1")],
        )
        .unwrap();
        let stored: PracticeRow = db.find(&id("P1")).unwrap().unwrap();
        assert_eq!(
            stored,
            PracticeRow {
                id: id("P1"),
                ..Default::default()
            }
        );
        db.execute(
            "INSERT INTO assets (id, created_at, updated_at, hlc_wall, hlc_ctr, \
             hlc_node, hlc_packed, content_hash, byte_size) \
             VALUES (?1, 0, 0, 0, 0, '', '', '', 0)",
            [id("A1")],
        )
        .unwrap();
        let stored: AssetRow = db.find(&id("A1")).unwrap().unwrap();
        assert_eq!(
            stored,
            AssetRow {
                id: id("A1"),
                ..Default::default()
            }
        );
        db.execute(
            "INSERT INTO interpretations (id, created_at, updated_at, hlc_wall, \
             hlc_ctr, hlc_node, hlc_packed, practice_id) \
             VALUES (?1, 0, 0, 0, 0, '', '', ?2)",
            [id("I1"), id("P1")],
        )
        .unwrap();
        db.execute(
            "INSERT INTO tracks (id, created_at, updated_at, hlc_wall, hlc_ctr, \
             hlc_node, hlc_packed, practice_id, kind) \
             VALUES (?1, 0, 0, 0, 0, '', '', ?2, 'vocal')",
            [id("T1"), id("P1")],
        )
        .unwrap();
        let stored: TrackRow = db.find(&id("T1")).unwrap().unwrap();
        assert_eq!(
            stored,
            TrackRow {
                id: id("T1"),
                practice_id: id("P1"),
                kind: "vocal".to_string(),
                ..Default::default()
            }
        );
        db.execute(
            "INSERT INTO fx_buses (id, created_at, updated_at, hlc_wall, hlc_ctr, \
             hlc_node, hlc_packed, practice_id) VALUES (?1, 0, 0, 0, 0, '', '', ?2)",
            [id("B1"), id("P1")],
        )
        .unwrap();
        let stored: FxBusRow = db.find(&id("B1")).unwrap().unwrap();
        assert_eq!(
            stored,
            FxBusRow {
                id: id("B1"),
                practice_id: id("P1"),
                ..Default::default()
            }
        );
        db.execute(
            "INSERT INTO techniques (id, created_at, updated_at, hlc_wall, hlc_ctr, \
             hlc_node, hlc_packed) VALUES (?1, 0, 0, 0, 0, '', '')",
            [id("Q1")],
        )
        .unwrap();
        let stored: TechniqueRow = db.find(&id("Q1")).unwrap().unwrap();
        assert_eq!(
            stored,
            TechniqueRow {
                id: id("Q1"),
                ..Default::default()
            }
        );
        db.execute(
            "INSERT INTO technique_media (id, created_at, updated_at, hlc_wall, \
             hlc_ctr, hlc_node, hlc_packed, technique_id, asset_id) \
             VALUES (?1, 0, 0, 0, 0, '', '', ?2, ?3)",
            [id("M1"), id("Q1"), id("A1")],
        )
        .unwrap();
        let stored: TechniqueMediaRow = db.find(&id("M1")).unwrap().unwrap();
        assert_eq!(
            stored,
            TechniqueMediaRow {
                id: id("M1"),
                technique_id: id("Q1"),
                asset_id: id("A1"),
                ..Default::default()
            }
        );
    }
}
