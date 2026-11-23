//! Reads and acknowledges the oplog for the sync pusher (port of
//! `lib/data/sync/oplog_drain.dart`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::db::rows::OplogRow;
use crate::db::AppDatabase;
use crate::error::Result;
use crate::sync::sync_writer::op;

/// One pending push item: the LATEST journaled op for a (table, id) within
/// the drained window, with the bookkeeping the pusher needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOplogEntry {
    /// The LATEST raw oplog sequence collapsed into this entry.
    pub seq: i64,
    /// The journaled row's table.
    pub table: String,
    /// The journaled row's id.
    pub id: String,
    /// insert | update | tombstone — the latest op wins the collapse, so a
    /// tombstone that is followed by nothing is never lost.
    pub op: String,
    /// The packed HLC of the latest journaled mutation (the LWW key the
    /// pushed row state will carry).
    pub hlc_packed: String,
    /// The latest entry's content hash (empty for tombstones, by
    /// convention).
    pub payload_hash: String,
    /// Asset gate: the pusher must hold this row back until the asset's
    /// bytes are uploaded. Collapsing keeps the latest NON-null value seen
    /// for the row (a later content-only update must not shed the insert's
    /// dependency and let the row race ahead of its bytes).
    ///
    /// A TOMBSTONE is exempt from that inheritance: a delete carries no
    /// payload and references no bytes, so gating it on the asset an earlier
    /// insert needed holds it forever once that asset is collected — and
    /// because the pusher acks only up to the entry before the first gated
    /// one, one such tombstone freezes the ack watermark for the WHOLE
    /// ledger. An explicitly journaled dependency on a tombstone is still
    /// honoured.
    pub depends_on_asset_id: Option<String>,
}

/// One [`OplogDrain::read_pending`] result: the collapsed entries plus the
/// ack cover for everything the read consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OplogDrainBatch {
    /// Collapsed to the latest per (table, id), ordered by ascending
    /// [`PendingOplogEntry::seq`].
    pub entries: Vec<PendingOplogEntry>,
    /// The highest RAW sequence this read consumed — including entries
    /// collapsed away as superseded — so `ack(max_seq)` acknowledges exactly
    /// this read and can never touch entries journaled AFTER it (they get
    /// higher sequences). 0 when the batch is empty (ack(0) deletes
    /// nothing).
    pub max_seq: i64,
}

impl OplogDrainBatch {
    /// The empty batch.
    pub const EMPTY: OplogDrainBatch = OplogDrainBatch {
        entries: Vec::new(),
        max_seq: 0,
    };

    /// Whether the read found nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Reads and acknowledges the durability ledger for the sync pusher.
///
/// The oplog is append-only under the SyncWriter invariant (an entry exists
/// iff its row mutation committed); this drain is its ONLY consumer. The
/// read/ack split is deliberately two-phase: the pusher reads a batch,
/// pushes it, and only acks after the server durably accepted it — a crash
/// between the two re-pushes the same rows (idempotent under LWW) instead of
/// losing them.
#[derive(Debug, Clone)]
pub struct OplogDrain {
    db: Arc<AppDatabase>,
}

impl OplogDrain {
    /// The raw window [`OplogDrain::read_pending`] reads by default.
    pub const DEFAULT_LIMIT: i64 = 500;

