//! The SQLite implementation of the technique library (port of
//! `lib/data/repositories/technique_repository_drift.dart`).

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use parking_lot::Mutex;
use serde_json::Value;
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::dart_date_time::DateTime;

use super::common::{
    col, dart_type_name, dt, json_decode, json_text, ms, placeholders, stamped, update, upsert,
};
use super::{DebugLoadHook, TechniqueRepository};
use crate::dart;
use crate::db::rows::{AssetRow, TechniqueMediaRow, TechniqueRow};
use crate::db::{AppDatabase, AssetStore};
use crate::error::{Error, Result};
use crate::sync::reconcile::{reconcile_collection, ReconcileCollection};
use crate::sync::sync_hashes::{
    technique_content_hash, technique_media_content_hash, technique_media_row_hash,
    technique_row_hash, Loaded, TechniqueContent, TechniqueMediaContent,
};
use crate::sync::sync_writer::{op, SyncStamp, SyncWriter};

/// Content hash per row id (base, sent or current).
type Hashes = IndexMap<String, String>;

/// drift/SQLite implementation of the technique library.
///
/// Domain models are unchanged: a [`TechniqueMedia`] still carries a file
/// `path`. The content hash bridges path <-> asset — on save each media file
/// is imported into the content-addressed [`AssetStore`] (idempotent,
/// deduped) to get its asset id; on load the asset id resolves back to a
/// local path. Writes go through the [`SyncWriter`] chokepoint + per-entity
/// [`reconcile_collection`], so editing one field dirties exactly one row.
pub struct TechniqueRepositoryDrift {
    db: Arc<AppDatabase>,
    writer: Arc<SyncWriter>,
    assets: Arc<AssetStore>,
    /// Runs at the start of every [`TechniqueRepository::load_technique`].
    /// Test-only.
    debug_load_hook: Mutex<Option<DebugLoadHook>>,
}

impl std::fmt::Debug for TechniqueRepositoryDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TechniqueRepositoryDrift")
            .finish_non_exhaustive()
    }
}

/// A media row's `kind` column as a save writes it.
fn kind_of(m: &TechniqueMedia) -> &'static str {
    if m.is_video() {
        "video"
    } else {
        "audio"
    }
}

/// A list of strings as a save writes it (`jsonEncode(list)`).
fn strings_json(items: &[String]) -> Result<String> {
    json_text(&Value::Array(
        items.iter().map(|s| Value::String(s.clone())).collect(),
    ))
}

// The MINE side of the three-way merge, and — over the hydrated model — the
// base side too (see [`TechniqueRepositoryDrift::hydrate`]): one projection
// each, so what a load recorded and what a save compares can never encode
// differently.

fn technique_hash(t: &Technique) -> Result<String> {
    Ok(technique_content_hash(&TechniqueContent {
        name: &t.name,
        aliases_json: &strings_json(&t.aliases)?,
        tags_json: &strings_json(&t.tags)?,
        summary: &t.summary,
        description: &t.description,
        cues: &t.cues,
        exercises: &t.exercises,
        acoustic_signature: &t.acoustic_signature,
    }))
}

fn media_hash(m: &TechniqueMedia, asset_id: &str, order: i64) -> String {
    technique_media_content_hash(&TechniqueMediaContent {
        asset_id,
        kind: kind_of(m),
        label: &m.label,
        order_index: order,
    })
}

/// `List<String>.from(jsonDecode(json) as List)`, converted here, not cast
/// lazily: a non-string item then fails this row (which a list skips)
/// instead of every view that later reads its tags.
fn strings_from(json: &str, what: &str) -> Result<Vec<String>> {
    match json_decode(json)? {
        Value::Array(items) => items
            .into_iter()
            .map(|v| match v {
                Value::String(s) => Ok(s),
                _ => Err(Error::Format(format!("{what}: an item is not a String"))),
            })
            .collect(),
        _ => Err(Error::Format(format!("{what}: not a JSON list"))),
    }
}

