//! One content-hash projection per synced entity, and the loaded-aggregate
//! wrapper that carries the base hashes (port of
//! `lib/data/sync/sync_hashes.dart`).
//!
//! The base/hydrate path and the save path MUST hash through the same
//! function, or the two sides silently drift apart and every "unchanged"
//! check breaks (phantom updates at best, wrongly-skipped writes at worst).
//! Each entity has:
//!   * `xxx_content_hash(&XxxContent { .. })` — the single definition of the
//!     projection: which content columns participate and in which order. The
//!     struct's named fields pin every value to its column so a call site
//!     can't reorder fields (Dart's named parameters).
//!   * `xxx_row_hash(&XxxRow)` — the projection of a loaded row, used by both
//!     the hydrate path (base hashes) and the save path (current-DB hashes).
//!
//! The save path calls `xxx_content_hash` directly with domain-derived
//! values.
//!
//! Projections cover CONTENT columns only — SyncCols (id/HLC/timestamps/
//! tombstone/schema_v) are bookkeeping and never part of content equality.
//! JSON columns are compared as their stored STRINGS: both sides produce
//! them with the same deterministic encoder, so an unchanged value
//! round-trips byte-identically (this is what already made no-op saves emit
//! nothing).
//!
//! A projection with a REAL column (practices, fx_buses, tracks) can refuse
//! a non-finite double ([`SyncWriter::content_hash_of`]); the others cannot
//! fail.

use indexmap::IndexMap;

use crate::db::rows::{
    ClipRow, FxBusRow, InterpretationRow, PracticeNoteRow, PracticeRow, TechniqueMarkerRow,
    TechniqueMediaRow, TechniqueRow, TrackRow,
};
use crate::error::Result;
use crate::sync::sync_writer::{ContentValue, SyncWriter};

/// An aggregate loaded from the store together with its per-row BASE
/// content hashes: the persisted state of the parent row and of every
/// reconciled child-collection row as of that read, keyed by row id. Row ids
/// are ULIDs (globally unique across tables), so one flat map covers the
/// whole aggregate without collisions.
///
/// The base map is what turns a later save into a THREE-WAY merge instead of
/// a blind two-way diff: reconcile compares BASE (what this editor loaded),
/// MINE (what the editor wants now) and the DB (what is persisted at save
/// time), so a remote change that landed while the aggregate was open is
/// neither reverted nor tombstoned by a stale save. Callers hold the map
/// unchanged between load and save, and swap in the freshly returned map
/// after every successful save.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded<T> {
    /// The aggregate as read.
    pub aggregate: T,

    /// rowId -> content hash of the row as persisted when the aggregate was
    /// read.
    pub base_hashes: IndexMap<String, String>,

    /// Set on a SAVE's result only (None for a plain load): rowId -> the
    /// MINE hash of every row of the aggregate exactly as that save SENT it.
    ///
    /// The base for a caller whose model moved on while the save was in
    /// flight (it keeps its newer model instead of adopting the aggregate).
    /// The base hashes would be wrong for it: they describe the re-read
    /// store, including rows the three-way rules kept from concurrent
    /// writers that the newer model has never seen — merging against them
    /// makes the next save tombstone a remote insert (in base, not desired),
    /// revert a remote edit (mine != base) and resurrect a remote delete.
    /// The sent state is exactly what that model derives from.
    pub sent_hashes: Option<IndexMap<String, String>>,

    /// False on a save that committed but could not re-read the store: the
    /// aggregate is the model as sent and the base hashes its sent hashes,
    /// so rows the merge kept from another writer are in neither. The caller
    /// reads the store again to show them.
    pub reread: bool,
}

impl<T> Loaded<T> {
    /// A plain load's result: no sent hashes, re-read.
    pub fn new(aggregate: T, base_hashes: IndexMap<String, String>) -> Self {
        Loaded {
            aggregate,
            base_hashes,
            sent_hashes: None,
            reread: true,
        }
    }
}

/// Hashes a projection that holds no double, which cannot fail.
fn hash_without_doubles(columns: &[ContentValue]) -> String {
    match SyncWriter::content_hash_of(columns) {
        Ok(hash) => hash,
        // Only a non-finite double is refused, and these projections carry
        // none (they are built from strings, ints and nulls only).
        Err(e) => unreachable!("a projection without doubles cannot be refused: {e}"),
    }
}

