//! The SQLite implementation of the song-practice store (port of
//! `lib/data/repositories/song_practice_repository_drift.dart`).
//!
//! Dart awaited every query; here every call blocks its thread (the store
//! runs off the UI thread, see PORTING.md). drift's `db.transaction` is
//! [`AppDatabase::transaction`]: every store call its closure makes on this
//! thread (the row writes, [`SyncWriter::journal`], the reconcile passes)
//! joins it, so a row write and its oplog entry commit together or not at
//! all. drift's `db.batch` (the waveform cache) is one transaction too.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use parking_lot::Mutex;
use serde_json::Value;
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx, TrackSend};
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::practice_summary::{
    PracticePreview, PracticeSummary, PreviewClip, PreviewSource,
};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_num::clamp_f64;
use vocara_model::util::dart_string;
use vocara_model::util::id::new_id;

use super::common::{
    col, dart_type_name, dt, json_decode, json_text, model_error, ms, placeholders, stamped,
    update, upsert,
};
use super::{DebugLoadHook, SongPracticeRepository};
use crate::dart;
use crate::db::rows::{
    AssetRow, ClipRow, FxBusRow, InterpretationRow, PracticeNoteRow, PracticeRow, SyncedRow,
    TechniqueMarkerRow, TrackRow, WaveformCacheRow,
};
use crate::db::{AppDatabase, AssetStore};
use crate::error::{Error, Result};
use crate::sync::reconcile::{
    reconcile_collection, reconcile_op_for, ReconcileCollection, ReconcileOp,
};
use crate::sync::sync_hashes::{
    clip_content_hash, clip_row_hash, fx_bus_content_hash, fx_bus_row_hash,
    interpretation_content_hash, interpretation_row_hash, practice_content_hash,
    practice_note_content_hash, practice_note_row_hash, practice_row_hash,
    technique_marker_content_hash, technique_marker_row_hash, track_content_hash, track_row_hash,
    ClipContent, FxBusContent, InterpretationContent, Loaded, PracticeContent, PracticeNoteContent,
    TechniqueMarkerContent, TrackContent,
};
use crate::sync::sync_writer::{op, SyncStamp, SyncWriter};

/// Analysis-code version stamped on cached waveform peaks (a pure function of
/// the immutable audio bytes; bump to invalidate after a peaks algorithm
/// change).
const WAVEFORM_ALGO_VERSION: i64 = 1;

/// The single-slot track kinds (at most one LIVE row each per practice, the
/// `ux_track_slot` index).
const SLOT_KINDS: [&str; 2] = ["accompaniment", "reference"];

/// Content hash per row id (base, sent or current).
type Hashes = IndexMap<String, String>;

/// drift/SQLite implementation of the song-practice store. The full
/// aggregate graph (practice -> interpretations / tracks -> clips / notes /
/// markers, fx buses) is normalized into rows; atomic config (a track/bus FX
/// chain, its sends, the practice reverb + lyric vector) rides as JSON
/// columns. Audio is content-addressed in the [`AssetStore`]; waveform peaks
/// are cached locally by content hash (regenerable, never synced). Every
/// write goes through the [`SyncWriter`] chokepoint + per-entity
/// [`reconcile_collection`].
pub struct SongPracticeRepositoryDrift {
    db: Arc<AppDatabase>,
    writer: Arc<SyncWriter>,
    assets: Arc<AssetStore>,
    /// Runs at the start of every [`SongPracticeRepository::load_practice`].
    /// Test-only.
    debug_load_hook: Mutex<Option<DebugLoadHook>>,
}

impl std::fmt::Debug for SongPracticeRepositoryDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SongPracticeRepositoryDrift")
            .finish_non_exhaustive()
    }
}

// A fader level and a pan as the faders reach them, as the JSON readers
// clamp them: a stored level past the fader (a peer's) went to the engine
// verbatim, and 1e30 is a gain the master's soft clip only squares off.
fn level(v: f64) -> f64 {
    clamp_f64(v, 0.0, 1.0)
}

fn pan(v: f64) -> f64 {
    clamp_f64(v, -1.0, 1.0)
}

// --- save-side content-hash projections -------------------------------------
// The MINE side of every three-way merge, hashed once per save by
// [`sent_hashes`]; the reconcile passes compare that very map, so "what a
// save sent" can never drift from what its merge compared. Hydration records
// the BASE side of the JSON-carrying rows through them too (see
// [`SongPracticeRepositoryDrift::hydrate`]).

/// The practice's reverb column exactly as a save writes it.
fn reverb_json_of(p: &SongPractice) -> Result<Option<String>> {
    p.reverb
        .as_ref()
        .map(|r| json_text(&r.to_json()))
        .transpose()
}

/// The practice's lyrics column exactly as a save writes it.
fn lyrics_json_of(p: &SongPractice) -> Result<String> {
    json_text(&Value::Array(
        p.lyrics.iter().map(LyricLine::to_json).collect(),
    ))
}

/// A sends column exactly as a save writes it.
fn sends_json_of(sends: &[TrackSend]) -> Result<String> {
    json_text(&Value::Array(
        sends.iter().map(TrackSend::to_json).collect(),
    ))
}

/// A track's FX chain column exactly as a save writes it.
fn track_fx_json_of(t: &AudioTrack) -> Result<Option<String>> {
    t.fx.as_ref().map(|fx| json_text(&fx.to_json())).transpose()
}

fn practice_hash(
    p: &SongPractice,
    separation_asset_id: Option<&str>,
    reverb_json: Option<&str>,
    lyrics_json: &str,
) -> Result<String> {
    practice_content_hash(&PracticeContent {
        title: &p.title,
        timeline_duration_ms: p.timeline_duration_ms,
        master_volume: p.master_volume,
        active_interpretation_id: p.active_version_id.as_deref(),
        separation_asset_id,
        reverb_json,
        lyrics_json: Some(lyrics_json),
    })
}

fn interpretation_hash(v: &TakeVersion, order: i64) -> String {
    interpretation_content_hash(&InterpretationContent {
        name: &v.name,
        order_index: order,
    })
}

fn bus_hash(b: &FxBus, order: i64) -> Result<String> {
    fx_bus_content_hash(&FxBusContent {
        name: &b.name,
        color_value: b.color_value,
        volume: b.volume,
        pan: b.pan,
        muted: b.muted,
        soloed: b.soloed,
        output_bus_id: b.output_bus_id.as_deref(),
        fx_json: &json_text(&b.fx.to_json())?,
        sends_json: &sends_json_of(&b.sends)?,
        order_index: order,
    })
}

fn track_hash(t: &AudioTrack, order: i64) -> Result<String> {
    track_content_hash(&TrackContent {
        kind: t.kind.name(),
        interpretation_id: t.version_id.as_deref(),
        display_name: &t.display_name,
        color_value: t.color_value,
        muted: t.muted,
        soloed: t.soloed,
        volume: t.volume,
        pan: t.pan,
        bus_id: t.bus_id.as_deref(),
        fx_json: track_fx_json_of(t)?.as_deref(),
        sends_json: &sends_json_of(&t.sends)?,
        order_index: order,
    })
}

/// One of a save's flattened lanes with the order_index it is written at.
struct TrackOrder<'a> {
    track: &'a AudioTrack,
    order: i64,
}

/// Each of [tracks] (a save's flattened lanes) with the order_index it is
/// written at: a vocal take's position among the vocal takes, 0 for the
/// accompaniment and reference slots. Hydration orders only the takes by it
/// (a slot is placed by its kind), and counting the slots in front of the
/// takes renumbered — and so rewrote, under a fresh stamp — every take
/// whenever a backing came or went. Indices an earlier build wrote with the
/// slots in front still sort the takes the same way.
fn track_orders<'a>(tracks: &[&'a AudioTrack]) -> Vec<TrackOrder<'a>> {
    let mut take = 0;
    tracks
        .iter()
        .map(|t| {
            let order = if t.is_backing() {
                0
            } else {
                take += 1;
                take - 1
            };
            TrackOrder { track: t, order }
        })
        .collect()
}

fn clip_hash(c: &AudioClip, asset_id: &str, order: i64) -> String {
    clip_content_hash(&ClipContent {
        asset_id,
        start_ms: c.start_ms,
        duration_ms: c.duration_ms,
        order_index: order,
    })
}

fn note_hash(n: &PracticeNote) -> String {
    practice_note_content_hash(&PracticeNoteContent {
        position_ms: n.position_ms,
        end_ms: n.end_ms,
        content: &n.content,
    })
}

fn marker_hash(m: &TechniqueMarker) -> String {
    technique_marker_content_hash(&TechniqueMarkerContent {
        position_ms: m.position_ms,
        end_ms: m.end_ms,
        technique_id: &m.technique_id,
        technique_name: &m.technique_name,
    })
}

