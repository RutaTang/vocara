//! The content-addressed asset pool (port of `lib/data/db/asset_store.dart`).
//!
//! Dart awaited file and database work; here every call blocks its thread
//! (the store runs off the UI thread, see PORTING.md). Two guarantees Dart
//! got from running on one isolate are kept explicitly:
//!
//! * the per-pool-file turn ([`AssetStore`]'s turn queue): imports of one
//!   pool file take turns, first come first served, behind a mutex and a
//!   condition variable instead of a chain of completers;
//! * the import progress reading ([`AssetStore::bytes_being_copied`]): the
//!   process-wide list of imports in flight and the banked total sit behind
//!   one mutex, so a finished copy moves its bytes from the live file length
//!   into the bank in one step, exactly as Dart's synchronous section did.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::types::Value as SqlValue;
use sha2::{Digest, Sha256};
use vocara_model::util::dart_path as p;
use vocara_model::util::id::new_id;

use crate::dart;
use crate::db::durable_fs::{self, DurableFs, SystemDurableFs};
use crate::db::rows::AssetRow;
use crate::db::write_gate::WriteGate;
use crate::db::AppDatabase;
use crate::error::{Error, Result};
use crate::sync::quarantine_store::QuarantineStore;
use crate::sync::sync_writer::{op, SyncWriter};

/// One (table, column) that can reference an asset id (see
/// [`AssetStore::ASSET_REF_COLUMNS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetRefColumn {
    /// The table.
    pub table: &'static str,
    /// The column holding an asset id.
    pub column: &'static str,
}

/// A test seam run with no argument (Dart's `Future<void> Function()?`).
pub type DebugHook = Arc<dyn Fn() + Send + Sync>;

/// A test seam run with a pool-relative path (Dart's
/// `Future<void> Function(String rel)?`).
pub type DebugRelHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Owns immutable audio/media files and the `assets` indirection table.
///
/// Files are content-addressed: one file per sha256 under
/// `assets/<h0:2>/<h2:4>/<hash>.<ext>`, so a name can never lie about its
/// contents and identical bytes are stored once. The DB never holds a path —
/// rows reference an asset id; [`AssetStore::resolve`] maps that to a live
/// absolute path on this device. The physical location is a per-device
/// detail, so the same row resolves correctly after a reinstall, machine
/// move, or cloud restore.
pub struct AssetStore {
    db: Arc<AppDatabase>,

    /// Root directory under which the content-addressed pool lives.
    root: PathBuf,

    /// [`AssetStore::root`] as the string Dart's `root.path` was: every path
    /// this store hands out (or compares) is built from it with
    /// `package:path` rules.
    root_str: String,

    writer: Arc<SyncWriter>,
    fs: Arc<dyn DurableFs>,

    /// The app-wide store write gate (see [`WriteGate`]): saves/imports run
    /// under its shared side, repairLibrary and the boot sweeps take it
    /// exclusively so a GC pass can never race an import+commit sequence.
    /// Owned here because every writer already holds an [`AssetStore`];
    /// Persistence exposes it too.
    gate: Arc<WriteGate>,

    /// The import holding each pool file's turn (see [`AssetStore::in_turn`]).
    turns: Turns,

    /// Runs between an import's commit and its move of a duplicate copy to
    /// the trash (see [`AssetStore::import_file`]): where another import
    /// could claim that copy. Test-only.
    debug_before_trashing_duplicate: Mutex<Option<DebugHook>>,

    /// Runs between an import's promote and its row work (see
    /// [`AssetStore::import_file`]). Test-only.
    debug_after_promote: Mutex<Option<DebugRelHook>>,
}

impl std::fmt::Debug for AssetStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// The outcome of [`AssetStore::stage_to_pool`]: hash and byte count
/// measured over exactly the bytes staged at `tmp_path` — one pass, one
/// truth.
#[derive(Debug)]
struct StagedImport {
    hash: String,
    size: i64,
    tmp_path: PathBuf,
}

/// The per-pool-file turn queues (Dart's `_turns` map of completers): each
/// key hands out tickets in call order and serves them one at a time.
#[derive(Default)]
struct Turns {
    queues: Mutex<HashMap<String, TurnQueue>>,
    changed: Condvar,
}

#[derive(Default)]
struct TurnQueue {
    /// The ticket the next caller takes.
    next: u64,
    /// The ticket allowed to run now.
    serving: u64,
}

// --- Import progress (process-wide, like Dart's statics) ------------------

/// The imports in flight in every store, and the settled part of the
/// reading (see [`AssetStore::bytes_being_copied`]).
struct Progress {
    /// The settled part of the [`AssetStore::bytes_being_copied`] reading:
    /// the bytes of every finished copy plus one for every finished landing
    /// step. Only grows.
    banked: i64,
    /// The token the next import takes.
    next_token: u64,
    /// The imports in flight in every store, from the start of an import
    /// until it returns. Each maps to the staging file its copy is writing
    /// right now, or `None` outside the copy (before it, and while the copied
    /// file is flushed, placed and its row committed).
    in_flight: BTreeMap<u64, Option<PathBuf>>,
}

static PROGRESS: Mutex<Progress> = Mutex::new(Progress {
    banked: 0,
    next_token: 0,
    in_flight: BTreeMap::new(),
});

fn progress() -> MutexGuard<'static, Progress> {
    // Nothing that can panic runs under this lock: a poisoned lock only
    // means a panic elsewhere, and the counters are still consistent.
    PROGRESS.lock().unwrap_or_else(|e| e.into_inner())
}

/// One step of landing an import finished (a flush, the rename into the
/// pool, the row commit): the reading moves.
fn step_landed() {
    progress().banked += 1;
}

/// One import's place in the in-flight list, from the start of
/// [`AssetStore::import_file`] until it returns, landed or failed (dropped).
struct InFlight {
    token: u64,
}

impl InFlight {
    fn list() -> Self {
        let mut state = progress();
        let token = state.next_token;
        state.next_token += 1;
        state.in_flight.insert(token, None);
        InFlight { token }
    }

    /// The staging file this import's copy is writing right now (`None`
    /// outside the copy).
    fn copying(&self, path: Option<PathBuf>) {
        if let Some(slot) = progress().in_flight.get_mut(&self.token) {
            *slot = path;
        }
    }

    /// The finished copy's bytes move from the live file length into the
    /// banked total in one step, so the reading neither drops nor
    /// double-counts them.
    fn bank_copy(&self, size: i64) {
        let mut state = progress();
        state.banked += size;
        if let Some(slot) = state.in_flight.get_mut(&self.token) {
            *slot = None;
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        progress().in_flight.remove(&self.token);
    }
}

// --- The store -------------------------------------------------------------

/// The SQL that tombstones one sweep candidate, re-verifying every guard the
/// candidate scan read (see [`AssetStore::sweep_unreferenced`]).
const TOMBSTONE_SQL: &str = "UPDATE assets SET deleted_at = ?, updated_at = ?, hlc_wall = ?, \
     hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ? \
     AND deleted_at IS NULL \
     AND record_sync_state = 'localOnly' \
     AND bytes_sync_state = 'localOnly' \
     AND (NOT EXISTS (SELECT 1 FROM sync_state \
     WHERE \"key\" = 'transport_seen') \
     OR NOT EXISTS (SELECT 1 FROM oplog \
     WHERE entity_table = 'assets' AND entity_id = assets.id))";

/// The local `sync_state` key recording when
/// [`AssetStore::sweep_orphan_pool_files`]' fuse first tripped (epoch ms).
const POOL_FUSE_KEY: &str = "pool_fuse_at";

impl AssetStore {
    /// Bound on the ids/paths one `IN (...)` query binds, far below
    /// SQLite's host-parameter ceiling; set reads go through
    /// [`AssetStore::in_chunks`].
    pub const IN_QUERY_CHUNK: usize = 500;

    /// The default age grace window of [`AssetStore::sweep_unreferenced`]
    /// (Dart's `minAge` default).
    pub const DEFAULT_SWEEP_MIN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

    /// The default retention of [`AssetStore::prune_trash`].
    pub const DEFAULT_TRASH_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    /// The asset kind an import records unless told otherwise (Dart's
    /// `kind = 'audio'` default).
    pub const DEFAULT_KIND: &'static str = "audio";

    /// Every (table, column) that can reference an asset id — THE single
    /// source of truth the GC derives reachability from. It MUST cover every
    /// `.references(Assets)` column in the schema: a column missing here
    /// makes [`AssetStore::sweep_unreferenced`] tombstone assets that are
    /// still in use and reclaim their bytes. Guarded by a test that derives
    /// this set structurally from the live schema and fails on any drift.
    ///
    /// NOTE: Persistence's referenced-content-hash set derives from this
    /// same constant, but WITHOUT the live-row filter — the staged recording
    /// recovery must respect tombstoned references too (a deleted take must
    /// not be resurrected); see its doc for why the filters differ.
    pub const ASSET_REF_COLUMNS: &'static [AssetRefColumn] = &[
        AssetRefColumn {
            table: "clips",
            column: "asset_id",
        },
        AssetRefColumn {
            table: "technique_media",
            column: "asset_id",
        },
        AssetRefColumn {
            table: "practices",
            column: "separation_asset_id",
        },
    ];

    /// A store over the pool under [root], journaling through [writer], on
    /// the real file system and with a gate of its own (Dart's defaults:
    /// `fs = const DurableFs()`, `gate = WriteGate()`).
    pub fn new(db: Arc<AppDatabase>, root: impl Into<PathBuf>, writer: Arc<SyncWriter>) -> Self {
        let root = root.into();
        let root_str = root.to_string_lossy().into_owned();
        AssetStore {
            db,
            root,
            root_str,
            writer,
            fs: Arc::new(SystemDurableFs),
            gate: Arc::new(WriteGate::new()),
            turns: Turns::default(),
            debug_before_trashing_duplicate: Mutex::new(None),
            debug_after_promote: Mutex::new(None),
        }
    }