/// [`TechniqueRepositoryDrift::read_media`]'s result: the live media rows by
/// technique id, and per media asset id its local path (absent = bytes not
/// here).
struct MediaRows {
    media: IndexMap<String, Vec<TechniqueMediaRow>>,
    paths: HashMap<String, String>,
}

impl TechniqueRepositoryDrift {
    /// The repository over [db], journaling through [writer], with its media
    /// in [assets].
    pub fn new(db: Arc<AppDatabase>, writer: Arc<SyncWriter>, assets: Arc<AssetStore>) -> Self {
        TechniqueRepositoryDrift {
            db,
            writer,
            assets,
            debug_load_hook: Mutex::new(None),
        }
    }

    /// The database.
    pub fn db(&self) -> &Arc<AppDatabase> {
        &self.db
    }

    /// The local-write chokepoint.
    pub fn writer(&self) -> &Arc<SyncWriter> {
        &self.writer
    }

    /// The asset pool.
    pub fn assets(&self) -> &Arc<AssetStore> {
        &self.assets
    }

    /// Sets the seam that runs at the start of every load (a save's re-read
    /// included); an `Err` fails that load. Test-only.
    pub fn set_debug_load_hook(&self, hook: Option<DebugLoadHook>) {
        *self.debug_load_hook.lock() = hook;
    }

    /// The live media rows of the technique [technique_id].
    fn live_media(&self, technique_id: &str) -> Result<Vec<TechniqueMediaRow>> {
        self.db
            .select("technique_id = ?1 AND deleted_at IS NULL", [technique_id])
    }

    /// Soft-deletes the media row [id] with the stamp [s] (no journal).
    fn tombstone_media(&self, id: &str, now: i64, s: &SyncStamp) -> Result<()> {
        update(
            &self.db,
            "technique_media",
            id,
            stamped(vec![col("deleted_at", now), col("updated_at", now)], s),
        )
    }

    /// The one save core. [base] is the per-row content-hash map captured
    /// when the caller loaded the technique (three-way merge); `None` means
    /// "adopt the current persisted state as base" (the legacy two-way
    /// diff).
    ///
    /// The WHOLE import+commit sequence runs under the store write gate's
    /// shared side: asset imports land before the media rows referencing
    /// them commit, and a maintenance pass (Persistence's repairLibrary / the
    /// boot sweeps, which hold the gate exclusively) running in that window
    /// would tombstone the just-imported asset and trash its pool file.
    fn save(&self, technique: &Technique, base: Option<&Hashes>) -> Result<Loaded<Technique>> {
        self.assets
            .gate()
            .shared(|| self.save_ungated(technique, base))
    }