    /// The dependency carry-forward [`OplogDrain::ack`] runs before deleting
    /// (bound: the ack watermark). Its correlated subqueries run once per
    /// surviving entry, so they must resolve through ix_oplog_entity (a test
    /// pins the plan). Only an entry that actually inherits something is
    /// written: SQLite rewrites every row an UPDATE matches, even to the
    /// value it held, and with no dependency anywhere (production today)
    /// each ack rewrote the whole ledger above its watermark.
    ///
    /// An entry inherits what [`OplogDrain::read_pending`]'s collapse
    /// resolves at it: the latest non-null dependency AT OR AFTER the row's
    /// latest tombstone before it (sequence 0 when it has none; the tombstone
    /// itself counts, its explicit dependency being what the collapse
    /// resolves it to). The collapse inherits nothing through a tombstone, so
    /// neither may the carry. The Dart app's carry
    /// looked back past tombstones: an insert gated on an asset, its delete,
    /// and a revive with no dependency resolve to NO dependency, yet an ack of
    /// the insert handed the revive the insert's asset. Once that asset was
    /// collected (its row was deleted) the revive was held forever, and with
    /// it the ack watermark and the rest of the ledger.
    /// `verification/VocaraVerify/Oplog.lean` proves the carry with the floor
    /// keeps every read's resolution, and its vectors replay through this SQL
    /// (`vocara_services/tests/lean_vectors_oplog.rs`).
    pub const ACK_CARRY_SQL: &'static str = "UPDATE oplog SET depends_on_asset_id = (\
         SELECT o.depends_on_asset_id FROM oplog o \
         WHERE o.entity_table = oplog.entity_table \
         AND o.entity_id = oplog.entity_id \
         AND o.depends_on_asset_id IS NOT NULL \
         AND o.seq <= oplog.seq \
         AND o.seq >= (SELECT COALESCE(MAX(t.seq), 0) FROM oplog t \
         WHERE t.entity_table = oplog.entity_table \
         AND t.entity_id = oplog.entity_id \
         AND t.op = 'tombstone' AND t.seq < oplog.seq) \
         ORDER BY o.seq DESC LIMIT 1\
         ) WHERE depends_on_asset_id IS NULL AND op <> 'tombstone' \
         AND seq > ? \
         AND EXISTS (SELECT 1 FROM oplog o \
         WHERE o.entity_table = oplog.entity_table \
         AND o.entity_id = oplog.entity_id \
         AND o.depends_on_asset_id IS NOT NULL \
         AND o.seq <= oplog.seq \
         AND o.seq >= (SELECT COALESCE(MAX(t.seq), 0) FROM oplog t \
         WHERE t.entity_table = oplog.entity_table \
         AND t.entity_id = oplog.entity_id \
         AND t.op = 'tombstone' AND t.seq < oplog.seq))";

    /// A drain over [db].
    pub fn new(db: Arc<AppDatabase>) -> Self {
        OplogDrain { db }
    }

    /// Reads up to [limit] RAW oplog entries (oldest first) and collapses
    /// them to the latest per (table, id). The pusher pushes CURRENT row
    /// state, so intermediate ops are redundant; the latest op wins, which
    /// preserves a trailing tombstone. [limit] bounds the raw scan (not the
    /// collapsed count), so [`OplogDrainBatch::max_seq`] is always a
    /// contiguous ack cover: a row's newer entries beyond the window survive
    /// the ack and are drained by the next read.
    pub fn read_pending(&self, limit: i64) -> Result<OplogDrainBatch> {
        if limit <= 0 {
            return Ok(OplogDrainBatch::EMPTY);
        }
        // Filter on synced = 0 to ride the ix_oplog_unsynced index. Nothing
        // sets synced today (ack DELETES), so this reads the whole ledger in
        // seq order.
        let rows: Vec<OplogRow> = self
            .db
            .select("\"synced\" = 0 ORDER BY \"seq\" ASC LIMIT ?1", [limit])?;
        if rows.is_empty() {
            return Ok(OplogDrainBatch::EMPTY);
        }

        let mut max_seq = 0;
        let mut latest: HashMap<(String, String), PendingOplogEntry> = HashMap::new();
        for row in rows {
            if row.seq > max_seq {
                max_seq = row.seq;
            }
            let key = (row.entity_table.clone(), row.entity_id.clone());
            // Keep the latest non-null dependency, except on a tombstone,
            // which inherits nothing (see PendingOplogEntry docs).
            let depends_on_asset_id = if row.op == op::TOMBSTONE {
                row.depends_on_asset_id
            } else {
                row.depends_on_asset_id.or_else(|| {
                    latest
                        .get(&key)
                        .and_then(|prev| prev.depends_on_asset_id.clone())
                })
            };
            latest.insert(
                key,
                PendingOplogEntry {
                    seq: row.seq,
                    table: row.entity_table,
                    id: row.entity_id,
                    op: row.op,
                    hlc_packed: row.hlc_packed,
                    payload_hash: row.payload_hash,
                    depends_on_asset_id,
                },
            );
        }
        let mut entries: Vec<PendingOplogEntry> = latest.into_values().collect();
        entries.sort_by_key(|e| e.seq);
        Ok(OplogDrainBatch { entries, max_seq })
    }

