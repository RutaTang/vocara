//! The single chokepoint for local writes (port of
//! `lib/data/sync/sync_writer.dart`).

use std::sync::Arc;

use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use vocara_model::util::dart_json;

use crate::dart;
use crate::db::rows::OplogRow;
use crate::db::schema::ALL_TABLES;
use crate::db::AppDatabase;
use crate::error::{Error, Result};
use crate::sync::hlc::{Hlc, HlcClock};
use crate::sync::sync_state_store::SyncStateStore;

/// The Hybrid Logical Clock stamp + bookkeeping for one local write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStamp {
    /// The write's HLC.
    pub hlc: Hlc,
}

impl SyncStamp {
    /// A stamp carrying [hlc].
    pub fn new(hlc: Hlc) -> Self {
        SyncStamp { hlc }
    }

    /// Wall time used for created_at / updated_at (the HLC's physical
    /// component).
    pub fn at(&self) -> i64 {
        self.hlc.wall_ms
    }
}

/// The oplog op names.
pub mod op {
    /// A new row (or a revived one).
    pub const INSERT: &str = "insert";
    /// A changed row.
    pub const UPDATE: &str = "update";
    /// A soft delete.
    pub const TOMBSTONE: &str = "tombstone";
}

/// One value of a row's content projection, as the Dart list
/// `SyncWriter.contentHashOf` hashed held it: Dart keeps an `int` and a
/// `double` apart (`1` and `1.0` encode differently), and so does this
/// type. An INTEGER column is an [`ContentValue::Int`], a REAL column a
/// [`ContentValue::Double`], whatever its value.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentValue {
    /// `null`.
    Null,
    /// A Dart `bool`.
    Bool(bool),
    /// A Dart `int`.
    Int(i64),
    /// A Dart `double`.
    Double(f64),
    /// A Dart `String`.
    Text(String),
    /// A Dart `List`.
    List(Vec<ContentValue>),
    /// A Dart `Map` with string keys (hashed with its keys sorted; a
    /// repeated key keeps its last value).
    Map(Vec<(String, ContentValue)>),
}

impl From<&str> for ContentValue {
    fn from(v: &str) -> Self {
        ContentValue::Text(v.to_string())
    }
}

impl From<String> for ContentValue {
    fn from(v: String) -> Self {
        ContentValue::Text(v)
    }
}

impl From<&String> for ContentValue {
    fn from(v: &String) -> Self {
        ContentValue::Text(v.clone())
    }
}

impl From<i64> for ContentValue {
    fn from(v: i64) -> Self {
        ContentValue::Int(v)
    }
}

impl From<i32> for ContentValue {
    fn from(v: i32) -> Self {
        ContentValue::Int(i64::from(v))
    }
}

impl From<f64> for ContentValue {
    fn from(v: f64) -> Self {
        ContentValue::Double(v)
    }
}

impl From<bool> for ContentValue {
    fn from(v: bool) -> Self {
        ContentValue::Bool(v)
    }
}

impl<T: Into<ContentValue>> From<Option<T>> for ContentValue {
    fn from(v: Option<T>) -> Self {
        v.map_or(ContentValue::Null, Into::into)
    }
}

impl From<Vec<ContentValue>> for ContentValue {
    fn from(v: Vec<ContentValue>) -> Self {
        ContentValue::List(v)
    }
}

/// The SINGLE chokepoint for LOCAL writes. It is the only producer of HLC
/// stamps for local edits and the only writer of the oplog, so a row
/// mutation and its journal entry always commit together (the durability
/// ledger).
///
/// Remote (synced-in) changes do NOT go through here — they adopt the remote
/// HLC and append no oplog (the sync applier is the other door), so an
/// applied remote change is never re-journaled into a ping-pong.
#[derive(Debug)]
pub struct SyncWriter {
    db: Arc<AppDatabase>,
    clock: Arc<HlcClock>,
}

impl SyncWriter {
    /// The writer for [db], stamping from [clock].
    pub fn new(db: Arc<AppDatabase>, clock: Arc<HlcClock>) -> Self {
        SyncWriter { db, clock }
    }

    /// The device clock.
    pub fn clock(&self) -> &Arc<HlcClock> {
        &self.clock
    }

    /// The database it journals into.
    pub fn db(&self) -> &Arc<AppDatabase> {
        &self.db
    }