    fn save_ungated(
        &self,
        technique: &Technique,
        base: Option<&Hashes>,
    ) -> Result<Loaded<Technique>> {
        // A deleted technique takes no save (the transaction refuses it too),
        // and is turned away before the imports below: asked only there, its
        // new files were already imported and journaled for a save that then
        // failed.
        if let Some(parent) = self.db.find::<TechniqueRow>(&technique.id)? {
            if parent.deleted_at.is_some() {
                return Err(Error::State(format!(
                    "technique {} is deleted; refusing to save over it",
                    technique.id
                )));
            }
        }

        // Assets first (outside the row transaction, idempotent + deduped): a
        // dependent media row never exists without its asset.
        let mut media_assets: Vec<String> = Vec::with_capacity(technique.media.len()); // assetId per media, by index
        for m in &technique.media {
            media_assets.push(self.assets.resolve_or_import(&m.path, kind_of(m))?);
        }

        let aliases_json = strings_json(&technique.aliases)?;
        let tags_json = strings_json(&technique.tags)?;
        let now = ms(&DateTime::now());
        // Every row as this save sends it, hashed as its merge compares it —
        // before the write, so nothing after the commit can fail the save
        // (see the re-read below).
        let mut sent = Hashes::new();
        sent.insert(technique.id.clone(), technique_hash(technique)?);
        for (i, (m, asset_id)) in technique.media.iter().zip(&media_assets).enumerate() {
            sent.insert(m.id.clone(), media_hash(m, asset_id, i as i64));
        }

        self.db.transaction(|| -> Result<()> {
            // --- the technique row (single-entity reconcile) ---
            let existing = self.db.find::<TechniqueRow>(&technique.id)?;
            if existing.as_ref().is_some_and(|e| e.deleted_at.is_some()) {
                // The technique was tombstoned before this save landed (a
                // save racing delete_technique, or a synced-in delete).
                // Continuing would NOT revive the row (the update below never
                // clears deleted_at) yet would still commit LIVE media rows
                // under the tombstoned parent — a journaled zombie subtree.
                // Same policy as save_practice: a deleted aggregate must
                // never be implicitly re-saved; abort so the whole
                // transaction rolls back and the caller surfaces an error.
                return Err(Error::State(format!(
                    "technique {} is deleted; refusing to save over it",
                    technique.id
                )));
            }
            let desired_hash = technique_hash(technique)?;
            // Tracks whether the technique ROW itself was written, so a
            // media-only edit (row hash unchanged) can still bump updatedAt
            // for list ordering.
            let mut technique_written = false;

            match existing {
                None => {
                    let s = self.writer.stamp();
                    technique_written = true;
                    self.db.insert(&TechniqueRow {
                        id: technique.id.clone(),
                        created_at: ms(&technique.created_at),
                        updated_at: ms(&technique.updated_at),
                        hlc_wall: s.hlc.wall_ms,
                        hlc_ctr: s.hlc.counter,
                        hlc_node: s.hlc.node.clone(),
                        hlc_packed: s.hlc.packed(),
                        name: technique.name.clone(),
                        aliases_json: Some(aliases_json.clone()),
                        tags_json: Some(tags_json.clone()),
                        summary: technique.summary.clone(),
                        description: technique.description.clone(),
                        cues: technique.cues.clone(),
                        exercises: technique.exercises.clone(),
                        acoustic_signature: technique.acoustic_signature.clone(),
                        ..Default::default()
                    })?;
                    self.writer.journal(
                        "techniques",
                        &technique.id,
                        op::INSERT,
                        &s.hlc,
                        &desired_hash,
                        None,
                    )?;
                }
                Some(existing) => {
                    let existing_hash = technique_row_hash(&existing);
                    // Three-way merge on the technique row (same rules as
                    // reconcile_collection). A `None` [base] adopts the
                    // current row as base (legacy two-way — the skip below
                    // then never fires); a with-base map missing this id
                    // degrades to conflict-local-wins.
                    let base_hash = match base {
                        None => Some(existing_hash.as_str()),
                        Some(b) => b.get(&technique.id).map(String::as_str),
                    };
                    let keep_theirs = existing_hash != desired_hash
                        && base_hash.is_some_and(|b| existing_hash != b && desired_hash == b);
                    // keep_theirs: the persisted row moved on since this
                    // editor loaded it (a remote/newer write) and the editor
                    // never touched the technique fields — keep the DB state;
                    // writing "mine" would revert the newer row to stale
                    // values under a fresh HLC.
                    if existing_hash != desired_hash && !keep_theirs {
                        // Plain local edit, or a true conflict (both sides
                        // changed): the ACTIVE EDITOR wins by policy and mine
                        // is written with a fresh HLC, so it also wins
                        // entity-level LWW downstream.
                        let s = self.writer.stamp_for("techniques", &technique.id)?;
                        technique_written = true;
                        update(
                            &self.db,
                            "techniques",
                            &technique.id,
                            stamped(
                                vec![
                                    col("name", technique.name.as_str()),
                                    col("aliases_json", aliases_json.as_str()),
                                    col("tags_json", tags_json.as_str()),
                                    col("summary", technique.summary.as_str()),
                                    col("description", technique.description.as_str()),
                                    col("cues", technique.cues.as_str()),
                                    col("exercises", technique.exercises.as_str()),
                                    col(
                                        "acoustic_signature",
                                        technique.acoustic_signature.as_str(),
                                    ),
                                    col("updated_at", now),
                                ],
                                &s,
                            ),
                        )?;
                        self.writer.journal(
                            "techniques",
                            &technique.id,
                            op::UPDATE,
                            &s.hlc,
                            &desired_hash,
                            None,
                        )?;
                    }
                }
            }

            // --- the media collection ---
            let live_media = self.live_media(&technique.id)?;
            let current_hashes: Hashes = live_media
                .iter()
                .map(|m| (m.id.clone(), technique_media_row_hash(m)))
                .collect();
            let desired: Vec<(&TechniqueMedia, &str, i64)> = technique
                .media
                .iter()
                .zip(&media_assets)
                .enumerate()
                .map(|(i, (m, a))| (m, a.as_str(), i as i64))
                .collect();
            let media_changes = reconcile_collection(ReconcileCollection {
                writer: &self.writer,
                table: "technique_media",
                // A `None` base adopts the current rows (legacy two-way); see
                // save_technique.
                base_hashes: base.unwrap_or(&current_hashes),
                current_hashes: &current_hashes,
                desired: &desired,
                id_of: &|d| d.0.id.clone(),
                hash_of: &|d| Ok(media_hash(d.0, d.1, d.2)),
                insert: &mut |d, s| {
                    let (m, asset_id, order) = *d;
                    upsert(
                        &self.db,
                        "technique_media",
                        &m.id,
                        stamped(
                            vec![
                                // Revive (clear the tombstone) if this id was
                                // previously soft-deleted, so a reused ULID
                                // upserts instead of a PK crash.
                                col("deleted_at", None::<i64>),
                                col("technique_id", technique.id.as_str()),
                                col("asset_id", asset_id),
                                col("kind", kind_of(m)),
                                col("label", m.label.as_str()),
                                col("order_index", order),
                                col("created_at", ms(&m.added_at)),
                                col("updated_at", now),
                            ],
                            s,
                        ),
                    )
                },
                update: &mut |d, s| {
                    let (m, asset_id, order) = *d;
                    update(
                        &self.db,
                        "technique_media",
                        &m.id,
                        stamped(
                            vec![
                                col("asset_id", asset_id),
                                col("kind", kind_of(m)),
                                col("label", m.label.as_str()),
                                col("order_index", order),
                                col("updated_at", now),
                            ],
                            s,
                        ),
                    )
                },
                tombstone: &mut |id, s| self.tombstone_media(id, now, s),
            })?
            .total();

            // A media-only edit (add/remove/reorder/relabel an example)
            // leaves the technique row's hash unchanged, so the row UPDATE
            // above is skipped and updatedAt would stay stale — mis-ordering
            // the just-edited technique in the updatedAt-sorted library
            // list. Touch updatedAt locally (plain update, NO journal / HLC
            // tick) so the row rises, mirroring save_practice.
            if !technique_written && media_changes > 0 {
                update(
                    &self.db,
                    "techniques",
                    &technique.id,
                    vec![col("updated_at", now)],
                )?;
            }
            Ok(())
        })?;

        // Return the saved copy with resolved (pooled) media paths, plus
        // fresh base hashes so the caller's next save merges against what
        // this save actually left in the store.
        match self.load_technique(&technique.id) {
            Ok(reloaded) => Ok(Loaded {
                aggregate: reloaded.aggregate,
                base_hashes: reloaded.base_hashes,
                sent_hashes: Some(sent),
                reread: true,
            }),
            Err(e) => {
                // Committed all the same. Reported as a failed write, the
                // editor reverted to the technique before it and kept the
                // base before it: its next save then took the stored edit for
                // a remote one and wrote the old values back over it. What
                // was sent stands in for the re-read, as for a practice (see
                // SongPracticeRepositoryDrift's save).
                DiagnosticsLog::instance().log(
                    "save",
                    &format!(
                        "technique committed; its re-read failed: {}",
                        dart_type_name(&e)
                    ),
                );
                Ok(Loaded {
                    aggregate: technique.clone(),
                    base_hashes: sent.clone(),
                    sent_hashes: Some(sent),
                    reread: false,
                })
            }
        }
    }