    /// Deletes every oplog entry with seq <= [upto_seq], in one transaction.
    /// Call ONLY with a sequence whose entries were durably pushed (or
    /// deliberately dropped): sequences are monotonic (AUTOINCREMENT), so
    /// entries journaled after the read carry higher sequences and are
    /// untouched. Returns the number of entries deleted.
    ///
    /// The asset gate survives a PARTIAL ack. The pusher acks up to the
    /// entry before the first gated one, so a row can keep entries on both
    /// sides of the cut — and the dependency that gates it may live on the
    /// deleted side (an insert carrying depends_on_asset_id, then a
    /// content-only update that does not). read_pending collapses to the
    /// latest non-null dependency, so the gate held the row on evidence this
    /// delete was about to destroy: the next drain would see only the bare
    /// later entry, and push a row REFERENCING BYTES THE SERVER HAS NEVER
    /// RECEIVED. Carrying the dependency onto the surviving entries first is
    /// the same repair boot compaction performs before it collapses history,
    /// for the same reason. Re-attaching an already-satisfied dependency is
    /// harmless: the gate re-checks the upload marker and lets the row
    /// straight through.
    ///
    /// The source entry is bounded by the TARGET ROW's own sequence
    /// (`o.seq <= oplog.seq`), not by the ack watermark. Bounding by the
    /// watermark takes the newest dependency from the DELETED prefix and
    /// stamps it on every surviving null entry — including ones that sit
    /// ABOVE a newer dependency, whose higher sequence then wins
    /// read_pending's collapse. The row would resolve to the OLD asset and
    /// be pushed while the NEW one's bytes are still missing, which is the
    /// exact failure this repair exists to prevent. Correlating on the
    /// target row makes each surviving entry inherit the newest dependency
    /// that precedes IT — the semantics boot compaction already has, since
    /// it runs without a bound.
    ///
    /// Tombstones are excluded: they reference no bytes (see
    /// [`PendingOplogEntry::depends_on_asset_id`]), so a stale value must
    /// never be persisted onto one.
    pub fn ack(&self, upto_seq: i64) -> Result<usize> {
        self.db.transaction(|| {
            self.db.execute(Self::ACK_CARRY_SQL, [upto_seq])?;
            self.db
                .execute("DELETE FROM \"oplog\" WHERE \"seq\" <= ?1", [upto_seq])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::hlc::{Hlc, HlcClock};
    use crate::sync::sync_writer::SyncWriter;

    fn id(s: &str) -> String {
        format!("{s:0<26}")[..26].to_string()
    }

    struct Fixture {
        db: Arc<AppDatabase>,
        writer: SyncWriter,
        drain: OplogDrain,
    }

    fn fixture() -> Fixture {
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        let clock = Arc::new(HlcClock::new(id("NODE")).with_now(|| 1000));
        Fixture {
            writer: SyncWriter::new(db.clone(), clock),
            drain: OplogDrain::new(db.clone()),
            db,
        }
    }

    impl Fixture {
        /// Journals one entry through the production chokepoint and returns
        /// its HLC.
        fn journal(
            &self,
            table: &str,
            seed: &str,
            op: &str,
            payload: &str,
            dep: Option<&str>,
        ) -> Hlc {
            let stamp = self.writer.stamp();
            let dep = dep.map(id);
            self.writer
                .journal(
                    table,
                    &id(seed),
                    op,
                    &stamp.hlc,
                    if op == "tombstone" { "" } else { payload },
                    dep.as_deref(),
                )
                .unwrap();
            stamp.hlc
        }

        fn count(&self) -> i64 {
            self.db
                .query_row("SELECT COUNT(*) FROM oplog", [], |r| r.get(0))
                .unwrap()
        }

        fn read(&self) -> OplogDrainBatch {
            self.drain.read_pending(OplogDrain::DEFAULT_LIMIT).unwrap()
        }

        fn entry<'a>(&self, batch: &'a OplogDrainBatch, seed: &str) -> &'a PendingOplogEntry {
            batch.entries.iter().find(|e| e.id == id(seed)).unwrap()
        }
    }

    #[test]
    fn an_empty_ledger_drains_to_an_empty_batch_and_ack_0_is_a_no_op() {
        let f = fixture();
        let batch = f.read();
        assert!(batch.is_empty());
        assert_eq!(batch.max_seq, 0);
        assert_eq!(f.drain.ack(batch.max_seq).unwrap(), 0);
    }

    #[test]
    fn entries_collapse_to_the_latest_op_and_a_trailing_tombstone_is_never_lost() {
        let f = fixture();
        f.journal("practices", "P1", "insert", "h", None);
        f.journal("practices", "P1", "update", "h2", None);
        f.journal("practices", "P2", "insert", "p2", None);
        let tomb = f.journal("practices", "P1", "tombstone", "h", None);

        let batch = f.read();
        assert_eq!(batch.entries.len(), 2);
        // Ordered by ascending (latest) seq: P2's insert (3), P1's tomb (4).
        assert_eq!(batch.entries[0].id, id("P2"));
        assert_eq!(batch.entries[0].op, "insert");
        assert_eq!(batch.entries[1].id, id("P1"));
        assert_eq!(batch.entries[1].op, "tombstone");
        assert_eq!(batch.entries[1].hlc_packed, tomb.packed());
        assert_eq!(batch.entries[1].payload_hash, "");
        assert_eq!(batch.entries[1].seq, 4);
        assert_eq!(batch.max_seq, 4); // covers the collapsed-away entries
    }

    #[test]
    fn ack_deletes_exactly_the_drained_range() {
        let f = fixture();
        f.journal("practices", "P1", "insert", "h", None);
        f.journal("practices", "P1", "update", "h", None);
        f.journal("tracks", "T1", "insert", "h", None);

        let batch = f.read();
        assert_eq!(batch.max_seq, 3);
        assert_eq!(f.drain.ack(batch.max_seq).unwrap(), 3);
        assert_eq!(f.count(), 0);
        assert!(f.read().is_empty());
    }

    #[test]
    fn entries_journaled_after_a_read_survive_that_reads_ack() {
        let f = fixture();
        f.journal("practices", "P1", "insert", "h", None);
        let batch = f.read();

        // A concurrent local edit lands between the push and the ack.
        let later = f.journal("practices", "P1", "update", "h9", None);

        f.drain.ack(batch.max_seq).unwrap();
        assert_eq!(f.count(), 1);

        let next = f.read();
        assert_eq!(next.entries.len(), 1);
        assert_eq!(next.entries[0].op, "update");
        assert_eq!(next.entries[0].hlc_packed, later.packed());
    }

    #[test]
    fn collapsing_keeps_the_latest_non_null_dependency() {
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("ASSET"));
        f.journal("clips", "C1", "update", "moved", None);

        let batch = f.read();
        assert_eq!(batch.entries.len(), 1);
        assert_eq!(batch.entries[0].op, "update");
        assert_eq!(batch.entries[0].depends_on_asset_id, Some(id("ASSET")));
    }