    /// This store with [fs] as its durability layer (Dart's `fs:`).
    pub fn with_fs(mut self, fs: Arc<dyn DurableFs>) -> Self {
        self.fs = fs;
        self
    }

    /// This store with [gate] as its write gate (Dart's `gate:`).
    pub fn with_gate(mut self, gate: Arc<WriteGate>) -> Self {
        self.gate = gate;
        self
    }

    /// The database.
    pub fn db(&self) -> &Arc<AppDatabase> {
        &self.db
    }

    /// Root directory under which the content-addressed pool lives.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The local-write chokepoint this store journals through.
    pub fn writer(&self) -> &Arc<SyncWriter> {
        &self.writer
    }

    /// The durability layer.
    pub fn fs(&self) -> &Arc<dyn DurableFs> {
        &self.fs
    }

    /// The app-wide store write gate (see the field's doc).
    pub fn gate(&self) -> &Arc<WriteGate> {
        &self.gate
    }

    /// Sets the seam that runs between an import's commit and its move of a
    /// duplicate copy to the trash. Test-only.
    pub fn set_debug_before_trashing_duplicate(&self, hook: Option<DebugHook>) {
        *lock(&self.debug_before_trashing_duplicate) = hook;
    }

    /// Sets the seam that runs between an import's promote and its row
    /// work. Test-only.
    pub fn set_debug_after_promote(&self, hook: Option<DebugRelHook>) {
        *lock(&self.debug_after_promote) = hook;
    }

    /// `p.join(root.path, rel)`.
    fn join_root(&self, rel: &str) -> String {
        p::join(&self.root_str, rel)
    }

    // --- Import -------------------------------------------------------------

    /// Imports [source_path] into the pool and returns its asset id. Dedups
    /// by content hash: re-importing identical bytes reuses the existing
    /// live asset (and re-materializes the file if it had been GC'd
    /// locally). [kind] is recorded on a new row ([`AssetStore::DEFAULT_KIND`]
    /// for audio).
    ///
    /// Runs under the shared side of the gate (nesting inside a caller's
    /// shared section is fine — the gate is reader-preferring), so an
    /// exclusive maintenance pass can never interleave with the pool write +
    /// asset-row commit and trash the in-flight `.part` or the just-placed
    /// file.
    pub fn import_file(&self, source_path: &str, kind: &str) -> Result<String> {
        self.gate
            .shared(|| self.import_file_ungated(source_path, kind))
    }

    fn import_file_ungated(&self, source_path: &str, kind: &str) -> Result<String> {
        // Listed as in flight until it returns, landed or failed: the quit
        // reads this list's progress to tell a save still landing a file from
        // a wedged one (see [`AssetStore::bytes_being_copied`]).
        let token = InFlight::list();
        self.import_listed(source_path, kind, &token)
    }

    fn import_listed(&self, source_path: &str, kind: &str, token: &InFlight) -> Result<String> {
        let ext = p::extension(source_path).replacen('.', "", 1);
        // ONE streaming pass produces the staged bytes, the hash AND the byte
        // count together, so all three provably describe the same content.
        // The old stat -> hash -> copy sequence opened the source three times
        // with awaits between them: a source modified mid-import could put
        // one file's bytes under another file's hash with a third file's
        // recorded size.
        let before = size_and_mtime(Path::new(source_path));
        let staged = self.stage_to_pool(Path::new(source_path), token)?;
        self.remember_staged_hash(source_path, before, &staged);
        step_landed();
        // One import at a time per pool file, from its promote to its row
        // (and its move to the trash, for a duplicate): see `in_turn`.
        // Case-folded: on a case-insensitive volume (macOS's default)
        // `<hash>.wav` and `<hash>.WAV` are one file.
        let result = Self::rel_path_for(&staged.hash, &ext)
            .and_then(|rel| self.in_turn(rel.to_lowercase(), || self.land(&staged, &ext, kind)));
        // The promote path renames the staging file away; on every other
        // exit (dedup hit, promote failure, DB failure) it must not linger.
        if staged.tmp_path.is_file() {
            let _ = fs::remove_file(&staged.tmp_path);
        }
        result
    }

    /// The part of an import that runs in its pool file's turn: promote,
    /// row work, and the move of a duplicate copy to the trash.
    fn land(&self, staged: &StagedImport, ext: &str, kind: &str) -> Result<String> {
        // Promote BEFORE the row work: idempotent by content address, and it
        // replaces a truncated crash leftover with the verified bytes
        // whichever branch below wins.
        let rel = self.promote_staged(staged, ext)?;
        // Taken out of the lock first: the hook may reset itself.
        let hook = lock(&self.debug_after_promote).clone();
        if let Some(hook) = hook {
            hook(&rel);
        }
        // The copy just promoted, when the bytes turn out to be held already
        // under another extension and nothing points at it (see below).
        let mut duplicate: Option<String> = None;
        // Dedup lookup and insert share ONE transaction so concurrent imports
        // of the same bytes serialize at the database: check-then-insert
        // across interleaved flows minted one asset row per caller.
        let asset_id = self.db.transaction(|| -> Result<String> {
            // content_hash is intentionally NON-unique: identical bytes under
            // two asset ULIDs across devices is the normal content-addressed
            // steady state, so there can be MORE THAN ONE live row for a
            // hash. The first whose bytes are here wins, else the first,
            // deterministically by HLC. Taking the first alone, a synced-in
            // row with no bytes here (another device's earlier import) was
            // re-pointed at the pool file a local row already owned: two live
            // rows on one path, and a clip loaded through that path came back
            // on a save as the other asset — an edit nobody made, which won
            // over a peer's real one.
            let rows: Vec<AssetRow> = self.db.select(
                "content_hash = ?1 AND deleted_at IS NULL ORDER BY hlc_packed",
                [&staged.hash],
            )?;
            let held = rows.iter().find(|r| {
                r.local_rel_path
                    .as_deref()
                    .is_some_and(|rel| Path::new(&self.join_root(rel)).is_file())
            });
            let present = held.is_some();
            if let Some(existing) = held.or_else(|| rows.first()) {
                // Deduped: re-point the (per-device) path if this row's bytes
                // went missing locally — the promote above already re-placed
                // them.
                if !present {
                    self.db.update_by_id(
                        "assets",
                        &existing.id,
                        &[("local_rel_path", &rel as &dyn rusqlite::types::ToSql)],
                    )?;
                } else if existing.local_rel_path.as_deref() != Some(rel.as_str()) {
                    // The same bytes under another extension (a.wav, then
                    // b.m4a): the copy promoted above is no row's file, and
                    // the orphan sweep keeps every file of a live hash — it
                    // stayed for the asset's life.
                    if !self.pool_file_claimed(&rel, &staged.hash)? {
                        duplicate = Some(rel.clone());
                    }
                }
                return Ok(existing.id.clone());
            }

            let id = new_id();
            let stamp = self.writer.stamp();
            // Created now by the real clock, not the HLC's wall: the GC's
            // grace window reads these, and a clock once set ahead keeps the
            // HLC ahead on every boot after it (by design, for LWW), which
            // stamped every asset in the future: never old enough to sweep.
            let created = now_ms();
            self.db.insert(&AssetRow {
                id: id.clone(),
                content_hash: staged.hash.clone(),
                byte_size: staged.size,
                ext: ext.to_string(),
                kind: kind.to_string(),
                local_rel_path: Some(rel.clone()),
                created_at: created,
                updated_at: created,
                hlc_wall: stamp.hlc.wall_ms,
                hlc_ctr: stamp.hlc.counter,
                hlc_node: stamp.hlc.node.clone(),
                hlc_packed: stamp.hlc.packed(),
                ..Default::default()
            })?;
            self.writer
                .journal("assets", &id, op::INSERT, &stamp.hlc, &staged.hash, None)?;
            Ok(id)
        })?;
        step_landed(); // the row committed
        if let Some(duplicate) = duplicate {
            let hook = lock(&self.debug_before_trashing_duplicate).clone();
            if let Some(hook) = hook {
                hook();
            }
            // No sweep runs meanwhile (they take the gate alone), and no
            // other import of this file (`in_turn`); but one may have pointed
            // a row at it before this one's turn and after the check above.
            // Ask again. To the trash, like every pool removal.
            if !self.pool_file_claimed(&duplicate, &staged.hash)? {
                self.move_to_trash(Path::new(&self.join_root(&duplicate)));
            }
        }
        Ok(asset_id)
    }

