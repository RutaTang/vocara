//! A simulated device for the multi-device suites: a library, its clock and
//! writer, a transport onto a shared fake cloud, and an engine — the parts
//! `Persistence.attachSync` wires in Dart.
//!
//! Persistence and the repositories are not ported yet, so local edits are
//! made the way the repository makes them at the row level: every write is
//! a row change plus its journal entry in ONE transaction, stamped through
//! `SyncWriter::stamp_for`, and a delete cascade-tombstones the practice's
//! subtree (children journaled before the practice, as `deletePractice`
//! does).

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::types::ToSql;
use vocara_store::db::rows::{
    AssetRow, ClipRow, InterpretationRow, PracticeNoteRow, PracticeRow, TrackRow,
};
use vocara_store::db::AppDatabase;
use vocara_store::error::Result;
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::oplog_drain::OplogDrain;
use vocara_store::sync::quarantine_store::QuarantineStore;
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_engine::{SyncEngine, SyncEngineOptions, SyncEngineParts};
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
}

/// The ids of one seeded aggregate: a practice with its default
/// interpretation, one vocal take (track + asset + clip) and one note.
#[derive(Debug, Clone)]
pub struct Aggregate {
    pub practice: String,
    pub interp: String,
    pub track: String,
    pub asset: String,
    pub clip: String,
    pub note: String,
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
        Device {
            db,
            now,
            writer,
            transport,
            engine: Arc::new(engine),
        }
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

    /// Sets [sets] on row [id] of [table] as a local edit (op 'update', or
    /// 'tombstone' when it sets deleted_at), stamping its SyncCols.
    pub fn edit(&self, table: &str, id: &str, sets: &[(&str, &dyn ToSql)]) -> Result<()> {
        let s = self.stamp(table, id);
        let at = s.at();
        let packed = s.hlc.packed();
        let mut all: Vec<(&str, &dyn ToSql)> = sets.to_vec();
        all.extend_from_slice(&[
            ("updated_at", &at as &dyn ToSql),
            ("hlc_wall", &s.hlc.wall_ms),
            ("hlc_ctr", &s.hlc.counter),
            ("hlc_node", &s.hlc.node),
            ("hlc_packed", &packed),
        ]);
        self.db.update_by_id(table, id, &all)?;
        let op = if sets.iter().any(|(c, _)| *c == "deleted_at") {
            "tombstone"
        } else {
            "update"
        };
        self.journal(table, id, op, &s)
    }

    /// Tombstones row [id] of [table] as a local delete.
    pub fn tombstone(&self, table: &str, id: &str) -> Result<()> {
        let at = self.now.get() + 1;
        self.edit(table, id, &[("deleted_at", &at)])
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

    /// importFile: an asset row for 8 bytes, journaled in its own
    /// transaction. Returns its id.
    pub fn import_asset(&self) -> String {
        let asset = vocara_model::util::id::new_id();
        self.tx(|| {
            let s = self.stamp("assets", &asset);
            self.db.insert(&AssetRow {
                id: asset.clone(),
                content_hash: format!("HASH_{asset}"),
                byte_size: 8,
                ext: "wav".into(),
                local_rel_path: Some(format!("assets/{asset}.wav")),
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                ..Default::default()
            })?;
            self.journal("assets", &asset, "insert", &s)
        });
        asset
    }

    /// A take (or a backing): a track of [kind] with one clip over a fresh
    /// asset, in one save. Returns (track, asset, clip).
    pub fn add_track(
        &self,
        practice: &str,
        interp: Option<&str>,
        kind: &str,
        name: &str,
    ) -> (String, String, String) {
        let asset = self.import_asset();
        let track = vocara_model::util::id::new_id();
        let clip = vocara_model::util::id::new_id();
        self.tx(|| {
            let s = self.stamp("tracks", &track);
            self.db.insert(&TrackRow {
                id: track.clone(),
                practice_id: practice.into(),
                kind: kind.into(),
                interpretation_id: interp.map(str::to_string),
                display_name: name.into(),
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                ..Default::default()
            })?;
            self.journal("tracks", &track, "insert", &s)?;
            let s = self.stamp("clips", &clip);
            self.db.insert(&ClipRow {
                id: clip.clone(),
                track_id: track.clone(),
                asset_id: asset.clone(),
                duration_ms: 1200,
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                ..Default::default()
            })?;
            self.journal("clips", &clip, "insert", &s)
        });
        (track, asset, clip)
    }

    /// A note on [track], in its own save. Returns its id.
    pub fn add_note(&self, track: &str, position_ms: i64, content: &str) -> String {
        let note = vocara_model::util::id::new_id();
        self.tx(|| {
            let s = self.stamp("practice_notes", &note);
            self.db.insert(&PracticeNoteRow {
                id: note.clone(),
                track_id: track.into(),
                position_ms,
                content: content.into(),
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                ..Default::default()
            })?;
            self.journal("practice_notes", &note, "insert", &s)
        });
        note
    }

    /// Seeds a practice: title, one vocal take with a clip, and one note.
    pub fn seed_practice(&self, title: &str) -> Aggregate {
        let (practice, interp) = self.create_practice(title);
        let (track, asset, clip) = self.add_track(&practice, Some(&interp), "vocal", "Take 1");
        let note = self.add_note(&track, 100, "original note");
        Aggregate {
            practice,
            interp,
            track,
            asset,
            clip,
            note,
        }
    }

    fn live_ids(&self, sql: &str, parent: &str) -> Vec<String> {
        self.db.query(sql, [parent], |r| r.get(0)).unwrap()
    }

    /// deletePractice: cascade-tombstones the whole live subtree in one
    /// transaction — each track's clips, notes and markers before the track,
    /// then buses, interpretations, and the practice last.
    pub fn delete_practice(&self, practice: &str) {
        self.tx(|| {
            self.db.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            for track in self.live_ids(
                "SELECT id FROM tracks WHERE practice_id = ? AND deleted_at IS NULL",
                practice,
            ) {
                for (table, sql) in [
                    (
                        "clips",
                        "SELECT id FROM clips WHERE track_id = ? AND deleted_at IS NULL",
                    ),
                    (
                        "practice_notes",
                        "SELECT id FROM practice_notes WHERE track_id = ? AND deleted_at IS NULL",
                    ),
                    (
                        "technique_markers",
                        "SELECT id FROM technique_markers WHERE track_id = ? AND deleted_at \
                         IS NULL",
                    ),
                ] {
                    for child in self.live_ids(sql, &track) {
                        self.tombstone(table, &child)?;
                    }
                }
                self.tombstone("tracks", &track)?;
            }
            for (table, sql) in [
                (
                    "fx_buses",
                    "SELECT id FROM fx_buses WHERE practice_id = ? AND deleted_at IS NULL",
                ),
                (
                    "interpretations",
                    "SELECT id FROM interpretations WHERE practice_id = ? AND deleted_at IS NULL",
                ),
            ] {
                for child in self.live_ids(sql, practice) {
                    self.tombstone(table, &child)?;
                }
            }
            self.tombstone("practices", practice)
        });
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
