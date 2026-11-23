//! The repositories' tests: ports of `client/test/`
//! `song_practice_repository_drift_test.dart`,
//! `technique_repository_drift_test.dart`,
//! `repository_parent_guards_test.dart`, `song_practice_save_base_test.dart`,
//! `song_practice_save_cost_test.dart` and `sync_hashes_test.dart`, and of the
//! repository-level cases of `reconcile_three_way_test.dart`,
//! `technique_example_identity_test.dart`,
//! `technique_media_pool_guard_test.dart`, `integrity_checker_test.dart` and
//! `asset_dedup_across_devices_test.dart`.
//!
//! A test binary of its own, apart from the asset store's: those tests read
//! the process-wide import progress, which the saves here move.

mod asset_dedup_across_devices_test;
mod integrity_checker_test;
mod reconcile_three_way_test;
mod repository_parent_guards_test;
mod song_practice_repository_drift_test;
mod song_practice_save_base_test;
mod song_practice_save_cost_test;
mod sync_hashes_test;
mod technique_example_identity_test;
mod technique_media_pool_guard_test;
mod technique_repository_drift_test;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::Connection;
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_store::db::rows::{OplogRow, TrackRow};
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::repositories::{
    DebugLoadHook, SongPracticeRepository, SongPracticeRepositoryDrift, TechniqueRepository,
    TechniqueRepositoryDrift,
};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_hashes::Loaded;
use vocara_store::sync::sync_writer::SyncWriter;
use vocara_store::Error;

/// `'NODE'.padRight(26, '0')`.
pub fn node() -> String {
    format!("{:0<26}", "NODE")
}

/// `seed.padRight(26, '0')`.
pub fn pad_id(seed: &str) -> String {
    format!("{seed:0<26}")
}

/// Dart's `DateTime(2026)`.
pub fn d2026() -> DateTime {
    DateTime::local_ymd(2026, 1, 1)
}

/// `AudioTrack(id: newId(), kind: kind, displayName: name, createdAt:
/// DateTime(2026))`.
pub fn lane(kind: AudioTrackKind, name: &str) -> AudioTrack {
    AudioTrack::new(new_id(), kind, name, d2026())
}

/// A vocal take in the interpretation [version].
pub fn take(name: &str, version: Option<&str>) -> AudioTrack {
    AudioTrack {
        version_id: version.map(str::to_string),
        ..lane(AudioTrackKind::Vocal, name)
    }
}

/// A point note at 0 ms.
pub fn note(content: &str) -> PracticeNote {
    PracticeNote {
        id: new_id(),
        position_ms: 0,
        end_ms: None,
        content: content.to_string(),
        created_at: d2026(),
        updated_at: d2026(),
    }
}

/// A point marker of a fresh technique id.
pub fn marker(position_ms: i64, technique_name: &str) -> TechniqueMarker {
    TechniqueMarker {
        id: new_id(),
        position_ms,
        end_ms: None,
        technique_id: new_id(),
        technique_name: technique_name.to_string(),
        created_at: d2026(),
        updated_at: d2026(),
    }
}

/// Every lane of a practice behind its `Arc`.
pub fn arcs<T>(items: Vec<T>) -> Vec<Arc<T>> {
    items.into_iter().map(Arc::new).collect()
}

/// `practice.copyWith(vocalTracks: tracks)`.
pub fn with_takes(p: &SongPractice, tracks: Vec<AudioTrack>) -> SongPractice {
    SongPractice {
        vocal_tracks: arcs(tracks),
        ..p.clone()
    }
}

/// A load seam that fails the next load once armed (Dart's `_FlakyReread`
/// with `failNextLoad`).
#[derive(Clone, Default)]
pub struct FailNextLoad(Arc<AtomicBool>);

impl FailNextLoad {
    pub fn arm(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn hook(&self) -> DebugLoadHook {
        let armed = self.0.clone();
        Arc::new(move |_id: &str| {
            if armed.swap(false, Ordering::SeqCst) {
                Err(Error::State("re-read failed".to_string()))
            } else {
                Ok(())
            }
        })
    }
}

/// Records the text of every statement the connection runs (Dart's query
/// interceptors).
#[derive(Clone, Default)]
pub struct Statements(Arc<Mutex<Vec<String>>>);

impl Statements {
    /// An in-memory library whose statements this records.
    pub fn database(&self) -> AppDatabase {
        let mut conn = Connection::open_in_memory().unwrap();
        let sql = self.0.clone();
        conn.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT,
            Some(move |e: TraceEvent<'_>| {
                if let TraceEvent::Stmt(_, text) = e {
                    sql.lock().unwrap().push(text.to_string());
                }
            }),
        )
        .unwrap();
        AppDatabase::from_connection(conn).unwrap()
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }

    pub fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    /// How many statements ran.
    pub fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// How many statements contain [fragment].
    pub fn matching(&self, fragment: &str) -> usize {
        self.all().iter().filter(|s| s.contains(fragment)).count()
    }
}