    #[test]
    fn a_newer_dependency_replaces_an_older_one_in_the_collapse() {
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("OLD"));
        f.journal("clips", "C1", "update", "h", Some("NEW"));
        assert_eq!(f.read().entries[0].depends_on_asset_id, Some(id("NEW")));
    }

    #[test]
    fn limit_bounds_the_raw_scan_and_entries_beyond_the_window_survive() {
        let f = fixture();
        let v1 = f.journal("practices", "P1", "insert", "h", None);
        f.journal("practices", "P2", "insert", "h", None);
        let v2 = f.journal("practices", "P1", "update", "h2", None);

        let first = f.drain.read_pending(2).unwrap();
        assert_eq!(first.entries.len(), 2);
        assert_eq!(first.max_seq, 2); // the window's max, NOT the table's
        assert_eq!(first.entries[0].hlc_packed, v1.packed());

        f.drain.ack(first.max_seq).unwrap();

        let second = f.drain.read_pending(2).unwrap();
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].id, id("P1"));
        assert_eq!(second.entries[0].op, "update");
        assert_eq!(second.entries[0].hlc_packed, v2.packed());
    }

    #[test]
    fn a_partial_ack_leaves_the_surviving_entry_still_gated() {
        let f = fixture();
        f.journal("practices", "P1", "insert", "h", None);
        f.journal("clips", "GATED", "insert", "h", Some("ASSET"));
        f.journal("clips", "GATED", "update", "moved", None);

        let batch = f.read();
        let gated = f.entry(&batch, "GATED");
        assert_eq!(gated.depends_on_asset_id, Some(id("ASSET")));
        // Everything strictly before the gated row's surviving entry.
        f.drain.ack(gated.seq - 1).unwrap();

        let next = f.read();
        assert_eq!(next.entries.len(), 1, "only the held row is left");
        assert_eq!(next.entries[0].id, id("GATED"));
        assert_eq!(
            next.entries[0].depends_on_asset_id,
            Some(id("ASSET")),
            "the gate must outlive the history that recorded it"
        );
    }

    #[test]
    fn a_partial_ack_must_not_stamp_an_old_dependency_over_a_newer_one() {
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("OLD"));
        f.journal("clips", "OTHER", "insert", "h", Some("GATE"));
        f.journal("clips", "C1", "update", "h", Some("NEW"));
        f.journal("clips", "C1", "update", "renamed", None);

        let batch = f.read();
        let other_seq = f.entry(&batch, "OTHER").seq;
        assert_eq!(f.entry(&batch, "C1").depends_on_asset_id, Some(id("NEW")));
        f.drain.ack(other_seq - 1).unwrap();

        let next = f.read();
        assert_eq!(
            f.entry(&next, "C1").depends_on_asset_id,
            Some(id("NEW")),
            "the surviving entry inherits the newest dependency that precedes IT"
        );
    }

    #[test]
    fn an_ack_carries_no_dependency_across_a_tombstone() {
        // An insert gated on an asset, its delete, then a revive with no
        // dependency of its own: the collapse resolves the revive to NO
        // dependency (a delete passes nothing on). The Dart carry looked back
        // past the tombstone, so an ack of the insert handed the revive the
        // insert's asset: once that asset was collected, the revive (and,
        // through the ack watermark, the whole ledger) was held forever.
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("ASSET"));
        f.journal("clips", "C1", "tombstone", "", None);
        let batch = f.read();
        assert_eq!(f.entry(&batch, "C1").depends_on_asset_id, None);
        // Revived while the tombstone was being pushed.
        f.journal("clips", "C1", "update", "back", None);
        f.drain.ack(batch.max_seq).unwrap();

        let next = f.read();
        assert_eq!(f.entry(&next, "C1").op, "update");
        assert_eq!(f.entry(&next, "C1").depends_on_asset_id, None);
    }

    #[test]
    fn an_ack_carries_a_tombstones_explicit_dependency_to_its_revive() {
        // The collapse resolves a tombstone to its own, explicitly journaled,
        // dependency, and a revive after it inherits that: so does the carry.
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("OLD"));
        f.journal("clips", "C1", "tombstone", "", Some("KEPT"));
        f.journal("clips", "C1", "update", "back", None);
        let before = f.read();
        assert_eq!(f.entry(&before, "C1").depends_on_asset_id, Some(id("KEPT")));
        f.drain.ack(before.max_seq - 2).unwrap();

        let after = f.read();
        assert_eq!(f.entry(&after, "C1").depends_on_asset_id, Some(id("KEPT")));
    }

    #[test]
    fn a_tombstone_inherits_no_asset_dependency() {
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("ASSET"));
        f.journal("clips", "C1", "tombstone", "", None);
        let batch = f.read();
        assert_eq!(batch.entries[0].op, "tombstone");
        assert_eq!(batch.entries[0].depends_on_asset_id, None);
    }

    #[test]
    fn a_partial_ack_does_not_persist_an_inherited_dependency_onto_a_tombstone() {
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("ASSET"));
        f.journal("clips", "OTHER", "insert", "h", Some("GATE"));
        f.journal("clips", "C1", "tombstone", "", None);

        let batch = f.read();
        let other_seq = f.entry(&batch, "OTHER").seq;
        f.drain.ack(other_seq - 1).unwrap();

        let next = f.read();
        assert_eq!(f.entry(&next, "C1").depends_on_asset_id, None);
    }

    #[test]
    fn an_explicit_dependency_journaled_on_a_tombstone_is_still_honoured() {
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", None);
        f.journal("clips", "C1", "tombstone", "", Some("ASSET"));
        assert_eq!(f.read().entries[0].depends_on_asset_id, Some(id("ASSET")));
    }

    #[test]
    fn a_fully_acked_row_edited_again_journals_no_dependency_of_its_own() {
        let f = fixture();
        f.journal("clips", "C1", "insert", "h", Some("ASSET"));
        let first = f.read();
        f.drain.ack(first.max_seq).unwrap();
        assert_eq!(f.count(), 0);

        f.journal("clips", "C1", "update", "renamed", None);
        assert_eq!(f.read().entries[0].depends_on_asset_id, None);
    }

    #[test]
    fn a_non_positive_limit_reads_nothing() {
        let f = fixture();
        f.journal("practices", "P1", "insert", "h", None);
        let batch = f.drain.read_pending(0).unwrap();
        assert!(batch.is_empty());
        assert_eq!(batch.max_seq, 0);
        assert_eq!(f.count(), 1);
    }

    #[test]
    fn with_no_dependency_anywhere_an_ack_rewrites_no_surviving_entry() {
        let f = fixture();
        let total_changes = || -> i64 {
            f.db.query_row("SELECT total_changes() AS n", [], |r| r.get(0))
                .unwrap()
        };
        for i in 0..4 {
            f.journal("clips", &format!("C{i}"), "insert", "h", None);
        }
        f.journal("clips", "C0", "update", "h2", None);

        let first = f.drain.read_pending(1).unwrap().max_seq;
        let before = total_changes();
        assert_eq!(f.drain.ack(first).unwrap(), 1);
        assert_eq!(total_changes() - before, 1, "the one acked entry");
        assert_eq!(f.count(), 4);
    }

    #[test]
    fn the_ack_carry_forward_probes_each_entity_through_ix_oplog_entity() {
        let f = fixture();
        for i in 0..50 {
            f.journal("clips", &format!("C{i}"), "insert", "h", None);
            f.journal("clips", &format!("C{i}"), "update", &format!("h{i}"), None);
        }
        let steps: Vec<String> =
            f.db.query(
                &format!("EXPLAIN QUERY PLAN {}", OplogDrain::ACK_CARRY_SQL),
                [0],
                |r| r.get("detail"),
            )
            .unwrap();
        assert!(
            steps
                .iter()
                .any(|s| s.starts_with("SEARCH o USING INDEX ix_oplog_entity")),
            "{}",
            steps.join("\n")
        );
        assert!(!steps.iter().any(|s| s == "SCAN o"), "{}", steps.join("\n"));
        // The tombstone floor's lookup too.
        assert!(
            steps
                .iter()
                .any(|s| s.starts_with("SEARCH t USING INDEX ix_oplog_entity")),
            "{}",
            steps.join("\n")
        );
        assert!(!steps.iter().any(|s| s == "SCAN t"), "{}", steps.join("\n"));
    }
}