    /// Stamps a local write (ticks the clock). Splice the stamp's HLC into
    /// the row's SyncCols, then call [`SyncWriter::journal`] in the SAME
    /// transaction. For a row that already exists, use
    /// [`SyncWriter::stamp_for`].
    pub fn stamp(&self) -> SyncStamp {
        SyncStamp::new(self.clock.tick())
    }

    /// Stamps a local write over the row [id] of [table] (one that exists,
    /// or may: a reused id revives a tombstone). The stamp orders after the
    /// row's stored stamp and after the durable high-water mark of accepted
    /// remote stamps, as well as after this clock's own last one. Run inside
    /// the write's transaction.
    ///
    /// The clock alone does not guarantee that. A second running instance
    /// shares the library (see `LibraryLock`) with a clock of its own, so a
    /// row it wrote or applied can hold a stamp ahead of this clock, and so
    /// can a newer state of the row it parked (whose stamp only the
    /// high-water mark records, see
    /// [`SyncStateStore::HLC_HIGH_WATER_KEY`]). An edit stamped from the
    /// clock alone then lost entity-level LWW to the very version it
    /// replaced (or to the parked one, on its replay). Both stamps merge
    /// without a drift re-check, as boot seeds the clock from the same two
    /// sources; one that does not parse is skipped.
    pub fn stamp_for(&self, table: &str, id: &str) -> Result<SyncStamp> {
        if !ALL_TABLES.contains(&table) {
            return Err(Error::InvalidArgument(format!("table: {table}")));
        }
        let (held, high): (Option<String>, Option<String>) = self.db.query_row(
            &format!(
                "SELECT (SELECT hlc_packed FROM {table} WHERE id = ?) AS held, \
                 (SELECT value FROM sync_state WHERE key = ?) AS high"
            ),
            [id, SyncStateStore::HLC_HIGH_WATER_KEY],
            |r| Ok((r.get("held")?, r.get("high")?)),
        )?;
        for packed in [held, high].into_iter().flatten() {
            let Ok(seen) = Hlc::parse(&packed) else {
                continue;
            };
            self.clock.merge_verified_if_not_behind(&seen);
        }
        Ok(self.stamp())
    }

    /// Appends one oplog entry ([op] is one of [`op`]'s names). MUST be
    /// called inside the same transaction as the row write so the ledger and
    /// the data are atomic.
    pub fn journal(
        &self,
        table: &str,
        id: &str,
        op: &str,
        hlc: &Hlc,
        payload_hash: &str,
        depends_on_asset_id: Option<&str>,
    ) -> Result<()> {
        let entry = OplogRow {
            seq: 0,
            entity_table: table.to_string(),
            entity_id: id.to_string(),
            op: op.to_string(),
            hlc_packed: hlc.packed(),
            payload_hash: payload_hash.to_string(),
            synced: false,
            depends_on_asset_id: depends_on_asset_id.map(str::to_string),
            created_at: hlc.wall_ms,
        };
        self.db.with_conn(|c| entry.insert_new(c))?;
        Ok(())
    }

    /// A stable content hash over a row's CONTENT columns
    /// (bookkeeping/SyncCols excluded by the caller). Used both as the oplog
    /// payload hash and for reconcile change-detection. Values are
    /// canonicalized (a fixed encoding) so a re-serialization that only
    /// reorders fields is not a phantom change.
    ///
    /// The sha256 (lowercase hex) of Dart's `jsonEncode` of the
    /// canonicalized list, byte for byte: maps get their keys sorted (in
    /// UTF-16 order, Dart's `String.compareTo`), doubles are written as Dart
    /// writes them. So it equals `SyncWriter.contentHashOf` for the same
    /// column values.
    ///
    /// Non-finite doubles (NaN / ±Infinity) are refused loudly with
    /// [`Error::InvalidArgument`]: jsonEncode would throw on them anyway,
    /// but with an unactionable message — and letting one anywhere near a
    /// row write would be worse (SQLite stores NaN as NULL, so the hash
    /// would stop describing the persisted row). No bounded UI input
    /// produces one today; this is the deterministic guard at the hashing
    /// boundary (repositories surface it as a save-time error, like every
    /// data-layer refusal).
    pub fn content_hash_of(content_columns: &[ContentValue]) -> Result<String> {
        let canonical = Value::Array(
            content_columns
                .iter()
                .map(canonical)
                .collect::<Result<Vec<_>>>()?,
        );
        let text = dart_json::encode(&canonical).map_err(|_| non_finite())?;
        Ok(hex::encode(Sha256::digest(text.as_bytes())))
    }
}