    /// Whether a row points at the pool file [rel]: by that path, or by
    /// another name of the same file. On a case-insensitive volume (macOS's
    /// default) `<hash>.WAV` IS a row's `<hash>.wav`: importing the same
    /// bytes as `x.WAV` reused that file, found no row on the upper-case
    /// path, and moved the row's only copy to the trash as a duplicate.
    fn pool_file_claimed(&self, rel: &str, hash: &str) -> Result<bool> {
        let rows: Vec<AssetRow> = self
            .db
            .select("local_rel_path = ?1 OR content_hash = ?2", [rel, hash])?;
        let file = self.join_root(rel);
        for r in rows {
            let Some(other) = r.local_rel_path else {
                continue;
            };
            if other == rel {
                return Ok(true);
            }
            match identical(Path::new(&self.join_root(&other)), Path::new(&file)) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                // A file that is not here is no other name of this one.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // Unknown (a permission or I/O error; an existence check
                // reads those as "not here" too) is not "another file": a
                // lingering copy is always safer than a row's only one in the
                // trash.
                Err(_) => return Ok(true),
            }
        }
        Ok(false)
    }

    /// A stable reference to an asset whose BYTES aren't present locally,
    /// encoded into the domain model's path field so a clip/media survives a
    /// reload (and a later save) instead of being dropped from the model and
    /// then tombstoned.
    pub fn ref_for(asset_id: &str) -> String {
        format!("asset://{asset_id}")
    }

    /// The asset id embedded in a [`AssetStore::ref_for`] reference, or
    /// `None` for a real path.
    pub fn asset_id_of(path_or_ref: &str) -> Option<&str> {
        path_or_ref.strip_prefix("asset://")
    }

    /// The asset id for a clip/media reference: if it's an `asset://` ref
    /// (bytes missing locally), the embedded id is reused as-is; if it's the
    /// pool path of a live asset (what [`AssetStore::resolve`] hands out, so
    /// every hydrated clip/media comes back to its next save this way), that
    /// asset's id is reused too; otherwise the file at the path is imported
    /// (idempotent + deduped).
    ///
    /// The pool-path case must NOT go through [`AssetStore::import_file`]:
    /// that streams a full copy into staging and re-hashes it only to find
    /// the bytes already pooled. A save that touched no audio would then
    /// need free disk for its largest clip (ENOSPC failing a rename), and a
    /// pooled file whose bytes changed after pooling (the case
    /// [`AssetStore::verify_pool_integrity`] reports and deliberately leaves
    /// alone) would be minted a NEW asset and its clip re-pointed at it —
    /// journaled, so the bad bytes would ship fleet-wide.
    ///
    /// Runs under the shared side of the gate, like
    /// [`AssetStore::import_file`].
    pub fn resolve_or_import(&self, path_or_ref: &str, kind: &str) -> Result<String> {
        if let Some(embedded) = Self::asset_id_of(path_or_ref) {
            return Ok(embedded.to_string());
        }
        self.gate
            .shared(|| match self.live_pool_asset_at(path_or_ref)? {
                Some(owner) => Ok(owner),
                None => self.import_file_ungated(path_or_ref, kind),
            })
    }

    /// [`AssetStore::resolve_or_import`] for a whole save's references,
    /// answered in order.
    ///
    /// Every save re-sends every clip, so resolving them one by one cost a
    /// gate entry plus an owner query per clip on EVERY save of a practice —
    /// even a mute click. Here the pool paths' owners are read in one query
    /// (`live_pool_owners`) and each element gets exactly the answer
    /// [`AssetStore::resolve_or_import`] would give at that point of a
    /// sequential walk: the owner map is trusted only until this call
    /// imports something, because an import can mint the live owner of a
    /// pool path further down the list (or re-point a deduped row's
    /// local_rel_path), and is re-read after one.
    pub fn resolve_or_import_all<S: AsRef<str>>(
        &self,
        paths_or_refs: &[S],
        kind: &str,
    ) -> Result<Vec<String>> {
        self.gate.shared(|| {
            let mut out = Vec::with_capacity(paths_or_refs.len());
            let mut owners: Option<HashMap<String, String>> = None;
            for (i, path) in paths_or_refs.iter().enumerate() {
                let path = path.as_ref();
                if let Some(embedded) = Self::asset_id_of(path) {
                    out.push(embedded.to_string());
                    continue;
                }
                if let Some(rel) = self.pool_rel_of(path) {
                    if owners.is_none() {
                        let mut rels = Vec::new();
                        let mut seen = HashSet::new();
                        for rest in &paths_or_refs[i..] {
                            if let Some(r) = self.pool_rel_of(rest.as_ref()) {
                                if seen.insert(r.clone()) {
                                    rels.push(r);
                                }
                            }
                        }
                        owners = Some(self.live_pool_owners(&rels)?);
                    }
                    if let Some(owner) = owners.as_ref().and_then(|o| o.get(&rel)) {
                        out.push(owner.clone());
                        continue;
                    }
                }
                out.push(self.import_file_ungated(path, kind)?);
                owners = None; // see the doc: an import invalidates the owner map
            }
            Ok(out)
        })
    }

    /// The pool-relative path of [path], or `None` when [path] is not inside
    /// the content-addressed pool.
    fn pool_rel_of(&self, path: &str) -> Option<String> {
        let pool = p::join(&self.root_str, "assets");
        if !p::is_within(&pool, path) {
            return None;
        }
        Some(relative(path, &self.root_str))
    }

    /// The live asset whose pooled file is [path], or `None` when [path] is
    /// not a live asset's pool path. Identity is the recorded local_rel_path
    /// itself ([`AssetStore::rel_path_for`] is a pure function of hash +
    /// ext), so nothing is read from the file. When identical bytes are live
    /// under more than one asset id, the first by HLC wins, mirroring
    /// [`AssetStore::import_file`]'s dedup order.
    fn live_pool_asset_at(&self, path: &str) -> Result<Option<String>> {
        let Some(rel) = self.pool_rel_of(path) else {
            return Ok(None);
        };
        Ok(self
            .live_pool_owners(std::slice::from_ref(&rel))?
            .remove(&rel))
    }

    /// rel -> the live asset owning that pool path, for every rel in [rels]
    /// that has one — `live_pool_asset_at`'s rule (first by HLC) applied to a
    /// set in one query per [`AssetStore::IN_QUERY_CHUNK`] paths.
    fn live_pool_owners(&self, rels: &[String]) -> Result<HashMap<String, String>> {
        let mut owners = HashMap::new();
        for chunk in Self::in_chunks(rels) {
            let rows: Vec<AssetRow> = self.db.select(
                &format!(
                    "local_rel_path IN ({}) AND deleted_at IS NULL ORDER BY hlc_packed",
                    placeholders(chunk.len())
                ),
                rusqlite::params_from_iter(chunk),
            )?;
            for r in rows {
                if let Some(rel) = r.local_rel_path {
                    owners.entry(rel).or_insert(r.id);
                }
            }
        }
        Ok(owners)
    }

    /// [items] in consecutive slices of at most
    /// [`AssetStore::IN_QUERY_CHUNK`], for set reads.
    pub fn in_chunks<T>(items: &[T]) -> std::slice::Chunks<'_, T> {
        items.chunks(Self::IN_QUERY_CHUNK)
    }

    /// Resolves an asset id to an absolute path on THIS device, or `None` if
    /// the asset is unknown or its bytes are not present locally.
    pub fn resolve(&self, asset_id: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .find::<AssetRow>(asset_id)?
            .and_then(|a| self.resolve_row(&a)))
    }

    /// [`AssetStore::resolve`] for an asset row the caller already read (a
    /// hydration that reads all of an aggregate's assets in one query).
    pub fn resolve_row(&self, a: &AssetRow) -> Option<String> {
        let rel = a.local_rel_path.as_deref()?;
        let abs = self.join_root(rel);
        Path::new(&abs).is_file().then_some(abs)
    }

    /// The pool-relative path for a (hash, ext), sharded two levels to keep
    /// any one directory small. Refuses a hash shorter than four characters
    /// (Dart's `substring` threw a `RangeError` there).
    pub fn rel_path_for(hash: &str, ext: &str) -> Result<String> {
        let name = if ext.is_empty() {
            hash.to_string()
        } else {
            format!("{hash}.{ext}")
        };
        let (Some(a), Some(b)) = (utf16_substring(hash, 0, 2), utf16_substring(hash, 2, 4)) else {
            return Err(Error::InvalidArgument(format!(
                "hash: too short for a pool path ({hash:?})"
            )));
        };
        Ok(p::join_all([
            "assets",
            a.as_str(),
            b.as_str(),
            name.as_str(),
        ]))
    }

    /// One streaming pass over [src]: the bytes land in a uniquely named
    /// staging file inside the pool tree while the sha256 and byte count
    /// accumulate over exactly the bytes written. The staging name is a
    /// fresh ULID — concurrent imports of the same content each stage
    /// privately and converge only at the (idempotent) promote rename, where
    /// the old shared `<hash>.part` name made them tear each other's copy
    /// apart.
    ///
    /// The pass runs on the calling thread (Dart moved it to a background
    /// isolate to keep the UI isolate free; the store already runs off the
    /// UI thread).
    fn stage_to_pool(&self, src: &Path, token: &InFlight) -> Result<StagedImport> {
        let staging_dir = self.root.join("assets").join(".staging");
        // Durably: on the first import this also creates the pool dir
        // itself, which every pooled file's directory entry hangs off.
        self.create_dir_durably(&staging_dir)?;
        let tmp_path = staging_dir.join(format!("{}.part", new_id()));
        token.copying(Some(tmp_path.clone()));
        match copy_and_hash(src, Some(&tmp_path)) {
            Ok((hash, size)) => {
                token.bank_copy(size);
                Ok(StagedImport {
                    hash,
                    size,
                    tmp_path,
                })
            }
            Err(e) => {
                token.copying(None);
                Err(e.into())
            }
        }
    }

    /// How many imports are in flight right now, in every store, copying or
    /// landing (see [`AssetStore::bytes_being_copied`]).
    pub fn copies_in_flight() -> usize {
        progress().in_flight.len()
    }

    /// A reading that moves while imports do real work, which a quit uses
    /// to tell a save that is past its budget but still pooling a file from
    /// a wedged one (see RecordingFinalizer). Only the change between two
    /// readings means anything; the value itself does not.
    ///
    /// It counts the bytes each copy has written to staging so far (read
    /// off the growing file), and it keeps moving after the copy: every step
    /// that lands the file (each directory flush, the file flush, the rename
    /// into the pool, the row commit) adds one as it finishes. Before, the
    /// reading stopped with the copy, so the F_FULLFSYNC and commit of a
    /// large file read as a wedge and a quit could exit while the save was
    /// still landing. It never goes down. One step that alone outlasts the
    /// quit's window still reads as a stall: the reading can only report
    /// steps that have finished.
    pub fn bytes_being_copied() -> i64 {
        // Banked first, in the same locked step as the list: a copy that
        // finishes during the reads below moves its bytes into the bank, and
        // reading the bank afterwards would count them twice.
        let (mut total, paths) = {
            let state = progress();
            let paths: Vec<PathBuf> = state.in_flight.values().flatten().cloned().collect();
            (state.banked, paths)
        };
        for path in paths {
            // Not created yet (or already placed): nothing to count.
            if let Ok(meta) = fs::metadata(&path) {
                total += meta.len() as i64;
            }
        }
        total
    }

    /// Leaves [staged]'s hash beside [source_path] when it is a staged
    /// recording (directly in a practice's folder under `audio/`, where boot
    /// recovery looks; imports and edits stage a level further down) that
    /// did not change while it was read. Best-effort: a missing sidecar only
    /// means boot hashes the file itself.
    ///
    /// Flushed to the drive before the import goes on, so it is on disk
    /// before any row naming these bytes commits: boot recovery trusts it
    /// over the staging file itself (see [`AssetStore::imported_hash_of`]).
    fn remember_staged_hash(
        &self,
        source_path: &str,
        before: Option<(i64, i64)>,
        staged: &StagedImport,
    ) {
        let audio = p::join(&self.root_str, "audio");
        let Some(before) = before else {
            return;
        };
        if !paths_equal(&p::dirname(&p::dirname(source_path)), &audio) {
            return;
        }
        let after = size_and_mtime(Path::new(source_path));
        if after != Some(before) || before.0 != staged.size {
            return;
        }
        let sidecar = hash_sidecar(Path::new(source_path));
        let record = format!("{} {} {}", before.0, before.1, staged.hash);
        let _ = self.write_hash_sidecar(&sidecar, &record);
    }

    /// Writes [record] to [sidecar] and lands it on the drive (the
    /// best-effort tail of `remember_staged_hash`).
    fn write_hash_sidecar(&self, sidecar: &Path, record: &str) -> io::Result<()> {
        let mut f = File::create(sidecar)?;
        f.write_all(record.as_bytes())?;
        // Dart's `writeAsString(flush: true)`: a plain fsync.
        durable_fs::plain_fsync(&f)?;
        drop(f);
        // The flush handed the bytes to the drive; the directory's
        // F_FULLFSYNC lands its new entry and empties the drive's cache.
        self.fs.fsync_dir(&parent_of(sidecar));
        Ok(())
    }

    /// The sha256 of the bytes an import pooled from the staged file [f],
    /// whether or not [f] still holds them; `None` when no import recorded
    /// one, or [f] has grown since.
    ///
    /// The recorder never flushes its file to the drive (it guards against a
    /// killed process, not a power cut), while an import flushes the pooled
    /// copy and this record before its rows commit. A power cut soon after a
    /// save can therefore bring the staging file back holding older bytes
    /// than the save pooled. Their own hash matches nothing saved, so boot
    /// recovery asks this one too, or the take came back a second time,
    /// truncated. A power cut rolls a file back, never forward: one longer
    /// than it was imported was written to since, and the record no longer
    /// speaks for it.
    pub fn imported_hash_of(f: &Path) -> Option<String> {
        let (size, _, hash) = read_hash_sidecar(f)?;
        let (now_size, _) = size_and_mtime(f)?;
        if now_size > size {
            return None;
        }
        Some(hash)
    }

    /// The sha256 an import measured for the staged file [f], while [f] is
    /// unchanged since (same size and modification time); `None` otherwise.
    ///
    /// Boot recovery judges each staged recording by its content hash, and a
    /// graceful stop leaves every take of the session in staging after its
    /// save pooled — and so hashed — it: without this the next boot re-read
    /// every one of them in full before the window appeared. A file that has
    /// no sidecar (a hard kill skips the save) or changed is hashed as
    /// before.
    pub fn known_hash_of(f: &Path) -> Option<String> {
        let (size, mtime, hash) = read_hash_sidecar(f)?;
        let now = size_and_mtime(f)?;
        (now == (size, mtime)).then_some(hash)
    }

    /// The sha256 of the file at [path]. Fails with what the read fails
    /// with.
    pub fn sha256_of_file(path: &Path) -> Result<String> {
        Ok(copy_and_hash(path, None)?.0)
    }

    /// Moves a staged file to its content address. An already-present pool
    /// file is reused only when its length matches the staged byte count — a
    /// truncated crash leftover is replaced by the fresh, verified bytes
    /// instead of being trusted on existence alone.
    ///
    /// Length is deliberately the whole test HERE. It is exact against what
    /// this path can produce (an interrupted copy is short, and a file
    /// already at this hash's address came from the same bytes), and a real
    /// content check would re-read the entire pooled file on every import of
    /// bytes the pool already holds. Content is verified in
    /// [`AssetStore::verify_pool_integrity`] instead, which the repair pass
    /// runs once and which also covers what no import check could: bytes
    /// that changed after they were pooled.
    fn promote_staged(&self, staged: &StagedImport, ext: &str) -> Result<String> {
        let rel = Self::rel_path_for(&staged.hash, ext)?;
        let dest = PathBuf::from(self.join_root(&rel));
        if dest.is_file() && fs::metadata(&dest)?.len() as i64 == staged.size {
            return Ok(rel); // identical bytes already pooled
        }
        let shard = parent_of(&dest);
        self.create_dir_durably(&shard)?;
        self.fs.fsync_file(&staged.tmp_path)?;
        step_landed();
        // A concurrent import of the same hash may have placed it first;
        // rename is idempotent enough (overwrites with identical bytes).
        self.fs.atomic_rename(&staged.tmp_path, &dest)?;
        step_landed();
        self.fs.fsync_dir(&shard);
        step_landed();
        Ok(rel)
    }

    /// Creates [dir] and any missing ancestors, flushing each directory THIS
    /// call creates into its parent. Flushing the file's own shard after the
    /// rename only makes the file's entry durable within that shard; a power
    /// loss must also find the shard itself, so a fresh `assets/<ab>/` or
    /// `assets/<ab>/<cd>/` has to be on permanent storage before the row
    /// referencing a file inside it commits. An ancestor that already exists
    /// is not re-flushed: its creator did that (or, for a concurrent import
    /// racing into the same fresh shard, is doing it — the same in-process
    /// window as the already-pooled early return in `promote_staged`).
    fn create_dir_durably(&self, dir: &Path) -> io::Result<()> {
        if dir.is_dir() {
            return Ok(());
        }
        let parent = parent_of(dir);
        if parent != dir {
            self.create_dir_durably(&parent)?;
        }
        match fs::create_dir(dir) {
            Ok(()) => {}
            // An EEXIST from a concurrent creator is success.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {}
            Err(e) => return Err(e),
        }
        self.fs.fsync_dir(&parent);
        step_landed();
        Ok(())
    }

    /// Runs [body] once every import before it of the pool file [rel] is
    /// done. Imports share the write gate, and a second import of the same
    /// bytes under the same extension promotes onto the same file: one
    /// deduped to a row under another extension moved that file to the trash
    /// while the other had promoted onto it and was about to point its new
    /// row there — leaving that asset with no bytes. Per file, promote, row
    /// and move take turns, in call order.
    fn in_turn<T>(&self, rel: String, body: impl FnOnce() -> T) -> T {
        {
            let mut queues = lock(&self.turns.queues);
            let queue = queues.entry(rel.clone()).or_default();
            let ticket = queue.next;
            queue.next += 1;
            while queues.get(&rel).is_some_and(|q| q.serving != ticket) {
                queues = self
                    .turns
                    .changed
                    .wait(queues)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }
        /// Hands the turn on however [body] ends, a panic included.
        struct Release<'a> {
            turns: &'a Turns,
            rel: String,
        }
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                let mut queues = lock(&self.turns.queues);
                if let Some(queue) = queues.get_mut(&self.rel) {
                    queue.serving += 1;
                    if queue.serving == queue.next {
                        queues.remove(&self.rel);
                    }
                }
                self.turns.changed.notify_all();
            }
        }
        let _release = Release {
            turns: &self.turns,
            rel,
        };
        body()
    }

    // --- Staging sweep --------------------------------------------------------

    /// Clears the transient staging trees (`audio/`, `technique_media/`)
    /// where recordings and imports are written before being copied into the
    /// pool. After the drift rebuild nothing persistent references those
    /// files (clips/media reference pool assets), so a leftover there is
    /// always a reclaimable orphan. Each tree is MOVED into
    /// [`AssetStore::trash_dir`] (recoverable until
    /// [`AssetStore::prune_trash`] ages it out), never deleted in place.
    /// Meant to run once at startup, when nothing is in flight. Never
    /// touches the content-addressed pool (`assets/`). Best-effort.
    ///
    /// [keep]: absolute paths of files that must SURVIVE this sweep — staged
    /// recordings whose boot recovery ATTEMPT threw (an environment-wide
    /// decoder or disk failure, not per-file junk). Sweeping those would
    /// destroy the only copy of a take with no retry ever; leaving them lets
    /// the next boot's recovery try again; their sidecars stay with them.
    /// Persistence bounds the exemption in time.
    ///
    /// [discard]: absolute paths of staged files whose bytes the pool
    /// provably holds (Persistence's pooled staging paths). They are
    /// deleted, with their hash record, instead of moved to the trash: the
    /// pool copy is the take, and a second one kept for 30 days doubled
    /// every recording's footprint.
    pub fn sweep_staging(&self, keep: &HashSet<String>, discard: &HashSet<String>) {
        // Files whose bytes the pool provably holds go for good, with their
        // hash record; the trash is for what nothing else holds a copy of.
        for path in discard {
            if keep.contains(path) {
                continue;
            }
            for f in [PathBuf::from(path), hash_sidecar(Path::new(path))] {
                let _ = fs::remove_file(f);
            }
        }
        for name in ["audio", "technique_media"] {
            let _ = self.sweep_staging_tree(&self.root.join(name), keep);
        }
    }

    /// One staging tree of [`AssetStore::sweep_staging`] (a failure leaves
    /// the rest of the tree where it is).
    fn sweep_staging_tree(&self, d: &Path, keep: &HashSet<String>) -> io::Result<()> {
        if !d.is_dir() {
            return Ok(());
        }
        if keep.is_empty() {
            self.move_to_trash(d);
            return Ok(());
        }
        self.sweep_tree_except(d, keep)?;
        // Drop the top-level tree itself once nothing kept remains inside.
        let _ = remove_dir_if_empty(d);
        Ok(())
    }

    /// Moves every entry under [dir] (recursively) into the trash except
    /// files whose exact path is in [keep], and their sidecars;
    /// subdirectories emptied by the sweep are removed. Best-effort per
    /// entry, like the whole-tree path.
    fn sweep_tree_except(&self, dir: &Path, keep: &HashSet<String>) -> io::Result<()> {
        for e in list_dir(dir)? {
            let e = e?;
            if e.kind == EntityKind::Directory {
                self.sweep_tree_except(&e.path, keep)?;
                let _ = remove_dir_if_empty(&e.path);
            } else {
                let path = e.path.to_string_lossy();
                if keep.contains(path.as_ref()) || sidecar_of_kept(&path, keep) {
                    continue;
                }
                self.move_to_trash(&e.path);
            }
        }
        Ok(())
    }

    // --- GC -------------------------------------------------------------------

    /// Reclaims local disk: tombstones any never-synced live asset row no
    /// live clip / technique-media / practice references, and moves its
    /// pooled file into the trash once no surviving asset shares that
    /// content hash (and drops its waveform cache). Returns the number of
    /// assets tombstoned. [min_age] is the age grace window
    /// ([`AssetStore::DEFAULT_SWEEP_MIN_AGE`] unless a test says otherwise).
    ///
    /// Refcount is DERIVED here (a query over
    /// [`AssetStore::ASSET_REF_COLUMNS`]), never a stored counter that a
    /// crash could desync. NOTE: this only reclaims LOCAL disk; cloud-byte
    /// deletion is a separate, knowledge-gated step that never runs on a
    /// time horizon.
    ///
    /// Four guards against sweeping assets that are only LOCALLY (or
    /// transiently) unreferenced — a tombstone here is journaled, so once
    /// sync ships it would propagate as a fleet-wide delete, and a moment of
    /// local blindness must never become permanent:
    ///
    ///  * Only NEVER-SYNCED assets (both per-device sync-state columns still
    ///    at their 'localOnly' default — see `AssetRow`) are sweepable at
    ///    all. Reachability below is computed from the LOCAL database only,
    ///    and for an asset that has crossed the wire that is not knowledge: a
    ///    device days behind on pull would see a remotely referenced asset as
    ///    garbage and broadcast its delete fleet-wide. An asset this device
    ///    has ever pushed (or whose bytes were ever synced) is therefore left
    ///    alone by this sweep entirely; a future server-side refcount — the
    ///    only party that can see EVERY reference — owns the lifecycle of
    ///    synced assets.
    ///  * Once a transport has EVER been active (the durable
    ///    `SyncStateStore::TRANSPORT_SEEN_KEY` marker, written before the
    ///    first wire call), an asset with a PENDING oplog entry (entity_table
    ///    'assets', matching entity_id) is never swept, even while both
    ///    sync-state columns still read 'localOnly'. The push ack is what
    ///    deletes a journal entry AND (just before it) what marks the record
    ///    synced — and a crash can land between the server durably
    ///    persisting a push and either of those: the row is then already
    ///    server-side, where peers may have pulled it, while this device
    ///    still looks never-synced. The boot GC runs BEFORE any sync pump
    ///    could repair that bookkeeping, so without this guard it would sweep
    ///    exactly such a row and journal a tombstone that broadcasts a
    ///    fleet-wide delete for it. An unacked journal entry therefore means
    ///    "this row may already be server-side; its fate is not locally
    ///    decidable yet" — once the entry IS acked, the row was either marked
    ///    synced (first guard) or truly never left. Failing safe is
    ///    deliberate: holding a sweep leaks a little disk until the ledger
    ///    drains; sweeping a server-held row loses data fleet-wide. On a
    ///    library where NO transport has ever existed the guard stands down —
    ///    nothing ever acks the oplog there (boot compaction keeps one entry
    ///    per row forever), so the guard would otherwise hold EVERY asset and
    ///    quietly turn off local disk reclaim, while the marker's absence is
    ///    proof no server can hold any of these rows.
    ///  * Rows parked in the sync quarantine count as references: a
    ///    quarantined payload (half-delivered page, forward-incompatible
    ///    record) may carry the ONLY pointer at an asset until a later app
    ///    upgrade re-applies it.
    ///  * An asset row younger than [min_age] (by its created/updated stamp)
    ///    is never swept: a save's clip/media rows commit AFTER its
    ///    import_file, so a brand-new unreferenced row is far more likely an
    ///    in-flight save (or a not-yet-applied sync backlog) than garbage.
    ///    Real garbage is still reclaimed by the next sweep once it ages past
    ///    the window.
    ///
    /// The transport-seen / pending-journal / never-synced guards are read
    /// TWICE: coarsely in the candidate scan below, and again ATOMICALLY in
    /// each asset's tombstone (as conditions of its UPDATE itself). The scan
    /// alone is racy: the first-ever push can run concurrently with a
    /// user-triggered repairLibrary sweep, and the durable transport marker,
    /// the asset's journal entry, or its sync-state columns can change
    /// between the scan and the tombstone write — the scan then sweeps (and
    /// journals a fleet-wide delete for) a row the server may already hold.
    /// Holding the gate exclusively across the push was rejected (it would
    /// block repair across a network await), so the guards are re-verified
    /// where they cannot race: a conditional UPDATE that affects 0 rows
    /// means the asset's fate changed hands mid-sweep, and it is skipped
    /// whole — no tombstone, no journal entry, no file trash.
    ///
    /// Every tombstone of one sweep commits in ONE transaction. Each used to
    /// commit on its own, and with the store's F_FULLFSYNC every commit is a
    /// full drive flush: the first boot after deleting a practice with
    /// dozens of takes paid one per take before the window showed. One
    /// transaction keeps each asset's re-verification exactly as atomic (it
    /// is still a condition of that asset's own UPDATE), and the bytes are
    /// reclaimed after the commit, as before: a crash in between leaves pool
    /// files no live row owns, which [`AssetStore::sweep_orphan_pool_files`]
    /// reclaims.
    pub fn sweep_unreferenced(&self, min_age: Duration) -> Result<usize> {
        self.sweep_unreferenced_with(min_age, None)
    }

    /// [`AssetStore::sweep_unreferenced`] with its test-only seam
    /// [debug_before_tombstone] (same pattern as SyncEngine's
    /// debugBeforePageCommit): it runs after the candidate scan decided to
    /// sweep an asset and before the tombstone transaction, exactly the
    /// window the atomic re-verification protects.
    pub fn sweep_unreferenced_with(
        &self,
        min_age: Duration,
        mut debug_before_tombstone: Option<&mut dyn FnMut(&str)>,
    ) -> Result<usize> {
        // Quarantine FIRST, tables second: a concurrent pull's replay can
        // apply a parked clip and delete its quarantine entry between the two
        // scans. In this order the reference is visible either way — still
        // parked when the quarantine is listed, or already a live row when
        // the tables are read. The reverse order has a window where it is
        // visible to neither.
        let mut referenced = self.quarantine_referenced_asset_ids();
        for r in Self::ASSET_REF_COLUMNS {
            let sql = format!(
                "SELECT DISTINCT {column} AS a FROM {table} \
                 WHERE deleted_at IS NULL AND {column} IS NOT NULL",
                column = r.column,
                table = r.table
            );
            for a in self.db.query(&sql, [], |row| row.get::<_, SqlValue>("a"))? {
                if let SqlValue::Text(a) = a {
                    referenced.insert(a);
                }
            }
        }

        // The never-synced guard (first bullet of the doc): only assets
        // whose record AND bytes never left this device are candidates. Both
        // columns are per-device bookkeeping that only this device's sync
        // engine ever advances past 'localOnly', so the predicate is exactly
        // "no other device can possibly hold a reference we can't see".
        // The pending-journal guard (second bullet of the doc) rides along as
        // a NOT EXISTS conjunct — but only once a transport has ever existed:
        // it can only shrink the candidate set the other guards produced, and
        // on a never-synced library it would shrink it to nothing forever
        // (see the bullet). Raw key read rather than SyncStateStore: this
        // store must not depend on the sync layer.
        let transport_seen = !self
            .db
            .query(
                "SELECT 1 AS x FROM sync_state WHERE \"key\" = 'transport_seen'",
                [],
                |_| Ok(()),
            )?
            .is_empty();
        let pending_journal_guard = if transport_seen {
            "AND NOT EXISTS (SELECT 1 FROM oplog \
             WHERE entity_table = 'assets' AND entity_id = assets.id)"
        } else {
            ""
        };
        let live = self.db.query(
            &format!(
                "SELECT id, content_hash, local_rel_path, created_at, updated_at \
                 FROM assets WHERE deleted_at IS NULL \
                 AND record_sync_state = 'localOnly' \
                 AND bytes_sync_state = 'localOnly' \
                 {pending_journal_guard}"
            ),
            [],
            |r| {
                Ok((
                    r.get::<_, String>("id")?,
                    r.get::<_, String>("content_hash")?,
                    r.get::<_, i64>("created_at")?,
                    r.get::<_, i64>("updated_at")?,
                ))
            },
        )?;

        let now_ms = now_ms();
        let min_age_ms = i64::try_from(min_age.as_millis()).unwrap_or(i64::MAX);
        let mut candidates: Vec<(String, String)> = Vec::new();
        for (id, hash, created_at, updated_at) in live {
            if referenced.contains(&id) {
                continue;
            }
            // The age grace window (see doc): a young unreferenced row is
            // treated as in flight, not as garbage. A row stamped more than
            // [`FUTURE_STAMP_SLACK_MS`] ahead of now is not young: rows are
            // stamped by the real clock now, so only a row from a clock once
            // set ahead (stamped by the HLC's wall, before) can be, and it
            // would otherwise wait until real time caught up.
            let newest = created_at.max(updated_at);
            let from_a_clock_set_ahead = newest > now_ms.saturating_add(FUTURE_STAMP_SLACK_MS);
            if !from_a_clock_set_ahead && now_ms.saturating_sub(newest) < min_age_ms {
                continue;
            }

            if let Some(hook) = debug_before_tombstone.as_mut() {
                hook(&id);
            }
            candidates.push((id, hash));
        }
        if candidates.is_empty() {
            return Ok(0);
        }

        // Each tombstone UPDATE and its oplog entry MUST commit together (a
        // row mutation and its journal entry in the SAME transaction),
        // exactly as import_file does; otherwise a crash between them would
        // tombstone a synced asset with no journal entry and the delete would
        // never propagate. One transaction holds them all (see the doc).
        //
        // Each WHERE clause RE-VERIFIES, atomically with its write, every
        // guard the candidate scan read in a separate step (see the doc): the
        // row must still be live, still never-synced on both per-device
        // columns, and still clear of the pending-journal guard — expressed
        // as "no transport marker OR no pending oplog entry", the exact
        // predicate the scan applies (the guard engages only once a transport
        // has ever existed). A first push racing this sweep flips one of
        // those between the scan and here; that UPDATE then matches 0 rows
        // and the asset is skipped whole.
        let tombstoned_hashes = self.db.transaction(|| -> Result<Vec<String>> {
            let mut hashes = Vec::new();
            for (id, hash) in &candidates {
                let s = self.writer.stamp_for("assets", id)?;
                let changed = self.db.execute(
                    TOMBSTONE_SQL,
                    rusqlite::params![
                        s.at(),
                        s.at(),
                        s.hlc.wall_ms,
                        s.hlc.counter,
                        s.hlc.node,
                        s.hlc.packed(),
                        id
                    ],
                )?;
                // Fate changed hands mid-sweep (first push landed, repair
                // raced us): not removed, and the file-trash step below must
                // not run for it.
                if changed == 0 {
                    continue;
                }
                // The journal entry is written ONLY when the row actually
                // tombstoned: a journaled tombstone for a row the conditional
                // UPDATE refused would broadcast a fleet-wide delete for a row
                // that is still live.
                self.writer
                    .journal("assets", id, op::TOMBSTONE, &s.hlc, "", None)?;
                hashes.push(hash.clone());
            }
            Ok(hashes)
        })?;

        // Reclaim the bytes of every hash no surviving asset still maps to.
        let mut reclaimed: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for hash in &tombstoned_hashes {
            if !seen.insert(hash) {
                continue;
            }
            let shared: i64 = self.db.query_row(
                "SELECT COUNT(*) AS n FROM assets WHERE content_hash = ? \
                 AND deleted_at IS NULL",
                [hash],
                |r| r.get("n"),
            )?;
            if shared != 0 {
                continue;
            }
            // No live asset maps to this hash anymore. Trash EVERY pooled
            // file for it, not just this row's local_rel_path: identical
            // bytes imported under a different extension on another device
            // produce `<hash>.wav` AND `<hash>.mp3` as distinct files, so
            // reclaiming only the current row's path would leak the rest. All
            // files for a hash live in one shard dir (assets/<h0:2>/<h2:4>).
            self.trash_pool_files_for_hash(hash)?;
            reclaimed.push(hash.clone());
        }
        if !reclaimed.is_empty() {
            // Their cached peaks go too, in one transaction rather than one
            // commit per hash.
            self.db.transaction(|| -> Result<()> {
                for chunk in Self::in_chunks(&reclaimed) {
                    self.db.execute(
                        &format!(
                            "DELETE FROM waveform_cache WHERE content_hash IN ({})",
                            placeholders(chunk.len())
                        ),
                        rusqlite::params_from_iter(chunk),
                    )?;
                }
                Ok(())
            })?;
        }
        Ok(tombstoned_hashes.len())
    }

    /// Asset ids referenced by row payloads parked in the sync quarantine.
    ///
    /// Quarantined records are rejected-but-not-dropped remote state (see
    /// QuarantineStore): a parked clip/media/practice payload may be the
    /// only thing pointing at an asset until an app upgrade or manual review
    /// re-applies it. Read through the store's own API and the SyncRow wire
    /// shape (`{"fields": {<column>: <value>, ...}}`), scanning exactly the
    /// [`AssetStore::ASSET_REF_COLUMNS`] columns. Best-effort: a malformed
    /// payload (or an unreadable quarantine table) contributes no
    /// references — the age grace window in
    /// [`AssetStore::sweep_unreferenced`] still softens that case.
    fn quarantine_referenced_asset_ids(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut cols_by_table: HashMap<&str, Vec<&str>> = HashMap::new();
        for r in Self::ASSET_REF_COLUMNS {
            cols_by_table.entry(r.table).or_default().push(r.column);
        }
        // Quarantine unreadable: fall through with whatever was gathered.
        let Ok(rows) = QuarantineStore::new(self.db.clone()).list() else {
            return out;
        };
        for row in rows {
            let Some(cols) = cols_by_table.get(row.tbl.as_str()) else {
                continue;
            };
            // Undecodable payload: nothing to protect from this row.
            let Ok(decoded) = serde_json::from_str::<serde_json::Value>(&row.payload) else {
                continue;
            };
            let Some(fields) = decoded
                .as_object()
                .and_then(|m| m.get("fields"))
                .and_then(|f| f.as_object())
            else {
                continue;
            };
            for col in cols {
                if let Some(serde_json::Value::String(v)) = fields.get(*col) {
                    if !v.is_empty() {
                        out.insert(v.clone());
                    }
                }
            }
        }
        out
    }

    /// Moves every pooled file named `<hash>` or `<hash>.<ext>` from the
    /// hash's shard directory into the trash — never a hard delete, so a GC
    /// mistake stays recoverable. Best-effort (a lingering file is a
    /// reclaimable orphan).
    fn trash_pool_files_for_hash(&self, hash: &str) -> Result<()> {
        if dart::string_length(hash) < 4 {
            return Ok(());
        }
        let (Some(a), Some(b)) = (utf16_substring(hash, 0, 2), utf16_substring(hash, 2, 4)) else {
            // Not representable as a path (a lone surrogate): no shard holds
            // it.
            return Ok(());
        };
        let shard = self.root.join("assets").join(a).join(b);
        if !shard.is_dir() {
            return Ok(());
        }
        let prefix = format!("{hash}.");
        for e in list_dir(&shard)? {
            let e = e?;
            if e.kind != EntityKind::File {
                continue;
            }
            let name = basename(&e.path);
            if name == hash || name.starts_with(&prefix) {
                self.move_to_trash(&e.path);
            }
        }
        Ok(())
    }

    /// Re-hashes every pooled file and returns the paths whose CONTENT no
    /// longer matches the content address they are stored under.
    ///
    /// Nothing on the import path can find these. `promote_staged` reuses an
    /// already-present pool file whose LENGTH matches the freshly staged
    /// bytes, which is the right guard for what import can actually create —
    /// a truncated crash leftover — but it is not a content check, and
    /// paying for one there would re-read the whole pooled file on every
    /// import of bytes the pool already holds. So the content check lives
    /// here instead, in the user-invoked repair pass, where reading the pool
    /// once is affordable — and where it also catches the case import never
    /// could: bytes that rot or are edited AFTER they were pooled.
    ///
    /// REPORTS ONLY. A mismatched file is still the user's recording, most
    /// likely playable; trashing it (or hiding it behind a bytes-missing
    /// row) on the strength of a checksum would take a take away from them
    /// to fix an accounting problem. The count goes in the repair report and
    /// the paths go to the diagnostics log, and what to do about it stays
    /// the user's call.
    pub fn verify_pool_integrity(&self) -> Result<Vec<String>> {
        let pool = self.root.join("assets");
        if !pool.is_dir() {
            return Ok(Vec::new());
        }
        let mut bad = Vec::new();
        for e in list_recursive(&pool)? {
            let e = e?;
            if e.kind != EntityKind::File {
                continue;
            }
            let name = basename(&e.path);
            if name.ends_with(".part") {
                continue; // in-flight/crashed import copy
            }
            let hash = match name.find('.') {
                Some(dot) => &name[..dot],
                None => name.as_str(),
            };
            if dart::string_length(hash) != 64 {
                continue; // not a content-addressed pool file
            }
            // Unreadable right now: that is a different failure (and one the
            // caller will meet again on playback). Not evidence of wrong
            // content.
            if let Ok(actual) = Self::sha256_of_file(&e.path) {
                if actual != hash {
                    bad.push(e.path.to_string_lossy().into_owned());
                }
            }
        }
        Ok(bad)
    }

    /// Reclaims pool files that no LIVE asset references — by moving them
    /// into [`AssetStore::trash_dir`] (recoverable until
    /// [`AssetStore::prune_trash`] ages them out). Two cases:
    ///  - a crash between the pool write in `promote_staged` and the
    ///    asset-row transaction in [`AssetStore::import_file`] leaves a file
    ///    with NO row at all; and
    ///  - a crash between the tombstone commit and the file reclaim in
    ///    [`AssetStore::sweep_unreferenced`] leaves a file whose only rows
    ///    are tombstoned (which the sweep never revisits, since it iterates
    ///    only live rows).
    ///
    /// Both are covered by treating only LIVE (deleted_at IS NULL) content
    /// hashes as owned.
    ///
    /// FUSE: when the assets table holds ZERO live rows while the pool holds
    /// files, the sweep SKIPS entirely. No normal state looks like that — a
    /// fresh install has no pool yet — so it means this database was lost
    /// or replaced while the bytes survived (wiped/restored DB file), and
    /// sweeping would reclaim every byte the user could still recover.
    ///
    /// The fuse is remembered (`pool_fuse_at`): the first take recorded into
    /// the new library gave it a live row, and the next sweep then reclaimed
    /// every byte of the old one (gone for good 30 days later). Once
    /// tripped, a pool file older than the trip is never reclaimed; a newer
    /// one can only be this database's own crash leftover.
    ///
    /// A file whose hash this database holds only in tombstoned rows is its
    /// own leftover however old (a reclaim cut short, a library emptied by
    /// its user): it neither trips the fuse nor is kept by it.
    ///
    /// Meant to run at startup when nothing is in flight. Best-effort;
    /// returns the count removed.
    pub fn sweep_orphan_pool_files(&self) -> Result<usize> {
        let pool = self.root.join("assets");
        if !pool.is_dir() {
            return Ok(0);
        }
        // Content hashes with a LIVE asset row: a file matching one is
        // legitimately owned. A file matching only a tombstoned hash (or no
        // hash) is an orphan to reclaim.
        let mut known: HashSet<String> = HashSet::new();
        // Hashes only tombstoned rows hold: their files are this library's
        // own leftovers, whatever the fuse says (see doc).
        let mut tombstoned: HashSet<String> = HashSet::new();
        for (h, gone) in self.db.query(
            "SELECT content_hash AS h, MIN(deleted_at IS NOT NULL) AS gone \
             FROM assets GROUP BY content_hash",
            [],
            |r| Ok((r.get::<_, SqlValue>("h")?, r.get::<_, SqlValue>("gone")?)),
        )? {
            let SqlValue::Text(h) = h else {
                continue;
            };
            if gone == SqlValue::Integer(1) {
                tombstoned.insert(h);
            } else {
                known.insert(h);
            }
        }
        if known.is_empty() {
            // The fuse (see doc): with no live assets at all, a pool file
            // this database never held means "database lost, bytes survived"
            // — protect the bytes and touch nothing.
            for e in list_recursive(&pool)? {
                let e = e?;
                if e.kind != EntityKind::File {
                    continue;
                }
                let name = basename(&e.path);
                let Some(hash) = pool_hash(&name) else {
                    continue;
                };
                if tombstoned.contains(hash) {
                    continue;
                }
                // Dart's debugPrint: a console line, not the diagnostics log.
                eprintln!(
                    "AssetStore.sweepOrphanPoolFiles: skipped — the assets table has \
                     zero live rows but the pool holds files; protecting the bytes."
                );
                self.db.execute(
                    "INSERT INTO sync_state (\"key\", value) VALUES (?, ?) \
                     ON CONFLICT (\"key\") DO NOTHING",
                    [POOL_FUSE_KEY, now_ms().to_string().as_str()],
                )?;
                return Ok(0);
            }
        }
        let fuse: Option<SqlValue> = self.db.query_opt(
            "SELECT value FROM sync_state WHERE \"key\" = ?",
            [POOL_FUSE_KEY],
            |r| r.get("value"),
        )?;
        // `int.tryParse('${fuse?.data['value'] ?? ''}')`.
        let protect_up_to = match fuse {
            Some(SqlValue::Text(t)) => dart::parse_int(&t),
            Some(SqlValue::Integer(i)) => Some(i),
            _ => None,
        };
        let mut removed = 0;
        for e in list_recursive(&pool)? {
            let e = e?;
            if e.kind != EntityKind::File {
                continue;
            }
            let name = basename(&e.path);
            if name.ends_with(".part") {
                // Transient copy artifact from a crashed import — never a
                // valid pool file, but trash it anyway (uniform
                // no-hard-delete policy).
                if self.move_to_trash(&e.path) {
                    removed += 1;
                }
                continue;
            }
            let Some(hash) = pool_hash(&name) else {
                continue;
            };
            if known.contains(hash) {
                continue;
            }
            // A file from before the fuse tripped, that this database never
            // held: the lost library's. Kept too when its age can't be read.
            if let Some(protect_up_to) = protect_up_to {
                if !tombstoned.contains(hash) {
                    match fs::metadata(&e.path).ok().map(|m| mtime_ms(&m)) {
                        Some(ms) if ms > protect_up_to => {}
                        _ => continue,
                    }
                }
            }
            if self.move_to_trash(&e.path) {
                removed += 1;
            }
        }
        Ok(removed)
    }

    // --- Trash (GC recoverability) -------------------------------------------

    /// The container-local trash under the root. Every pool/staging reclaim
    /// in this store MOVES bytes here instead of unlinking them, so no
    /// single GC pass (or GC bug) can ever destroy the only copy of a take.
    /// Entries are named `<epochMs>-<originalName>`;
    /// [`AssetStore::prune_trash`] ages them out.
    pub fn trash_dir(&self) -> PathBuf {
        self.root.join("trash")
    }

    /// Moves [entity] (a pool/staging file, or a whole staging tree) into
    /// the trash as `<epochMs>-<originalName>`. Best-effort: returns whether
    /// it moved; on failure the entity is left in place (a lingering orphan
    /// is always safer than an unlink).
    fn move_to_trash(&self, entity: &Path) -> bool {
        self.try_move_to_trash(entity).is_ok()
    }

    fn try_move_to_trash(&self, entity: &Path) -> io::Result<()> {
        let dir = self.trash_dir();
        fs::create_dir_all(&dir)?;
        let base = p::basename(&entity.to_string_lossy());
        let stamp_ms = now_ms();
        let mut dest = dir.join(format!("{stamp_ms}-{base}"));
        // Uniquify if the same name lands twice within one millisecond —
        // never overwrite an existing trash entry.
        let mut n = 1;
        while fs::metadata(&dest).is_ok() {
            dest = dir.join(format!("{stamp_ms}-{n}-{base}"));
            n += 1;
        }
        fs::rename(entity, &dest)
    }

    /// Hard-deletes trash entries older than [retention]
    /// ([`AssetStore::DEFAULT_TRASH_RETENTION`] at boot), dated by the
    /// epoch-ms prefix the move to the trash stamped into each entry's name.
    /// An entry without a parseable prefix is never deleted (don't age out
    /// bytes we can't date). Returns the number of entries removed.
    ///
    /// Deliberately called by NOTHING in this store: the boot sequence wires
    /// it to run once AFTER a successful store open, so trash only ages out
    /// on a boot that proved the database (and with it the pool's ownership
    /// info) is healthy.
    pub fn prune_trash(&self, retention: Duration) -> Result<usize> {
        let dir = self.trash_dir();
        if !dir.is_dir() {
            return Ok(0);
        }
        let retention_us = i64::try_from(retention.as_micros()).unwrap_or(i64::MAX);
        let cutoff_ms = now_micros().saturating_sub(retention_us).div_euclid(1000);
        let mut removed = 0;
        for e in list_dir(&dir)? {
            let e = e?;
            let name = basename(&e.path);
            let stamp_ms = match name.find('-') {
                Some(dash) if dash > 0 => dart::parse_int(&name[..dash]),
                _ => None,
            };
            let Some(stamp_ms) = stamp_ms else {
                continue;
            };
            if stamp_ms >= cutoff_ms {
                continue;
            }
            if delete_recursive(&e.path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

// --- Helpers -----------------------------------------------------------------

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // No section under these locks leaves state half-written on a panic.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `?, ?, ...` for [n] bound values.
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Wall time, epoch microseconds (Dart's `DateTime.now()` precision).
fn now_micros() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_micros()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_micros()).unwrap_or(i64::MAX),
    }
}

/// Wall time, epoch milliseconds (`DateTime.now().millisecondsSinceEpoch`).
/// How far ahead of now an asset row may be stamped and still count as in
/// flight for the GC's grace window (see `sweep_unreferenced_with`): past
/// it, the stamp is a clock's, set ahead, not a recent write's.
const FUTURE_STAMP_SLACK_MS: i64 = 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    now_micros().div_euclid(1000)
}

