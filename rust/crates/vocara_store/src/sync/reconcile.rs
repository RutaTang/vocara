//! The three-way per-entity reconcile of a saved aggregate's child
//! collections (port of `lib/data/sync/reconcile.dart`).

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::error::Result;
use crate::sync::sync_writer::{op, SyncStamp, SyncWriter};

/// Counts of what a reconcile actually changed (for tests / diagnostics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileResult {
    pub inserts: u32,
    pub updates: u32,
    pub tombstones: u32,
    /// Rows deliberately NOT written because, under the three-way rules, the
    /// persisted (remote/newer) state wins: a remote-updated row this editor
    /// never touched, a remote-created row absent from a stale aggregate, or
    /// a remote-deleted row whose delete is accepted. Diagnostics only — a
    /// skip emits no write and no oplog entry, so it is not part of
    /// [`ReconcileResult::total`].
    pub skips: u32,
}

impl ReconcileResult {
    /// Every write the reconcile emitted.
    pub fn total(&self) -> u32 {
        self.inserts + self.updates + self.tombstones
    }
}

/// The write [`reconcile_collection`] emits for one desired row — the
/// three-way rules as a pure function of the row's three hashes. The
/// collection pass itself runs on this, and it is exposed so a caller that
/// must pre-stage writes around a per-statement UNIQUE constraint (the
/// track-slot vacate in the song practice repository's track reconcile) can
/// predict the pass without keeping a second, divergence-prone copy of the
/// rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOp {
    /// Rule 7 (new local row) or rule 5's edited side (revive through
    /// upsert).
    Insert,
    /// Rules 1 and 3, and the no-base save over an existing row.
    Update,
    /// Rule 2 or rule 5's untouched side: the persisted state wins, no
    /// write.
    Skip,
    /// mine == DB: nothing to write, no HLC tick, no oplog.
    Unchanged,
}

/// See [`ReconcileOp`]. [mine]/[db]/[base] are the row's content hashes per
/// the three sides documented on [`reconcile_collection`].
pub fn reconcile_op_for(mine: &str, db: Option<&str>, base: Option<&str>) -> ReconcileOp {
    let Some(db) = db else {
        // Not live in the DB: rule 5's untouched side accepts the remote
        // delete; everything else inserts (rule 7 new row, rule 5's
        // edited-side revive).
        return if base.is_some_and(|b| mine == b) {
            ReconcileOp::Skip
        } else {
            ReconcileOp::Insert
        };
    };
    if mine == db {
        return ReconcileOp::Unchanged;
    }
    if let Some(base) = base {
        if db != base && mine == base {
            return ReconcileOp::Skip;
        }
    }
    ReconcileOp::Update
}

/// The inputs of one [`reconcile_collection`] pass (Dart's named
/// parameters). The callbacks write ONE row each; the pass journals it.
pub struct ReconcileCollection<'a, Item> {
    pub writer: &'a SyncWriter,
    pub table: &'a str,
    /// BASE: rowId -> the row's content hash when this editor LOADED the
    /// aggregate.
    pub base_hashes: &'a IndexMap<String, String>,
    /// DB: rowId -> the hash of each LIVE row in this collection's scope as
    /// persisted at save time. Iterated in order (tombstones are emitted in
    /// this order).
    pub current_hashes: &'a IndexMap<String, String>,
    /// MINE: what the editor wants now.
    pub desired: &'a [Item],
    pub id_of: &'a dyn Fn(&Item) -> String,
    /// The row's content hash (a projection of `sync_hashes`), which can
    /// refuse a non-finite double.
    pub hash_of: &'a dyn Fn(&Item) -> Result<String>,
    /// Writes a new (or revived) row: an UPSERT that clears `deleted_at`.
    pub insert: &'a mut dyn FnMut(&Item, &SyncStamp) -> Result<()>,
    /// Writes a changed row.
    pub update: &'a mut dyn FnMut(&Item, &SyncStamp) -> Result<()>,
    /// Soft-deletes the row with that id.
    pub tombstone: &'a mut dyn FnMut(&str, &SyncStamp) -> Result<()>,
}