fn non_finite() -> Error {
    Error::InvalidArgument(
        "contentColumns: non-finite double is not representable in a sync content hash".to_string(),
    )
}

fn canonical(v: &ContentValue) -> Result<Value> {
    Ok(match v {
        ContentValue::Null => Value::Null,
        ContentValue::Bool(b) => Value::Bool(*b),
        ContentValue::Int(i) => Value::Number(Number::from(*i)),
        ContentValue::Double(d) => {
            if !d.is_finite() {
                return Err(non_finite());
            }
            Value::Number(Number::from_f64(*d).ok_or_else(non_finite)?)
        }
        ContentValue::Text(s) => Value::String(s.clone()),
        ContentValue::List(items) => {
            Value::Array(items.iter().map(canonical).collect::<Result<Vec<_>>>()?)
        }
        ContentValue::Map(entries) => {
            let mut sorted: Vec<&(String, ContentValue)> = entries.iter().collect();
            // Stable, so of two equal keys the later stays later and wins
            // the insert below.
            sorted.sort_by(|a, b| dart::compare_strings(&a.0, &b.0));
            let mut map = Map::new();
            for (k, item) in sorted {
                map.insert(k.clone(), canonical(item)?);
            }
            Value::Object(map)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::rows::OplogRow;
    use crate::db::rows::PracticeRow;
    use ContentValue as V;

    fn node() -> String {
        format!("{:0<26}", "NODE")
    }

    fn writer() -> (Arc<AppDatabase>, SyncWriter) {
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        let clock = Arc::new(HlcClock::new(node()).with_now(|| 1000));
        (db.clone(), SyncWriter::new(db, clock))
    }

    fn map(entries: &[(&str, ContentValue)]) -> ContentValue {
        V::Map(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn stamp_ticks_the_hlc_monotonically() {
        let (_db, writer) = writer();
        let a = writer.stamp();
        let b = writer.stamp();
        assert!(b.hlc > a.hlc);
    }

    #[test]
    fn journal_writes_one_oplog_row_committed_with_the_transaction() {
        let (db, writer) = writer();
        let s = writer.stamp();
        db.transaction(|| {
            writer.journal(
                "practices",
                &format!("{:0<26}", "P"),
                op::INSERT,
                &s.hlc,
                "abc",
                None,
            )
        })
        .unwrap();
        let rows = db.select_all::<OplogRow>().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_table, "practices");
        assert_eq!(rows[0].op, "insert");
        assert_eq!(rows[0].hlc_packed, s.hlc.packed());
        assert_eq!(rows[0].created_at, s.at());
        assert!(!rows[0].synced);
    }

    #[test]
    fn content_hash_of_is_canonical() {
        let h1 = SyncWriter::content_hash_of(&[
            "title".into(),
            map(&[("a", 1.into()), ("b", 2.into())]),
            42.into(),
        ])
        .unwrap();
        let h2 = SyncWriter::content_hash_of(&[
            "title".into(),
            map(&[("b", 2.into()), ("a", 1.into())]), // reordered
            42.into(),
        ])
        .unwrap();
        assert_eq!(h1, h2);
        let h3 = SyncWriter::content_hash_of(&[
            "title".into(),
            map(&[("a", 1.into()), ("b", 3.into())]),
            42.into(),
        ])
        .unwrap();
        assert_ne!(h3, h1);
    }

    #[test]
    fn content_hash_of_rejects_non_finite_doubles() {
        let refused = |cols: &[ContentValue]| {
            matches!(
                SyncWriter::content_hash_of(cols),
                Err(Error::InvalidArgument(_))
            )
        };
        assert!(refused(&[f64::NAN.into()]));
        assert!(refused(&["x".into(), f64::INFINITY.into()]));
        // Nested values are guarded too (the canonicalizer recurses).
        assert!(refused(&[map(&[("pan", f64::NEG_INFINITY.into())])]));
        assert!(refused(&[V::List(vec![1.0.into(), f64::NAN.into()])]));
        // Ordinary finite doubles keep hashing deterministically.
        assert_eq!(
            SyncWriter::content_hash_of(&[1.0.into(), (-0.5).into()]).unwrap(),
            SyncWriter::content_hash_of(&[1.0.into(), (-0.5).into()]).unwrap()
        );
    }

    #[test]
    fn content_hash_of_hashes_dart_s_json_encoding_byte_for_byte() {
        // Each text is what Dart's jsonEncode writes for the list (a whole
        // double keeps its ".0", an int does not, keys are sorted, null and
        // booleans are literal, strings escape as Dart escapes), and each
        // hash is that text's sha256 computed independently (Python's
        // hashlib), so a change to any layer is caught.
        let cases: Vec<(Vec<ContentValue>, &str, &str)> = vec![
            (
                vec![
                    "title".into(),
                    map(&[("b", 2.into()), ("a", 1.into())]),
                    42.into(),
                ],
                r#"["title",{"a":1,"b":2},42]"#,
                "1af36209a946e8a08f6ae9216687a24f10ce7a5a0e2db7f11f28e56cd6ba573f",
            ),
            (
                // practiceContentHash of a fresh practice titled "Song".
                vec![
                    "Song".into(),
                    0.into(),
                    1.0.into(),
                    V::Null,
                    V::Null,
                    V::Null,
                    V::Null,
                ],
                r#"["Song",0,1.0,null,null,null,null]"#,
                "243b0f97e801172ef7e7388c0a48587d474fb0f346e39f454ed373b23d496e70",
            ),
            (
                vec![
                    1.into(),
                    1.0.into(),
                    (-0.0).into(),
                    1e21.into(),
                    1.5e-7.into(),
                ],
                "[1,1.0,-0.0,1e+21,1.5e-7]",
                "e3e35c8b171d2542f7007918b3d0f20fee545cae064bcb41f75c54104776a3fd",
            ),
            (
                vec![true.into(), false.into(), "q\"\n\u{1}é".into()],
                "[true,false,\"q\\\"\\n\\u0001é\"]",
                "c2cc39723f642094ba3a5ada482aaf4fe8b95beab75ba4a916c617c4fb7d2cf0",
            ),
            (
                // Map keys sort in UTF-16 order (Dart's String.compareTo):
                // U+FF61 sorts after a surrogate pair there, but before it
                // in UTF-8 bytes.
                vec![map(&[("\u{FF61}", 1.into()), ("\u{1F600}", 2.into())])],
                "[{\"\u{1F600}\":2,\"\u{FF61}\":1}]",
                "a489cf60cf6d16298cdba3e785c981becf4e915f83a88357e5a10fe2c5073847",
            ),
        ];
        for (cols, text, hash) in &cases {
            let canonical = Value::Array(cols.iter().map(|c| canonical(c).unwrap()).collect());
            assert_eq!(dart_json::encode(&canonical).unwrap(), *text);
            assert_eq!(SyncWriter::content_hash_of(cols).unwrap(), *hash, "{text}");
        }
    }

    #[test]
    fn stamp_for_orders_after_the_held_row_and_the_high_water_mark() {
        let (db, writer) = writer();
        let id = format!("{:0<26}", "P");
        let ahead = Hlc::new(1_000_000, 7, format!("{:0<26}", "OTHER"));
        db.insert(&PracticeRow {
            id: id.clone(),
            hlc_packed: ahead.packed(),
            ..Default::default()
        })
        .unwrap();
        let s = writer.stamp_for("practices", &id).unwrap();
        assert!(s.hlc > ahead, "{} vs {}", s.hlc, ahead);

        // A parked newer state raised only the mark.
        let higher = Hlc::new(2_000_000, 3, format!("{:0<26}", "OTHER"));
        SyncStateStore::new(db.clone())
            .raise_hlc_high_water(&higher.packed())
            .unwrap();
        let s = writer.stamp_for("practices", &id).unwrap();
        assert!(s.hlc > higher);
        // An older held stamp does not move the clock: a plain tick.
        let before = writer.clock().last();
        let s = writer.stamp_for("practices", &id).unwrap();
        assert_eq!(s.hlc.wall_ms, before.wall_ms);
        assert_eq!(s.hlc.counter, before.counter + 1);
    }

    #[test]
    fn stamp_for_skips_a_stamp_that_does_not_parse_and_refuses_unknown_tables() {
        let (db, writer) = writer();
        let id = format!("{:0<26}", "P");
        db.insert(&PracticeRow {
            id: id.clone(),
            hlc_packed: "REMOTE-PACKED".to_string(),
            ..Default::default()
        })
        .unwrap();
        let s = writer.stamp_for("practices", &id).unwrap();
        assert_eq!(s.hlc.wall_ms, 1000);
        // A row that does not exist yet: just a tick.
        assert!(writer.stamp_for("practices", "missing").is_ok());
        assert!(matches!(
            writer.stamp_for("nope", &id),
            Err(Error::InvalidArgument(_))
        ));
    }
}