/// A file's modification time in epoch milliseconds, the way Dart's
/// `FileStat.modified` / `File.lastModified` read it (seconds * 1000 plus
/// whole milliseconds of the nanoseconds).
fn mtime_ms(meta: &fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mtime()
            .saturating_mul(1000)
            .saturating_add(meta.mtime_nsec() / 1_000_000)
    }
    #[cfg(not(unix))]
    {
        match meta.modified() {
            Ok(t) => match t.duration_since(UNIX_EPOCH) {
                Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
                Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
            },
            Err(_) => 0,
        }
    }
}

/// The (size, modification time in epoch microseconds) of the regular file
/// at [path] (links followed), or `None` when there is none. Dart's stat
/// reads the time at millisecond precision, so the microseconds always end
/// in `000` — exactly what the sidecars hold.
fn size_and_mtime(path: &Path) -> Option<(i64, i64)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some((meta.len() as i64, mtime_ms(&meta).saturating_mul(1000)))
}

/// Where an import keeps the hash it measured for a staged recording,
/// beside the file (see [`AssetStore::known_hash_of`]); swept with the
/// staging tree.
fn hash_sidecar(path: &Path) -> PathBuf {
    let mut s: OsString = path.as_os_str().to_owned();
    s.push(".sha256");
    PathBuf::from(s)
}