    /// Builds a domain [`Technique`] from a row + its live media (paths
    /// resolved). When [base_hashes] is given, the content hash of every row
    /// this read returns is recorded — the base side of a later three-way
    /// save, hashed with the SAME projections the save path uses, over the
    /// row AS READ INTO THE MODEL: the alias/tag JSON re-encoded as a save
    /// sends it, an example kind this build doesn't know as the audio it
    /// loads as. A stored form the model can't reproduce (another build's
    /// encoding, a newer kind) would otherwise make every save rewrite the
    /// row as an edit nobody made.
    fn hydrate(
        &self,
        row: &TechniqueRow,
        media: Option<&MediaRows>,
        mut base_hashes: Option<&mut Hashes>,
    ) -> Result<Technique> {
        let own;
        let read = match media {
            Some(m) => m,
            None => {
                own = self.read_media(std::slice::from_ref(&row.id))?;
                &own
            }
        };
        let mut out = Vec::new();
        for m in read.media.get(&row.id).map(Vec::as_slice).unwrap_or(&[]) {
            let example = TechniqueMedia {
                id: m.id.clone(),
                // Keep the media even if its bytes are missing locally (carry
                // an `asset://` ref) so a reload + save never drops the
                // example.
                path: read
                    .paths
                    .get(&m.asset_id)
                    .cloned()
                    .unwrap_or_else(|| AssetStore::ref_for(&m.asset_id)),
                kind: if m.kind == "video" {
                    TechniqueMediaKind::Video
                } else {
                    TechniqueMediaKind::Audio
                },
                label: m.label.clone(),
                added_at: dt(m.created_at),
            };
            if let Some(hashes) = base_hashes.as_deref_mut() {
                hashes.insert(
                    m.id.clone(),
                    media_hash(&example, &m.asset_id, m.order_index),
                );
            }
            out.push(example);
        }
        let technique = Technique {
            id: row.id.clone(),
            name: row.name.clone(),
            aliases: strings_from(row.aliases_json.as_deref().unwrap_or("[]"), "aliases_json")?,
            tags: strings_from(row.tags_json.as_deref().unwrap_or("[]"), "tags_json")?,
            summary: row.summary.clone(),
            description: row.description.clone(),
            cues: row.cues.clone(),
            exercises: row.exercises.clone(),
            acoustic_signature: row.acoustic_signature.clone(),
            media: out,
            created_at: dt(row.created_at),
            updated_at: dt(row.updated_at),
        };
        if let Some(hashes) = base_hashes {
            hashes.insert(row.id.clone(), technique_hash(&technique)?);
        }
        Ok(technique)
    }

