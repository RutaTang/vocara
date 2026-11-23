//! The sync wire format, the synced-table registry, the inbound value gates
//! and the outbound row projection (port of `lib/data/sync/sync_row.dart`).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use parking_lot::Mutex;
use rusqlite::types::{ToSql, ToSqlOutput, Value as SqlValue, ValueRef};
use serde_json::{Map, Number, Value};
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx, TrackSend};
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::util::dart_json;
use vocara_model::util::id::is_path_component_id;

use crate::dart;
use crate::db::AppDatabase;
use crate::error::{Error, Result};
use crate::sync::hlc::{Hlc, HlcClock};

/// One column value of a row in transit, in the database's value conventions
/// (booleans as ints, REAL as double, NULL as null) — Dart's `Object?` slot
/// of a [`SyncRow`] field map, narrowed to the scalars a column can hold.
///
/// [`FieldValue::Int`] and [`FieldValue::Double`] stay apart exactly as Dart
/// kept `int` and `double` apart: SQLite binds them differently, and the
/// wire JSON spells them differently (`1` vs `1.0`). A JSON `true`/`false`
/// is tolerated on the wire ([`FieldValue::Bool`]; the applier binds it as
/// an int). A hand-built value of the wrong type for its column (a string in
/// an INTEGER column) is representable on purpose: the applier's value gate
/// is what refuses it.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// SQL NULL / JSON null.
    Null,
    /// A JSON boolean (bound as 1 / 0).
    Bool(bool),
    /// A Dart `int` (INTEGER).
    Int(i64),
    /// A Dart `double` (REAL). May be non-finite when hand-built: the wire
    /// decoder refuses those, and so does every gate after it.
    Double(f64),
    /// A Dart `String` (TEXT).
    Text(String),
}

impl FieldValue {
    /// Whether this is null.
    pub fn is_null(&self) -> bool {
        matches!(self, FieldValue::Null)
    }

    /// The text, when this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FieldValue::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, when this is an int (a bool is not one, as in Dart).
    pub fn as_int(&self) -> Option<i64> {
        match self {
            FieldValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The name of the Dart runtime type the value would have (the
    /// `runtimeType` the value gate's messages name).
    pub fn runtime_type(&self) -> &'static str {
        match self {
            FieldValue::Null => "Null",
            FieldValue::Bool(_) => "bool",
            FieldValue::Int(_) => "int",
            FieldValue::Double(_) => "double",
            FieldValue::Text(_) => "String",
        }
    }

    /// The value as Dart's string interpolation writes it (`'$value'`).
    pub fn dart_to_string(&self) -> String {
        match self {
            FieldValue::Null => "null".to_string(),
            FieldValue::Bool(b) => b.to_string(),
            FieldValue::Int(i) => i.to_string(),
            FieldValue::Double(d) => dart_double_to_string(*d),
            FieldValue::Text(s) => s.clone(),
        }
    }

    /// The JSON value of this field, or `None` for a non-finite double
    /// (Dart's `jsonEncode` throws on those).
    pub fn to_json(&self) -> Option<Value> {
        Some(match self {
            FieldValue::Null => Value::Null,
            FieldValue::Bool(b) => Value::Bool(*b),
            FieldValue::Int(i) => Value::Number(Number::from(*i)),
            FieldValue::Double(d) => Value::Number(Number::from_f64(*d)?),
            FieldValue::Text(s) => Value::String(s.clone()),
        })
    }

    /// The field a decoded JSON scalar becomes, as Dart's `jsonDecode` typed
    /// it; `None` for an array or an object (not a scalar). An integer
    /// literal past the 64-bit range is a double, as Dart's decoder makes
    /// it.
    pub fn from_json(value: &Value) -> Option<FieldValue> {
        Some(match value {
            Value::Null => FieldValue::Null,
            Value::Bool(b) => FieldValue::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    FieldValue::Int(i)
                } else {
                    FieldValue::Double(n.as_f64()?)
                }
            }
            Value::String(s) => FieldValue::Text(s.clone()),
            Value::Array(_) | Value::Object(_) => return None,
        })
    }

    /// The field a raw SQLite value reads as (drift's `QueryRow.data`), or
    /// `None` for a BLOB.
    pub fn from_sql(value: ValueRef<'_>) -> Option<FieldValue> {
        Some(match value {
            ValueRef::Null => FieldValue::Null,
            ValueRef::Integer(i) => FieldValue::Int(i),
            ValueRef::Real(d) => FieldValue::Double(d),
            ValueRef::Text(t) => FieldValue::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(_) => return None,
        })
    }
}

/// Dart's `double.toString()`, non-finite values included.
fn dart_double_to_string(d: f64) -> String {
    if d.is_nan() {
        "NaN".to_string()
    } else if d.is_infinite() {
        if d > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        dart_json::dart_double(d).unwrap_or_default()
    }
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.dart_to_string())
    }
}

/// SQLite bind values are int / double / String / null / blob; a bool binds
/// as its integer form (the applier's `_bind`).
impl ToSql for FieldValue {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            FieldValue::Null => ToSqlOutput::Owned(SqlValue::Null),
            FieldValue::Bool(b) => ToSqlOutput::Owned(SqlValue::Integer(i64::from(*b))),
            FieldValue::Int(i) => ToSqlOutput::Owned(SqlValue::Integer(*i)),
            FieldValue::Double(d) => ToSqlOutput::Owned(SqlValue::Real(*d)),
            FieldValue::Text(s) => ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes())),
        })
    }
}

impl From<i64> for FieldValue {
    fn from(v: i64) -> Self {
        FieldValue::Int(v)
    }
}

impl From<i32> for FieldValue {
    fn from(v: i32) -> Self {
        FieldValue::Int(i64::from(v))
    }
}

impl From<f64> for FieldValue {
    fn from(v: f64) -> Self {
        FieldValue::Double(v)
    }
}

impl From<bool> for FieldValue {
    fn from(v: bool) -> Self {
        FieldValue::Bool(v)
    }
}

impl From<&str> for FieldValue {
    fn from(v: &str) -> Self {
        FieldValue::Text(v.to_string())
    }
}

impl From<String> for FieldValue {
    fn from(v: String) -> Self {
        FieldValue::Text(v)
    }
}

impl From<&String> for FieldValue {
    fn from(v: &String) -> Self {
        FieldValue::Text(v.clone())
    }
}

impl<T: Into<FieldValue>> From<Option<T>> for FieldValue {
    fn from(v: Option<T>) -> Self {
        v.map_or(FieldValue::Null, Into::into)
    }
}

/// A row's field map: SQL column name -> value, in insertion order (Dart's
/// map literals keep insertion order, and so does the wire JSON).
pub type Fields = IndexMap<String, FieldValue>;