/// The asset a save resolved for the clip [clip_id] (every clip of the save
/// has one).
fn asset_of<'a>(clip_asset: &'a HashMap<String, String>, clip_id: &str) -> Result<&'a str> {
    clip_asset
        .get(clip_id)
        .map(String::as_str)
        .ok_or_else(|| Error::State(format!("clip {clip_id} has no resolved asset")))
}

/// The MINE hash a save sent for the row [id] (every desired row has one).
fn mine_of(mine: &Hashes, id: &str) -> Result<String> {
    mine.get(id)
        .cloned()
        .ok_or_else(|| Error::State(format!("row {id} has no sent hash")))
}

/// The JSON columns of a practice row, as a save writes them.
struct PracticeJson<'a> {
    separation_asset_id: Option<&'a str>,
    reverb_json: Option<&'a str>,
    lyrics_json: &'a str,
}

/// rowId -> MINE hash of every row of [p] exactly as a save sends it — the
/// whole aggregate, including children the merge never reached (those of a
/// track whose remote delete it accepted). See [`Loaded::sent_hashes`].
fn sent_hashes(
    p: &SongPractice,
    tracks: &[&AudioTrack],
    clip_asset: &HashMap<String, String>,
    json: &PracticeJson<'_>,
) -> Result<Hashes> {
    let mut out = Hashes::new();
    out.insert(
        p.id.clone(),
        practice_hash(
            p,
            json.separation_asset_id,
            json.reverb_json,
            json.lyrics_json,
        )?,
    );
    for (i, v) in p.versions.iter().enumerate() {
        out.insert(v.id.clone(), interpretation_hash(v, i as i64));
    }
    for (i, b) in p.fx_buses.iter().enumerate() {
        out.insert(b.id.clone(), bus_hash(b, i as i64)?);
    }
    for d in track_orders(tracks) {
        out.insert(d.track.id.clone(), track_hash(d.track, d.order)?);
    }
    for t in tracks {
        for (i, c) in t.clips.iter().enumerate() {
            out.insert(
                c.id.clone(),
                clip_hash(c, asset_of(clip_asset, &c.id)?, i as i64),
            );
        }
        for n in &t.notes {
            out.insert(n.id.clone(), note_hash(n));
        }
        for m in &t.technique_markers {
            out.insert(m.id.clone(), marker_hash(m));
        }
    }
    Ok(out)
}

/// The live clips / notes / markers of a set of tracks, grouped by track id
/// (see [`SongPracticeRepositoryDrift::live_child_rows`]).
#[derive(Default)]
struct LiveChildren {
    clips: IndexMap<String, Vec<ClipRow>>,
    notes: IndexMap<String, Vec<PracticeNoteRow>>,
    markers: IndexMap<String, Vec<TechniqueMarkerRow>>,
}

/// [`SongPracticeRepositoryDrift::read_children`]'s result: live child rows
/// by track id, and per clip asset id its local path (absent = bytes not
/// here) and its cached peak bytes (absent = not cached).
struct ChildRows {
    live: LiveChildren,
    paths: HashMap<String, String>,
    peaks: HashMap<String, Vec<u8>>,
}

/// Groups [rows] by their track, keeping their order.
fn by_track<R>(rows: Vec<R>, track: impl Fn(&R) -> &str) -> IndexMap<String, Vec<R>> {
    let mut out: IndexMap<String, Vec<R>> = IndexMap::new();
    for r in rows {
        out.entry(track(&r).to_string()).or_default().push(r);
    }
    out
}

/// The rows of [map] under [key], or none.
fn rows_of<'a, R>(map: &'a IndexMap<String, Vec<R>>, key: &str) -> &'a [R] {
    map.get(key).map(Vec::as_slice).unwrap_or(&[])
}

/// `Float32List.fromList(peaks).buffer.asUint8List()` (little-endian, as on
/// every platform the app ships on).
fn encode_peaks(peaks: &[f64]) -> Vec<u8> {
    peaks
        .iter()
        .flat_map(|p| (*p as f32).to_le_bytes())
        .collect()
}

/// The peaks [encode_peaks] wrote; trailing bytes short of a float are
/// ignored. Decoded per clip: two clips over the same bytes never share one
/// list.
fn decode_peaks(bytes: &[u8]) -> Vec<f64> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f64::from(f32::from_le_bytes(*c)))
        .collect()
}

/// Dart's `jsonDecode(json) as List`: anything else is a `TypeError`.
fn json_list(json: &str, what: &str) -> Result<Vec<Value>> {
    match json_decode(json)? {
        Value::Array(items) => Ok(items),
        _ => Err(Error::Format(format!("{what}: not a JSON list"))),
    }
}

fn lyrics_from(json: Option<&str>) -> Result<Vec<LyricLine>> {
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    json_list(json, "lyrics_json")?
        .iter()
        .map(|m| LyricLine::from_json(m).map_err(model_error))
        .collect()
}

fn sends_from(json: Option<&str>) -> Result<Vec<TrackSend>> {
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    json_list(json, "sends_json")?
        .iter()
        .map(|m| TrackSend::from_json(m).map_err(model_error))
        .collect()
}

fn fx_from(json: &str) -> Result<TrackFx> {
    TrackFx::from_json(&json_decode(json)?).map_err(model_error)
}