    /// The live media of [technique_ids] grouped by technique, each group in
    /// (order_index, id) order — stable tie-break for equal indices
    /// (concurrent offline inserts): ULID ids order by creation time, so
    /// every replica agrees — plus each media asset's local path (absent =
    /// bytes not here). Asset rows are read live or tombstoned, like
    /// [`AssetStore::resolve`].
    fn read_media(&self, technique_ids: &[String]) -> Result<MediaRows> {
        let mut rows: Vec<TechniqueMediaRow> = Vec::new();
        for chunk in AssetStore::in_chunks(technique_ids) {
            rows.extend(self.db.select::<TechniqueMediaRow>(
                &format!(
                    "technique_id IN ({}) AND deleted_at IS NULL \
                     ORDER BY technique_id, order_index, id",
                    placeholders(chunk.len())
                ),
                rusqlite::params_from_iter(chunk),
            )?);
        }
        let asset_ids: IndexSet<String> = rows.iter().map(|m| m.asset_id.clone()).collect();
        let asset_ids: Vec<String> = asset_ids.into_iter().collect();
        let mut asset_rows: Vec<AssetRow> = Vec::new();
        for chunk in AssetStore::in_chunks(&asset_ids) {
            asset_rows.extend(self.db.select::<AssetRow>(
                &format!("id IN ({})", placeholders(chunk.len())),
                rusqlite::params_from_iter(chunk),
            )?);
        }
        let mut paths = HashMap::new();
        for a in &asset_rows {
            if let Some(path) = self.assets.resolve_row(a) {
                paths.insert(a.id.clone(), path);
            }
        }
        let mut media: IndexMap<String, Vec<TechniqueMediaRow>> = IndexMap::new();
        for m in rows {
            media.entry(m.technique_id.clone()).or_default().push(m);
        }
        Ok(MediaRows { media, paths })
    }
}

