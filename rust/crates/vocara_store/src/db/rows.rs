//! One Rust struct per library table, with read/write helpers (the data
//! classes drift generated from `lib/data/db/tables.dart`).
//!
//! Field names are the column names, and fields come in the table's column
//! order. `Default` gives every column its DDL default (`schema_v` 1,
//! `master_volume` 1.0, `kind` 'audio', ...), so a write that names only some
//! columns reads like drift's `XCompanion.insert(...)`:
//!
//! ```ignore
//! db.insert(&InterpretationRow { id, practice_id, created_at: at, ..Default::default() })?;
//! ```
//!
//! Every write through these helpers runs the check drift ran in code before
//! it reached SQLite: a synced row's `id` must be 26..=36 characters (Dart's
//! UTF-16 length). The DDL itself carries no such CHECK, so a raw SQL write
//! is not checked, exactly as in Dart.

use rusqlite::types::ToSql;
use rusqlite::{Connection, OptionalExtension, Row};

use crate::dart;
use crate::error::{Error, Result};

/// A table's row type: its name, columns and primary key, and how to read
/// and bind one row.
pub trait TableRow: Sized {
    /// The SQL table name.
    const TABLE: &'static str;
    /// Every column, in the table's declaration order.
    const COLUMNS: &'static [&'static str];
    /// The primary-key columns.
    const PRIMARY_KEY: &'static [&'static str];

    /// Reads one row by column name (so `SELECT *` and aliased projections
    /// both work, as long as every column is present).
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self>;

    /// The row's values, in [`TableRow::COLUMNS`] order.
    fn params(&self) -> Vec<&dyn ToSql>;

    /// The checks drift ran in code on every insert and update.
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

/// A row of a SYNCED table: it carries the shared SyncCols (id, timestamps,
/// tombstone, HLC, schema_v).
pub trait SyncedRow: TableRow {
    /// The row's stable ULID primary key.
    fn id(&self) -> &str;
    /// The tombstone (None = live).
    fn deleted_at(&self) -> Option<i64>;
    /// The packed HLC (the LWW key).
    fn hlc_packed(&self) -> &str;
}

/// The length drift enforced on every synced table's `id`
/// (`text().withLength(min: 26, max: 36)`), counted in UTF-16 code units
/// like Dart's `String.length`.
pub const ID_LENGTH: std::ops::RangeInclusive<usize> = 26..=36;

/// Refuses a synced-table id outside [`ID_LENGTH`], as drift's companion
/// validation did.
pub fn check_id_length(table: &str, id: &str) -> Result<()> {
    let len = dart::string_length(id);
    if ID_LENGTH.contains(&len) {
        Ok(())
    } else {
        Err(Error::InvalidData(format!(
            "{table}.id: must be {}..={} characters long, got {len} (\"{id}\")",
            ID_LENGTH.start(),
            ID_LENGTH.end()
        )))
    }
}