/// The sidecar `remember_staged_hash` left for [path]: the size and
/// modification time the file had then, and its hash. `None` when there is
/// none or it does not parse.
fn read_hash_sidecar(path: &Path) -> Option<(i64, i64, String)> {
    let text = fs::read_to_string(hash_sidecar(path)).ok()?;
    let parts: Vec<&str> = text.split(' ').collect();
    if parts.len() != 3 || !is_sha256_hex(parts[2]) {
        return None;
    }
    let size = dart::parse_int(parts[0])?;
    let mtime = dart::parse_int(parts[1])?;
    Some((size, mtime, parts[2].to_string()))
}

/// `^[0-9a-f]{64}$`.
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The content hash a pool file is named for (`<sha256>` or
/// `<sha256>.<ext>`), or `None` for anything else (`.DS_Store`, a `.part`):
/// `^([0-9a-f]{64})(?:\.|$)`.
fn pool_hash(name: &str) -> Option<&str> {
    let hash = name.get(..64)?;
    if !is_sha256_hex(hash) {
        return None;
    }
    match name.as_bytes().get(64) {
        None | Some(b'.') => Some(hash),
        Some(_) => None,
    }
}

/// Reads [src_path] once, in fixed-size chunks, hashing every byte and —
/// when [tmp_path] is given — writing it there, so the staged bytes, the
/// hash and the byte count provably describe the same content. A single
/// reused buffer keeps memory flat on any file size. A failed copy leaves no
/// partial staging file behind; the error is returned, so the save fails
/// loudly instead of silently dropping the take.
fn copy_and_hash(src_path: &Path, tmp_path: Option<&Path>) -> io::Result<(String, i64)> {
    let result = copy_and_hash_once(src_path, tmp_path);
    if result.is_err() {
        if let Some(tmp) = tmp_path {
            if tmp.is_file() {
                let _ = fs::remove_file(tmp);
            }
        }
    }
    result
}

fn copy_and_hash_once(src_path: &Path, tmp_path: Option<&Path>) -> io::Result<(String, i64)> {
    let mut out = match tmp_path {
        Some(tmp) => Some(File::create(tmp)?),
        None => None,
    };
    let mut input = File::open(src_path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size: i64 = 0;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        // The hasher copies what it is handed, so the buffer is free to
        // reuse.
        hasher.update(&buf[..n]);
        if let Some(out) = out.as_mut() {
            out.write_all(&buf[..n])?;
        }
        size += n as i64;
    }
    drop(input);
    drop(out);
    Ok((hex::encode(hasher.finalize()), size))
}

/// `FileSystemEntity.identical`: whether two paths name the same file
/// system object (links not followed at the last component). Fails when
/// either cannot be examined (`NotFound` for a missing one).
fn identical(a: &Path, b: &Path) -> io::Result<bool> {
    let ma = fs::symlink_metadata(a)?;
    let mb = fs::symlink_metadata(b)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (ma, mb);
        Ok(fs::canonicalize(a)? == fs::canonicalize(b)?)
    }
}

