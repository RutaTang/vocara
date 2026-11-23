//! A simulated device for the multi-device suites: a library, its clock and
//! writer, its asset pool and song repository, a transport onto a shared
//! fake cloud, and an engine — the parts `Persistence` and
//! `Persistence.attachSync` wire in Dart. (`vocara_services::persistence`
//! wires the same parts, but takes no clock; these devices run on a
//! settable one, so they are wired here.)
//!
//! Local edits go through the repository ([`Device::repo`]), as the Dart
//! suites made them. One row-level write is left, [`Device::create_practice`]
//! — a practice and its interpretation, each a row change plus its journal
//! entry stamped through `SyncWriter::stamp_for`, in ONE transaction, as the
//! repository's `createPractice` writes them — for the boot-safety case
//! that only needs some save to land while a pull run is paused.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::TempDir;
use vocara_model::models::song_practice::SongPractice;
use vocara_store::db::rows::{InterpretationRow, PracticeRow};
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::error::Result;
use vocara_store::repositories::{SongPracticeRepository, SongPracticeRepositoryDrift};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::oplog_drain::OplogDrain;
use vocara_store::sync::quarantine_store::QuarantineStore;
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_engine::{SyncEngine, SyncEngineOptions, SyncEngineParts};
use vocara_store::sync::sync_hashes::Loaded;
use vocara_store::sync::sync_row::{synced_table_registry, SyncRowBuilder};
use vocara_store::sync::sync_state_store::SyncStateStore;
use vocara_store::sync::sync_transport::{FakeCloud, FakeSyncTransport, SyncTransport};
use vocara_store::sync::sync_writer::{SyncStamp, SyncWriter};

use crate::support::*;

pub struct Device {
    pub db: Arc<AppDatabase>,
    pub now: Now,
    pub writer: Arc<SyncWriter>,
    pub transport: Arc<FakeSyncTransport>,
    pub engine: Arc<SyncEngine>,
    /// The song repository over this library and its pool.
    pub songs: SongPracticeRepositoryDrift,
    /// The asset pool's root, then a scratch directory for source files
    /// (both removed with the device).
    _pool: TempDir,
    scratch: TempDir,
}

impl Device {
    /// A device named [name] (its node id is `id(name)`) whose wall clock
    /// starts at [start], syncing through [cloud].
    pub fn new(cloud: &Arc<FakeCloud>, name: &str, start: i64) -> Device {
        let transport = Arc::new(FakeSyncTransport::new(cloud.clone(), name));
        Self::with_transport(transport.clone(), transport, name, start)
    }

    /// A device whose engine talks to [wire] (a wrapper around [transport]).
    pub fn with_transport(
        transport: Arc<FakeSyncTransport>,
        wire: Arc<dyn SyncTransport>,
        name: &str,
        start: i64,
    ) -> Device {
        let db = memory_db();
        let now = Now::new(start);
        let clock = Arc::new(HlcClock::new(id(name)).with_now(now.reader()));
        let writer = Arc::new(SyncWriter::new(db.clone(), clock.clone()));
        // One builder shared by the engine (push projection) and the applier
        // (the FK sweep's park-instead-of-lose snapshot), as attachSync wires
        // it.
        let builder = Arc::new(SyncRowBuilder::new(db.clone()));
        let engine = SyncEngine::new(
            SyncEngineParts {
                db: db.clone(),
                applier: Arc::new(SyncApplier::new(
                    db.clone(),
                    clock,
                    builder.clone(),
                    Some(QuarantineStore::new(db.clone())),
                )),
                drain: OplogDrain::new(db.clone()),
                builder,
                state: SyncStateStore::new(db.clone()),
                integrity: Arc::new(IntegrityChecker::new(db.clone(), writer.clone())),
                transport: wire,
            },
            SyncEngineOptions::default(),
        );
        let pool = tempfile::Builder::new()
            .prefix("vocara_sync_pool")
            .tempdir()
            .unwrap();
        let scratch = tempfile::Builder::new()
            .prefix("vocara_sync_src")
            .tempdir()
            .unwrap();
        let assets = Arc::new(AssetStore::new(
            db.clone(),
            pool.path().to_path_buf(),
            writer.clone(),
        ));
        Device {
            songs: SongPracticeRepositoryDrift::new(db.clone(), writer.clone(), assets),
            db,
            now,
            writer,
            transport,
            engine: Arc::new(engine),
            _pool: pool,
            scratch,
        }
    }