/// The column-level clock slot: column name -> packed HLC.
pub type FieldClocks = IndexMap<String, String>;

/// A `TypeError` from one of [`SyncRow`]'s typed getters (Dart's `as` cast
/// on a hand-built field map).
fn type_error(key: &str, expected: &str, got: Option<&FieldValue>) -> Error {
    Error::InvalidData(format!(
        "type '{}' is not a subtype of type '{expected}' in type cast (SyncRow \"{key}\")",
        got.map_or("Null", FieldValue::runtime_type)
    ))
}

fn format_error(message: impl Into<String>) -> Error {
    Error::Format(message.into())
}

/// One synced row in transit — the single wire shape shared by the transport
/// (push/pull) and the `SyncApplier`. [`SyncRow::fields`] carries every
/// SYNCED column of the row under its SQL name (all columns minus the few
/// [`PER_DEVICE_COLUMNS`]), in the database's value conventions (booleans as
/// ints, REAL as double, NULL as null); the envelope duplicates the identity
/// trio (id / schema_v / hlc_packed) at the top level for cheap routing
/// without parsing the field map — encode/decode verifies the two stay
/// consistent.
///
/// A `SyncRow` is a value in transit: its fields are only readable once
/// built (Dart froze them with `Map.unmodifiable`), so nothing can mutate
/// them after an LWW comparison (or a future column-clock validation).
#[derive(Debug, Clone, PartialEq)]
pub struct SyncRow {
    table: String,
    fields: Fields,
    field_clocks: Option<FieldClocks>,
}

impl SyncRow {
    /// A row of [table] carrying [fields] (and no column clocks).
    pub fn new(table: impl Into<String>, fields: Fields) -> Self {
        SyncRow {
            table: table.into(),
            fields,
            field_clocks: None,
        }
    }

    /// A row of [table] carrying [fields] and the reserved [field_clocks]
    /// slot.
    pub fn with_field_clocks(
        table: impl Into<String>,
        fields: Fields,
        field_clocks: Option<FieldClocks>,
    ) -> Self {
        SyncRow {
            table: table.into(),
            fields,
            field_clocks,
        }
    }

    /// The synced table this row belongs to (SQL name, e.g. 'practices').
    pub fn table(&self) -> &str {
        &self.table
    }

    /// Every column of the row keyed by SQL column name (SyncCols included).
    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// One field by column name (`fields[name]`).
    pub fn field(&self, name: &str) -> Option<&FieldValue> {
        self.fields.get(name)
    }

    /// RESERVED: the column-level-LWW upgrade slot (column name -> packed
    /// HLC of that column's last writer). ALWAYS `None` in wire format v1 —
    /// the builder never sets it and entity-level LWW ignores it — but the
    /// slot is carried through encode/decode untouched so a future
    /// column-LWW peer's clocks survive a round-trip through a v1 device
    /// instead of being stripped.
    pub fn field_clocks(&self) -> Option<&FieldClocks> {
        self.field_clocks.as_ref()
    }

    /// The row id (`fields['id'] as String`).
    pub fn id(&self) -> Result<&str> {
        match self.fields.get("id") {
            Some(FieldValue::Text(s)) => Ok(s),
            other => Err(type_error("id", "String", other)),
        }
    }

    /// The record schema version (`fields['schema_v'] as int`).
    pub fn schema_v(&self) -> Result<i64> {
        match self.fields.get("schema_v") {
            Some(FieldValue::Int(v)) => Ok(*v),
            other => Err(type_error("schema_v", "int", other)),
        }
    }

    /// The packed HLC, the LWW key (`fields['hlc_packed'] as String`).
    pub fn hlc_packed(&self) -> Result<&str> {
        match self.fields.get("hlc_packed") {
            Some(FieldValue::Text(s)) => Ok(s),
            other => Err(type_error("hlc_packed", "String", other)),
        }
    }

    /// The HLC quad as a stamp.
    pub fn hlc(&self) -> Result<Hlc> {
        let wall = match self.fields.get("hlc_wall") {
            Some(FieldValue::Int(v)) => *v,
            other => return Err(type_error("hlc_wall", "int", other)),
        };
        let counter = match self.fields.get("hlc_ctr") {
            Some(FieldValue::Int(v)) => *v,
            other => return Err(type_error("hlc_ctr", "int", other)),
        };
        let node = match self.fields.get("hlc_node") {
            Some(FieldValue::Text(s)) => s.clone(),
            other => return Err(type_error("hlc_node", "String", other)),
        };
        Ok(Hlc::new(wall, counter, node))
    }

    /// The wire JSON object: table, the identity trio, the fields, and the
    /// column-clock slot (emitted even when null, so a v1 payload is
    /// explicit about it). Fails where Dart's `toJson`/`jsonEncode` threw: a
    /// mistyped identity, or a non-finite double.
    pub fn to_json(&self) -> Result<Value> {
        let mut fields = Map::new();
        for (k, v) in &self.fields {
            let value = v.to_json().ok_or_else(|| {
                Error::InvalidArgument(format!(
                    "Converting object to an encodable object failed: {}",
                    v.dart_to_string()
                ))
            })?;
            fields.insert(k.clone(), value);
        }
        let mut out = Map::new();
        out.insert("table".into(), Value::String(self.table.clone()));
        out.insert("id".into(), Value::String(self.id()?.to_string()));
        out.insert(
            "schemaV".into(),
            Value::Number(Number::from(self.schema_v()?)),
        );
        out.insert(
            "hlcPacked".into(),
            Value::String(self.hlc_packed()?.to_string()),
        );
        out.insert("fields".into(), Value::Object(fields));
        out.insert(
            "fieldClocks".into(),
            match &self.field_clocks {
                None => Value::Null,
                Some(clocks) => Value::Object(
                    clocks
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect(),
                ),
            },
        );
        Ok(Value::Object(out))
    }

    /// The wire JSON text, byte for byte what Dart's `jsonEncode(toJson())`
    /// writes.
    pub fn encode(&self) -> Result<String> {
        dart_json::encode(&self.to_json()?)
            .map_err(|e| Error::InvalidArgument(format!("SyncRow: {e}")))
    }