/// Reconciles ONE child collection of a saved aggregate against what is
/// persisted, as a THREE-WAY merge, emitting the MINIMAL set of writes
/// through the [`SyncWriter`] chokepoint.
///
/// The three sides per row id:
///   * BASE  — base_hashes\[id\]: the row's content hash when this editor
///     LOADED the aggregate (None = the editor never held the row). The map
///     may be a flat superset covering the whole aggregate: ids are globally
///     unique ULIDs — plus the legacy `<trackId>-c0` clip shape, which
///     carries a '-' no ULID contains — so lookups never cross-contaminate
///     between collections. That is a constraint on how ids are MINTED, not
///     a property of this map, and it is load-bearing: a shared id would let
///     one table's base hash answer for another's row, which decides the
///     rules below and can send an UPDATE down the tombstone branch.
///   * MINE  — hash_of(item) for id in desired: what the editor wants now.
///   * DB    — current_hashes\[id\]: the row's hash as persisted at save time
///     (LIVE rows in this collection's scope only; None = tombstoned or
///     never existed). Local edits never move a child id to a different
///     parent (cross-parent transplants always mint fresh ids), so "not live
///     in this scope" equates to "not live at all".
///
/// The rules (each has a named test):
///   1. in desired, DB == base, mine != base  -> UPDATE (a plain local edit
///      — the only write path for edits).
///   2. in desired, DB != base, mine == base  -> SKIP: the DB moved on since
///      load (a remote/newer write) and this editor never touched the row;
///      writing mine would REVERT the newer state under a fresh HLC.
///   3. in desired, DB != base, mine != base  -> TRUE CONFLICT: both sides
///      changed. Policy: the ACTIVE EDITOR wins — the open editor is the
///      intent-bearer, and silently discarding what the user is looking at
///      would be worse than overriding a background change. Mine is written
///      with a fresh HLC (so it also wins entity-level LWW downstream).
///   4. live in DB, NOT in base, NOT in desired -> SKIP: created remotely
///      while this aggregate was open. The editor never saw it, so its
///      absence from desired is staleness, not a removal — never tombstone
///      it.
///   5. in base, in desired, NOT live in DB -> deleted remotely while held:
///      if mine == base, accept the delete (skip — reviving an untouched row
///      would resurrect a tombstone); if mine != base it is an
///      edit-vs-delete conflict and the active editor wins — the row is
///      revived through the upsert insert path (deleted_at cleared) with a
///      fresh HLC.
///   6. in base, NOT in desired, live in DB -> the user explicitly removed a
///      row they held: TOMBSTONE, even when DB != base (an explicit delete
///      intent outweighs a concurrent remote edit; the tombstone syncs and
///      wins LWW with its fresh HLC).
///   7. in desired, NOT in base, NOT live in DB -> new local row -> INSERT
///      (an upsert: if the id exists tombstoned — a reused ULID — the row is
///      revived instead of crashing on the primary key).
///   * unchanged (mine == DB) -> NOTHING: no write, no HLC tick, no oplog.
///
/// A no-base legacy save passes base_hashes == current_hashes, which
/// degenerates exactly to the old two-way diff (rules 2/4/5 can then never
/// fire).
///
/// This per-entity change detection is what keeps a fader nudge from
/// dirtying the whole practice and re-syncing it: only rows that truly
/// changed get a new HLC + a journal entry. Run inside the parent's save
/// transaction.
pub fn reconcile_collection<Item>(c: ReconcileCollection<'_, Item>) -> Result<ReconcileResult> {
    let mut result = ReconcileResult::default();
    let desired_ids: HashSet<String> = c.desired.iter().map(|item| (c.id_of)(item)).collect();

    // Tombstone removed rows FIRST, then insert/update desired ones. Order
    // matters under a partial-UNIQUE index (e.g. one live accompaniment/
    // reference per practice): replacing such a row means the old id is
    // removed and a new id is added with the same (practice_id, kind).
    // Inserting the new before removing the old would momentarily have two
    // live rows -> UNIQUE violation -> the whole save rolls back. Removing
    // first frees the slot. This still holds under the three-way rules: a
    // locally REPLACED slot row was loaded by this editor, so it is in base
    // and is tombstoned here before the insert pass runs. A slot MOVED
    // between rows that both stay in desired (a rotation's re-kinded old
    // row, a symmetric swap) is beyond any statement order — SQLite checks
    // the index per statement, so whichever row writes first collides with
    // the other still holding its old slot. The caller owns that case: it
    // pre-vacates the affected rows inside the same transaction before this
    // pass runs, predicting the pass via [`reconcile_op_for`]. The one case
    // nothing local covers is a slot row created REMOTELY while this
    // aggregate was open: rule 4 deliberately keeps it, so a colliding local
    // insert fails the whole save (an error at the repository boundary; the
    // caller reverts/reloads). That is the safe side: failing loudly over
    // silently tombstoning a row this editor never saw.
    for id in c.current_hashes.keys() {
        if desired_ids.contains(id) {
            continue;
        }
        if !c.base_hashes.contains_key(id) {
            // Rule 4: live in DB, not in base, not desired — created
            // remotely while the aggregate was open. Not a removal; keep it.
            result.skips += 1;
            continue;
        }
        // Rule 6: the editor held this row and dropped it — an explicit
        // removal. Tombstone even if the DB content moved on since load
        // (delete wins).
        let stamp = c.writer.stamp_for(c.table, id)?;
        (c.tombstone)(id, &stamp)?;
        c.writer
            .journal(c.table, id, op::TOMBSTONE, &stamp.hlc, "", None)?;
        result.tombstones += 1;
    }

    for item in c.desired {
        let id = (c.id_of)(item);
        let mine = (c.hash_of)(item)?;

        match reconcile_op_for(
            &mine,
            c.current_hashes.get(&id).map(String::as_str),
            c.base_hashes.get(&id).map(String::as_str),
        ) {
            ReconcileOp::Unchanged => {
                // Unchanged relative to what is persisted (including the
                // case where a remote write already equals mine): emit
                // nothing — no phantom op.
            }
            ReconcileOp::Skip => {
                // Rule 2: the DB moved on since load and this editor never
                // touched the row. Keep THEIRS — writing mine would revert
                // the newer state to stale values under a fresh HLC. Or rule
                // 5's untouched side: the editor held the row, never changed
                // it, and it is gone from the DB — deleted remotely while the
                // aggregate was open. Accept the delete; reviving would
                // resurrect a tombstone.
                result.skips += 1;
            }
            ReconcileOp::Insert => {
                // Rule 7 (base == None): a brand-new local row. Or rule 5
                // (edited side, base != None && mine != base): an
                // edit-vs-delete conflict — the active editor wins and the
                // row is revived. Both go through the same upsert insert path
                // (which clears deleted_at) with a fresh HLC.
                let stamp = c.writer.stamp_for(c.table, &id)?;
                (c.insert)(item, &stamp)?;
                c.writer
                    .journal(c.table, &id, op::INSERT, &stamp.hlc, &mine, None)?;
                result.inserts += 1;
            }
            ReconcileOp::Update => {
                // Rule 1 (db == base, mine != base): a plain local edit.
                // Rule 3 (db != base, mine != base): a true conflict —
                // active editor wins, mine is written with a fresh HLC.
                // base == None with a live row: no base knowledge (a no-base
                // save over an existing row) — treated as a conflict, local
                // wins.
                let stamp = c.writer.stamp_for(c.table, &id)?;
                (c.update)(item, &stamp)?;
                c.writer
                    .journal(c.table, &id, op::UPDATE, &stamp.hlc, &mine, None)?;
                result.updates += 1;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::db::rows::{InterpretationRow, OplogRow, PracticeRow};
    use crate::db::AppDatabase;
    use crate::sync::hlc::HlcClock;
    use crate::sync::sync_hashes::{
        interpretation_content_hash, interpretation_row_hash, InterpretationContent,
    };

    /// A tiny desired-item for the test, mapped onto `interpretations`.
    #[derive(Clone)]
    struct Interp {
        id: String,
        name: String,
        order: i64,
    }

    fn interp(seed: &str, name: &str, order: i64) -> Interp {
        Interp {
            id: id(seed),
            name: name.to_string(),
            order,
        }
    }

    fn id(s: &str) -> String {
        format!("{s:0<26}")[..26].to_string()
    }

    struct Fixture {
        db: Arc<AppDatabase>,
        writer: SyncWriter,
        practice_id: String,
    }

    fn fixture() -> Fixture {
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        let writer = SyncWriter::new(db.clone(), Arc::new(HlcClock::new(id("NODE"))));
        let practice_id = id("P1");
        // A parent practice the interpretations hang off (FK).
        let s = writer.stamp();
        db.insert(&PracticeRow {
            id: practice_id.clone(),
            created_at: s.at(),
            updated_at: s.at(),
            hlc_wall: s.hlc.wall_ms,
            hlc_ctr: s.hlc.counter,
            hlc_node: s.hlc.node.clone(),
            hlc_packed: s.hlc.packed(),
            ..Default::default()
        })
        .unwrap();
        Fixture {
            db,
            writer,
            practice_id,
        }
    }

    // Both sides hash through the SHARED sync_hashes projections, like the
    // production repositories.
    fn hash_of(i: &Interp) -> Result<String> {
        Ok(interpretation_content_hash(&InterpretationContent {
            name: &i.name,
            order_index: i.order,
        }))
    }

    impl Fixture {
        fn current_hashes(&self) -> IndexMap<String, String> {
            self.db
                .select::<InterpretationRow>("deleted_at IS NULL", [])
                .unwrap()
                .into_iter()
                .map(|r| (r.id.clone(), interpretation_row_hash(&r)))
                .collect()
        }

        /// One save of [desired] against an explicit [base] (the editor's
        /// loaded snapshot); current hashes are always read fresh from the
        /// DB. None = base == current (the two-way degenerate case).
        fn sync(
            &self,
            desired: &[Interp],
            base: Option<&IndexMap<String, String>>,
        ) -> ReconcileResult {
            self.db
                .transaction(|| {
                    let current = self.current_hashes();
                    let base = base.cloned().unwrap_or_else(|| current.clone());
                    let db = &self.db;
                    let practice_id = &self.practice_id;
                    reconcile_collection(ReconcileCollection {
                        writer: &self.writer,
                        table: "interpretations",
                        base_hashes: &base,
                        current_hashes: &current,
                        desired,
                        id_of: &|i: &Interp| i.id.clone(),
                        hash_of: &hash_of,
                        // The insert path is an UPSERT that clears the
                        // tombstone, mirroring the repositories (rule 5's
                        // edit-vs-delete revive relies on it).
                        insert: &mut |i: &Interp, s: &SyncStamp| {
                            db.upsert(&InterpretationRow {
                                id: i.id.clone(),
                                deleted_at: None,
                                practice_id: practice_id.clone(),
                                name: i.name.clone(),
                                order_index: i.order,
                                created_at: s.at(),
                                updated_at: s.at(),
                                hlc_wall: s.hlc.wall_ms,
                                hlc_ctr: s.hlc.counter,
                                hlc_node: s.hlc.node.clone(),
                                hlc_packed: s.hlc.packed(),
                                ..Default::default()
                            })
                        },
                        update: &mut |i: &Interp, s: &SyncStamp| {
                            db.update_by_id(
                                "interpretations",
                                &i.id,
                                &[
                                    ("name", &i.name),
                                    ("order_index", &i.order),
                                    ("updated_at", &s.at()),
                                    ("hlc_wall", &s.hlc.wall_ms),
                                    ("hlc_ctr", &s.hlc.counter),
                                    ("hlc_node", &s.hlc.node),
                                    ("hlc_packed", &s.hlc.packed()),
                                ],
                            )
                            .map(|_| ())
                        },
                        tombstone: &mut |rid: &str, s: &SyncStamp| {
                            db.update_by_id(
                                "interpretations",
                                rid,
                                &[
                                    ("deleted_at", &s.at()),
                                    ("updated_at", &s.at()),
                                    ("hlc_wall", &s.hlc.wall_ms),
                                    ("hlc_ctr", &s.hlc.counter),
                                    ("hlc_node", &s.hlc.node),
                                    ("hlc_packed", &s.hlc.packed()),
                                ],
                            )
                            .map(|_| ())
                        },
                    })
                })
                .unwrap()
        }

        fn oplog_count(&self) -> usize {
            self.db.select_all::<OplogRow>().unwrap().len()
        }

        fn row(&self, seed: &str) -> InterpretationRow {
            self.db.find(&id(seed)).unwrap().unwrap()
        }

        /// Seeds rows through the normal local path and returns the base map
        /// an editor loading NOW would capture.
        fn seed(&self, items: &[Interp]) -> IndexMap<String, String> {
            self.sync(items, Some(&IndexMap::new()));
            self.current_hashes()
        }

        /// A "remote apply" of an UPDATE: rewrites content + adopts a
        /// foreign HLC, journals nothing — the SyncApplier contract.
        fn remote_update(&self, seed: &str, name: &str) {
            self.db
                .execute(
                    "UPDATE interpretations SET name = ?, hlc_wall = 9999999999999, \
                     hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' \
                     WHERE id = ?",
                    [name, &id(seed)],
                )
                .unwrap();
        }

        /// A "remote apply" of a brand-new row (no oplog, foreign HLC).
        fn remote_insert(&self, seed: &str, name: &str, order: i64) {
            self.db
                .execute(
                    "INSERT INTO interpretations \
                     (id, practice_id, name, order_index, created_at, updated_at, \
                      deleted_at, hlc_wall, hlc_ctr, hlc_node, hlc_packed) VALUES \
                     (?, ?, ?, ?, 1, 1, NULL, 9999999999999, 0, 'REMOTE', \
                      'REMOTE-PACKED')",
                    rusqlite::params![id(seed), self.practice_id, name, order],
                )
                .unwrap();
        }

        /// A "remote apply" of a tombstone (no oplog, foreign HLC).
        fn remote_tombstone(&self, seed: &str) {
            self.db
                .execute(
                    "UPDATE interpretations SET deleted_at = 999, hlc_wall = 9999999999999, \
                     hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' \
                     WHERE id = ?",
                    [id(seed)],
                )
                .unwrap();
        }
    }

    // --- reconcile_test.dart: the editor holds the latest state (base ==
    // current, the two-way degenerate case) ---

    #[test]
    fn first_save_inserts_everything() {
        let f = fixture();
        let r = f.sync(
            &[interp("A", "Tender", 0), interp("B", "Powerful", 1)],
            None,
        );
        assert_eq!((r.inserts, r.updates, r.tombstones), (2, 0, 0));
        assert_eq!(f.oplog_count(), 2);
    }

    #[test]
    fn a_no_op_save_changes_nothing() {
        let f = fixture();
        let items = [interp("A", "Tender", 0), interp("B", "Powerful", 1)];
        f.sync(&items, None);
        let before = f.oplog_count();
        let r = f.sync(&items, None); // identical
        assert_eq!(r.total(), 0);
        assert_eq!(f.oplog_count(), before);
    }

    #[test]
    fn only_the_changed_row_is_updated() {
        let f = fixture();
        f.sync(
            &[interp("A", "Tender", 0), interp("B", "Powerful", 1)],
            None,
        );
        let before = f.oplog_count();
        let r = f.sync(&[interp("A", "Tender", 0), interp("B", "Belted", 1)], None);
        assert_eq!((r.inserts, r.updates, r.tombstones), (0, 1, 0));
        assert_eq!(f.oplog_count(), before + 1);
        assert_eq!(f.row("B").name, "Belted");
    }

    #[test]
    fn a_removed_item_is_tombstoned_not_hard_deleted() {
        let f = fixture();
        f.sync(
            &[interp("A", "Tender", 0), interp("B", "Powerful", 1)],
            None,
        );
        let r = f.sync(&[interp("A", "Tender", 0)], None); // B dropped
        assert_eq!(r.tombstones, 1);
        assert!(f.row("B").deleted_at.is_some()); // still there, tombstoned
        let live: Vec<String> = f.current_hashes().keys().cloned().collect();
        assert_eq!(live, vec![id("A")]);
        let last = f.db.select_all::<OplogRow>().unwrap().pop().unwrap();
        assert_eq!(
            (last.op.as_str(), last.payload_hash.as_str()),
            ("tombstone", "")
        );
    }

    #[test]
    fn insert_update_and_tombstone_in_one_reconcile() {
        let f = fixture();
        f.sync(
            &[interp("A", "Tender", 0), interp("B", "Powerful", 1)],
            None,
        );
        let r = f.sync(&[interp("A", "Soft", 0), interp("C", "Raspy", 2)], None);
        assert_eq!((r.inserts, r.updates, r.tombstones), (1, 1, 1));
    }

    // --- reconcile_three_way_test.dart: the unit group ---

    #[test]
    fn rule_1_a_plain_local_edit_updates() {
        let f = fixture();
        let base = f.seed(&[interp("A", "Tender", 0)]);
        let r = f.sync(&[interp("A", "Soft", 0)], Some(&base));
        assert_eq!(r.updates, 1);
        assert_eq!(r.skips, 0);
        assert_eq!(f.row("A").name, "Soft");
    }

    #[test]
    fn rule_2_a_remote_updated_row_the_editor_never_touched_is_kept() {
        let f = fixture();
        let a = interp("A", "Tender", 0);
        let base = f.seed(std::slice::from_ref(&a)); // editor loads here
        f.remote_update("A", "Remote"); // remote lands while open
        let ops = f.oplog_count();

        // Stale save: mine == base, db != base.
        let r = f.sync(&[a], Some(&base));
        assert_eq!(r.total(), 0, "nothing may be written");
        assert_eq!(r.skips, 1);
        let row = f.row("A");
        assert_eq!(row.name, "Remote", "the remote value must survive");
        assert_eq!(row.hlc_packed, "REMOTE-PACKED");
        assert_eq!(f.oplog_count(), ops, "no echo op");
    }

    #[test]
    fn rule_3_a_true_conflict_is_won_by_the_active_editor() {
        let f = fixture();
        let base = f.seed(&[interp("A", "Tender", 0)]);
        f.remote_update("A", "Remote");
        let r = f.sync(&[interp("A", "MyEdit", 0)], Some(&base));
        assert_eq!(r.updates, 1);
        let row = f.row("A");
        assert_eq!(row.name, "MyEdit");
        assert_ne!(row.hlc_packed, "REMOTE-PACKED");
    }

    #[test]
    fn rule_4_a_remote_created_row_absent_from_the_stale_aggregate_is_not_tombstoned() {
        let f = fixture();
        let a = interp("A", "Tender", 0);
        let base = f.seed(std::slice::from_ref(&a));
        f.remote_insert("B", "RemoteNew", 1);
        let ops = f.oplog_count();

        let r = f.sync(&[a], Some(&base));
        assert_eq!(r.tombstones, 0);
        assert_eq!(r.skips, 1);
        assert!(f.row("B").deleted_at.is_none(), "B must stay live");
        assert_eq!(f.oplog_count(), ops);
    }

    #[test]
    fn rule_5_a_remote_delete_of_an_untouched_held_row_is_accepted() {
        let f = fixture();
        let a = interp("A", "Tender", 0);
        let b = interp("B", "Powerful", 1);
        let base = f.seed(&[a.clone(), b.clone()]);
        f.remote_tombstone("B");
        let ops = f.oplog_count();

        let r = f.sync(&[a, b], Some(&base));
        assert_eq!(r.total(), 0);
        assert_eq!(r.skips, 1);
        assert!(f.row("B").deleted_at.is_some(), "the tombstone must hold");
        assert_eq!(f.oplog_count(), ops);
    }

    #[test]
    fn rule_5_an_edit_vs_delete_conflict_revives_the_row_with_the_local_edit() {
        let f = fixture();
        let a = interp("A", "Tender", 0);
        let b = interp("B", "Powerful", 1);
        let base = f.seed(&[a.clone(), b]);
        f.remote_tombstone("B");

        let r = f.sync(&[a, interp("B", "Belted", 1)], Some(&base));
        assert_eq!(r.inserts, 1, "revive goes through the upsert path");
        let row = f.row("B");
        assert!(row.deleted_at.is_none(), "the active editor wins");
        assert_eq!(row.name, "Belted");
        assert_ne!(row.hlc_packed, "REMOTE-PACKED");
    }

    #[test]
    fn rule_6_an_explicit_local_removal_tombstones_even_over_a_concurrent_remote_edit() {
        let f = fixture();
        let a = interp("A", "Tender", 0);
        let b = interp("B", "Powerful", 1);
        let base = f.seed(&[a.clone(), b]);
        f.remote_update("B", "RemoteEdit"); // db != base

        let r = f.sync(&[a], Some(&base)); // the user dropped B
        assert_eq!(r.tombstones, 1);
        assert!(f.row("B").deleted_at.is_some());
    }

    #[test]
    fn rule_7_a_new_local_row_inserts() {
        let f = fixture();
        let a = interp("A", "Tender", 0);
        let base = f.seed(std::slice::from_ref(&a));
        let r = f.sync(&[a, interp("C", "Fresh", 1)], Some(&base));
        assert_eq!(r.inserts, 1);
        assert!(f.row("C").deleted_at.is_none());
    }

    #[test]
    fn unchanged_rows_emit_nothing_whatever_the_base() {
        let f = fixture();
        let a = interp("A", "Tender", 0);
        f.seed(std::slice::from_ref(&a));
        let ops = f.oplog_count();
        let bogus: IndexMap<String, String> = [(id("A"), "bogus-base-hash".to_string())]
            .into_iter()
            .collect();
        let r = f.sync(&[a], Some(&bogus));
        assert_eq!(r.total(), 0);
        assert_eq!(r.skips, 0);
        assert_eq!(f.oplog_count(), ops);
    }

    #[test]
    fn reconcile_op_for_is_the_rule_table() {
        use ReconcileOp::*;
        assert_eq!(reconcile_op_for("m", None, None), Insert); // rule 7
        assert_eq!(reconcile_op_for("m", None, Some("m")), Skip); // rule 5
        assert_eq!(reconcile_op_for("m", None, Some("b")), Insert); // rule 5
        assert_eq!(reconcile_op_for("m", Some("m"), Some("x")), Unchanged);
        assert_eq!(reconcile_op_for("m", Some("d"), Some("m")), Skip); // rule 2
        assert_eq!(reconcile_op_for("m", Some("d"), Some("d")), Update); // rule 1
        assert_eq!(reconcile_op_for("m", Some("d"), Some("b")), Update); // rule 3
        assert_eq!(reconcile_op_for("m", Some("d"), None), Update); // no base
    }

    #[test]
    fn a_failing_callback_fails_the_whole_save() {
        let f = fixture();
        let current = IndexMap::new();
        let failed: Result<ReconcileResult> = f.db.transaction(|| {
            reconcile_collection(ReconcileCollection {
                writer: &f.writer,
                table: "interpretations",
                base_hashes: &current,
                current_hashes: &current,
                desired: &[interp("A", "x", 0)],
                id_of: &|i: &Interp| i.id.clone(),
                hash_of: &hash_of,
                insert: &mut |_, _| Err(crate::Error::State("boom".to_string())),
                update: &mut |_, _| Ok(()),
                tombstone: &mut |_, _| Ok(()),
            })
        });
        assert!(failed.is_err());
        assert_eq!(f.oplog_count(), 0);
    }
}