// --- practices --------------------------------------------------------------

/// The content columns of a `practices` row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PracticeContent<'a> {
    pub title: &'a str,
    pub timeline_duration_ms: i64,
    pub master_volume: f64,
    pub active_interpretation_id: Option<&'a str>,
    pub separation_asset_id: Option<&'a str>,
    pub reverb_json: Option<&'a str>,
    pub lyrics_json: Option<&'a str>,
}

/// The `practices` projection.
pub fn practice_content_hash(c: &PracticeContent<'_>) -> Result<String> {
    SyncWriter::content_hash_of(&[
        c.title.into(),
        c.timeline_duration_ms.into(),
        c.master_volume.into(),
        c.active_interpretation_id.into(),
        c.separation_asset_id.into(),
        c.reverb_json.into(),
        c.lyrics_json.into(),
    ])
}

/// The `practices` projection of a loaded row.
pub fn practice_row_hash(r: &PracticeRow) -> Result<String> {
    practice_content_hash(&PracticeContent {
        title: &r.title,
        timeline_duration_ms: r.timeline_duration_ms,
        master_volume: r.master_volume,
        active_interpretation_id: r.active_interpretation_id.as_deref(),
        separation_asset_id: r.separation_asset_id.as_deref(),
        reverb_json: r.reverb_json.as_deref(),
        lyrics_json: r.lyrics_json.as_deref(),
    })
}

// --- interpretations ---------------------------------------------------------

/// The content columns of an `interpretations` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterpretationContent<'a> {
    pub name: &'a str,
    pub order_index: i64,
}

/// The `interpretations` projection.
pub fn interpretation_content_hash(c: &InterpretationContent<'_>) -> String {
    hash_without_doubles(&[c.name.into(), c.order_index.into()])
}

/// The `interpretations` projection of a loaded row.
pub fn interpretation_row_hash(r: &InterpretationRow) -> String {
    interpretation_content_hash(&InterpretationContent {
        name: &r.name,
        order_index: r.order_index,
    })
}

// --- fx_buses ----------------------------------------------------------------

/// The content columns of an `fx_buses` row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FxBusContent<'a> {
    pub name: &'a str,
    pub color_value: Option<i64>,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    pub soloed: bool,
    pub output_bus_id: Option<&'a str>,
    pub fx_json: &'a str,
    pub sends_json: &'a str,
    pub order_index: i64,
}

/// The `fx_buses` projection.
pub fn fx_bus_content_hash(c: &FxBusContent<'_>) -> Result<String> {
    SyncWriter::content_hash_of(&[
        c.name.into(),
        c.color_value.into(),
        c.volume.into(),
        c.pan.into(),
        c.muted.into(),
        c.soloed.into(),
        c.output_bus_id.into(),
        c.fx_json.into(),
        c.sends_json.into(),
        c.order_index.into(),
    ])
}

/// Row projection. The null fallbacks are the pre-existing row-side
/// semantics: a NULL sends_json equals the save side's empty-list encoding
/// ('[]'), while a NULL fx_json ('' here) can never equal a save-side
/// encoding. A load records such a row's BASE over the model it hydrates
/// to, not this projection, so an untouched legacy/remote row is still
/// skipped by a save; only the current side of a three-way merge reads it
/// this way.
pub fn fx_bus_row_hash(r: &FxBusRow) -> Result<String> {
    fx_bus_content_hash(&FxBusContent {
        name: &r.name,
        color_value: r.color_value,
        volume: r.volume,
        pan: r.pan,
        muted: r.muted,
        soloed: r.soloed,
        output_bus_id: r.output_bus_id.as_deref(),
        fx_json: r.fx_json.as_deref().unwrap_or(""),
        sends_json: r.sends_json.as_deref().unwrap_or("[]"),
        order_index: r.order_index,
    })
}

// --- tracks ------------------------------------------------------------------

/// The content columns of a `tracks` row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackContent<'a> {
    pub kind: &'a str,
    pub interpretation_id: Option<&'a str>,
    pub display_name: &'a str,
    pub color_value: Option<i64>,
    pub muted: bool,
    pub soloed: bool,
    pub volume: f64,
    pub pan: f64,
    pub bus_id: Option<&'a str>,
    pub fx_json: Option<&'a str>,
    pub sends_json: &'a str,
    pub order_index: i64,
}