    /// Strict decode: fails with [`Error::Format`] on a structurally-invalid
    /// payload (missing/mistyped identity, nested values, an envelope that
    /// disagrees with the field map). Strictness is deliberate — a malformed
    /// record must surface as a per-record failure (and quarantine) at the
    /// door, not corrupt a row deep inside an apply.
    pub fn from_json(json: &Map<String, Value>) -> Result<SyncRow> {
        let table = match json.get("table") {
            Some(Value::String(t)) if !t.is_empty() => t.clone(),
            _ => return Err(format_error("SyncRow: missing/invalid \"table\"")),
        };
        let Some(Value::Object(raw_fields)) = json.get("fields") else {
            return Err(format_error("SyncRow: missing/invalid \"fields\""));
        };
        let mut fields = Fields::new();
        for (key, value) in raw_fields {
            // Column values are SQL scalars only. Booleans ride as ints by DB
            // convention but a JSON true/false is tolerated (the applier binds
            // bools to ints); structured values are always a malformed
            // payload.
            let Some(field) = FieldValue::from_json(value) else {
                return Err(format_error(format!(
                    "SyncRow: field \"{key}\" is not a scalar"
                )));
            };
            // Dart's jsonDecode spells an overflowing literal (1e999) as
            // ±Infinity, which no column can hold and no peer can re-encode
            // (see check_column_types). serde_json refuses the literal while
            // parsing; this guards hand-built maps the same way.
            if let FieldValue::Double(d) = field {
                if !d.is_finite() {
                    return Err(format_error(format!(
                        "SyncRow: field \"{key}\" is not a finite number"
                    )));
                }
            }
            fields.insert(key.clone(), field);
        }
        if !matches!(fields.get("id"), Some(FieldValue::Text(_)))
            || !matches!(fields.get("schema_v"), Some(FieldValue::Int(_)))
            || !matches!(fields.get("hlc_packed"), Some(FieldValue::Text(_)))
        {
            return Err(format_error(
                "SyncRow: fields must carry id/schema_v/hlc_packed",
            ));
        }
        // The two HLC representations must agree — see
        // check_hlc_consistency. Checking here (the wire door) also removes
        // the last unchecked casts behind the hlc() getter: a mistyped quad
        // fails HERE as a per-record parse error instead of a type error deep
        // inside an apply.
        Self::check_hlc_consistency(&fields)?;
        // NOTE: check_id_shapes deliberately does NOT run here — from_json
        // decodes whole transport pages, where one hostile record must not
        // fail every good record with it. The applier runs it per record
        // (quarantining the one offender), the push engine per conflicted
        // ack.
        // The envelope trio must agree with the field map — a mismatch means
        // a corrupt or tampered payload and is refused rather than guessed
        // at.
        if !dart_equals(json.get("id"), fields.get("id"))
            || !dart_equals(json.get("schemaV"), fields.get("schema_v"))
            || !dart_equals(json.get("hlcPacked"), fields.get("hlc_packed"))
        {
            return Err(format_error("SyncRow: envelope/fields mismatch"));
        }
        let field_clocks = match json.get("fieldClocks") {
            None | Some(Value::Null) => None,
            Some(Value::Object(raw)) => {
                let mut clocks = FieldClocks::new();
                for (key, value) in raw {
                    let Value::String(v) = value else {
                        return Err(format_error("SyncRow: invalid fieldClocks entry"));
                    };
                    clocks.insert(key.clone(), v.clone());
                }
                Some(clocks)
            }
            Some(_) => return Err(format_error("SyncRow: invalid \"fieldClocks\"")),
        };
        Ok(SyncRow {
            table,
            fields,
            field_clocks,
        })
    }

    /// Decodes the wire JSON text (see [`SyncRow::from_json`]).
    pub fn decode(json: &str) -> Result<SyncRow> {
        let obj: Value = serde_json::from_str(json).map_err(|e| format_error(e.to_string()))?;
        let Value::Object(map) = obj else {
            return Err(format_error("SyncRow: payload is not a JSON object"));
        };
        Self::from_json(&map)
    }

    /// Verifies that [fields] carries a well-typed HLC quad
    /// (hlc_wall / hlc_ctr / hlc_node) plus a String hlc_packed the quad
    /// re-packs to EXACTLY (via the same [`Hlc::packed`] routine every stamp
    /// in the codebase uses); fails with [`Error::Format`] otherwise.
    ///
    /// This is the SINGLE consistency rule for a row's two HLC
    /// representations: LWW compares the PACKED string while the drift gate
    /// (and [`HlcClock::receive`]) judge the QUAD's wall — if the two could
    /// disagree, a record with an honest-looking quad but a forged
    /// far-future hlc_packed would sail through the drift gate and then win
    /// every LWW comparison fleet-wide. Shared by [`SyncRow::from_json`] (the
    /// pull-side wire door) and the push engine's conflicted-ack validation
    /// (a serverRow driving the CAS retry-or-drop decision must satisfy the
    /// same rule) — deliberately ONE routine so the two doors can never
    /// diverge.
    pub fn check_hlc_consistency(fields: &Fields) -> Result<()> {
        let Some(FieldValue::Text(hlc_packed)) = fields.get("hlc_packed") else {
            return Err(format_error(
                "SyncRow: fields must carry a String hlc_packed",
            ));
        };
        let (Some(FieldValue::Int(hlc_wall)), Some(FieldValue::Int(hlc_ctr))) =
            (fields.get("hlc_wall"), fields.get("hlc_ctr"))
        else {
            return Err(quad_type_error());
        };
        let Some(FieldValue::Text(hlc_node)) = fields.get("hlc_node") else {
            return Err(quad_type_error());
        };
        if hlc_node.is_empty() {
            return Err(quad_type_error());
        }
        // Numeric bounds are ORDERING-CORRECTNESS, not pedantry: the packed
        // encoding is fixed-width (13-digit wall, 6-digit counter), so a
        // counter of 1000000 or a 14-digit wall would repack "consistently"
        // while its string order diverged from the tuple order — the LWW
        // comparison and the drift gate would then disagree about which stamp
        // is newer. The node charset admits real (Crockford-envelope) ids and
        // the reserved '!'-extended derived space, bounded so a hostile stamp
        // cannot bloat every packed key it touches.
        if *hlc_ctr < 0 || *hlc_ctr > HlcClock::MAX_COUNTER {
            return Err(format_error(
                "SyncRow: hlc_ctr outside the packed 6-digit range",
            ));
        }
        if *hlc_wall < 0 || *hlc_wall > 9_999_999_999_999 {
            return Err(format_error(
                "SyncRow: hlc_wall outside the packed 13-digit range",
            ));
        }
        if dart::string_length(hlc_node) > HlcClock::MAX_NODE_LENGTH || !node_shape(hlc_node) {
            return Err(format_error(
                "SyncRow: hlc_node outside the supported node grammar",
            ));
        }
        let repacked = Hlc::new(*hlc_wall, *hlc_ctr, hlc_node.clone()).packed();
        if repacked != *hlc_packed {
            return Err(format_error(
                "SyncRow: hlc_packed disagrees with the hlc_wall/hlc_ctr/hlc_node quad",
            ));
        }
        Ok(())
    }