impl TechniqueRepository for TechniqueRepositoryDrift {
    fn list_techniques(&self) -> Result<Vec<Technique>> {
        let mut rows: Vec<TechniqueRow> = self.db.select("deleted_at IS NULL", [])?;
        // Deterministic order: updatedAt desc, then id desc as a total-order
        // tiebreaker so rows sharing a millisecond don't reshuffle between
        // launches.
        rows.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| dart::compare_strings(&b.id, &a.id))
        });
        // Every technique's media (and their assets) in one set read, not a
        // query per technique plus one per example: 601 statements and 400
        // file checks for a 200-technique library, on every library open and
        // every "add technique" picker. Should that read fail, each row
        // falls back to its own read inside the per-row guard below — the
        // old path exactly, so an unreadable row still only costs itself.
        let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let media = self.read_media(&ids).ok();
        // Skip a single corrupt row (e.g. a malformed JSON column) rather
        // than failing the whole list — the other readable records must
        // still show.
        Ok(rows
            .iter()
            .filter_map(|r| self.hydrate(r, media.as_ref(), None).ok())
            .collect())
    }

    fn get_technique(&self, id: &str) -> Result<Technique> {
        self.load_technique(id).map(|loaded| loaded.aggregate)
    }

    fn load_technique(&self, id: &str) -> Result<Loaded<Technique>> {
        let hook = self.debug_load_hook.lock().clone();
        if let Some(hook) = hook {
            hook(id)?;
        }
        let row = self
            .db
            .select::<TechniqueRow>("id = ?1 AND deleted_at IS NULL", [id])?
            .into_iter()
            .next();
        let Some(row) = row else {
            return Err(Error::State(format!("Technique \"{id}\" not found")));
        };
        // Collect the base hash of every row this read returns, so a later
        // save can three-way merge against exactly what the editor was
        // shown.
        let mut hashes = Hashes::new();
        let technique = self.hydrate(&row, None, Some(&mut hashes))?;
        Ok(Loaded::new(technique, hashes))
    }

    fn save_technique(&self, technique: &Technique) -> Result<Technique> {
        // Base-less save: save adopts the CURRENT persisted rows as the
        // base, which reproduces the old two-way diff exactly. Safe only for
        // callers that just created or just re-read the technique (the
        // library's create flow); editors must use save_technique_with_base
        // — see the interface.
        self.save(technique, None).map(|loaded| loaded.aggregate)
    }

    fn save_technique_with_base(
        &self,
        technique: &Technique,
        base_hashes: &IndexMap<String, String>,
    ) -> Result<Loaded<Technique>> {
        self.save(technique, Some(base_hashes))
    }

    fn delete_technique(&self, id: &str) -> Result<()> {
        self.db.transaction(|| -> Result<()> {
            // Already gone (a stale list after a remote delete, a second tap)
            // or never there: nothing to delete. Tombstoned again, it
            // journaled a fresh delete — for a row that did not exist, too.
            match self.db.find::<TechniqueRow>(id)? {
                Some(parent) if parent.deleted_at.is_none() => {}
                _ => return Ok(()),
            }
            let media = self.live_media(id)?;
            let now = ms(&DateTime::now());
            for m in &media {
                let s = self.writer.stamp_for("technique_media", &m.id)?;
                self.tombstone_media(&m.id, now, &s)?;
                self.writer
                    .journal("technique_media", &m.id, op::TOMBSTONE, &s.hlc, "", None)?;
            }
            let s = self.writer.stamp_for("techniques", id)?;
            update(
                &self.db,
                "techniques",
                id,
                stamped(vec![col("deleted_at", now), col("updated_at", now)], &s),
            )?;
            self.writer
                .journal("techniques", id, op::TOMBSTONE, &s.hlc, "", None)
        })
    }
}