impl SongPracticeRepositoryDrift {
    /// The repository over [db], journaling through [writer], with its audio
    /// in [assets].
    pub fn new(db: Arc<AppDatabase>, writer: Arc<SyncWriter>, assets: Arc<AssetStore>) -> Self {
        SongPracticeRepositoryDrift {
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

    /// Live (non-tombstoned) rows of [R]'s table whose [column] is [value].
    fn live<R: SyncedRow>(&self, column: &str, value: &str) -> Result<Vec<R>> {
        self.db
            .select(&format!("{column} = ?1 AND deleted_at IS NULL"), [value])
    }

    /// Sorts practice rows newest first: updatedAt desc, then id desc as a
    /// total-order tiebreaker so rows sharing a millisecond don't reshuffle
    /// between launches.
    fn sort_newest_first(rows: &mut [PracticeRow]) {
        rows.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| dart::compare_strings(&b.id, &a.id))
        });
    }

    // --- write ----------------------------------------------------------------

    /// The one save core. [base] is the per-row content-hash map captured
    /// when the caller loaded the aggregate (three-way merge); `None` means
    /// "adopt the current persisted state as base" (the legacy two-way
    /// diff).
    ///
    /// The WHOLE import+commit sequence runs under the store write gate's
    /// shared side: asset imports land before the clip/media rows referencing
    /// them commit, and a maintenance pass (Persistence's repairLibrary / the
    /// boot sweeps, which hold the gate exclusively) running in that window
    /// would tombstone the just-imported asset and trash its pool file.
    fn save(&self, practice: &SongPractice, base: Option<&Hashes>) -> Result<Loaded<SongPractice>> {
        self.assets
            .gate()
            .shared(|| self.save_ungated(practice, base))
    }

    fn save_ungated(
        &self,
        practice: &SongPractice,
        base: Option<&Hashes>,
    ) -> Result<Loaded<SongPractice>> {
        // All tracks flattened (accompaniment + reference + vocals).
        let all_tracks: Vec<&AudioTrack> = practice
            .accompaniment
            .iter()
            .chain(practice.reference.iter())
            .chain(practice.vocal_tracks.iter())
            .map(|t| t.as_ref())
            .collect();

        // A deleted practice takes no save (the transaction refuses it too,
        // see upsert_practice), and is turned away before the imports below:
        // asked only there, its new files were already imported and
        // journaled for a save that then failed.
        if let Some(parent) = self.db.find::<PracticeRow>(&practice.id)? {
            if parent.deleted_at.is_some() {
                return Err(Error::State(format!(
                    "practice {} is deleted; refusing to save over it",
                    practice.id
                )));
            }
        }

        // Assets first (idempotent, deduped): every clip's audio + the
        // separation source. A dependent clip row never exists without its
        // asset.
        let clips: Vec<&AudioClip> = all_tracks.iter().flat_map(|t| t.clips.iter()).collect();
        // resolve_or_import_all: an `asset://` ref (bytes missing locally) or
        // a live asset's pool path (every hydrated clip) keeps its id without
        // re-importing; any other path is imported + deduped — per clip, in
        // order, exactly as resolve_or_import would answer.
        let paths: Vec<&str> = clips.iter().map(|c| c.audio_file_path.as_str()).collect();
        let clip_asset_ids = self
            .assets
            .resolve_or_import_all(&paths, AssetStore::DEFAULT_KIND)?;
        let mut clip_asset: HashMap<String, String> = HashMap::new(); // clipId -> assetId
        for (c, a) in clips.iter().zip(&clip_asset_ids) {
            clip_asset.insert(c.id.clone(), a.clone());
        }
        let waveforms: Vec<(&str, &[f64])> = clips
            .iter()
            .zip(&clip_asset_ids)
            .map(|(c, a)| (a.as_str(), c.waveform_peaks.as_slice()))
            .collect();
        self.cache_waveforms(&waveforms)?;
        let separation_asset_id = match practice.separation_source.as_deref() {
            Some(src) if !src.is_empty() => Some(
                self.assets
                    .resolve_or_import(src, AssetStore::DEFAULT_KIND)?,
            ),
            _ => None,
        };

        let now = ms(&DateTime::now());
        let reverb_json = reverb_json_of(practice)?;
        let lyrics_json = lyrics_json_of(practice)?;
        let json = PracticeJson {
            separation_asset_id: separation_asset_id.as_deref(),
            reverb_json: reverb_json.as_deref(),
            lyrics_json: &lyrics_json,
        };
        // Before the write, so nothing after the commit can fail the save.
        let sent = sent_hashes(practice, &all_tracks, &clip_asset, &json)?;

        self.db.transaction(|| -> Result<()> {
            self.db.execute_batch("PRAGMA defer_foreign_keys = ON")?;

            // --- practice row (single-entity reconcile) ---
            // Every pass below compares the MINE hashes in [sent], never
            // hashes the model again: what a save sent is then what its merge
            // compared by construction, and the rows are hashed once per
            // save, not twice.
            let practice_written = self.upsert_practice(practice, &json, now, base, &sent)?;

            // --- interpretations ---
            let mut child_changes = self.reconcile_interpretations(practice, now, base, &sent)?;

            // --- fx buses (before tracks so a track's bus_id resolves) ---
            child_changes += self.reconcile_fx_buses(practice, now, base, &sent)?;

            // --- tracks (+ cascade their clips / notes / markers) ---
            child_changes +=
                self.reconcile_tracks(practice, &all_tracks, &clip_asset, now, base, &sent)?;

            // A child-only edit (clip move, note/marker, fader, add/remove an
            // interpretation or FX bus) updates its own row but leaves the
            // practice row's updatedAt stale, so the home list (ordered by
            // updatedAt) wouldn't surface the just-edited practice. Touch
            // updatedAt to the children's true modification time — local
            // ordering ONLY: no HLC tick and no oplog entry (those would
            // manufacture a phantom sync op for a row that didn't change).
            if !practice_written && child_changes > 0 {
                update(
                    &self.db,
                    "practices",
                    &practice.id,
                    vec![col("updated_at", now)],
                )?;
            }
            Ok(())
        })?;

        // Re-read the saved aggregate WITH fresh base hashes, so the caller's
        // next save merges against what this save actually left in the store
        // (including any concurrent rows the three-way rules kept) — plus the
        // hashes of what it SENT, for a caller whose model moved on
        // meanwhile.
        match self.load_practice(&practice.id) {
            Ok(reloaded) => Ok(Loaded {
                aggregate: reloaded.aggregate,
                base_hashes: reloaded.base_hashes,
                sent_hashes: Some(sent),
                reread: true,
            }),
            Err(e) => {
                // Committed all the same. Reported as a failed write, the
                // caller reverted to a store that held it, or took its change
                // back over it (a separation's undo then collided with its
                // own stored backing). What was sent stands in for the
                // re-read: it is what the caller's model derives from, and
                // what the next merge must hold it to.
                DiagnosticsLog::instance().log(
                    "save",
                    &format!(
                        "practice committed; its re-read failed: {}",
                        dart_type_name(&e)
                    ),
                );
                Ok(Loaded {
                    aggregate: practice.clone(),
                    base_hashes: sent.clone(),
                    sent_hashes: Some(sent),
                    reread: false,
                })
            }
        }
    }

    // --- write helpers --------------------------------------------------------

    /// Writes the practice-level row. Returns whether the row was actually
    /// written (insert or content change) — false on the no-change early
    /// return AND on the three-way "keep theirs" skip, so the caller knows
    /// whether updatedAt was already bumped here.
    fn upsert_practice(
        &self,
        p: &SongPractice,
        json: &PracticeJson<'_>,
        now: i64,
        base: Option<&Hashes>,
        mine: &Hashes,
    ) -> Result<bool> {
        let existing = self.db.find::<PracticeRow>(&p.id)?;
        if existing.as_ref().is_some_and(|e| e.deleted_at.is_some()) {
            // The practice was tombstoned before this save landed (e.g. an
            // in-flight save racing delete_practice, or a synced-in delete).
            // Continuing would NOT revive the practice (the update below
            // never clears deleted_at) yet would still commit LIVE child rows
            // under the tombstoned parent — a journaled zombie subtree. A
            // deleted aggregate must not be implicitly re-saved — for the
            // AGGREGATE ROOT this abort deliberately outranks the per-row
            // edit-vs-delete "active editor wins" rule: abort the whole save
            // (the transaction rolls back, no children are written) so the
            // caller surfaces an error, like the pre-upsert-revive PK
            // failure.
            return Err(Error::State(format!(
                "practice {} is deleted; refusing to save over it",
                p.id
            )));
        }
        let hash = mine_of(mine, &p.id)?;
        let Some(existing) = existing else {
            let s = self.writer.stamp();
            self.db.insert(&PracticeRow {
                id: p.id.clone(),
                created_at: ms(&p.created_at),
                updated_at: ms(&p.updated_at),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                title: p.title.clone(),
                timeline_duration_ms: p.timeline_duration_ms,
                master_volume: p.master_volume,
                active_interpretation_id: p.active_version_id.clone(),
                separation_asset_id: json.separation_asset_id.map(str::to_string),
                reverb_json: json.reverb_json.map(str::to_string),
                lyrics_json: Some(json.lyrics_json.to_string()),
                ..Default::default()
            })?;
            self.writer
                .journal("practices", &p.id, op::INSERT, &s.hlc, &hash, None)?;
            return Ok(true);
        };
        let existing_hash = practice_row_hash(&existing)?;
        if existing_hash == hash {
            return Ok(false);
        }
        // Three-way merge on the parent row itself (same rules as
        // reconcile_collection). A `None` [base] adopts the current row as
        // base (legacy two-way — the skip below then never fires); a
        // with-base map missing this id means the editor never held the row,
        // which degrades to conflict-local-wins below.
        let base_hash = match base {
            None => Some(existing_hash.as_str()),
            Some(b) => b.get(&p.id).map(String::as_str),
        };
        if let Some(base_hash) = base_hash {
            if existing_hash != base_hash && hash == base_hash {
                // The persisted row moved on since this editor loaded it (a
                // remote/newer write) and the editor never touched the
                // practice-level fields: keep the DB state. Writing "mine"
                // would revert the newer row to stale values under a fresh
                // HLC. Not "written" — a child-only change may still bump
                // updatedAt locally in the caller.
                return Ok(false);
            }
        }
        // Plain local edit (existing_hash == base_hash) or a true conflict
        // (both sides changed): the ACTIVE EDITOR wins by policy and mine is
        // written with a fresh HLC, so it also wins entity-level LWW
        // downstream.
        let s = self.writer.stamp_for("practices", &p.id)?;
        update(
            &self.db,
            "practices",
            &p.id,
            stamped(
                vec![
                    col("title", p.title.as_str()),
                    col("timeline_duration_ms", p.timeline_duration_ms),
                    col("master_volume", p.master_volume),
                    col("active_interpretation_id", p.active_version_id.as_deref()),
                    col("separation_asset_id", json.separation_asset_id),
                    col("reverb_json", json.reverb_json),
                    col("lyrics_json", json.lyrics_json),
                    col("updated_at", now),
                ],
                &s,
            ),
        )?;
        self.writer
            .journal("practices", &p.id, op::UPDATE, &s.hlc, &hash, None)?;
        Ok(true)
    }

    fn reconcile_interpretations(
        &self,
        p: &SongPractice,
        now: i64,
        base: Option<&Hashes>,
        mine: &Hashes,
    ) -> Result<u32> {
        let live: Vec<InterpretationRow> = self.live("practice_id", &p.id)?;
        let current: Hashes = live
            .iter()
            .map(|r| (r.id.clone(), interpretation_row_hash(r)))
            .collect();
        let desired: Vec<(&TakeVersion, i64)> = p
            .versions
            .iter()
            .enumerate()
            .map(|(i, v)| (v.as_ref(), i as i64))
            .collect();
        let result = reconcile_collection(ReconcileCollection {
            writer: &self.writer,
            table: "interpretations",
            // A `None` base (legacy no-base save) adopts the current persisted
            // rows, reproducing the old two-way diff; see save_practice.
            base_hashes: base.unwrap_or(&current),
            current_hashes: &current,
            desired: &desired,
            id_of: &|d| d.0.id.clone(),
            hash_of: &|d| mine_of(mine, &d.0.id),
            insert: &mut |d, s| {
                upsert(
                    &self.db,
                    "interpretations",
                    &d.0.id,
                    stamped(
                        vec![
                            // Revive (clear the tombstone) if this id was
                            // previously soft-deleted, so a reused ULID
                            // upserts instead of a PK crash.
                            col("deleted_at", None::<i64>),
                            col("practice_id", p.id.as_str()),
                            col("name", d.0.name.as_str()),
                            col("order_index", d.1),
                            col("created_at", ms(&d.0.created_at)),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            update: &mut |d, s| {
                update(
                    &self.db,
                    "interpretations",
                    &d.0.id,
                    stamped(
                        vec![
                            col("name", d.0.name.as_str()),
                            col("order_index", d.1),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            tombstone: &mut |id, s| self.tombstone_row("interpretations", id, now, s),
        })?;
        Ok(result.total())
    }

    fn reconcile_fx_buses(
        &self,
        p: &SongPractice,
        now: i64,
        base: Option<&Hashes>,
        mine: &Hashes,
    ) -> Result<u32> {
        let live: Vec<FxBusRow> = self.live("practice_id", &p.id)?;
        let current = live
            .iter()
            .map(|r| Ok((r.id.clone(), fx_bus_row_hash(r)?)))
            .collect::<Result<Hashes>>()?;
        let desired: Vec<(&FxBus, i64)> = p
            .fx_buses
            .iter()
            .enumerate()
            .map(|(i, b)| (b.as_ref(), i as i64))
            .collect();
        let result = reconcile_collection(ReconcileCollection {
            writer: &self.writer,
            table: "fx_buses",
            // A `None` base adopts the current rows (legacy two-way); see
            // save_practice.
            base_hashes: base.unwrap_or(&current),
            current_hashes: &current,
            desired: &desired,
            id_of: &|d| d.0.id.clone(),
            hash_of: &|d| mine_of(mine, &d.0.id),
            insert: &mut |d, s| {
                let bus = d.0;
                upsert(
                    &self.db,
                    "fx_buses",
                    &bus.id,
                    stamped(
                        vec![
                            // Revive (clear the tombstone) if this id was
                            // previously soft-deleted, so a reused ULID
                            // upserts instead of a PK crash.
                            col("deleted_at", None::<i64>),
                            col("practice_id", p.id.as_str()),
                            col("name", bus.name.as_str()),
                            col("color_value", bus.color_value),
                            col("volume", bus.volume),
                            col("pan", bus.pan),
                            col("muted", bus.muted),
                            col("soloed", bus.soloed),
                            col("output_bus_id", bus.output_bus_id.as_deref()),
                            col("fx_json", json_text(&bus.fx.to_json())?),
                            col("sends_json", sends_json_of(&bus.sends)?),
                            col("order_index", d.1),
                            col("created_at", now),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            update: &mut |d, s| {
                let bus = d.0;
                update(
                    &self.db,
                    "fx_buses",
                    &bus.id,
                    stamped(
                        vec![
                            col("name", bus.name.as_str()),
                            col("color_value", bus.color_value),
                            col("volume", bus.volume),
                            col("pan", bus.pan),
                            col("muted", bus.muted),
                            col("soloed", bus.soloed),
                            col("output_bus_id", bus.output_bus_id.as_deref()),
                            col("fx_json", json_text(&bus.fx.to_json())?),
                            col("sends_json", sends_json_of(&bus.sends)?),
                            col("order_index", d.1),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            tombstone: &mut |id, s| self.tombstone_row("fx_buses", id, now, s),
        })?;
        Ok(result.total())
    }

    fn reconcile_tracks(
        &self,
        p: &SongPractice,
        tracks: &[&AudioTrack],
        clip_asset: &HashMap<String, String>,
        now: i64,
        base: Option<&Hashes>,
        mine: &Hashes,
    ) -> Result<u32> {
        let live: Vec<TrackRow> = self.live("practice_id", &p.id)?;
        let current = live
            .iter()
            .map(|r| Ok((r.id.clone(), track_row_hash(r)?)))
            .collect::<Result<Hashes>>()?;
        let desired = track_orders(tracks);
        let base_or_current = base.unwrap_or(&current);

        // A slot MOVE between live rows — the separation flow's rotation (the
        // old accompaniment re-kinded to reference while a fresh instrumental
        // takes its slot) or a symmetric swap — cannot be saved by any
        // statement order: ux_track_slot is enforced per statement, so
        // whichever row writes first collides with the other still holding
        // its old kind. Two-phase instead: every live slot row this pass WILL
        // re-kind (predicted with [reconcile_op_for], the same rules the pass
        // runs on — a rule-2/5 skip must NOT be vacated, its DB kind is being
        // kept) is parked as 'vocal' first — outside the partial index, still
        // within the kind CHECK. The row's real update then writes its final
        // kind inside this same transaction, so the parked value is never
        // observable outside it.
        let desired_by_id: HashMap<&str, &TrackOrder<'_>> =
            desired.iter().map(|d| (d.track.id.as_str(), d)).collect();
        for r in &live {
            if !SLOT_KINDS.contains(&r.kind.as_str()) {
                continue;
            }
            let Some(d) = desired_by_id.get(r.id.as_str()) else {
                continue;
            };
            if d.track.kind.name() == r.kind {
                continue;
            }
            let op = reconcile_op_for(
                &mine_of(mine, &r.id)?,
                current.get(&r.id).map(String::as_str),
                base_or_current.get(&r.id).map(String::as_str),
            );
            if op != ReconcileOp::Update {
                continue;
            }
            self.db.execute(
                "UPDATE tracks SET kind = ? WHERE id = ?",
                ["vocal", r.id.as_str()],
            )?;
        }

        let mut changed = reconcile_collection(ReconcileCollection {
            writer: &self.writer,
            table: "tracks",
            // A `None` base adopts the current rows (legacy two-way); see
            // save_practice.
            base_hashes: base_or_current,
            current_hashes: &current,
            desired: &desired,
            id_of: &|d| d.track.id.clone(),
            hash_of: &|d| mine_of(mine, &d.track.id),
            insert: &mut |d, s| {
                let t = d.track;
                upsert(
                    &self.db,
                    "tracks",
                    &t.id,
                    stamped(
                        vec![
                            // Revive (clear the tombstone) if this id was
                            // previously soft-deleted, so a reused ULID
                            // upserts instead of a PK crash.
                            col("deleted_at", None::<i64>),
                            col("practice_id", p.id.as_str()),
                            col("kind", t.kind.name()),
                            col("interpretation_id", t.version_id.as_deref()),
                            col("display_name", t.display_name.as_str()),
                            col("color_value", t.color_value),
                            col("muted", t.muted),
                            col("soloed", t.soloed),
                            col("volume", t.volume),
                            col("pan", t.pan),
                            col("bus_id", t.bus_id.as_deref()),
                            col("fx_json", track_fx_json_of(t)?),
                            col("sends_json", sends_json_of(&t.sends)?),
                            col("order_index", d.order),
                            col("created_at", ms(&t.created_at)),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            update: &mut |d, s| {
                let t = d.track;
                update(
                    &self.db,
                    "tracks",
                    &t.id,
                    stamped(
                        vec![
                            col("kind", t.kind.name()),
                            col("interpretation_id", t.version_id.as_deref()),
                            col("display_name", t.display_name.as_str()),
                            col("color_value", t.color_value),
                            col("muted", t.muted),
                            col("soloed", t.soloed),
                            col("volume", t.volume),
                            col("pan", t.pan),
                            col("bus_id", t.bus_id.as_deref()),
                            col("fx_json", track_fx_json_of(t)?),
                            col("sends_json", sends_json_of(&t.sends)?),
                            col("order_index", d.order),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            tombstone: &mut |id, s| self.cascade_tombstone_track(id, now, Some(s)),
        })?
        .total();

        // Children of every surviving track. Note the composition with the
        // per-row three-way rules: if a track was deleted remotely (cascade-
        // tombstoning its children) and the local editor's track-level edit
        // revives the track row, only children the editor ALSO edited are
        // revived here — untouched children accept the remote delete (rule
        // 5), row by row.
        //
        // A track whose delete was ACCEPTED (rule 5's untouched side:
        // remotely deleted, unedited here) is NOT live after the pass above,
        // and its children must go with it. Reconciling them anyway would
        // revive every child the editor edited — and insert every child it
        // added — as a LIVE row hanging off a tombstoned parent: invisible
        // locally, yet synced out as an orphan. Re-read the live set rather
        // than infer it.
        let live_track_ids: HashSet<String> = self
            .live::<TrackRow>("practice_id", &p.id)?
            .into_iter()
            .map(|r| r.id)
            .collect();
        //
        // Their live children in one set read per table, not three per
        // track (90 for a 30-lane practice, on every save). Read after the
        // track pass, whose cascades are the only writes to other tracks'
        // children: each pass below writes its own track's rows alone, since
        // local edits never move a child id to another parent (cross-parent
        // transplants mint fresh ids; see reconcile.rs), so what is listed
        // under a track stays its own until that track's pass.
        let reconciled: Vec<&AudioTrack> = tracks
            .iter()
            .copied()
            .filter(|t| live_track_ids.contains(&t.id))
            .collect();
        let ids: Vec<String> = reconciled.iter().map(|t| t.id.clone()).collect();
        let children = self.live_child_rows(&ids)?;
        for t in reconciled {
            changed += self.reconcile_clips(
                t,
                clip_asset,
                now,
                base,
                mine,
                rows_of(&children.clips, &t.id),
            )?;
            changed += self.reconcile_notes(t, now, base, mine, rows_of(&children.notes, &t.id))?;
            changed +=
                self.reconcile_markers(t, now, base, mine, rows_of(&children.markers, &t.id))?;
        }
        Ok(changed)
    }

    fn reconcile_clips(
        &self,
        t: &AudioTrack,
        clip_asset: &HashMap<String, String>,
        now: i64,
        base: Option<&Hashes>,
        mine: &Hashes,
        live: &[ClipRow],
    ) -> Result<u32> {
        let current: Hashes = live
            .iter()
            .map(|r| (r.id.clone(), clip_row_hash(r)))
            .collect();
        let desired = t
            .clips
            .iter()
            .enumerate()
            .map(|(i, c)| Ok((c, asset_of(clip_asset, &c.id)?, i as i64)))
            .collect::<Result<Vec<(&AudioClip, &str, i64)>>>()?;
        let result = reconcile_collection(ReconcileCollection {
            writer: &self.writer,
            table: "clips",
            // A `None` base adopts the current rows (legacy two-way); see
            // save_practice.
            base_hashes: base.unwrap_or(&current),
            current_hashes: &current,
            desired: &desired,
            id_of: &|d| d.0.id.clone(),
            hash_of: &|d| mine_of(mine, &d.0.id),
            insert: &mut |d, s| {
                let (clip, asset_id, order) = *d;
                upsert(
                    &self.db,
                    "clips",
                    &clip.id,
                    stamped(
                        vec![
                            // Revive (clear the tombstone) if this id was
                            // previously soft-deleted, so a reused ULID
                            // upserts instead of a PK crash.
                            col("deleted_at", None::<i64>),
                            col("track_id", t.id.as_str()),
                            col("asset_id", asset_id),
                            col("start_ms", clip.start_ms),
                            col("duration_ms", clip.duration_ms),
                            col("order_index", order),
                            col("created_at", now),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            update: &mut |d, s| {
                let (clip, asset_id, order) = *d;
                update(
                    &self.db,
                    "clips",
                    &clip.id,
                    stamped(
                        vec![
                            col("asset_id", asset_id),
                            col("start_ms", clip.start_ms),
                            col("duration_ms", clip.duration_ms),
                            col("order_index", order),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            tombstone: &mut |id, s| self.tombstone_row("clips", id, now, s),
        })?;
        Ok(result.total())
    }

    fn reconcile_notes(
        &self,
        t: &AudioTrack,
        now: i64,
        base: Option<&Hashes>,
        mine: &Hashes,
        live: &[PracticeNoteRow],
    ) -> Result<u32> {
        let current: Hashes = live
            .iter()
            .map(|r| (r.id.clone(), practice_note_row_hash(r)))
            .collect();
        let result = reconcile_collection(ReconcileCollection {
            writer: &self.writer,
            table: "practice_notes",
            // A `None` base adopts the current rows (legacy two-way); see
            // save_practice.
            base_hashes: base.unwrap_or(&current),
            current_hashes: &current,
            desired: &t.notes,
            id_of: &|n| n.id.clone(),
            hash_of: &|n| mine_of(mine, &n.id),
            insert: &mut |n, s| {
                upsert(
                    &self.db,
                    "practice_notes",
                    &n.id,
                    stamped(
                        vec![
                            // Revive (clear the tombstone) if this id was
                            // previously soft-deleted, so a reused ULID
                            // upserts instead of a PK crash.
                            col("deleted_at", None::<i64>),
                            col("track_id", t.id.as_str()),
                            col("position_ms", n.position_ms),
                            col("end_ms", n.end_ms),
                            col("content", n.content.as_str()),
                            col("created_at", ms(&n.created_at)),
                            col("updated_at", ms(&n.updated_at)),
                        ],
                        s,
                    ),
                )
            },
            update: &mut |n, s| {
                update(
                    &self.db,
                    "practice_notes",
                    &n.id,
                    stamped(
                        vec![
                            col("position_ms", n.position_ms),
                            col("end_ms", n.end_ms),
                            col("content", n.content.as_str()),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            tombstone: &mut |id, s| self.tombstone_row("practice_notes", id, now, s),
        })?;
        Ok(result.total())
    }

    fn reconcile_markers(
        &self,
        t: &AudioTrack,
        now: i64,
        base: Option<&Hashes>,
        mine: &Hashes,
        live: &[TechniqueMarkerRow],
    ) -> Result<u32> {
        let current: Hashes = live
            .iter()
            .map(|r| (r.id.clone(), technique_marker_row_hash(r)))
            .collect();
        let result = reconcile_collection(ReconcileCollection {
            writer: &self.writer,
            table: "technique_markers",
            // A `None` base adopts the current rows (legacy two-way); see
            // save_practice.
            base_hashes: base.unwrap_or(&current),
            current_hashes: &current,
            desired: &t.technique_markers,
            id_of: &|m| m.id.clone(),
            hash_of: &|m| mine_of(mine, &m.id),
            insert: &mut |m, s| {
                upsert(
                    &self.db,
                    "technique_markers",
                    &m.id,
                    stamped(
                        vec![
                            // Revive (clear the tombstone) if this id was
                            // previously soft-deleted, so a reused ULID
                            // upserts instead of a PK crash.
                            col("deleted_at", None::<i64>),
                            col("track_id", t.id.as_str()),
                            col("position_ms", m.position_ms),
                            col("end_ms", m.end_ms),
                            col("technique_id", m.technique_id.as_str()),
                            col("technique_name_snapshot", m.technique_name.as_str()),
                            col("created_at", ms(&m.created_at)),
                            col("updated_at", ms(&m.updated_at)),
                        ],
                        s,
                    ),
                )
            },
            update: &mut |m, s| {
                update(
                    &self.db,
                    "technique_markers",
                    &m.id,
                    stamped(
                        vec![
                            col("position_ms", m.position_ms),
                            col("end_ms", m.end_ms),
                            col("technique_id", m.technique_id.as_str()),
                            col("technique_name_snapshot", m.technique_name.as_str()),
                            col("updated_at", now),
                        ],
                        s,
                    ),
                )
            },
            tombstone: &mut |id, s| self.tombstone_row("technique_markers", id, now, s),
        })?;
        Ok(result.total())
    }

    /// Tombstones a track and its live clips / notes / markers (a removed
    /// track must never leave live children dangling). The children are not
    /// separately reconciled (only surviving tracks' children are), so they
    /// self-journal.
    ///
    /// [row_stamp] is set ONLY when called from the tracks reconcile: there
    /// the track is the reconciled entity, so [`reconcile_collection`] emits
    /// its oplog entry and the row must take that exact stamp WITHOUT
    /// self-journaling (else the row gets a second, HLC-mismatched tombstone
    /// entry). On the standalone delete_practice path [row_stamp] is `None`
    /// and the track row self-journals.
    fn cascade_tombstone_track(
        &self,
        track_id: &str,
        now: i64,
        row_stamp: Option<&SyncStamp>,
    ) -> Result<()> {
        for c in self.live::<ClipRow>("track_id", track_id)? {
            self.tombstone("clips", &c.id, now)?;
        }
        for n in self.live::<PracticeNoteRow>("track_id", track_id)? {
            self.tombstone("practice_notes", &n.id, now)?;
        }
        for m in self.live::<TechniqueMarkerRow>("track_id", track_id)? {
            self.tombstone("technique_markers", &m.id, now)?;
        }
        match row_stamp {
            Some(s) => self.tombstone_row("tracks", track_id, now, s),
            None => self.tombstone("tracks", track_id, now),
        }
    }

    // --- generic helpers ------------------------------------------------------

    /// Soft-deletes a row using a stamp MINTED BY THE CALLER and does NOT
    /// journal — for the reconcile path, where [`reconcile_collection`]
    /// emits the one oplog entry. Taking the caller's stamp keeps the row's
    /// HLC equal to that entry's (self-journaling here would add a second,
    /// HLC-mismatched tombstone op).
    fn tombstone_row(&self, table: &str, id: &str, now: i64, s: &SyncStamp) -> Result<()> {
        self.db.execute(
            &format!(
                "UPDATE {table} SET deleted_at = ?, updated_at = ?, \
                 hlc_wall = ?, hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?"
            ),
            rusqlite::params![
                now,
                now,
                s.hlc.wall_ms,
                s.hlc.counter,
                s.hlc.node,
                s.hlc.packed(),
                id
            ],
        )?;
        Ok(())
    }

    /// Soft-deletes a row + journals it. Works for any synced table via raw
    /// SQL so one helper covers them all.
    fn tombstone(&self, table: &str, id: &str, now: i64) -> Result<()> {
        let s = self.writer.stamp_for(table, id)?;
        self.tombstone_row(table, id, now, &s)?;
        self.writer
            .journal(table, id, op::TOMBSTONE, &s.hlc, "", None)
    }

    /// Caches each clip's peaks under its asset's content hash: one asset
    /// read and ONE batch (a single transaction) for the whole save. It used
    /// to be a select plus an auto-committed upsert per clip — 100 write
    /// transactions on every save of a 100-clip practice, each rewriting a
    /// blob that almost never changed (the peaks were hydrated from this very
    /// row). The upsert's WHERE skips a row whose bytes already match, and
    /// the batch runs in clip order, so the final cache is exactly the
    /// per-clip loop's (last write per hash wins).
    fn cache_waveforms(&self, clips: &[(&str, &[f64])]) -> Result<()> {
        let pending: Vec<&(&str, &[f64])> = clips.iter().filter(|c| !c.1.is_empty()).collect();
        if pending.is_empty() {
            return Ok(());
        }
        let ids: IndexSet<String> = pending.iter().map(|c| c.0.to_string()).collect();
        let hashes = self.content_hashes(&ids.into_iter().collect::<Vec<_>>())?;
        let writes: Vec<(&String, &[f64])> = pending
            .iter()
            .filter_map(|c| hashes.get(c.0).map(|hash| (hash, c.1)))
            .collect();
        if writes.is_empty() {
            return Ok(());
        }
        self.db.transaction(|| -> Result<()> {
            for (hash, peaks) in &writes {
                self.db.execute(
                    "INSERT INTO waveform_cache (content_hash, algo_version, peaks) \
                     VALUES (?, ?, ?) ON CONFLICT (content_hash, algo_version) \
                     DO UPDATE SET peaks = excluded.peaks \
                     WHERE waveform_cache.peaks IS NOT excluded.peaks",
                    rusqlite::params![hash, WAVEFORM_ALGO_VERSION, encode_peaks(peaks)],
                )?;
            }
            Ok(())
        })
    }

    /// assetId -> content hash for every id in [ids] that has an asset row
    /// (live or tombstoned, like the per-clip lookups it replaces).
    fn content_hashes(&self, ids: &[String]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        for a in self.asset_rows(ids)? {
            out.insert(a.id, a.content_hash);
        }
        Ok(out)
    }

    /// The asset rows (live or tombstoned) of [ids], one set read per
    /// [`AssetStore::IN_QUERY_CHUNK`] ids.
    fn asset_rows(&self, ids: &[String]) -> Result<Vec<AssetRow>> {
        let mut out = Vec::new();
        for chunk in AssetStore::in_chunks(ids) {
            out.extend(self.db.select::<AssetRow>(
                &format!("id IN ({})", placeholders(chunk.len())),
                rusqlite::params_from_iter(chunk),
            )?);
        }
        Ok(out)
    }

    // --- hydration ------------------------------------------------------------

    /// Builds the domain aggregate from [row]. When [base_hashes] is given,
    /// the content hash of EVERY row this read returns (the practice row and
    /// each live child row, whether or not it survives domain-level dedup)
    /// is recorded — the base side of a later three-way save. The
    /// projections come from sync_hashes, the SAME functions the save path
    /// hashes with.
    ///
    /// A row with JSON columns (the practice's reverb and lyrics, a track's
    /// or bus's chain and sends) is hashed as READ INTO THE MODEL — decoded
    /// and re-encoded the way a save sends it — not as stored. Decoding
    /// clamps out-of-range values and drops keys this build doesn't know, so
    /// a stored form the model can't reproduce (another build's, a hand
    /// edit) made every save see the row as edited: rewritten as a change
    /// nobody made, and a concurrent edit of it by another device reverted.
    /// Its own order index stays the stored one: a renumbering must be
    /// written, or the rows it skipped would interleave stale indices with
    /// the ones it wrote.
    fn hydrate(
        &self,
        row: &PracticeRow,
        mut base_hashes: Option<&mut Hashes>,
    ) -> Result<SongPractice> {
        let interp_rows: Vec<InterpretationRow> = self.db.select(
            // Ties are possible (two devices inserting offline pick the same
            // index); without a second key SQLite is free to order them
            // differently on each device, and the next save writes each
            // machine's accidental order back as real indices. The id is a
            // ULID, so it breaks ties by creation time and every replica
            // lands on the same list.
            "practice_id = ?1 AND deleted_at IS NULL ORDER BY order_index, id",
            [&row.id],
        )?;
        let versions: Vec<Arc<TakeVersion>> = interp_rows
            .iter()
            .map(|i| {
                Arc::new(TakeVersion::new(
                    i.id.clone(),
                    i.name.clone(),
                    dt(i.created_at),
                ))
            })
            .collect();
        if let Some(hashes) = base_hashes.as_deref_mut() {
            for i in &interp_rows {
                hashes.insert(i.id.clone(), interpretation_row_hash(i));
            }
        }

        let track_rows: Vec<TrackRow> = self.db.select(
            // Stable tie-break; see interpretations.
            "practice_id = ?1 AND deleted_at IS NULL ORDER BY order_index, id",
            [&row.id],
        )?;
        // Every live child of every track in a handful of set reads, not
        // three queries per track plus three per clip (394 round trips for a
        // 30-lane, 100-clip practice — paid again by every save's re-read).
        let track_ids: Vec<String> = track_rows.iter().map(|t| t.id.clone()).collect();
        let children = self.read_children(&track_ids)?;
        let mut accompaniment: Option<Arc<AudioTrack>> = None;
        let mut reference: Option<Arc<AudioTrack>> = None;
        let mut vocals: Vec<Arc<AudioTrack>> = Vec::new();
        for tr in &track_rows {
            let track = self.hydrate_track(tr, &children, base_hashes.as_deref_mut())?;
            if let Some(hashes) = base_hashes.as_deref_mut() {
                hashes.insert(tr.id.clone(), track_hash(&track, tr.order_index)?);
            }
            match tr.kind.as_str() {
                "accompaniment" => {
                    if accompaniment.is_none() {
                        accompaniment = Some(Arc::new(track));
                    }
                }
                "reference" => {
                    if reference.is_none() {
                        reference = Some(Arc::new(track));
                    }
                }
                _ => vocals.push(Arc::new(track)),
            }
        }

        let bus_rows: Vec<FxBusRow> = self.db.select(
            // Stable tie-break; see interpretations.
            "practice_id = ?1 AND deleted_at IS NULL ORDER BY order_index, id",
            [&row.id],
        )?;
        let mut buses: Vec<Arc<FxBus>> = Vec::with_capacity(bus_rows.len());
        for b in &bus_rows {
            buses.push(Arc::new(FxBus {
                id: b.id.clone(),
                name: b.name.clone(),
                fx: match &b.fx_json {
                    None => TrackFx::flat(),
                    Some(json) => fx_from(json)?,
                },
                color_value: b.color_value,
                volume: level(b.volume),
                muted: b.muted,
                soloed: b.soloed,
                pan: pan(b.pan),
                output_bus_id: b.output_bus_id.clone(),
                sends: sends_from(b.sends_json.as_deref())?,
            }));
        }
        if let Some(hashes) = base_hashes.as_deref_mut() {
            for (b, bus) in bus_rows.iter().zip(&buses) {
                hashes.insert(b.id.clone(), bus_hash(bus, b.order_index)?);
            }
        }

        let separation_source = match &row.separation_asset_id {
            None => None,
            Some(asset_id) => Some(
                self.assets
                    .resolve(asset_id)?
                    .unwrap_or_else(|| AssetStore::ref_for(asset_id)),
            ),
        };
        let reverb = match &row.reverb_json {
            None => None,
            Some(json) => {
                Some(ReverbSettings::from_json(&json_decode(json)?).map_err(model_error)?)
            }
        };
        let practice = SongPractice {
            id: row.id.clone(),
            title: row.title.clone(),
            created_at: dt(row.created_at),
            updated_at: dt(row.updated_at),
            accompaniment,
            reference,
            vocal_tracks: vocals,
            versions,
            active_version_id: row.active_interpretation_id.clone(),
            lyrics: lyrics_from(row.lyrics_json.as_deref())?,
            timeline_duration_ms: row.timeline_duration_ms,
            separation_source,
            reverb,
            master_volume: level(row.master_volume),
            fx_buses: buses,
        };
        if let Some(hashes) = base_hashes {
            let reverb_json = reverb_json_of(&practice)?;
            let lyrics_json = lyrics_json_of(&practice)?;
            hashes.insert(
                row.id.clone(),
                practice_hash(
                    &practice,
                    row.separation_asset_id.as_deref(),
                    reverb_json.as_deref(),
                    &lyrics_json,
                )?,
            );
        }
        Ok(practice)
    }

    fn hydrate_track(
        &self,
        tr: &TrackRow,
        children: &ChildRows,
        mut base_hashes: Option<&mut Hashes>,
    ) -> Result<AudioTrack> {
        let mut clips = Vec::new();
        for c in rows_of(&children.live.clips, &tr.id) {
            if let Some(hashes) = base_hashes.as_deref_mut() {
                hashes.insert(c.id.clone(), clip_row_hash(c));
            }
            clips.push(AudioClip {
                id: c.id.clone(),
                // Keep the clip even when its bytes are missing locally
                // (carry an `asset://` ref) so a reload never drops it —
                // dropping it would make the next save tombstone the row,
                // losing the take permanently.
                audio_file_path: children
                    .paths
                    .get(&c.asset_id)
                    .cloned()
                    .unwrap_or_else(|| AssetStore::ref_for(&c.asset_id)),
                duration_ms: c.duration_ms,
                start_ms: c.start_ms,
                // Decoded per clip: two clips over the same bytes never share
                // one mutable list.
                waveform_peaks: children
                    .peaks
                    .get(&c.asset_id)
                    .map(|p| decode_peaks(p))
                    .unwrap_or_default(),
            });
        }

        let note_rows = rows_of(&children.live.notes, &tr.id);
        let notes: Vec<PracticeNote> = note_rows
            .iter()
            .map(|n| PracticeNote {
                id: n.id.clone(),
                position_ms: n.position_ms,
                end_ms: n.end_ms,
                content: n.content.clone(),
                created_at: dt(n.created_at),
                updated_at: dt(n.updated_at),
            })
            .collect();
        if let Some(hashes) = base_hashes.as_deref_mut() {
            for n in note_rows {
                hashes.insert(n.id.clone(), practice_note_row_hash(n));
            }
        }

        let marker_rows = rows_of(&children.live.markers, &tr.id);
        if let Some(hashes) = base_hashes {
            for m in marker_rows {
                hashes.insert(m.id.clone(), technique_marker_row_hash(m));
            }
        }
        let markers: Vec<TechniqueMarker> = marker_rows
            .iter()
            .map(|m| TechniqueMarker {
                id: m.id.clone(),
                position_ms: m.position_ms,
                end_ms: m.end_ms,
                technique_id: m.technique_id.clone(),
                technique_name: m.technique_name_snapshot.clone(),
                created_at: dt(m.created_at),
                updated_at: dt(m.updated_at),
            })
            .collect();

        Ok(AudioTrack {
            id: tr.id.clone(),
            kind: AudioTrackKind::by_name(&tr.kind).unwrap_or(AudioTrackKind::Vocal),
            display_name: tr.display_name.clone(),
            created_at: dt(tr.created_at),
            clips,
            notes,
            technique_markers: markers,
            fx: tr.fx_json.as_deref().map(fx_from).transpose()?,
            bus_id: tr.bus_id.clone(),
            sends: sends_from(tr.sends_json.as_deref())?,
            version_id: tr.interpretation_id.clone(),
            color_value: tr.color_value,
            muted: tr.muted,
            soloed: tr.soloed,
            volume: level(tr.volume),
            pan: pan(tr.pan),
        })
    }

    /// The live clips / notes / markers of [track_ids], grouped by track,
    /// plus the clips' resolved paths and cached peaks — six set reads
    /// (chunked by [`AssetStore::in_chunks`]) whatever the practice's size.
    /// Each group keeps the order its old per-track query returned: clips by
    /// (order_index, id); notes and markers in rowid order, which is what
    /// the per-track `track_id = ?` scan over ix_notes_track /
    /// ix_markers_track yielded. Asset rows are read live or tombstoned,
    /// like [`AssetStore::resolve`].
    fn read_children(&self, track_ids: &[String]) -> Result<ChildRows> {
        let live = self.live_child_rows(track_ids)?;
        let asset_ids: IndexSet<String> = live
            .clips
            .values()
            .flatten()
            .map(|c| c.asset_id.clone())
            .collect();
        let asset_rows = self.asset_rows(&asset_ids.into_iter().collect::<Vec<_>>())?;

        let hashes: IndexSet<String> = asset_rows.iter().map(|a| a.content_hash.clone()).collect();
        let hashes: Vec<String> = hashes.into_iter().collect();
        let mut peaks_by_hash: HashMap<String, Vec<u8>> = HashMap::new();
        for chunk in AssetStore::in_chunks(&hashes) {
            let mut params: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|h| h as &dyn rusqlite::types::ToSql)
                .collect();
            params.push(&WAVEFORM_ALGO_VERSION);
            for w in self.db.select::<WaveformCacheRow>(
                &format!(
                    "content_hash IN ({}) AND algo_version = ?{}",
                    placeholders(chunk.len()),
                    chunk.len() + 1
                ),
                &*params,
            )? {
                peaks_by_hash.insert(w.content_hash, w.peaks);
            }
        }

        let mut paths = HashMap::new();
        let mut peaks = HashMap::new();
        for a in &asset_rows {
            if let Some(path) = self.assets.resolve_row(a) {
                paths.insert(a.id.clone(), path);
            }
            if let Some(p) = peaks_by_hash.get(&a.content_hash) {
                peaks.insert(a.id.clone(), p.clone());
            }
        }
        Ok(ChildRows { live, paths, peaks })
    }

    /// The live clips / notes / markers of [track_ids], grouped by track:
    /// one set read per table (chunked by [`AssetStore::in_chunks`]), each
    /// group in the order its old per-track query returned — clips by
    /// (order_index, id); notes and markers in rowid order, which is what
    /// the per-track `track_id = ?` scan over ix_notes_track /
    /// ix_markers_track yielded.
    fn live_child_rows(&self, track_ids: &[String]) -> Result<LiveChildren> {
        let mut clips: Vec<ClipRow> = Vec::new();
        let mut notes: Vec<PracticeNoteRow> = Vec::new();
        let mut markers: Vec<TechniqueMarkerRow> = Vec::new();
        for chunk in AssetStore::in_chunks(track_ids) {
            let within = format!(
                "track_id IN ({}) AND deleted_at IS NULL",
                placeholders(chunk.len())
            );
            clips.extend(self.db.select::<ClipRow>(
                // Stable tie-break; see interpretations.
                &format!("{within} ORDER BY track_id, order_index, id"),
                rusqlite::params_from_iter(chunk),
            )?);
            notes.extend(self.db.select::<PracticeNoteRow>(
                &format!("{within} ORDER BY track_id, rowid"),
                rusqlite::params_from_iter(chunk),
            )?);
            markers.extend(self.db.select::<TechniqueMarkerRow>(
                &format!("{within} ORDER BY track_id, rowid"),
                rusqlite::params_from_iter(chunk),
            )?);
        }
        Ok(LiveChildren {
            clips: by_track(clips, |c| &c.track_id),
            notes: by_track(notes, |n| &n.track_id),
            markers: by_track(markers, |m| &m.track_id),
        })
    }
}

impl SongPracticeRepository for SongPracticeRepositoryDrift {
    fn list_practice_summaries(&self) -> Result<Vec<PracticeSummary>> {
        let mut rows: Vec<PracticeRow> = self.db.select("deleted_at IS NULL", [])?;
        Self::sort_newest_first(&mut rows);
        // The counts hydration would produce, from the live rows it reads: a
        // vocal take is any live track that is not a backing, and the note
        // count spans every live lane.
        let counts: HashMap<String, (i64, i64, i64)> = self
            .db
            .query(
                "SELECT t.practice_id AS pid, \
                 SUM(t.kind NOT IN ('accompaniment', 'reference')) AS takes, \
                 MAX(t.kind = 'accompaniment') AS backing, \
                 SUM((SELECT COUNT(*) FROM practice_notes n \
                   WHERE n.track_id = t.id AND n.deleted_at IS NULL)) AS notes \
                 FROM tracks t WHERE t.deleted_at IS NULL GROUP BY t.practice_id",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>("pid")?,
                        (
                            r.get::<_, Option<i64>>("takes")?.unwrap_or(0),
                            r.get::<_, Option<i64>>("backing")?.unwrap_or(0),
                            r.get::<_, Option<i64>>("notes")?.unwrap_or(0),
                        ),
                    ))
                },
            )?
            .into_iter()
            .collect();
        Ok(rows
            .into_iter()
            .map(|p| {
                let (takes, backing, notes) = counts.get(&p.id).copied().unwrap_or((0, 0, 0));
                PracticeSummary {
                    updated_at: dt(p.updated_at),
                    take_count: usize::try_from(takes).unwrap_or(0),
                    has_accompaniment: backing != 0,
                    note_count: usize::try_from(notes).unwrap_or(0),
                    id: p.id,
                    title: p.title,
                }
            })
            .collect())
    }

    fn list_practice_previews(&self) -> Result<Vec<PracticePreview>> {
        // Three cheap reads (no blob but the chosen tracks' peaks): the
        // practices, their live vocal and backing tracks, and those tracks'
        // live clips with their assets' content hashes.
        let practices: Vec<(String, i64, Option<String>)> = self.db.query(
            "SELECT id, timeline_duration_ms, active_interpretation_id FROM practices \
             WHERE deleted_at IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        // (track, practice, vocal, interpretation), in the tracks' order.
        let tracks: Vec<(String, String, bool, Option<String>)> = self.db.query(
            "SELECT id, practice_id, kind = 'vocal', interpretation_id FROM tracks \
             WHERE deleted_at IS NULL AND kind IN ('vocal', 'accompaniment') \
             ORDER BY practice_id, order_index, created_at, id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        // (start, duration, content hash) per track, in the clips' order.
        let mut clips: HashMap<String, Vec<(i64, i64, String)>> = HashMap::new();
        for (track, start, duration, hash) in self.db.query(
            "SELECT c.track_id, c.start_ms, c.duration_ms, a.content_hash FROM clips c \
             JOIN assets a ON a.id = c.asset_id WHERE c.deleted_at IS NULL \
             ORDER BY c.track_id, c.order_index, c.id",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )? {
            clips
                .entry(track)
                .or_default()
                .push((start, duration, hash));
        }
        // Each practice's clips' end, over every track it has.
        let mut ends: HashMap<&str, i64> = HashMap::new();
        for (track, practice, _, _) in &tracks {
            for (start, duration, _) in clips.get(track).map_or(&[][..], Vec::as_slice) {
                let end = ends.entry(practice.as_str()).or_default();
                *end = (*end).max(start + duration);
            }
        }
        // The track each card draws: the first take with audio (the active
        // interpretation's first), else the first backing with audio.
        let active: HashMap<&str, Option<&str>> = practices
            .iter()
            .map(|(id, _, active)| (id.as_str(), active.as_deref()))
            .collect();
        let mut chosen: HashMap<&str, (&str, PreviewSource, bool)> = HashMap::new();
        for (track, practice, vocal, interpretation) in &tracks {
            if !clips.contains_key(track) {
                continue;
            }
            let source = if *vocal {
                PreviewSource::Take
            } else {
                PreviewSource::Backing
            };
            let in_active = *vocal
                && active.get(practice.as_str()).copied().flatten() == interpretation.as_deref();
            let better = match chosen.get(practice.as_str()) {
                None => true,
                Some((_, PreviewSource::Backing, _)) => source == PreviewSource::Take,
                Some((_, PreviewSource::Take, was_active)) => {
                    source == PreviewSource::Take && in_active && !was_active
                }
            };
            if better {
                chosen.insert(practice.as_str(), (track.as_str(), source, in_active));
            }
        }
        // Their peaks, from the waveform cache.
        let hashes: Vec<String> = chosen
            .values()
            .flat_map(|(track, _, _)| clips.get(*track).into_iter().flatten())
            .map(|(_, _, hash)| hash.clone())
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect();
        let mut peaks: HashMap<String, Vec<f64>> = HashMap::new();
        for chunk in AssetStore::in_chunks(&hashes) {
            let mut params: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|h| h as &dyn rusqlite::types::ToSql)
                .collect();
            params.push(&WAVEFORM_ALGO_VERSION);
            for w in self.db.select::<WaveformCacheRow>(
                &format!(
                    "content_hash IN ({}) AND algo_version = ?{}",
                    placeholders(chunk.len()),
                    chunk.len() + 1
                ),
                &*params,
            )? {
                peaks.insert(w.content_hash, decode_peaks(&w.peaks));
            }
        }
        Ok(practices
            .iter()
            .map(|(id, timeline, _)| {
                let length = (*timeline).max(ends.get(id.as_str()).copied().unwrap_or(0));
                let Some((track, source, _)) = chosen.get(id.as_str()) else {
                    return PracticePreview::compose(id.clone(), length, None, &[]);
                };
                let drawn: Vec<PreviewClip> = clips
                    .get(*track)
                    .into_iter()
                    .flatten()
                    .map(|(start, duration, hash)| PreviewClip {
                        start_ms: *start,
                        duration_ms: *duration,
                        peaks: peaks.get(hash).cloned().unwrap_or_default(),
                    })
                    .collect();
                PracticePreview::compose(id.clone(), length, Some(*source), &drawn)
            })
            .collect())
    }

    fn list_practices(&self) -> Result<Vec<SongPractice>> {
        let mut rows: Vec<PracticeRow> = self.db.select("deleted_at IS NULL", [])?;
        // Deterministic order: updatedAt desc, then id desc as a total-order
        // tiebreaker so rows sharing a millisecond don't reshuffle between
        // launches.
        Self::sort_newest_first(&mut rows);
        // Skip a single corrupt row (e.g. a malformed JSON column) rather
        // than failing the whole list — the other readable records must
        // still show.
        Ok(rows
            .iter()
            .filter_map(|r| self.hydrate(r, None).ok())
            .collect())
    }

    fn get_practice(&self, id: &str) -> Result<SongPractice> {
        self.load_practice(id).map(|loaded| loaded.aggregate)
    }

    fn load_practice(&self, id: &str) -> Result<Loaded<SongPractice>> {
        let hook = self.debug_load_hook.lock().clone();
        if let Some(hook) = hook {
            hook(id)?;
        }
        let row = self
            .db
            .select::<PracticeRow>("id = ?1 AND deleted_at IS NULL", [id])?
            .into_iter()
            .next();
        let Some(row) = row else {
            return Err(Error::State(format!("Practice \"{id}\" not found")));
        };
        // Collect the base hash of every row this read returns, so a later
        // save can three-way merge against exactly what the editor was
        // shown.
        let mut hashes = Hashes::new();
        let practice = self.hydrate(&row, Some(&mut hashes))?;
        Ok(Loaded::new(practice, hashes))
    }

    fn create_practice(&self, title: &str) -> Result<SongPractice> {
        let now = DateTime::now();
        let trimmed = dart_string::trim(title);
        let first_version = TakeVersion::new(new_id(), "Interpretation 1", now);
        let practice = SongPractice {
            active_version_id: Some(first_version.id.clone()),
            versions: vec![Arc::new(first_version)],
            ..SongPractice::new(
                new_id(),
                if trimmed.is_empty() {
                    "Untitled practice"
                } else {
                    trimmed
                },
                now,
                now,
            )
        };
        self.save_practice(&practice)
    }

    fn delete_practice(&self, id: &str) -> Result<()> {
        self.db.transaction(|| -> Result<()> {
            // Already gone (a stale list after a remote delete, a second tap)
            // or never there: nothing to delete. Tombstoned again, it
            // journaled a fresh delete — for a row that did not exist, too.
            match self.db.find::<PracticeRow>(id)? {
                Some(parent) if parent.deleted_at.is_none() => {}
                _ => return Ok(()),
            }
            self.db.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            let now = ms(&DateTime::now());
            // Cascade-tombstone the whole subtree (children before parents is
            // not required for soft-delete, but we tombstone everything
            // live).
            for t in self.live::<TrackRow>("practice_id", id)? {
                self.cascade_tombstone_track(&t.id, now, None)?;
            }
            for bus in self.live::<FxBusRow>("practice_id", id)? {
                self.tombstone("fx_buses", &bus.id, now)?;
            }
            for iv in self.live::<InterpretationRow>("practice_id", id)? {
                self.tombstone("interpretations", &iv.id, now)?;
            }
            self.tombstone("practices", id, now)
        })
    }

    fn save_practice(&self, practice: &SongPractice) -> Result<SongPractice> {
        // Base-less save: save adopts the CURRENT persisted rows as the
        // base, which reproduces the old two-way diff exactly. Safe only for
        // callers that just created or just re-read the aggregate
        // (create_practice, boot recovery); editors must use
        // save_practice_with_base — see the interface.
        self.save(practice, None).map(|loaded| loaded.aggregate)
    }

    fn save_practice_with_base(
        &self,
        practice: &SongPractice,
        base_hashes: &IndexMap<String, String>,
    ) -> Result<Loaded<SongPractice>> {
        self.save(practice, Some(base_hashes))
    }
}