/// The `tracks` projection.
pub fn track_content_hash(c: &TrackContent<'_>) -> Result<String> {
    SyncWriter::content_hash_of(&[
        c.kind.into(),
        c.interpretation_id.into(),
        c.display_name.into(),
        c.color_value.into(),
        c.muted.into(),
        c.soloed.into(),
        c.volume.into(),
        c.pan.into(),
        c.bus_id.into(),
        c.fx_json.into(),
        c.sends_json.into(),
        c.order_index.into(),
    ])
}

/// Row projection. fx_json passes through as-is (a null row value equals
/// the save side's null for "no FX chain"); a NULL sends_json equals the
/// save side's empty-list encoding ('[]') — both pre-existing semantics,
/// kept.
pub fn track_row_hash(r: &TrackRow) -> Result<String> {
    track_content_hash(&TrackContent {
        kind: &r.kind,
        interpretation_id: r.interpretation_id.as_deref(),
        display_name: &r.display_name,
        color_value: r.color_value,
        muted: r.muted,
        soloed: r.soloed,
        volume: r.volume,
        pan: r.pan,
        bus_id: r.bus_id.as_deref(),
        fx_json: r.fx_json.as_deref(),
        sends_json: r.sends_json.as_deref().unwrap_or("[]"),
        order_index: r.order_index,
    })
}

// --- clips -------------------------------------------------------------------

/// The content columns of a `clips` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipContent<'a> {
    pub asset_id: &'a str,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub order_index: i64,
}

/// The `clips` projection.
pub fn clip_content_hash(c: &ClipContent<'_>) -> String {
    hash_without_doubles(&[
        c.asset_id.into(),
        c.start_ms.into(),
        c.duration_ms.into(),
        c.order_index.into(),
    ])
}

/// The `clips` projection of a loaded row.
pub fn clip_row_hash(r: &ClipRow) -> String {
    clip_content_hash(&ClipContent {
        asset_id: &r.asset_id,
        start_ms: r.start_ms,
        duration_ms: r.duration_ms,
        order_index: r.order_index,
    })
}

// --- practice_notes ----------------------------------------------------------

/// The content columns of a `practice_notes` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PracticeNoteContent<'a> {
    pub position_ms: i64,
    pub end_ms: Option<i64>,
    pub content: &'a str,
}

/// The `practice_notes` projection.
pub fn practice_note_content_hash(c: &PracticeNoteContent<'_>) -> String {
    hash_without_doubles(&[c.position_ms.into(), c.end_ms.into(), c.content.into()])
}

/// The `practice_notes` projection of a loaded row.
pub fn practice_note_row_hash(r: &PracticeNoteRow) -> String {
    practice_note_content_hash(&PracticeNoteContent {
        position_ms: r.position_ms,
        end_ms: r.end_ms,
        content: &r.content,
    })
}

// --- technique_markers -------------------------------------------------------

/// The content columns of a `technique_markers` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TechniqueMarkerContent<'a> {
    pub position_ms: i64,
    pub end_ms: Option<i64>,
    pub technique_id: &'a str,
    pub technique_name: &'a str,
}

/// The `technique_markers` projection.
pub fn technique_marker_content_hash(c: &TechniqueMarkerContent<'_>) -> String {
    hash_without_doubles(&[
        c.position_ms.into(),
        c.end_ms.into(),
        c.technique_id.into(),
        c.technique_name.into(),
    ])
}

/// The `technique_markers` projection of a loaded row.
pub fn technique_marker_row_hash(r: &TechniqueMarkerRow) -> String {
    technique_marker_content_hash(&TechniqueMarkerContent {
        position_ms: r.position_ms,
        end_ms: r.end_ms,
        technique_id: &r.technique_id,
        technique_name: &r.technique_name_snapshot,
    })
}

// --- techniques --------------------------------------------------------------

/// The content columns of a `techniques` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TechniqueContent<'a> {
    pub name: &'a str,
    pub aliases_json: &'a str,
    pub tags_json: &'a str,
    pub summary: &'a str,
    pub description: &'a str,
    pub cues: &'a str,
    pub exercises: &'a str,
    pub acoustic_signature: &'a str,
}

