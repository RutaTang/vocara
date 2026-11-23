//! What both repositories share: the timestamp conversions, the JSON text
//! of the JSON columns, the drift companion writes, and the Dart names of
//! the errors a diagnostics line reports.

use rusqlite::types::ToSql;
use serde_json::Value;
use vocara_model::util::dart_date_time::{DateTime, MAX_MILLISECONDS_SINCE_EPOCH};
use vocara_model::util::dart_json;
use vocara_model::util::dart_num::clamp_i64;

use crate::db::rows::check_id_length;
use crate::db::AppDatabase;
use crate::error::{Error, Result};
use crate::sync::sync_writer::SyncStamp;

/// `d.millisecondsSinceEpoch`.
pub(crate) fn ms(d: &DateTime) -> i64 {
    d.millis_since_epoch()
}

/// `DateTime.fromMillisecondsSinceEpoch(ms)`, clamped to what a DateTime
/// holds: a stored time past it (written by a peer, or any build that let
/// one through) threw for every list of the library, which then showed
/// nothing.
pub(crate) fn dt(ms: i64) -> DateTime {
    let clamped = clamp_i64(
        ms,
        -MAX_MILLISECONDS_SINCE_EPOCH,
        MAX_MILLISECONDS_SINCE_EPOCH,
    );
    match DateTime::from_millis_since_epoch(clamped, false) {
        Some(d) => d,
        None => unreachable!("a clamped time is representable"),
    }
}

/// Dart's `jsonEncode` of [value].
pub(crate) fn json_text(value: &Value) -> Result<String> {
    dart_json::encode(value).map_err(|e| Error::InvalidArgument(e.to_string()))
}

/// Dart's `jsonDecode(text)`: a malformed column is a `FormatException`.
pub(crate) fn json_decode(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| Error::Format(e.to_string()))
}

/// A model `fromJson` failure (Dart's `TypeError` on a cast, or a
/// `FormatException`): the row cannot be read into the model.
pub(crate) fn model_error(e: vocara_model::Error) -> Error {
    Error::Format(e.to_string())
}

/// The Dart `runtimeType` of the exception [e] stands for, for a
/// diagnostics line that must not carry the message (it can hold paths and
/// user text).
pub(crate) fn dart_type_name(e: &Error) -> &'static str {
    match e {
        Error::Sqlite(_) => "SqliteException",
        Error::Io(_) => "FileSystemException",
        Error::InvalidData(_) => "InvalidDataException",
        Error::InvalidArgument(_) => "ArgumentError",
        Error::State(_) => "StateError",
        Error::Format(_) => "FormatException",
        Error::Migration { .. } => "MigrationException",
        Error::HlcDrift(_) => "HlcDriftException",
    }
}

/// The SyncCols a stamped write sets, as `(column, value)` pairs.
pub(crate) fn hlc_columns(s: &SyncStamp) -> [(&'static str, Box<dyn ToSql + '_>); 4] {
    [
        ("hlc_wall", Box::new(s.hlc.wall_ms)),
        ("hlc_ctr", Box::new(s.hlc.counter)),
        ("hlc_node", Box::new(s.hlc.node.as_str())),
        ("hlc_packed", Box::new(s.hlc.packed())),
    ]
}

/// `?1, ?2, ...` for [n] values.
pub(crate) fn placeholders(n: usize) -> String {
    (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One column write of a drift companion: its name and its value.
pub(crate) type Column<'a> = (&'a str, Box<dyn ToSql + 'a>);

/// drift's `insertOnConflictUpdate(XCompanion.insert(...))` on a synced
/// table: inserts the row with exactly [columns] (the others take their DDL
/// defaults) or, when the id exists (a reused id, a tombstoned row), sets
/// exactly those columns — a column the companion leaves absent (such as
/// `schema_v`) keeps its stored value. Never `INSERT OR REPLACE`, which
/// would delete the row and trip its children's foreign keys. The id gets
/// drift's length check.
pub(crate) fn upsert(
    db: &AppDatabase,
    table: &str,
    id: &str,
    columns: Vec<Column<'_>>,
) -> Result<()> {
    check_id_length(table, id)?;
    let mut names = vec!["id"];
    names.extend(columns.iter().map(|(c, _)| *c));
    let quoted = names
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let set = names
        .iter()
        .map(|c| format!("\"{c}\" = excluded.\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO \"{table}\" ({quoted}) VALUES ({}) \
         ON CONFLICT(\"id\") DO UPDATE SET {set}",
        placeholders(names.len())
    );
    let mut params: Vec<&dyn ToSql> = vec![&id];
    params.extend(columns.iter().map(|(_, v)| v.as_ref()));
    db.execute(&sql, &*params)?;
    Ok(())
}

/// drift's `(update(table)..where(id)).write(XCompanion(...))`: sets exactly
/// [columns] of the row [id].
pub(crate) fn update(
    db: &AppDatabase,
    table: &str,
    id: &str,
    columns: Vec<Column<'_>>,
) -> Result<()> {
    let sets: Vec<(&str, &dyn ToSql)> = columns.iter().map(|(c, v)| (*c, v.as_ref())).collect();
    db.update_by_id(table, id, &sets)?;
    Ok(())
}

/// [columns] followed by the SyncCols [s] stamps.
pub(crate) fn stamped<'a>(mut columns: Vec<Column<'a>>, s: &'a SyncStamp) -> Vec<Column<'a>> {
    columns.extend(hlc_columns(s));
    columns
}

/// A boxed column value.
pub(crate) fn col<'a, T: ToSql + 'a>(name: &'a str, value: T) -> Column<'a> {
    (name, Box::new(value))
}
