//! The asset pool's tests: ports of `client/test/asset_store_test.dart`,
//! `asset_gc_test.dart`, `asset_dedup_across_devices_test.dart`,
//! `asset_import_progress_test.dart` and the asset-GC case of
//! `database_durability_test.dart`.
//!
//! A test binary of its own: the import progress reading is process-wide
//! (Dart's statics), so only this file's tests may import while one of them
//! reads it. Rust runs tests on parallel threads where Dart ran a file's
//! tests one after another: every test that imports holds [`shared`], and a
//! test that reads the progress holds [`alone`].

mod asset_dedup_across_devices_test;
mod asset_gc_test;
mod asset_import_progress_test;
mod asset_store_test;
mod database_durability_test;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use vocara_store::db::durable_fs::{self, DurableFs};
use vocara_store::db::rows::{AssetRow, OplogRow};
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_writer::SyncWriter;

static PROCESS: RwLock<()> = RwLock::new(());

/// Held by every test that imports: they may run together.
pub fn shared() -> RwLockReadGuard<'static, ()> {
    PROCESS.read().unwrap_or_else(|e| e.into_inner())
}

/// Held by a test that reads the process-wide import progress: no other
/// import runs meanwhile.
pub fn alone() -> RwLockWriteGuard<'static, ()> {
    PROCESS.write().unwrap_or_else(|e| e.into_inner())
}

/// `'NODE'.padRight(26, '0')`.
pub fn node() -> String {
    format!("{:0<26}", "NODE")
}

/// `seed.padRight(26, '0')`.
pub fn pad_id(seed: &str) -> String {
    format!("{seed:0<26}")
}

/// One test's library: an in-memory database and a temporary pool root.
pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub db: Arc<AppDatabase>,
    pub writer: Arc<SyncWriter>,
    pub store: Arc<AssetStore>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_db(AppDatabase::open_in_memory().unwrap())
    }

    /// A fixture over [db] (a connection the test set up itself).
    pub fn with_db(db: AppDatabase) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(db);
        let writer = Arc::new(SyncWriter::new(db.clone(), Arc::new(HlcClock::new(node()))));
        let store = Arc::new(AssetStore::new(
            db.clone(),
            tmp.path().to_path_buf(),
            writer.clone(),
        ));
        Fixture {
            tmp,
            db,
            writer,
            store,
        }
    }

    /// Another store over the same database and root, on [fs], with a clock
    /// of its own (as the Dart tests built one).
    pub fn store_with(&self, fs: Arc<dyn DurableFs>) -> AssetStore {
        let writer = Arc::new(SyncWriter::new(
            self.db.clone(),
            Arc::new(HlcClock::new(node())),
        ));
        AssetStore::new(self.db.clone(), self.root().to_path_buf(), writer).with_fs(fs)
    }

    pub fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// Writes [bytes] to `root/<name>` and returns its path.
    pub fn make_src(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.root().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// `importFile(path)` (kind 'audio').
    pub fn import(&self, path: &str) -> String {
        self.store
            .import_file(path, AssetStore::DEFAULT_KIND)
            .unwrap()
    }

    pub fn resolve(&self, id: &str) -> Option<String> {
        self.store.resolve(id).unwrap()
    }

    pub fn asset(&self, id: &str) -> AssetRow {
        self.db.find::<AssetRow>(id).unwrap().unwrap()
    }

    pub fn assets(&self) -> Vec<AssetRow> {
        self.db.select_all::<AssetRow>().unwrap()
    }

    pub fn live_assets(&self) -> Vec<AssetRow> {
        self.db
            .select::<AssetRow>("deleted_at IS NULL", [])
            .unwrap()
    }

    pub fn oplog(&self) -> Vec<OplogRow> {
        self.db.select_all::<OplogRow>().unwrap()
    }

    /// Marks [id] tombstoned with a raw write (`deleted_at = 1`).
    pub fn tombstone(&self, id: &str) {
        self.db
            .execute("UPDATE assets SET deleted_at = 1 WHERE id = ?", [id])
            .unwrap();
    }

    /// Ages [id] past every grace window.
    pub fn age(&self, id: &str) {
        self.db
            .execute(
                "UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?",
                [id],
            )
            .unwrap();
    }
}

/// Every regular file under [dir], recursively.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// The names in [dir] (not recursive), sorted.
pub fn names_in(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Sets [path]'s modification time [offset_ms] from now.
pub fn set_mtime_from_now(path: &Path, offset_ms: i64) {
    let now = std::time::SystemTime::now();
    let delta = std::time::Duration::from_millis(offset_ms.unsigned_abs());
    let t = if offset_ms < 0 {
        now - delta
    } else {
        now + delta
    };
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Records every directory flush (the real flush still runs underneath).
#[derive(Default)]
pub struct DirFlushLog {
    pub dirs: Mutex<Vec<PathBuf>>,
}

impl DurableFs for DirFlushLog {
    fn fsync_dir(&self, path: &Path) {
        self.dirs.lock().unwrap().push(path.to_path_buf());
        durable_fs::fsync_dir(path);
    }
}