    /// Tables whose OWN id becomes a filesystem path component: the media
    /// services build `audio/<practiceId>/` and
    /// `technique_media/<techniqueId>/` from them and recursively delete
    /// those trees. For these the wire must enforce the same strict shape the
    /// services do ([`is_path_component_id`]) — the superset id shape
    /// otherwise admits a row that syncs cleanly and is then un-operable
    /// locally, because every media call for it throws.
    ///
    /// Child tables are absent on purpose: a clip carries `<trackId>-c0`
    /// under the legacy shape and never names a directory itself.
    pub const PATH_COMPONENT_TABLES: &'static [&'static str] = &["practices", "techniques"];

    /// Validates `id` and every `*_id` column (when non-null) against the
    /// wire id shape, plus the stricter [`is_path_component_id`] for the id
    /// of a [`SyncRow::PATH_COMPONENT_TABLES`] row; fails with
    /// [`Error::Format`] otherwise. Runs at the same wire doors as
    /// [`SyncRow::check_hlc_consistency`]. `hlc_node` is deliberately
    /// excluded — it never touches a path and has its own rules (the `'!'`
    /// derived space).
    pub fn check_id_shapes(table: &str, fields: &Fields) -> Result<()> {
        for (key, value) in fields {
            if key != "id" && !key.ends_with("_id") {
                continue;
            }
            if value.is_null() && key != "id" {
                continue;
            }
            let id = match value {
                FieldValue::Text(s) if id_shape(s) => s,
                _ => {
                    return Err(format_error(format!(
                        "SyncRow: \"{key}\" is not a well-formed entity id"
                    )))
                }
            };
            if key == "id"
                && Self::PATH_COMPONENT_TABLES.contains(&table)
                && !is_path_component_id(id)
            {
                return Err(format_error(format!(
                    "SyncRow: \"{table}.id\" must be usable as a path component"
                )));
            }
        }
        Ok(())
    }
}

fn quad_type_error() -> Error {
    format_error("SyncRow: fields must carry an int hlc_wall/hlc_ctr and a non-empty hlc_node")
}

/// Node grammar: a DERIVATION CHAIN — the minting device's 26-character id,
/// then one `'!'`-joined segment per derived write ([`HlcClock::derived`]):
/// `^[0-9A-Za-z]{26}(?:![0-9A-Za-z]{26})*$`. Pinning the segment WIDTH, not
/// just the alphabet, is what keeps the node-space derivation available: an
/// arbitrary blob one character below the ceiling would force every stamp
/// derived from it into the ceiling rule. Deliberately a SUPERSET of the
/// Crockford alphabet ids are minted from, like the id shape — the wire
/// depends on the shape, and only the mint needs the exact alphabet. No
/// dash: the packed encoding uses it as the field separator.
fn node_shape(node: &str) -> bool {
    node.split('!').all(|segment| {
        segment.len() == HlcClock::NODE_SEGMENT_LENGTH
            && segment.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

/// The envelope every entity id on the wire must fit: 26–36 characters of
/// `[0-9A-Za-z_-]`. This is a SECURITY gate, not a style check: ids become
/// filesystem path components (`audio/<practiceId>/`,
/// `technique_media/<techniqueId>/`) and are handed to recursive deletes —
/// an id carrying `..`, a separator, or an absolute path would let a hostile
/// sync record write or delete OUTSIDE the media tree. No dot and no
/// separator in the alphabet closes that entirely.
///
/// The length is the one every synced table's id column declares (SyncCols
/// in tables.dart). drift enforced it only on its own inserts, so the
/// applier's raw-SQL insert would otherwise land a row no local write could
/// have produced — and that the repository's revive path (a checked insert)
/// then refuses, failing the save.
///
/// Deliberately a SUPERSET of today's id grammars — `new_id` ULIDs plus the
/// legacy-migration shapes `<trackId>-c0` (clips) and `<practiceId>__v1`
/// (interpretations) — rather than an exact enumeration: the services whose
/// components really are pure ULIDs (audio/technique media dirs) enforce
/// that stricter shape themselves. Quarantine slot keys
/// (`QuarantineStore::sweep_slot_id`, dead letters) never ride the wire.
fn id_shape(id: &str) -> bool {
    (26..=36).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Dart's `==` between a decoded envelope value and a field value: numbers
/// compare numerically (`1.0 == 1`), everything else by type and content.
fn dart_equals(json: Option<&Value>, field: Option<&FieldValue>) -> bool {
    let json = json.unwrap_or(&Value::Null);
    let field = field.unwrap_or(&FieldValue::Null);
    match (json, field) {
        (Value::Null, FieldValue::Null) => true,
        (Value::Bool(a), FieldValue::Bool(b)) => a == b,
        (Value::String(a), FieldValue::Text(b)) => a == b,
        (Value::Number(n), FieldValue::Int(i)) => match n.as_i64() {
            Some(a) => a == *i,
            None => n.as_f64() == Some(*i as f64),
        },
        (Value::Number(n), FieldValue::Double(d)) => match n.as_i64() {
            Some(a) => a as f64 == *d,
            None => n.as_f64() == Some(*d),
        },
        _ => false,
    }
}

/// The record schema version this build writes (and fully understands).
/// Every synced table's schema_v column defaults to it, and the
/// `SyncApplier` quarantines a record claiming a NEWER version: such a
/// record's shape is by definition not fully understood here, and the
/// quarantine replay lets it in after the app upgrade that is. Bump ONLY
/// together with the schema default when the wire record shape changes.
pub const CURRENT_RECORD_SCHEMA_V: i64 = 1;

/// The full SyncCols column set — the structural marker of a synced table.
/// Requiring ALL of them (not just hlc_packed) keeps the registry from ever
/// picking up a local-only table: the oplog carries hlc_packed but no
/// id/deleted_at/schema_v, and sync_quarantine carries id/schema_v but its
/// stamp column is remote_hlc_packed by design.
pub const SYNC_COL_NAMES: &[&str] = &[
    "id",
    "created_at",
    "updated_at",
    "deleted_at",
    "hlc_wall",
    "hlc_ctr",
    "hlc_node",
    "hlc_packed",
    "schema_v",
];

/// PER-DEVICE columns, by table: local bookkeeping that must never cross the
/// wire ("stripped from the wire projection"). [`SyncRowBuilder`] omits them
/// from the outbound projection and the `SyncApplier` drops them from
/// inbound records, so one device's bookkeeping can never clobber another's
/// (e.g. A's `<hash>.wav` local_rel_path overwriting the path of B's
/// already-materialized `<hash>.mp3`, making B's resolve() miss bytes that
/// exist). An absent column is the applier's normal missing-column case: an
/// UPDATE leaves the local value untouched and an INSERT takes the schema
/// default.
pub const PER_DEVICE_COLUMNS: &[(&str, &[&str])] = &[(
    "assets",
    &[
        "local_rel_path",
        "record_sync_state",
        "bytes_sync_state",
        "remote_ref",
    ],
)];

/// The per-device columns of [table] ([`PER_DEVICE_COLUMNS`]; empty for a
/// table without any).
pub fn per_device_columns(table: &str) -> &'static [&'static str] {
    PER_DEVICE_COLUMNS
        .iter()
        .find(|(t, _)| *t == table)
        .map_or(&[], |(_, cols)| cols)
}

/// Per-device column VALUES the `SyncApplier` force-sets on an INBOUND row
/// (insert and update alike), after stripping [`PER_DEVICE_COLUMNS`]. Kept
/// next to that map — the two are the single structural description of how
/// per-device bookkeeping behaves at the remote door — so a new override can
/// never be string-scattered through the applier. Every value is TEXT.
///
/// Why `assets.record_sync_state = 'synced'`: a row that arrived over the
/// wire has BY DEFINITION crossed it, and another device may already
/// reference the asset. Left at the schema default 'localOnly' (which is
/// exactly what an insert of a stripped row lands with), the asset-GC's
/// never-synced guard (AssetStore.sweepUnreferenced) would treat the very
/// rows it must protect as sweepable, tombstone them off local knowledge
/// alone, and replicate the delete fleet-wide. `bytes_sync_state` is
/// deliberately NOT overridden: it tracks this device's blob
/// upload/download state, which has no transport yet — and the GC predicate
/// requires BOTH columns to be 'localOnly', so marking the record side alone
/// already protects every wire-arrived row.
pub const INBOUND_PER_DEVICE_OVERRIDES: &[(&str, &[(&str, &str)])] =
    &[("assets", &[("record_sync_state", "synced")])];

/// The inbound overrides of [table] ([`INBOUND_PER_DEVICE_OVERRIDES`]).
pub fn inbound_per_device_overrides(
    table: &str,
) -> Option<&'static [(&'static str, &'static str)]> {
    INBOUND_PER_DEVICE_OVERRIDES
        .iter()
        .find(|(t, _)| *t == table)
        .map(|(_, overrides)| *overrides)
}

/// What one column holds — drift's `DriftSqlType` of the compiled schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    /// `integer()` (INTEGER).
    Int,
    /// `boolean()` (INTEGER with a `CHECK (c IN (0, 1))`).
    Bool,
    /// `real()` (REAL).
    Double,
    /// `text()` (TEXT).
    Text,
    /// `blob()` (BLOB).
    Blob,
}

/// One column of the compiled schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSpec {
    /// The SQL column name.
    pub name: &'static str,
    /// What it holds.
    pub ty: ColumnType,
}