fn quoted(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn placeholders(n: usize) -> String {
    (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `SELECT <every column> FROM <table>`.
pub fn select_sql<R: TableRow>() -> String {
    format!("SELECT {} FROM \"{}\"", quoted(R::COLUMNS), R::TABLE)
}

/// Inserts [row] (a primary-key conflict is an error).
pub fn insert_row<R: TableRow>(conn: &Connection, row: &R) -> Result<()> {
    row.validate()?;
    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES ({})",
        R::TABLE,
        quoted(R::COLUMNS),
        placeholders(R::COLUMNS.len())
    );
    conn.prepare_cached(&sql)?.execute(&*row.params())?;
    Ok(())
}

/// Inserts [row], or on a primary-key conflict overwrites every other
/// column of the existing row (drift's `insertOnConflictUpdate`: an
/// `ON CONFLICT DO UPDATE`, never `INSERT OR REPLACE`, which would delete
/// the old row and trip the foreign keys of its children).
pub fn upsert_row<R: TableRow>(conn: &Connection, row: &R) -> Result<()> {
    row.validate()?;
    let set = R::COLUMNS
        .iter()
        .filter(|c| !R::PRIMARY_KEY.contains(c))
        .map(|c| format!("\"{c}\" = excluded.\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}",
        R::TABLE,
        quoted(R::COLUMNS),
        placeholders(R::COLUMNS.len()),
        quoted(R::PRIMARY_KEY),
        set
    );
    conn.prepare_cached(&sql)?.execute(&*row.params())?;
    Ok(())
}

/// Overwrites every non-key column of the row with [row]'s primary key.
/// Returns the number of rows changed (0 when there is no such row).
pub fn update_row<R: TableRow>(conn: &Connection, row: &R) -> Result<usize> {
    row.validate()?;
    let params = row.params();
    let mut set = Vec::new();
    let mut key = Vec::new();
    for (i, c) in R::COLUMNS.iter().enumerate() {
        if R::PRIMARY_KEY.contains(c) {
            key.push(format!("\"{c}\" = ?{}", i + 1));
        } else {
            set.push(format!("\"{c}\" = ?{}", i + 1));
        }
    }
    let sql = format!(
        "UPDATE \"{}\" SET {} WHERE {}",
        R::TABLE,
        set.join(", "),
        key.join(" AND ")
    );
    Ok(conn.prepare_cached(&sql)?.execute(&*params)?)
}

/// The rows of [R]'s table matching [where_sql] (empty for all rows), in
/// the order SQLite returns them unless [where_sql] carries an ORDER BY.
pub fn select_rows<R: TableRow>(
    conn: &Connection,
    where_sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<R>> {
    let sql = if where_sql.trim().is_empty() {
        select_sql::<R>()
    } else {
        format!("{} WHERE {where_sql}", select_sql::<R>())
    };
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, |r| R::from_row(r))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The synced row with primary key [id] (live or tombstoned), if any.
pub fn find_row<R: SyncedRow>(conn: &Connection, id: &str) -> Result<Option<R>> {
    let sql = format!("{} WHERE \"id\" = ?1", select_sql::<R>());
    Ok(conn
        .prepare_cached(&sql)?
        .query_row([id], |r| R::from_row(r))
        .optional()?)
}

/// Declares one row struct with its `Default` (the DDL defaults) and its
/// [`TableRow`] impl.
macro_rules! table_row {
    (
        $(#[$meta:meta])*
        $name:ident in $table:literal, key [$($pk:literal),+], synced $synced:ident {
            $( $(#[$fmeta:meta])* $field:ident : $ty:ty = $default:expr ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: $ty, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        impl TableRow for $name {
            const TABLE: &'static str = $table;
            const COLUMNS: &'static [&'static str] = &[$(stringify!($field)),*];
            const PRIMARY_KEY: &'static [&'static str] = &[$($pk),+];

            fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
                Ok(Self { $( $field: row.get(stringify!($field))?, )* })
            }

            fn params(&self) -> Vec<&dyn ToSql> {
                vec![$( &self.$field as &dyn ToSql ),*]
            }

            fn validate(&self) -> Result<()> {
                table_row!(@validate $synced, self, $table)
            }
        }

        table_row!(@synced $synced, $name);
    };
    (@validate yes, $self:ident, $table:literal) => { check_id_length($table, &$self.id) };
    (@validate no, $self:ident, $table:literal) => { Ok(()) };
    (@synced yes, $name:ident) => {
        impl SyncedRow for $name {
            fn id(&self) -> &str { &self.id }
            fn deleted_at(&self) -> Option<i64> { self.deleted_at }
            fn hlc_packed(&self) -> &str { &self.hlc_packed }
        }
    };
    (@synced no, $name:ident) => {};
}

/// Declares a SYNCED table's row: the SyncCols every synced row carries
/// (identity, the Hybrid Logical Clock for entity-level Last-Writer-Wins,
/// the soft-delete tombstone and the record schema version), then the
/// table's own columns.
///
/// IMPORTANT: the SyncCols are bookkeeping, NEVER part of a row's content
/// equality — the reconcile-diff compares only a table's domain columns, so
/// a fader nudge that rewrites a sibling's HLC can't make it look "changed".
macro_rules! synced_row {
    (
        $(#[$meta:meta])*
        $name:ident in $table:literal {
            $( $(#[$fmeta:meta])* $field:ident : $ty:ty = $default:expr ),* $(,)?
        }
    ) => {
        table_row! {
            $(#[$meta])*
            $name in $table, key ["id"], synced yes {
                /// Stable ULID primary key; doubles as the wire record key
                /// (26..=36 characters).
                id: String = String::new(),
                /// Creation wall time (epoch ms).
                created_at: i64 = 0,
                /// Last-modification wall time (epoch ms).
                updated_at: i64 = 0,
                /// Tombstone: None = live. Reads filter `deleted_at IS NULL`;
                /// deletes set it.
                deleted_at: Option<i64> = None,
                /// Hybrid Logical Clock wall (the LWW key's first part).
                hlc_wall: i64 = 0,
                /// Hybrid Logical Clock counter.
                hlc_ctr: i64 = 0,
                /// Hybrid Logical Clock node.
                hlc_node: String = String::new(),
                /// The fixed-width lexicographic encoding of the HLC, stored
                /// for a one-comparison conflict check.
                hlc_packed: String = String::new(),
                /// The app/record schema version that produced this row
                /// (cross-device skew gate + JSON serializer version).
                schema_v: i64 = 1,
                $( $(#[$fmeta])* $field : $ty = $default ),*
            }
        }
    };
}

synced_row! {
    /// A song practice (the top-level aggregate). Atomic config (reverb,
    /// lyric lines) rides as JSON because it is always edited as one unit.
    PracticeRow in "practices" {
        /// The practice's title.
        title: String = String::new(),
        /// The timeline length (ms).
        timeline_duration_ms: i64 = 0,
        /// The master fader (linear gain).
        master_volume: f64 = 1.0,
        /// Which interpretation is active. Logical nullable pointer
        /// (self-healed by the IntegrityChecker if it ever resolves to a
        /// tombstoned interpretation).
        active_interpretation_id: Option<String> = None,
        /// The pre-separation full mix this practice's stems came from (an
        /// asset).
        separation_asset_id: Option<String> = None,
        /// The reverb settings (JSON).
        reverb_json: Option<String> = None,
        /// The lyric lines (JSON).
        lyrics_json: Option<String> = None,
    }
}

synced_row! {
    /// A named interpretation ("reading") grouping a practice's vocal takes.
    InterpretationRow in "interpretations" {
        /// The owning practice.
        practice_id: String = String::new(),
        /// The interpretation's name.
        name: String = String::new(),
        /// Position among the practice's interpretations.
        order_index: i64 = 0,
    }
}

synced_row! {
    /// A practice-level FX group/aux bus tracks can route into.
    FxBusRow in "fx_buses" {
        /// The owning practice.
        practice_id: String = String::new(),
        /// The bus name.
        name: String = String::new(),
        /// The bus colour (ARGB), if set.
        color_value: Option<i64> = None,
        /// The bus fader (linear gain).
        volume: f64 = 1.0,
        /// The bus pan (-1..1).
        pan: f64 = 0.0,
        /// Muted.
        muted: bool = false,
        /// Soloed.
        soloed: bool = false,
        /// The bus this one routes into (None = master).
        output_bus_id: Option<String> = None,
        /// The FX chain (JSON).
        fx_json: Option<String> = None,
        /// The sends (JSON).
        sends_json: Option<String> = None,
        /// Position among the practice's buses.
        order_index: i64 = 0,
    }
}

synced_row! {
    /// A timeline lane: a vocal take (with an interpretation), or the
    /// practice-level accompaniment / reference. Mixer state + FX chain
    /// (JSON) live here.
    TrackRow in "tracks" {
        /// The owning practice.
        practice_id: String = String::new(),
        /// vocal | accompaniment | reference
        kind: String = String::new(),
        /// The interpretation a vocal take belongs to.
        interpretation_id: Option<String> = None,
        /// The lane name.
        display_name: String = String::new(),
        /// The lane colour (ARGB), if set.
        color_value: Option<i64> = None,
        /// Muted.
        muted: bool = false,
        /// Soloed.
        soloed: bool = false,
        /// The fader (linear gain).
        volume: f64 = 1.0,
        /// The pan (-1..1).
        pan: f64 = 0.0,
        /// The FX bus the track routes into (None = master).
        bus_id: Option<String> = None,
        /// The FX chain (JSON).
        fx_json: Option<String> = None,
        /// The sends (JSON).
        sends_json: Option<String> = None,
        /// Position among the practice's takes.
        order_index: i64 = 0,
    }
}

synced_row! {
    /// A clip on a track. References an immutable audio asset (never a
    /// path).
    ClipRow in "clips" {
        /// The owning track.
        track_id: String = String::new(),
        /// The audio asset.
        asset_id: String = String::new(),
        /// Timeline position (ms).
        start_ms: i64 = 0,
        /// Length (ms, >= 0).
        duration_ms: i64 = 0,
        /// Position among the track's clips.
        order_index: i64 = 0,
    }
}

synced_row! {
    /// A free-text note pinned to a track at a point (or range) on the
    /// timeline.
    PracticeNoteRow in "practice_notes" {
        /// The owning track.
        track_id: String = String::new(),
        /// Start (ms).
        position_ms: i64 = 0,
        /// End (ms) of a range note, after the start.
        end_ms: Option<i64> = None,
        /// The note text.
        content: String = String::new(),
    }
}

synced_row! {
    /// A technique annotation pinned to a track. `technique_id` is a LOGICAL
    /// ref into the global library (NOT an enforced FK — it may dangle by
    /// design); the name snapshot keeps the marker readable if the technique
    /// is gone.
    TechniqueMarkerRow in "technique_markers" {
        /// The owning track.
        track_id: String = String::new(),
        /// Start (ms).
        position_ms: i64 = 0,
        /// End (ms) of a range marker, after the start.
        end_ms: Option<i64> = None,
        /// The technique (logical reference).
        technique_id: String = String::new(),
        /// The technique's name when the marker was placed (never empty).
        technique_name_snapshot: String = String::new(),
    }
}

synced_row! {
    /// A technique in the global, cross-practice library. Alias/tag sets are
    /// atomic JSON.
    TechniqueRow in "techniques" {
        /// The technique's name.
        name: String = String::new(),
        /// Aliases (JSON list).
        aliases_json: Option<String> = None,
        /// Tags (JSON list).
        tags_json: Option<String> = None,
        /// One-line summary.
        summary: String = String::new(),
        /// Long description.
        description: String = String::new(),
        /// Coaching cues.
        cues: String = String::new(),
        /// Exercises.
        exercises: String = String::new(),
        /// What it sounds like in the analysis.
        acoustic_signature: String = String::new(),
        /// Position in the library.
        order_index: i64 = 0,
    }
}

synced_row! {
    /// An example clip belonging to a technique. References an immutable
    /// asset.
    TechniqueMediaRow in "technique_media" {
        /// The owning technique.
        technique_id: String = String::new(),
        /// The media asset.
        asset_id: String = String::new(),
        /// audio | video
        kind: String = "audio".to_string(),
        /// The caption.
        label: String = String::new(),
        /// Position among the technique's media.
        order_index: i64 = 0,
    }
}

synced_row! {
    /// The asset indirection. The RECORD syncs; the BYTES sync out-of-band as
    /// a content-addressed blob keyed by `content_hash`. The physical
    /// `local_rel_path` and the sync state columns are PER-DEVICE (stripped
    /// from the wire projection).
    AssetRow in "assets" {
        /// sha256 of the bytes (dedup + integrity).
        content_hash: String = String::new(),
        /// Size of the bytes (>= 0).
        byte_size: i64 = 0,
        /// File extension.
        ext: String = String::new(),
        /// audio | video ...
        kind: String = "audio".to_string(),
        /// Per-device, local-only: the pool path (not in the synced
        /// projection).
        local_rel_path: Option<String> = None,
        /// Per-device, local-only record sync state.
        record_sync_state: String = "localOnly".to_string(),
        /// Per-device, local-only bytes sync state.
        bytes_sync_state: String = "localOnly".to_string(),
        /// Per-device, local-only remote reference.
        remote_ref: Option<String> = None,
    }
}

table_row! {
    /// The durability ledger / change journal (LOCAL-ONLY). A row mutation
    /// and its oplog entry commit in the SAME transaction (an entry exists
    /// iff the mutation committed). The sync pusher drains it;
    /// `depends_on_asset_id` holds a row back until that asset's bytes are
    /// uploaded.
    OplogRow in "oplog", key ["seq"], synced no {
        /// AUTOINCREMENT sequence (see [`OplogRow::insert_new`]).
        seq: i64 = 0,
        /// The journaled row's table.
        entity_table: String = String::new(),
        /// The journaled row's id.
        entity_id: String = String::new(),
        /// insert | update | tombstone
        op: String = String::new(),
        /// The write's packed HLC.
        hlc_packed: String = String::new(),
        /// The written content's hash ('' for tombstones).
        payload_hash: String = String::new(),
        /// Never set today (acks delete).
        synced: bool = false,
        /// The asset whose bytes must upload first.
        depends_on_asset_id: Option<String> = None,
        /// The write's wall time (epoch ms).
        created_at: i64 = 0,
    }
}

impl OplogRow {
    /// Inserts this entry WITHOUT its `seq`, which SQLite assigns
    /// (AUTOINCREMENT, as drift's `OplogCompanion.insert` did). Returns the
    /// assigned sequence.
    pub fn insert_new(&self, conn: &Connection) -> Result<i64> {
        conn.prepare_cached(
            "INSERT INTO \"oplog\" (\"entity_table\", \"entity_id\", \"op\", \"hlc_packed\", \
             \"payload_hash\", \"synced\", \"depends_on_asset_id\", \"created_at\") \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(rusqlite::params![
            self.entity_table,
            self.entity_id,
            self.op,
            self.hlc_packed,
            self.payload_hash,
            self.synced,
            self.depends_on_asset_id,
            self.created_at,
        ])?;
        Ok(conn.last_insert_rowid())
    }
}

table_row! {
    /// RESERVED — currently unwired: nothing in the app reads or writes this
    /// table. It was designed as an in-progress-recording registry (a
    /// pid-tied anchor so an age-based sweep could never reclaim a long live
    /// recording), but no such protection is needed today: every destructive
    /// sweep runs ONLY at boot, when no recording can be in flight. Kept
    /// rather than dropped to avoid schema churn; it MUST be wired up before
    /// any sweep is ever scheduled to run while the app is live.
    RecordingInflightRow in "recordings_inflight", key ["id"], synced no {
        /// The asset ULID being recorded.
        id: String = String::new(),
        /// The practice it records into.
        practice_id: String = String::new(),
        /// The recording process.
        pid: i64 = 0,
        /// When it started (epoch ms).
        started_at: i64 = 0,
    }
}

table_row! {
    /// Local-only waveform cache, keyed by (content_hash, algo_version) so it
    /// is a pure function of immutable bytes + analysis code version and is
    /// regenerated per device — never synced, never stale across app
    /// upgrades.
    WaveformCacheRow in "waveform_cache", key ["content_hash", "algo_version"], synced no {
        /// The asset bytes' sha256.
        content_hash: String = String::new(),
        /// The analysis code version.
        algo_version: i64 = 0,
        /// The encoded peaks.
        peaks: Vec<u8> = Vec::new(),
    }
}

table_row! {
    /// Local-only key-value state for the sync engine: the server change
    /// token, pagination cursors and misc bookkeeping (see
    /// `SyncStateStore`).
    SyncStateRow in "sync_state", key ["key"], synced no {
        /// The key.
        key: String = String::new(),
        /// The value (callers encode anything richer as JSON).
        value: String = String::new(),
    }
}

table_row! {
    /// Local-only quarantine parking lot: the LATEST rejected remote state
    /// per (table, id) — a record whose HLC was implausibly far in the
    /// future, or one the local schema could not apply. The FK sweep's park
    /// of a row whose parent has not arrived sits beside it under a slot key
    /// of its own (`QuarantineStore::sweep_slot_id`).
    ///
    /// SAFETY: the remote stamp column is deliberately named
    /// remote_hlc_packed, NOT hlc_packed. The boot clock seed takes MAX of
    /// every table carrying an `hlc_packed` column, and the synced-table
    /// registry keys off the same SyncCols column names — quarantined stamps
    /// are by definition suspect (often far-future), so reusing the
    /// `hlc_packed` name would poison the local clock at boot, which is
    /// exactly what quarantine exists to prevent.
    SyncQuarantineRow in "sync_quarantine", key ["tbl", "id"], synced no {
        /// The synced table the rejected record targets.
        tbl: String = String::new(),
        /// The rejected record's row id within `tbl` — or a key derived for
        /// its slot (the FK sweep's slot, a dead letter's content hash); a
        /// replayable payload always carries the record's real id.
        id: String = String::new(),
        /// The remote record's packed HLC as received.
        remote_hlc_packed: String = String::new(),
        /// The schema_v the remote record claims.
        schema_v: i64 = 0,
        /// The full record in the SyncRow wire format (JSON), replayable
        /// later.
        payload: String = String::new(),
        /// Why it was quarantined (human-readable, stable prefix).
        reason: String = String::new(),
        /// Wall time (epoch ms) the record was quarantined at.
        quarantined_at: i64 = 0,
    }
}

table_row! {
    /// Local-only asset upload markers: which content hashes this device has
    /// durably uploaded (bytes sync out-of-band from records).
    AssetSyncRow in "asset_sync", key ["hash"], synced no {
        /// The uploaded bytes' sha256.
        hash: String = String::new(),
        /// When the upload completed (epoch ms).
        uploaded_at: i64 = 0,
    }
}