/// The directory a path sits in (`.` for a bare relative name, like Dart's
/// `parent`).
fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) => PathBuf::from("."),
        None => path.to_path_buf(),
    }
}

/// `p.basename` of a listed entry.
fn basename(path: &Path) -> String {
    p::basename(&path.to_string_lossy())
}

/// The lexically normalized parts of [path], made absolute against the
/// current directory when relative (what `package:path` compares in
/// `equals` and `relative`).
fn normalized_parts(path: &str) -> Vec<String> {
    let full = if p::is_absolute(path) {
        path.to_string()
    } else {
        let cwd = std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_string());
        p::join(&cwd, path)
    };
    let mut out: Vec<String> = Vec::new();
    for part in full.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            part => out.push(part.to_string()),
        }
    }
    out
}

/// `p.equals(a, b)`.
fn paths_equal(a: &str, b: &str) -> bool {
    normalized_parts(a) == normalized_parts(b)
}

/// `p.relative(path, from: root)` for a [path] inside [root].
fn relative(path: &str, root: &str) -> String {
    let root = normalized_parts(root);
    let path = normalized_parts(path);
    path.get(root.len()..).unwrap_or_default().join("/")
}

/// Dart's `s.substring(start, end)` in UTF-16 code units; `None` where Dart
/// threw a `RangeError` (or the cut splits a surrogate pair).
fn utf16_substring(s: &str, start: usize, end: usize) -> Option<String> {
    if s.is_ascii() {
        return s.get(start..end).map(str::to_string);
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    String::from_utf16(units.get(start..end)?).ok()
}

/// Whether [path] is a sidecar (`<kept>.<ext>`, like the hash an import
/// recorded) of a file in [keep]: a file held back for another attempt keeps
/// what that attempt reads beside it.
fn sidecar_of_kept(path: &str, keep: &HashSet<String>) -> bool {
    // In the file's own name: a dot in a folder name says nothing.
    let Some(dot) = path.rfind('.') else {
        return false;
    };
    let after_slash = path.rfind('/').map_or(0, |s| s + 1);
    dot > after_slash && keep.contains(&path[..dot])
}

/// Removes the directory [dir] when it holds nothing (`list().isEmpty`, then
/// a non-recursive delete).
fn remove_dir_if_empty(dir: &Path) -> io::Result<()> {
    if fs::read_dir(dir)?.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(())
}

/// `entity.delete(recursive: true)`: a directory goes with everything in it
/// (links inside are removed, never followed); anything else is unlinked.
fn delete_recursive(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

// --- Directory listing with Dart's `Directory.list` semantics ----------------

/// What Dart's `Directory.list` (links followed, its default) reports an
/// entry as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntityKind {
    /// A file (anything that is neither a directory nor a broken link).
    File,
    /// A directory, or a link to one.
    Directory,
    /// A link whose target cannot be examined (or one that loops).
    Link,
}

/// One listed entry.
#[derive(Debug)]
struct Entity {
    path: PathBuf,
    kind: EntityKind,
}

/// Classifies [entry] as Dart's lister does with `followLinks: true`.
fn classify(entry: &fs::DirEntry) -> io::Result<EntityKind> {
    let file_type = entry.file_type()?;
    if file_type.is_symlink() {
        return Ok(match fs::metadata(entry.path()) {
            Ok(meta) if meta.is_dir() => EntityKind::Directory,
            Ok(_) => EntityKind::File,
            Err(_) => EntityKind::Link,
        });
    }
    Ok(if file_type.is_dir() {
        EntityKind::Directory
    } else {
        EntityKind::File
    })
}

/// `dir.list()`: the entries directly in [dir].
fn list_dir(dir: &Path) -> io::Result<impl Iterator<Item = io::Result<Entity>>> {
    Ok(fs::read_dir(dir)?.map(|entry| {
        let entry = entry?;
        let kind = classify(&entry)?;
        Ok(Entity {
            path: entry.path(),
            kind,
        })
    }))
}

/// The identity of a directory (device, inode), for link-loop detection.
#[cfg(unix)]
fn dir_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn dir_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// `dir.list(recursive: true)`: every entry under [dir], depth first, each
/// directory reported before its contents, read lazily so an error surfaces
/// where Dart's stream raised it. A linked directory that is one of its own
/// ancestors is reported as a link and not entered.
fn list_recursive(dir: &Path) -> io::Result<RecursiveListing> {
    Ok(RecursiveListing {
        stack: vec![(fs::read_dir(dir)?, dir_identity(dir))],
        pending_error: None,
    })
}

struct RecursiveListing {
    /// The directories being read, outermost first, with their identities.
    stack: Vec<(fs::ReadDir, Option<(u64, u64)>)>,
    /// A directory that was reported but could not be opened: its error is
    /// the next item.
    pending_error: Option<io::Error>,
}

impl Iterator for RecursiveListing {
    type Item = io::Result<Entity>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(e) = self.pending_error.take() {
            return Some(Err(e));
        }
        loop {
            let (top, _) = self.stack.last_mut()?;
            let entry = match top.next() {
                None => {
                    self.stack.pop();
                    continue;
                }
                Some(Err(e)) => return Some(Err(e)),
                Some(Ok(entry)) => entry,
            };
            let mut kind = match classify(&entry) {
                Ok(kind) => kind,
                Err(e) => return Some(Err(e)),
            };
            let path = entry.path();
            if kind == EntityKind::Directory {
                let identity = dir_identity(&path);
                if identity.is_some() && self.stack.iter().any(|(_, id)| *id == identity) {
                    kind = EntityKind::Link;
                } else {
                    match fs::read_dir(&path) {
                        Ok(read) => self.stack.push((read, identity)),
                        Err(e) => self.pending_error = Some(e),
                    }
                }
            }
            return Some(Ok(Entity { path, kind }));
        }
    }
}