/// One table of the compiled schema (drift's `TableInfo`): its name and its
/// columns in declaration order (`$columns`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSpec {
    /// The SQL table name.
    pub name: &'static str,
    /// Every column, in declaration order.
    pub columns: &'static [ColumnSpec],
}

impl TableSpec {
    /// Whether the table has a column named [name].
    pub fn has_column(&self, name: &str) -> bool {
        self.columns.iter().any(|c| c.name == name)
    }
}

macro_rules! columns {
    ($($name:literal: $ty:ident),* $(,)?) => {
        &[$(ColumnSpec { name: $name, ty: ColumnType::$ty }),*]
    };
}

macro_rules! synced_columns {
    ($($name:literal: $ty:ident),* $(,)?) => {
        columns![
            "id": Text,
            "created_at": Int,
            "updated_at": Int,
            "deleted_at": Int,
            "hlc_wall": Int,
            "hlc_ctr": Int,
            "hlc_node": Text,
            "hlc_packed": Text,
            "schema_v": Int,
            $($name: $ty),*
        ]
    };
}

/// Every table of the compiled schema, in drift's declaration order
/// (`AppDatabase.allTables`): the column lists of `db::rows` with their
/// drift types. A test pins it to `ALL_TABLES`, to the row structs' columns
/// and to the live schema's declared types, so it cannot drift from either.
pub const ALL_TABLE_SPECS: &[TableSpec] = &[
    TableSpec {
        name: "practices",
        columns: synced_columns![
            "title": Text,
            "timeline_duration_ms": Int,
            "master_volume": Double,
            "active_interpretation_id": Text,
            "separation_asset_id": Text,
            "reverb_json": Text,
            "lyrics_json": Text,
        ],
    },
    TableSpec {
        name: "interpretations",
        columns: synced_columns![
            "practice_id": Text,
            "name": Text,
            "order_index": Int,
        ],
    },
    TableSpec {
        name: "tracks",
        columns: synced_columns![
            "practice_id": Text,
            "kind": Text,
            "interpretation_id": Text,
            "display_name": Text,
            "color_value": Int,
            "muted": Bool,
            "soloed": Bool,
            "volume": Double,
            "pan": Double,
            "bus_id": Text,
            "fx_json": Text,
            "sends_json": Text,
            "order_index": Int,
        ],
    },
    TableSpec {
        name: "clips",
        columns: synced_columns![
            "track_id": Text,
            "asset_id": Text,
            "start_ms": Int,
            "duration_ms": Int,
            "order_index": Int,
        ],
    },
    TableSpec {
        name: "practice_notes",
        columns: synced_columns![
            "track_id": Text,
            "position_ms": Int,
            "end_ms": Int,
            "content": Text,
        ],
    },
    TableSpec {
        name: "technique_markers",
        columns: synced_columns![
            "track_id": Text,
            "position_ms": Int,
            "end_ms": Int,
            "technique_id": Text,
            "technique_name_snapshot": Text,
        ],
    },
    TableSpec {
        name: "fx_buses",
        columns: synced_columns![
            "practice_id": Text,
            "name": Text,
            "color_value": Int,
            "volume": Double,
            "pan": Double,
            "muted": Bool,
            "soloed": Bool,
            "output_bus_id": Text,
            "fx_json": Text,
            "sends_json": Text,
            "order_index": Int,
        ],
    },
    TableSpec {
        name: "techniques",
        columns: synced_columns![
            "name": Text,
            "aliases_json": Text,
            "tags_json": Text,
            "summary": Text,
            "description": Text,
            "cues": Text,
            "exercises": Text,
            "acoustic_signature": Text,
            "order_index": Int,
        ],
    },
    TableSpec {
        name: "technique_media",
        columns: synced_columns![
            "technique_id": Text,
            "asset_id": Text,
            "kind": Text,
            "label": Text,
            "order_index": Int,
        ],
    },
    TableSpec {
        name: "assets",
        columns: synced_columns![
            "content_hash": Text,
            "byte_size": Int,
            "ext": Text,
            "kind": Text,
            "local_rel_path": Text,
            "record_sync_state": Text,
            "bytes_sync_state": Text,
            "remote_ref": Text,
        ],
    },
    TableSpec {
        name: "oplog",
        columns: columns![
            "seq": Int,
            "entity_table": Text,
            "entity_id": Text,
            "op": Text,
            "hlc_packed": Text,
            "payload_hash": Text,
            "synced": Bool,
            "depends_on_asset_id": Text,
            "created_at": Int,
        ],
    },
    TableSpec {
        name: "recordings_inflight",
        columns: columns![
            "id": Text,
            "practice_id": Text,
            "pid": Int,
            "started_at": Int,
        ],
    },
    TableSpec {
        name: "waveform_cache",
        columns: columns![
            "content_hash": Text,
            "algo_version": Int,
            "peaks": Blob,
        ],
    },
    TableSpec {
        name: "sync_state",
        columns: columns!["key": Text, "value": Text],
    },
    TableSpec {
        name: "sync_quarantine",
        columns: columns![
            "tbl": Text,
            "id": Text,
            "remote_hlc_packed": Text,
            "schema_v": Int,
            "payload": Text,
            "reason": Text,
            "quarantined_at": Int,
        ],
    },
    TableSpec {
        name: "asset_sync",
        columns: columns!["hash": Text, "uploaded_at": Int],
    },
];