    /// The song repository (Dart's `songRepo`).
    pub fn repo(&self) -> &dyn SongPracticeRepository {
        &self.songs
    }

    /// Writes [bytes] to a fresh source file (`<prefix>_<id>.wav`) outside
    /// the pool, as a drag-and-drop source, and returns its path.
    pub fn source_file(&self, prefix: &str, bytes: &[u8]) -> String {
        let f = self
            .scratch
            .path()
            .join(format!("{prefix}_{}.wav", vocara_model::util::id::new_id()));
        std::fs::write(&f, bytes).unwrap();
        f.to_string_lossy().into_owned()
    }

    /// `loadPractice` (Dart's `loadOn`).
    pub fn load(&self, practice: &str) -> Loaded<SongPractice> {
        self.repo().load_practice(practice).unwrap()
    }

    /// `savePracticeWithBase` over a load's base hashes.
    pub fn save_with_base(&self, next: &SongPractice, base: &Loaded<SongPractice>) {
        self.repo()
            .save_practice_with_base(next, &base.base_hashes)
            .unwrap();
    }

    pub fn checker(&self) -> IntegrityChecker {
        IntegrityChecker::new(self.db.clone(), self.writer.clone())
    }

    /// The stamp of one local write over row [id] of [table] (the clock
    /// moves on a millisecond per write, like real time between edits).
    pub fn stamp(&self, table: &str, id: &str) -> SyncStamp {
        self.now.add(1);
        self.writer.stamp_for(table, id).unwrap()
    }

    /// Runs [f] in one transaction (a repository save).
    pub fn tx(&self, f: impl FnOnce() -> Result<()>) {
        self.db.transaction(f).unwrap();
    }

    /// Journals a write of row [id] of [table] stamped [s].
    pub fn journal(&self, table: &str, id: &str, op: &str, s: &SyncStamp) -> Result<()> {
        self.writer
            .journal(table, id, op, &s.hlc, &format!("h:{table}/{id}"), None)
    }

    /// createPractice: the practice and its default interpretation, in one
    /// transaction. Returns (practice id, interpretation id).
    pub fn create_practice(&self, title: &str) -> (String, String) {
        let practice = vocara_model::util::id::new_id();
        let interp = vocara_model::util::id::new_id();
        self.tx(|| {
            let s = self.stamp("practices", &practice);
            self.db.insert(&PracticeRow {
                id: practice.clone(),
                title: title.into(),
                active_interpretation_id: Some(interp.clone()),
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                ..Default::default()
            })?;
            self.journal("practices", &practice, "insert", &s)?;
            let s = self.stamp("interpretations", &interp);
            self.db.insert(&InterpretationRow {
                id: interp.clone(),
                practice_id: practice.clone(),
                name: "Take 1".into(),
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                ..Default::default()
            })?;
            self.journal("interpretations", &interp, "insert", &s)
        });
        (practice, interp)
    }

    pub fn oplog_count(&self) -> i64 {
        oplog_count(&self.db)
    }

    /// Every synced row: "table/id" -> "hlcPacked|deletedAt". Two converged
    /// devices produce IDENTICAL snapshots (entity-level LWW means equal
    /// winning HLC implies equal row content).
    pub fn snapshot(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for table in synced_table_registry().keys() {
            for (row_id, packed, deleted) in self.row_states(table) {
                out.insert(
                    format!("{table}/{row_id}"),
                    format!(
                        "{packed}|{}",
                        deleted.map_or("null".into(), |d| d.to_string())
                    ),
                );
            }
        }
        out
    }

    /// (id, hlc_packed, deleted_at) of every row of [table].
    pub fn row_states(&self, table: &str) -> Vec<(String, String, Option<i64>)> {
        self.db
            .query(
                &format!("SELECT id, hlc_packed, deleted_at FROM {table} ORDER BY id"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    /// The wire text of row [id] of [table] (None when absent): the
    /// row-content fingerprint, per-device bookkeeping excluded.
    pub fn wire(&self, table: &str, row_id: &str) -> Option<String> {
        SyncRowBuilder::new(self.db.clone())
            .build(table, row_id)
            .unwrap()
            .map(|r| r.encode().unwrap())
    }
}