/// The `techniques` projection.
pub fn technique_content_hash(c: &TechniqueContent<'_>) -> String {
    hash_without_doubles(&[
        c.name.into(),
        c.aliases_json.into(),
        c.tags_json.into(),
        c.summary.into(),
        c.description.into(),
        c.cues.into(),
        c.exercises.into(),
        c.acoustic_signature.into(),
    ])
}

/// Row projection. NULL alias/tag JSON equals the save side's empty-list
/// encoding ('[]') — pre-existing semantics, kept.
pub fn technique_row_hash(r: &TechniqueRow) -> String {
    technique_content_hash(&TechniqueContent {
        name: &r.name,
        aliases_json: r.aliases_json.as_deref().unwrap_or("[]"),
        tags_json: r.tags_json.as_deref().unwrap_or("[]"),
        summary: &r.summary,
        description: &r.description,
        cues: &r.cues,
        exercises: &r.exercises,
        acoustic_signature: &r.acoustic_signature,
    })
}

// --- technique_media ---------------------------------------------------------

/// The content columns of a `technique_media` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TechniqueMediaContent<'a> {
    pub asset_id: &'a str,
    pub kind: &'a str,
    pub label: &'a str,
    pub order_index: i64,
}

/// The `technique_media` projection.
pub fn technique_media_content_hash(c: &TechniqueMediaContent<'_>) -> String {
    hash_without_doubles(&[
        c.asset_id.into(),
        c.kind.into(),
        c.label.into(),
        c.order_index.into(),
    ])
}