/// Table name -> [`TableSpec`] for exactly the SYNCED tables. Derived
/// structurally from the SyncCols columns rather than a hand-kept list so a
/// new synced table can never be silently left out (same philosophy as the
/// boot clock seed's hlc_packed scan, tightened to the full marker set so
/// local-only bookkeeping tables are excluded). In declaration order.
pub fn synced_table_registry() -> &'static IndexMap<&'static str, &'static TableSpec> {
    static REGISTRY: OnceLock<IndexMap<&'static str, &'static TableSpec>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        ALL_TABLE_SPECS
            .iter()
            .filter(|t| SYNC_COL_NAMES.iter().all(|c| t.has_column(c)))
            .map(|t| (t.name, t))
            .collect()
    })
}

/// The compiled spec of the synced table [name], if it is one.
pub fn synced_table(name: &str) -> Option<&'static TableSpec> {
    synced_table_registry().get(name).copied()
}

/// A `*_json` column's body decoder: runs the decoder of the repository that
/// hydrates the column, failing if the body is not that shape.
pub type JsonColumnDecoder = fn(&Value) -> vocara_model::Result<()>;

/// Decodes one `*_json` column body the way the REPOSITORY that hydrates it
/// does, failing if the body is not that shape. Keyed by column name (the
/// name carries the shape: `fx_json` is a `TrackFx` object in every table
/// that has one), and exhaustive over the schema — a schema-derived test
/// fails if a synced table gains a `*_json` column with no entry here.
///
/// Parseable JSON is not enough. `lyrics_json = '{}'` is valid JSON that
/// binds to a TEXT column and commits happily, and then the hydrator's
/// `as List` cast throws — inside a view model, long after the record could
/// have been parked with its evidence, leaving a practice that has vanished
/// from the list and cannot be opened. Running the real decoder at the door
/// makes the gate exactly as strict as the reader, by construction: anything
/// that survives here hydrates.
pub const JSON_COLUMN_DECODERS: &[(&str, JsonColumnDecoder)] = &[
    ("reverb_json", decode_reverb),
    ("lyrics_json", decode_lyrics),
    ("fx_json", decode_fx),
    ("sends_json", decode_sends),
    ("aliases_json", decode_string_list),
    ("tags_json", decode_string_list),
];

/// The decoder of the `*_json` column [column], if one is registered.
pub fn json_column_decoder(column: &str) -> Option<JsonColumnDecoder> {
    JSON_COLUMN_DECODERS
        .iter()
        .find(|(c, _)| *c == column)
        .map(|(_, d)| *d)
}

fn list_of<'a>(body: &'a Value, what: &str) -> vocara_model::Result<&'a Vec<Value>> {
    match body {
        Value::Array(items) => Ok(items),
        other => Err(vocara_model::Error::Type(format!(
            "{what}: {} is not a subtype of List<dynamic>",
            json_kind(other)
        ))),
    }
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() => "int",
        Value::Number(_) => "double",
        Value::String(_) => "String",
        Value::Array(_) => "List",
        Value::Object(_) => "Map",
    }
}

fn decode_reverb(body: &Value) -> vocara_model::Result<()> {
    ReverbSettings::from_json(body).map(|_| ())
}

fn decode_fx(body: &Value) -> vocara_model::Result<()> {
    TrackFx::from_json(body).map(|_| ())
}

fn decode_lyrics(body: &Value) -> vocara_model::Result<()> {
    for e in list_of(body, "lyrics_json")? {
        LyricLine::from_json(e)?;
    }
    Ok(())
}

fn decode_sends(body: &Value) -> vocara_model::Result<()> {
    for e in list_of(body, "sends_json")? {
        TrackSend::from_json(e)?;
    }
    Ok(())
}

// Converted, not cast lazily: a cast only throws when the UI later iterates
// it, the "fails far away" problem this gate exists to end.
fn decode_string_list(body: &Value) -> vocara_model::Result<()> {
    for e in list_of(body, "List<String>")? {
        if !e.is_string() {
            return Err(vocara_model::Error::Type(format!(
                "{} is not a subtype of String",
                json_kind(e)
            )));
        }
    }
    Ok(())
}

