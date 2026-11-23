//! Helpers shared by the sync suites.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::types::FromSql;
use rusqlite::Connection;
use vocara_store::db::AppDatabase;
use vocara_store::sync::hlc::Hlc;
use vocara_store::sync::sync_row::{FieldValue, Fields, SyncRow};

/// A 26-char ULID-shaped stub id: [s] padded with '0' (Dart's
/// `s.padRight(26, '0').substring(0, 26)`).
pub fn id(s: &str) -> String {
    format!("{s:0<26}")[..26].to_string()
}

/// Builds a [`Fields`] map in the order given (a Dart map literal).
macro_rules! fields {
    ($($k:expr => $v:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut f = vocara_store::sync::sync_row::Fields::new();
        $(f.insert(($k).to_string(), vocara_store::sync::sync_row::FieldValue::from($v));)*
        f
    }};
}

/// SQL NULL.
pub const NULL: FieldValue = FieldValue::Null;

/// `{...row.fields, key: value}`: [row] with [key] set (in place when it
/// exists, appended otherwise — Dart's spread-then-override).
pub fn with(row: &SyncRow, overrides: &[(&str, FieldValue)]) -> SyncRow {
    let mut f = row.fields().clone();
    for (k, v) in overrides {
        f.insert((*k).to_string(), v.clone());
    }
    SyncRow::with_field_clocks(row.table(), f, row.field_clocks().cloned())
}

/// [row]'s fields with [key] removed (order kept).
pub fn without(row: &SyncRow, key: &str) -> SyncRow {
    let mut f = row.fields().clone();
    f.shift_remove(key);
    SyncRow::new(row.table(), f)
}

/// The SyncCols of a row stamped [h] (created/updated at its wall).
pub fn sync_cols(h: &Hlc, deleted_at: Option<i64>) -> Fields {
    fields! {
        "created_at" => h.wall_ms,
        "updated_at" => h.wall_ms,
        "deleted_at" => deleted_at,
        "hlc_wall" => h.wall_ms,
        "hlc_ctr" => h.counter,
        "hlc_node" => h.node.clone(),
        "hlc_packed" => h.packed(),
        "schema_v" => 1i64,
    }
}

/// [a] followed by [b] (a map literal with a spread).
pub fn merged(mut a: Fields, b: Fields) -> Fields {
    for (k, v) in b {
        a.insert(k, v);
    }
    a
}

/// A settable wall clock for an `HlcClock`.
#[derive(Clone)]
pub struct Now(pub Arc<AtomicI64>);

impl Now {
    pub fn new(at: i64) -> Self {
        Now(Arc::new(AtomicI64::new(at)))
    }
    pub fn get(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    pub fn set(&self, at: i64) {
        self.0.store(at, Ordering::SeqCst);
    }
    pub fn add(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
    pub fn reader(&self) -> impl Fn() -> i64 + Send + Sync + 'static {
        let at = self.0.clone();
        move || at.load(Ordering::SeqCst)
    }
}

/// Every statement a database runs (drift's QueryInterceptor in the Dart
/// tests): the SQL text of each statement as it starts.
#[derive(Clone, Default)]
pub struct Statements(pub Arc<Mutex<Vec<String>>>);

impl Statements {
    pub fn all(&self) -> Vec<String> {
        self.0.lock().clone()
    }
    pub fn reset(&self) {
        self.0.lock().clear();
    }
    pub fn matching(&self, fragment: &str) -> usize {
        self.0
            .lock()
            .iter()
            .filter(|s| s.contains(fragment))
            .count()
    }
    /// Transactions opened (BEGIN or SAVEPOINT).
    pub fn transactions(&self) -> usize {
        self.0
            .lock()
            .iter()
            .filter(|s| {
                let s = s.trim_start().to_ascii_uppercase();
                s.starts_with("BEGIN") || s.starts_with("SAVEPOINT")
            })
            .count()
    }
    /// The writing statements (INSERT / UPDATE / DELETE / a PRAGMA that
    /// sets something / DDL).
    pub fn writes(&self) -> Vec<String> {
        self.0
            .lock()
            .iter()
            .filter(|s| {
                let u = s.trim_start().to_ascii_uppercase();
                [
                    "INSERT", "UPDATE", "DELETE", "REPLACE", "CREATE", "DROP", "ALTER",
                ]
                .iter()
                .any(|p| u.starts_with(p))
                    || (u.starts_with("PRAGMA") && u.contains('='))
            })
            .cloned()
            .collect()
    }
}

/// An in-memory library whose every statement is recorded.
pub fn traced_db() -> (Arc<AppDatabase>, Statements) {
    let statements = Statements::default();
    let sink = statements.0.clone();
    let mut conn = Connection::open_in_memory().unwrap();
    conn.trace_v2(
        TraceEventCodes::SQLITE_TRACE_STMT,
        Some(move |event: TraceEvent<'_>| {
            if let TraceEvent::Stmt(_, sql) = event {
                sink.lock().push(sql.to_string());
            }
        }),
    )
    .unwrap();
    (
        Arc::new(AppDatabase::from_connection(conn).unwrap()),
        statements,
    )
}

/// A fresh in-memory library.
pub fn memory_db() -> Arc<AppDatabase> {
    Arc::new(AppDatabase::open_in_memory().unwrap())
}

/// One column of row [id] of [table]: None when the row does not exist
/// (use `Option<T>` as [T] for a nullable column).
pub fn col<T: FromSql>(db: &AppDatabase, table: &str, id: &str, column: &str) -> Option<T> {
    db.query_opt(
        &format!("SELECT {column} FROM {table} WHERE id = ?"),
        [id],
        |r| r.get::<_, T>(0),
    )
    .unwrap()
}

/// Whether row [id] of [table] exists.
pub fn exists(db: &AppDatabase, table: &str, id: &str) -> bool {
    db.query_opt(&format!("SELECT 1 FROM {table} WHERE id = ?"), [id], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap()
    .is_some()
}

/// `SELECT COUNT(*)` of [sql] (a FROM/WHERE tail) with string args.
pub fn count(db: &AppDatabase, sql: &str, args: &[&str]) -> i64 {
    db.query_row(sql, rusqlite::params_from_iter(args.iter()), |r| r.get(0))
        .unwrap()
}

/// How many oplog entries the library holds.
pub fn oplog_count(db: &AppDatabase) -> i64 {
    count(db, "SELECT COUNT(*) FROM oplog", &[])
}

/// Spins (up to 5 s) until [cond] holds.
pub fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