/// The `technique_media` projection of a loaded row.
pub fn technique_media_row_hash(r: &TechniqueMediaRow) -> String {
    technique_media_content_hash(&TechniqueMediaContent {
        asset_id: &r.asset_id,
        kind: &r.kind,
        label: &r.label,
        order_index: r.order_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use sha2::{Digest, Sha256};

    fn sha(text: &str) -> String {
        hex::encode(Sha256::digest(text.as_bytes()))
    }

    // The Dart suite proves the load-side and save-side projections agree
    // through the repositories (not ported yet). Here the same invariant is
    // checked at this layer: every row projection equals the content
    // projection of the values a save derives from that row, and both
    // encode the columns in the documented order, as Dart's jsonEncode does.

    #[test]
    fn practice_projection_order_and_types() {
        let row = PracticeRow {
            title: "Full graph".into(),
            timeline_duration_ms: 30000,
            master_volume: 0.8,
            active_interpretation_id: Some("I".into()),
            separation_asset_id: None,
            reverb_json: Some(r#"{"roomSize":0.6}"#.into()),
            lyrics_json: Some("[]".into()),
            ..Default::default()
        };
        assert_eq!(
            practice_row_hash(&row).unwrap(),
            sha(r#"["Full graph",30000,0.8,"I",null,"{\"roomSize\":0.6}","[]"]"#)
        );
        // A whole master volume is still a double: 1.0, never 1.
        let fresh = PracticeRow {
            title: "Song".into(),
            ..Default::default()
        };
        assert_eq!(
            practice_row_hash(&fresh).unwrap(),
            "243b0f97e801172ef7e7388c0a48587d474fb0f346e39f454ed373b23d496e70"
        );
        let mut bad = fresh;
        bad.master_volume = f64::INFINITY;
        assert!(matches!(
            practice_row_hash(&bad),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn interpretation_projection() {
        let row = InterpretationRow {
            name: "Tender".into(),
            order_index: 2,
            ..Default::default()
        };
        assert_eq!(interpretation_row_hash(&row), sha(r#"["Tender",2]"#));
        assert_eq!(
            interpretation_content_hash(&InterpretationContent {
                name: "Tender",
                order_index: 2
            }),
            interpretation_row_hash(&row)
        );
    }

    #[test]
    fn fx_bus_projection_and_its_null_fallbacks() {
        let row = FxBusRow {
            name: "Reverb bus".into(),
            color_value: Some(0xFF112233),
            volume: 0.9,
            pan: 0.1,
            muted: false,
            soloed: true,
            output_bus_id: None,
            fx_json: None,
            sends_json: None,
            order_index: 0,
            ..Default::default()
        };
        assert_eq!(
            fx_bus_row_hash(&row).unwrap(),
            sha(r#"["Reverb bus",4279312947,0.9,0.1,false,true,null,"","[]",0]"#)
        );
        assert_eq!(
            fx_bus_row_hash(&row).unwrap(),
            fx_bus_content_hash(&FxBusContent {
                name: "Reverb bus",
                color_value: Some(0xFF112233),
                volume: 0.9,
                pan: 0.1,
                muted: false,
                soloed: true,
                output_bus_id: None,
                fx_json: "",
                sends_json: "[]",
                order_index: 0,
            })
            .unwrap()
        );
    }

    #[test]
    fn track_projection_and_its_null_fallbacks() {
        let row = TrackRow {
            kind: "vocal".into(),
            interpretation_id: Some("I".into()),
            display_name: "Take 1".into(),
            color_value: None,
            muted: true,
            soloed: false,
            volume: 1.0,
            pan: -0.2,
            bus_id: None,
            fx_json: None,
            sends_json: None,
            order_index: 3,
            ..Default::default()
        };
        assert_eq!(
            track_row_hash(&row).unwrap(),
            sha(r#"["vocal","I","Take 1",null,true,false,1.0,-0.2,null,null,"[]",3]"#)
        );
        let mut bad = row;
        bad.pan = f64::NAN;
        assert!(track_row_hash(&bad).is_err());
    }

    #[test]
    fn clip_note_marker_media_projections() {
        let clip = ClipRow {
            asset_id: "A".into(),
            start_ms: 250,
            duration_ms: 4000,
            order_index: 0,
            ..Default::default()
        };
        assert_eq!(clip_row_hash(&clip), sha(r#"["A",250,4000,0]"#));

        let note = PracticeNoteRow {
            position_ms: 1500,
            end_ms: Some(2500),
            content: "range note".into(),
            ..Default::default()
        };
        assert_eq!(
            practice_note_row_hash(&note),
            sha(r#"[1500,2500,"range note"]"#)
        );
        let point = PracticeNoteRow {
            position_ms: 1200,
            end_ms: None,
            content: "point note".into(),
            ..Default::default()
        };
        assert_eq!(
            practice_note_row_hash(&point),
            sha(r#"[1200,null,"point note"]"#)
        );

        let marker = TechniqueMarkerRow {
            position_ms: 2000,
            end_ms: Some(2600),
            technique_id: "tech-1".into(),
            technique_name_snapshot: "Twang".into(),
            ..Default::default()
        };
        assert_eq!(
            technique_marker_row_hash(&marker),
            sha(r#"[2000,2600,"tech-1","Twang"]"#)
        );

        let media = TechniqueMediaRow {
            asset_id: "A".into(),
            kind: "video".into(),
            label: "demo".into(),
            order_index: 1,
            ..Default::default()
        };
        assert_eq!(
            technique_media_row_hash(&media),
            sha(r#"["A","video","demo",1]"#)
        );
    }

    #[test]
    fn technique_projection_and_its_null_fallbacks() {
        let row = TechniqueRow {
            name: "Belting".into(),
            aliases_json: None,
            tags_json: Some(r#"["CVT","Effect"]"#.into()),
            summary: "loud + bright".into(),
            description: "d".into(),
            cues: "c".into(),
            exercises: "e".into(),
            acoustic_signature: "strong H2".into(),
            ..Default::default()
        };
        assert_eq!(
            technique_row_hash(&row),
            sha(
                r#"["Belting","[]","[\"CVT\",\"Effect\"]","loud + bright","d","c","e","strong H2"]"#
            )
        );
    }

    #[test]
    fn bookkeeping_columns_never_change_a_hash() {
        let a = TrackRow {
            kind: "vocal".into(),
            ..Default::default()
        };
        let mut b = a.clone();
        b.id = "X".repeat(26);
        b.hlc_packed = "anything".into();
        b.updated_at = 99;
        b.deleted_at = Some(5);
        b.schema_v = 7;
        assert_eq!(track_row_hash(&a).unwrap(), track_row_hash(&b).unwrap());
    }

    #[test]
    fn a_plain_load_has_no_sent_hashes_and_was_re_read() {
        let loaded = Loaded::new((), IndexMap::new());
        assert!(loaded.sent_hashes.is_none());
        assert!(loaded.reread);
    }
}