/// Rejects any wire value that would not BIND to its column, using the
/// compiled schema as the single source of what each column holds. Fails
/// with [`Error::Format`] on the first mismatch (in column order).
///
/// SQLite's dynamic typing accepts a string into an INTEGER column without
/// complaint, so the failure surfaced far away — when the row was hydrated
/// and threw inside a view model — rather than at the door where the record
/// could still be quarantined with its evidence. `*_json` columns are parsed
/// too: a malformed body is only detectable there, and it fails the same
/// way. A REAL must be finite: SQLite stores ±Infinity happily, but the
/// content hash every repository computes on load refuses it (the practice
/// could no longer be opened) and no row carrying it can be JSON-encoded
/// again (the FK sweep's park, a re-push).
///
/// Blob columns are refused outright: the v1 wire format encodes scalars
/// only ([`SyncRow::from_json`]), so a synced table gaining such a column is
/// a protocol change that must be designed, not silently admitted through
/// whatever JSON happened to arrive.
pub fn check_column_types(table: &TableSpec, fields: &Fields) -> Result<()> {
    for column in table.columns {
        let Some(value) = fields.get(column.name) else {
            continue;
        };
        if value.is_null() {
            continue; // nullability is the database's own gate
        }
        let ok = match column.ty {
            ColumnType::Int => matches!(value, FieldValue::Int(_)),
            ColumnType::Bool => matches!(value, FieldValue::Bool(_) | FieldValue::Int(_)),
            ColumnType::Double => match value {
                FieldValue::Int(_) => true,
                FieldValue::Double(d) => d.is_finite(),
                _ => false,
            },
            ColumnType::Text => matches!(value, FieldValue::Text(_)),
            ColumnType::Blob => false,
        };
        if !ok {
            return Err(format_error(format!(
                "SyncRow: \"{}.{}\" cannot hold a {}",
                table.name,
                column.name,
                value.runtime_type()
            )));
        }
        if let FieldValue::Int(v) = value {
            // Compared on both sides, not by abs(): the most negative int has
            // none.
            if let Some(bound) = int_column_bound(column.name) {
                if *v > bound || *v < -bound {
                    return Err(format_error(format!(
                        "SyncRow: \"{}.{}\" is out of range ({v})",
                        table.name, column.name
                    )));
                }
            }
        }
        if let (true, FieldValue::Text(text)) = (column.name.ends_with("_json"), value) {
            let Ok(body) = serde_json::from_str::<Value>(text) else {
                return Err(format_error(format!(
                    "SyncRow: \"{}.{}\" is not JSON",
                    table.name, column.name
                )));
            };
            let Some(decode) = json_column_decoder(column.name) else {
                // A synced table gained a JSON column and nobody said what it
                // holds. Refusing it is the same stance the blob branch takes:
                // an undesigned wire shape is quarantined, never admitted on
                // the strength of "it parsed". The schema-derived test makes
                // this unreachable in a build that ships.
                return Err(format_error(format!(
                    "SyncRow: \"{}.{}\" has no registered JSON body shape",
                    table.name, column.name
                )));
            };
            match decode(&body) {
                Ok(()) => {}
                // A decoder's own FormatException passes through as is.
                Err(vocara_model::Error::Format { message, .. }) => {
                    return Err(format_error(message));
                }
                // Every other decoder failure must leave here as a format
                // error: that is what the applier catches to park the record
                // with its evidence.
                Err(e) => {
                    let kind = match e {
                        vocara_model::Error::Type(_) => "TypeError",
                        _ => "JsonUnsupportedObjectError",
                    };
                    return Err(format_error(format!(
                        "SyncRow: \"{}.{}\" is JSON but not the shape that column holds ({kind})",
                        table.name, column.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The magnitude an int column may hold on the wire, by its name, or None
/// for none. A time (`*_at`, ms since the epoch) within what a DateTime
/// holds: past it, every list of the library threw. A timeline position or
/// length (`*_ms`) within a day: past it, the timeline was laid out and
/// gridded over trillions of steps and the practice froze.
fn int_column_bound(column: &str) -> Option<i64> {
    if column.ends_with("_at") {
        return Some(8_640_000_000_000_000);
    }
    if column.ends_with("_ms") {
        return Some(24 * 60 * 60 * 1000);
    }
    None
}

/// Reads current rows generically (via the compiled schema registry) and
/// projects them into the wire format. Every column of a synced table is
/// synced by design, so the projection is "all columns" minus the few
/// [`PER_DEVICE_COLUMNS`] (local bookkeeping that must not leave this
/// device).
#[derive(Debug)]
pub struct SyncRowBuilder {
    db: Arc<AppDatabase>,
    ranks: Mutex<Option<Arc<HashMap<String, i64>>>>,
}

impl SyncRowBuilder {
    /// A builder reading [db].
    pub fn new(db: Arc<AppDatabase>) -> Self {
        SyncRowBuilder {
            db,
            ranks: Mutex::new(None),
        }
    }

    /// The synced tables this builder (and the wire format) recognizes.
    pub fn table_names(&self) -> HashSet<&'static str> {
        synced_table_registry().keys().copied().collect()
    }

    /// Parent-before-child upload rank per synced table, derived from the
    /// live SQLite foreign-key graph: a table ranks strictly above every
    /// synced table it references, so sorting a batch by rank puts every
    /// parent row before its children.
    ///
    /// This exists for the pusher: the oplog's collapsed order is LATEST-op
    /// order, and a cascade delete journals children before their parent, so
    /// a deleted aggregate would otherwise be uploaded child-first. A
    /// receiver that has never seen those rows applies them as fresh
    /// inserts, and a page boundary inside the cascade would leave a child
    /// whose parent never arrived — the applier's FK sweep then drops it
    /// with the change token already advanced, losing the row permanently
    /// (found by the convergence simulation, seed 0x5eed0001). Sorting by
    /// this rank restores the `FetchPage` ordering contract for every push.
    ///
    /// Derived (not hand-kept) so a new synced table can never be missed;
    /// memoized because the schema is fixed for the life of the process.
    pub fn table_ranks(&self) -> Result<Arc<HashMap<String, i64>>> {
        if let Some(cached) = self.ranks.lock().clone() {
            return Ok(cached);
        }
        let registry = synced_table_registry();
        let mut deps: HashMap<&str, Vec<String>> = HashMap::new();
        for table in registry.keys() {
            let parents: Vec<String> =
                self.db
                    .query(&format!("PRAGMA foreign_key_list(\"{table}\")"), [], |r| {
                        r.get::<_, String>("table")
                    })?;
            let mut set: Vec<String> = Vec::new();
            for parent in parents {
                if registry.contains_key(parent.as_str())
                    && parent != *table
                    && !set.contains(&parent)
                {
                    set.push(parent);
                }
            }
            deps.insert(table, set);
        }
        fn rank_of(
            table: &str,
            deps: &HashMap<&str, Vec<String>>,
            ranks: &mut HashMap<String, i64>,
            visiting: &mut HashSet<String>,
        ) -> Result<i64> {
            if let Some(known) = ranks.get(table) {
                return Ok(*known);
            }
            if !visiting.insert(table.to_string()) {
                // An FK cycle would make parent-first ordering undefined. No
                // synced table participates in one today; fail loudly if that
                // ever changes.
                return Err(Error::State(format!(
                    "SyncRowBuilder: FK cycle involving \"{table}\""
                )));
            }
            let mut rank = 0;
            for parent in deps.get(table).map(Vec::as_slice).unwrap_or_default() {
                let r = rank_of(parent, deps, ranks, visiting)? + 1;
                if r > rank {
                    rank = r;
                }
            }
            visiting.remove(table);
            ranks.insert(table.to_string(), rank);
            Ok(rank)
        }
        let mut ranks = HashMap::new();
        for table in registry.keys() {
            rank_of(table, &deps, &mut ranks, &mut HashSet::new())?;
        }
        let ranks = Arc::new(ranks);
        *self.ranks.lock() = Some(ranks.clone());
        Ok(ranks)
    }

    /// Builds the SyncRow for the CURRENT state of ([table], [id]), or None
    /// if no such row exists (e.g. it was hard-deleted as a never-synced
    /// draft before the drain caught up). Fails with
    /// [`Error::InvalidArgument`] for a non-synced table name — pushing
    /// local-only tables is always a programming error.
    pub fn build(&self, table: &str, id: &str) -> Result<Option<SyncRow>> {
        let Some(info) = synced_table(table) else {
            return Err(Error::InvalidArgument(format!(
                "table: {table}: not a synced table"
            )));
        };
        // Select the spec's own column list explicitly (not *) so the
        // projection is exactly the compiled schema even if the raw file
        // carries stray columns from some future/debug state.
        let cols = info
            .columns
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect::<Vec<_>>()
            .join(", ");
        let row: Option<Vec<SqlValue>> = self.db.query_opt(
            &format!("SELECT {cols} FROM \"{}\" WHERE id = ?", info.name),
            [id],
            |r| {
                (0..info.columns.len())
                    .map(|i| r.get::<_, SqlValue>(i))
                    .collect()
            },
        )?;
        let Some(row) = row else {
            return Ok(None);
        };
        let per_device = per_device_columns(info.name);
        let mut fields = Fields::new();
        for (column, value) in info.columns.iter().zip(row.iter()) {
            if per_device.contains(&column.name) {
                continue; // never leaves this device
            }
            let Some(value) = FieldValue::from_sql(ValueRef::from(value)) else {
                // No synced table carries a BLOB today; refuse loudly rather
                // than let the encoder fail (or silently mangle bytes) if one
                // ever appears.
                return Err(Error::State(format!(
                    "SyncRowBuilder: BLOB column {}.{} is not representable in the v1 \
                     wire format",
                    info.name, column.name
                )));
            };
            fields.insert(column.name.to_string(), value);
        }
        Ok(Some(SyncRow::new(info.name, fields)))
    }
}

impl fmt::Display for SyncRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let show = |key: &str| {
            self.fields
                .get(key)
                .map_or_else(|| "null".to_string(), FieldValue::dart_to_string)
        };
        write!(
            f,
            "SyncRow({}/{} @{})",
            self.table,
            show("id"),
            show("hlc_packed")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::rows::{
        AssetRow, AssetSyncRow, ClipRow, FxBusRow, InterpretationRow, OplogRow, PracticeNoteRow,
        PracticeRow, RecordingInflightRow, SyncQuarantineRow, SyncStateRow, TableRow,
        TechniqueMarkerRow, TechniqueMediaRow, TechniqueRow, TrackRow, WaveformCacheRow,
    };
    use crate::db::schema::ALL_TABLES;

    fn row_columns() -> Vec<(&'static str, &'static [&'static str])> {
        vec![
            (PracticeRow::TABLE, PracticeRow::COLUMNS),
            (InterpretationRow::TABLE, InterpretationRow::COLUMNS),
            (TrackRow::TABLE, TrackRow::COLUMNS),
            (ClipRow::TABLE, ClipRow::COLUMNS),
            (PracticeNoteRow::TABLE, PracticeNoteRow::COLUMNS),
            (TechniqueMarkerRow::TABLE, TechniqueMarkerRow::COLUMNS),
            (FxBusRow::TABLE, FxBusRow::COLUMNS),
            (TechniqueRow::TABLE, TechniqueRow::COLUMNS),
            (TechniqueMediaRow::TABLE, TechniqueMediaRow::COLUMNS),
            (AssetRow::TABLE, AssetRow::COLUMNS),
            (OplogRow::TABLE, OplogRow::COLUMNS),
            (RecordingInflightRow::TABLE, RecordingInflightRow::COLUMNS),
            (WaveformCacheRow::TABLE, WaveformCacheRow::COLUMNS),
            (SyncStateRow::TABLE, SyncStateRow::COLUMNS),
            (SyncQuarantineRow::TABLE, SyncQuarantineRow::COLUMNS),
            (AssetSyncRow::TABLE, AssetSyncRow::COLUMNS),
        ]
    }

    #[test]
    fn the_compiled_schema_matches_the_tables_the_row_structs_and_the_live_schema() {
        // Same tables, same order as drift's allTables.
        let names: Vec<&str> = ALL_TABLE_SPECS.iter().map(|t| t.name).collect();
        assert_eq!(names, ALL_TABLES);
        // Same columns, same order as the row structs.
        for ((table, columns), spec) in row_columns().into_iter().zip(ALL_TABLE_SPECS) {
            assert_eq!(table, spec.name);
            let spec_columns: Vec<&str> = spec.columns.iter().map(|c| c.name).collect();
            assert_eq!(spec_columns, columns, "{table}");
        }
        // Same types as the live schema declares: INTEGER for int and bool
        // (a bool carries its CHECK (c IN (0, 1))), REAL, TEXT, BLOB.
        let db = AppDatabase::open_in_memory().unwrap();
        for spec in ALL_TABLE_SPECS {
            let live: Vec<(String, String)> = db
                .query(&format!("PRAGMA table_info(\"{}\")", spec.name), [], |r| {
                    Ok((r.get("name")?, r.get("type")?))
                })
                .unwrap();
            let ddl: String = db
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
                    [spec.name],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(live.len(), spec.columns.len(), "{}", spec.name);
            for (column, (name, declared)) in spec.columns.iter().zip(live) {
                assert_eq!(column.name, name);
                let bool_check = ddl.contains(&format!("CHECK (\"{name}\" IN (0, 1))"));
                let want = match declared.as_str() {
                    "INTEGER" if bool_check => ColumnType::Bool,
                    "INTEGER" => ColumnType::Int,
                    "REAL" => ColumnType::Double,
                    "TEXT" => ColumnType::Text,
                    "BLOB" => ColumnType::Blob,
                    other => panic!("{}.{name}: unexpected type {other}", spec.name),
                };
                assert_eq!(column.ty, want, "{}.{name}", spec.name);
            }
        }
    }

    #[test]
    fn node_and_id_shapes() {
        let seg = "A".repeat(26);
        assert!(node_shape(&seg));
        assert!(node_shape(&format!("{seg}!{seg}")));
        assert!(!node_shape(""));
        assert!(!node_shape(&format!("{seg}!")));
        assert!(!node_shape(&"A".repeat(25)));
        assert!(!node_shape(&format!("{}-", "A".repeat(25))));
        assert!(id_shape(&"a_-".repeat(9)));
        assert!(!id_shape(&"A".repeat(25)));
        assert!(!id_shape(&"A".repeat(37)));
        assert!(!id_shape(&format!("{}.", "A".repeat(25))));
    }

    #[test]
    fn dart_display_of_values() {
        assert_eq!(
            FieldValue::Double(f64::INFINITY).dart_to_string(),
            "Infinity"
        );
        assert_eq!(
            FieldValue::Double(f64::NEG_INFINITY).dart_to_string(),
            "-Infinity"
        );
        assert_eq!(FieldValue::Double(1.0).dart_to_string(), "1.0");
        assert_eq!(FieldValue::Null.dart_to_string(), "null");
    }
}