/// One test's library: an in-memory database, a temporary pool root, and
/// both repositories over them (sharing one writer and one asset store).
pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub db: Arc<AppDatabase>,
    pub writer: Arc<SyncWriter>,
    pub assets: Arc<AssetStore>,
    pub songs: SongPracticeRepositoryDrift,
    pub techs: TechniqueRepositoryDrift,
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
        let assets = Arc::new(AssetStore::new(
            db.clone(),
            tmp.path().to_path_buf(),
            writer.clone(),
        ));
        let songs = SongPracticeRepositoryDrift::new(db.clone(), writer.clone(), assets.clone());
        let techs = TechniqueRepositoryDrift::new(db.clone(), writer.clone(), assets.clone());
        Fixture {
            tmp,
            db,
            writer,
            assets,
            songs,
            techs,
        }
    }

    /// Another song repository over the same library, with a writer and an
    /// asset store of its own (as the Dart tests built `_FlakyReread`).
    pub fn other_songs(&self) -> SongPracticeRepositoryDrift {
        let writer = Arc::new(SyncWriter::new(
            self.db.clone(),
            Arc::new(HlcClock::new(node())),
        ));
        let assets = Arc::new(AssetStore::new(
            self.db.clone(),
            self.root().to_path_buf(),
            writer.clone(),
        ));
        SongPracticeRepositoryDrift::new(self.db.clone(), writer, assets)
    }

    /// Another technique repository over the same library (see
    /// [`Fixture::other_songs`]).
    pub fn other_techs(&self) -> TechniqueRepositoryDrift {
        let writer = Arc::new(SyncWriter::new(
            self.db.clone(),
            Arc::new(HlcClock::new(node())),
        ));
        let assets = Arc::new(AssetStore::new(
            self.db.clone(),
            self.root().to_path_buf(),
            writer.clone(),
        ));
        TechniqueRepositoryDrift::new(self.db.clone(), writer, assets)
    }

    pub fn root(&self) -> &std::path::Path {
        self.tmp.path()
    }

    /// Writes [bytes] to `root/<name>` and returns its path.
    pub fn file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.root().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A 5 s clip at 0 over a fresh file `root/<name>` holding [bytes].
    pub fn clip(&self, name: &str, bytes: &[u8]) -> AudioClip {
        AudioClip::new(new_id(), self.file(name, bytes), 5000)
    }

    /// [Fixture::clip] carrying [peaks].
    pub fn clip_with_peaks(&self, name: &str, bytes: &[u8], peaks: &[f64]) -> AudioClip {
        AudioClip {
            waveform_peaks: peaks.to_vec(),
            ..self.clip(name, bytes)
        }
    }

    pub fn create(&self, title: &str) -> SongPractice {
        self.songs.create_practice(title).unwrap()
    }

    pub fn save(&self, p: &SongPractice) -> SongPractice {
        self.songs.save_practice(p).unwrap()
    }

    pub fn load(&self, id: &str) -> Loaded<SongPractice> {
        self.songs.load_practice(id).unwrap()
    }

    pub fn save_with_base(
        &self,
        p: &SongPractice,
        base: &IndexMap<String, String>,
    ) -> vocara_store::Result<Loaded<SongPractice>> {
        self.songs.save_practice_with_base(p, base)
    }

    pub fn save_technique(&self, t: &Technique) -> Technique {
        self.techs.save_technique(t).unwrap()
    }

    pub fn oplog(&self) -> Vec<OplogRow> {
        self.db.select_all::<OplogRow>().unwrap()
    }

    /// `'<table>:<op>'` of every oplog entry, in order.
    pub fn ops(&self) -> Vec<String> {
        self.oplog()
            .into_iter()
            .map(|o| format!("{}:{}", o.entity_table, o.op))
            .collect()
    }

    /// How many oplog entries journal the row [id] of [table].
    pub fn ops_for(&self, table: &str, id: &str) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM oplog WHERE entity_table = ? AND entity_id = ?",
            [table, id],
        )
    }

    /// The live rows of [table].
    pub fn live_count(&self, table: &str) -> i64 {
        self.count(
            &format!("SELECT COUNT(*) FROM {table} WHERE deleted_at IS NULL"),
            [],
        )
    }

    /// The single integer [sql] returns.
    pub fn count(&self, sql: &str, params: impl rusqlite::Params) -> i64 {
        self.db.query_row(sql, params, |r| r.get(0)).unwrap()
    }

    /// The [column] of the row [id] of [table].
    pub fn column<T: rusqlite::types::FromSql>(&self, table: &str, column: &str, id: &str) -> T {
        self.db
            .query_row(
                &format!("SELECT {column} FROM {table} WHERE id = ?"),
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Runs one raw statement (Dart's `customStatement`).
    pub fn raw(&self, sql: &str, params: impl rusqlite::Params) {
        self.db.execute(sql, params).unwrap();
    }

    pub fn track_row(&self, id: &str) -> TrackRow {
        self.db.find::<TrackRow>(id).unwrap().unwrap()
    }
}
