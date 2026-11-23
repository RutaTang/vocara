//! Cross-row invariants the database's own constraints can't catch, and
//! their repair (port of `lib/data/sync/integrity_checker.dart`).

use std::fmt;
use std::sync::Arc;

use crate::db::AppDatabase;
use crate::error::Result;
use crate::sync::hlc::{Hlc, HlcClock};
use crate::sync::quarantine_store::QuarantineStore;
use crate::sync::sync_writer::{op, SyncStamp, SyncWriter};

/// One detected integrity violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityViolation {
    /// Short stable id of the broken invariant (for logging / tests).
    pub invariant: String,
    pub table: String,
    pub id: String,
    pub detail: String,
}

impl IntegrityViolation {
    fn new(invariant: &str, table: &str, id: String, detail: String) -> Self {
        IntegrityViolation {
            invariant: invariant.to_string(),
            table: table.to_string(),
            id,
            detail,
        }
    }
}

impl fmt::Display for IntegrityViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {}/{}: {}",
            self.invariant, self.table, self.id, self.detail
        )
    }
}

/// (childTable, fkColumn, parentTable, invariantId). Ordered PARENT-FIRST so
/// a repair pass tombstones a dangling parent before its children are
/// examined.
const CHILD_EDGES: [(&str, &str, &str, &str); 9] = [
    ("tracks", "practice_id", "practices", "track-practice"),
    (
        "interpretations",
        "practice_id",
        "practices",
        "interp-practice",
    ),
    ("fx_buses", "practice_id", "practices", "bus-practice"),
    (
        "technique_media",
        "technique_id",
        "techniques",
        "media-technique",
    ),
    ("clips", "track_id", "tracks", "clip-track"),
    ("practice_notes", "track_id", "tracks", "note-track"),
    ("technique_markers", "track_id", "tracks", "marker-track"),
    ("clips", "asset_id", "assets", "clip-asset"),
    ("technique_media", "asset_id", "assets", "media-asset"),
];

/// Checks (and optionally repairs) the cross-row invariants that the
/// database's own FK + CHECK constraints can't catch under soft-delete +
/// cross-device merges — e.g. a LIVE child whose parent was tombstoned (the
/// row still exists, so foreign_key_check passes), or a scalar pointer
/// resolving to a tombstoned row, or two live single-slot tracks after two
/// offline devices both added one.
///
/// Invoked at three points: post-write (debug + periodic),
/// migration-validate (full DB on the migrated copy), and sync-apply (the
/// affected closure). Repairs journal through the [`SyncWriter`] like any
/// edit, but are stamped with a DERIVED HLC, never a fresh clock tick — see
/// `repair_stamp`. And no row is judged against a referenced row whose NEWER
/// state is still parked in the sync quarantine: that verdict is deferred
/// until the parked state settles, and neither reported nor repaired
/// meanwhile — see `parked_newer`.
///
/// Its statements run on the store's connection, so a caller's
/// `db.transaction(|| checker.repair())` makes a whole repair pass (every
/// row write and its journal entry) one atomic unit.
#[derive(Debug, Clone)]
pub struct IntegrityChecker {
    db: Arc<AppDatabase>,
    writer: Arc<SyncWriter>,
}

impl IntegrityChecker {
    /// The pass count [`IntegrityChecker::repair`] gives up after.
    pub const DEFAULT_MAX_PASSES: usize = 8;

    /// A checker over [db], journaling repairs through [writer].
    pub fn new(db: Arc<AppDatabase>, writer: Arc<SyncWriter>) -> Self {
        IntegrityChecker { db, writer }
    }

    /// The stamp for one repair write, DERIVED from the repaired row's own
    /// HLC via [`HlcClock::derived`]: same (wall, counter), node extended by
    /// `'!' + this device's node`.
    ///
    /// A repair is derived state, not user intent. The extension is strictly
    /// ABOVE the row's current stamp (so the repair wins LWW on every peer
    /// and the fleet converges) yet strictly BELOW every stamp that
    /// out-ranks the base — a held-wall author edit at (wall, counter + 1),
    /// a node-space tie winner, and any later tick. A fresh `writer.stamp()`
    /// here — the oldest behavior — minted user-grade intent for bookkeeping
    /// and resurrected a deleted practice (simulation seed 0xab1a57); the
    /// intermediate counter + 1 derivation lost held-wall author edits to
    /// repairs, and let CHAINED repairs (base+2) beat base+1 real edits.
    /// Deriving in node space closes all three. A row whose stored stamp
    /// does not parse fails the repair (Dart threw its FormatException).
    fn repair_stamp(&self, row_hlc_packed: &str) -> Result<SyncStamp> {
        Ok(SyncStamp::new(HlcClock::derived(
            &Hlc::parse(row_hlc_packed)?,
            self.writer.clock().node(),
        )))
    }

    /// SQL predicate: the [table] row named by [id_expr] has a NEWER state
    /// still PARKED — a replayable `sync_quarantine` entry for it with no
    /// local row at least as new (the local row is older, or absent). Every
    /// verdict that judges a row against such a REFERENCED row (a child
    /// against its parent, a pointer against its target, a route against
    /// the bus it leaves) is DEFERRED while this holds: it is not a
    /// violation yet, only a pending state.
    ///
    /// Why: a repair judged against a state the quarantine already
    /// supersedes manufactures intent. The canonical case: a delete of track
    /// T on one device, concurrent edits of T and its clip C on another
    /// (both beat the delete, so the converged state is T live + C live). A
    /// puller that parks the T edit at the drift gate but applies the C edit
    /// sees a live clip under a tombstoned track, and repairing it
    /// tombstones C under a derived stamp ABOVE C's winning edit —
    /// journaled, pushed, adopted fleet-wide, and still standing after T's
    /// parked edit lands live. The same holds for a newer-schema park, and
    /// for an update the FK sweep parked and restored over (its prior state
    /// may be the tombstone).
    ///
    /// A deferral lasts exactly as long as the park. The replay that ends it
    /// — the parked state lands, or settles as an LWW skip because a newer
    /// row is held — gates the sync engine's end-of-run repair, so the
    /// verdict is re-judged in that same run (an entry removed any other
    /// way, e.g. a manual discard, waits for the next repair). A park that
    /// never settles (a newer-schema row on a build that is never upgraded)
    /// defers its dependents as long: the newest known state of the
    /// referenced row is simply not applicable here, and guessing at it
    /// would be the bug above. An UNREPLAYABLE entry
    /// ([`QuarantineStore::replayable_sql`]) can never land, so it defers
    /// nothing. Only the REFERENCED row's parked state matters: a parked
    /// newer state of the repaired row ITSELF already out-ranks the derived
    /// repair stamp, here and on every peer, so it is never overridden by
    /// the repair.
    ///
    /// Both of the referenced row's parking-lot entries count — the door's,
    /// and the FK sweep's own slot, which holds exactly that
    /// parked-and-restored update (see [`QuarantineStore::entry_of_row_sql`]).
    fn parked_newer(table: &str, id_expr: &str) -> String {
        format!(
            "EXISTS (SELECT 1 FROM sync_quarantine q WHERE q.tbl = '{table}' \
             AND {} \
             AND {} \
             AND NOT EXISTS (SELECT 1 FROM {table} x WHERE x.id = {id_expr} \
             AND x.hlc_packed >= q.remote_hlc_packed))",
            QuarantineStore::entry_of_row_sql("q", id_expr),
            QuarantineStore::replayable_sql("q"),
        )
    }

    /// Detects all current violations without changing anything.
    pub fn check(&self) -> Result<Vec<IntegrityViolation>> {
        self.scan(false)
    }

    /// Detects AND repairs, looping to a fixpoint so a cascade (tombstoning
    /// a parent then its now-orphaned children) fully resolves. Returns
    /// whatever could not be repaired (should be empty).
    pub fn repair(&self) -> Result<Vec<IntegrityViolation>> {
        self.repair_with_max_passes(Self::DEFAULT_MAX_PASSES)
    }

    /// [`IntegrityChecker::repair`] with an explicit pass cap.
    pub fn repair_with_max_passes(&self, max_passes: usize) -> Result<Vec<IntegrityViolation>> {
        for _ in 0..max_passes {
            let found = self.scan(true)?;
            if found.is_empty() {
                return Ok(Vec::new());
            }
        }
        self.scan(false) // remaining after the cap
    }

    /// (id, hlc_packed) of every row [sql] selects as `id` and `h`.
    fn ids_and_stamps(&self, sql: &str) -> Result<Vec<(String, String)>> {
        self.db.query(sql, [], |r| Ok((r.get("id")?, r.get("h")?)))
    }

    fn scan(&self, repair: bool) -> Result<Vec<IntegrityViolation>> {
        let mut out = Vec::new();

        // 1. Dangling children: a live row whose parent is missing or
        //    tombstoned — unless the parent's newer state is still parked
        //    (deferred, see parked_newer).
        for (child, fk, parent, inv) in CHILD_EDGES {
            let rows = self.ids_and_stamps(&format!(
                "SELECT c.id AS id, c.hlc_packed AS h FROM {child} c WHERE \
                 c.deleted_at IS NULL AND NOT EXISTS \
                 (SELECT 1 FROM {parent} p WHERE p.id = c.{fk} AND p.deleted_at IS NULL) \
                 AND NOT {}",
                Self::parked_newer(parent, &format!("c.{fk}"))
            ))?;
            for (id, h) in rows {
                if repair {
                    self.tombstone(child, &id, &h)?;
                }
                out.push(IntegrityViolation::new(
                    inv,
                    child,
                    id,
                    format!("{parent} missing/tombstoned"),
                ));
            }
        }

        // 2. Scalar self-heal: a pointer resolving to a missing/tombstoned
        //    target is nulled (foreign_key_check passes it, so we must catch
        //    it here) — unless the target's newer state is still parked
        //    (deferred, see parked_newer; every query below carries that
        //    guard).
        //
        //    The two FK-less pointers (active interpretation, bus output)
        //    heal only once their target EXISTS — tombstoned, or another
        //    practice's. An ABSENT target has not arrived yet: the pusher
        //    uploads in drain windows and chunks, and compaction leaves each
        //    row only its latest op, so the pointing row can reach a puller
        //    one request ahead of the row it names. Healing then would
        //    journal a derived null above the author's own row and reset the
        //    pointer fleet-wide; left alone, the late arrival simply
        //    satisfies it. (FK pointers never get here absent: the applier's
        //    FK sweep parks the row instead.)
        out.extend(self.scalar(
            repair,
            "active-interp",
            &format!(
                "SELECT p.id AS id, p.hlc_packed AS h FROM practices p WHERE \
                 p.deleted_at IS NULL AND p.active_interpretation_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM \
                 interpretations i WHERE i.id = p.active_interpretation_id AND \
                 i.deleted_at IS NULL AND i.practice_id = p.id) \
                 AND EXISTS (SELECT 1 FROM interpretations i \
                 WHERE i.id = p.active_interpretation_id) \
                 AND NOT {}",
                Self::parked_newer("interpretations", "p.active_interpretation_id")
            ),
            "practices",
            "active_interpretation_id",
        )?);
        out.extend(self.scalar(
            repair,
            "separation-asset",
            &format!(
                "SELECT p.id AS id, p.hlc_packed AS h FROM practices p WHERE \
                 p.deleted_at IS NULL AND p.separation_asset_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM assets \
                 a WHERE a.id = p.separation_asset_id AND a.deleted_at IS NULL) \
                 AND NOT {}",
                Self::parked_newer("assets", "p.separation_asset_id")
            ),
            "practices",
            "separation_asset_id",
        )?);
        // A vocal take's interpretation must be a live interpretation of its
        // OWN practice; otherwise unassign it.
        out.extend(self.scalar(
            repair,
            "vocal-interp",
            &format!(
                "SELECT t.id AS id, t.hlc_packed AS h FROM tracks t WHERE \
                 t.deleted_at IS NULL AND t.kind = 'vocal' AND t.interpretation_id IS NOT NULL AND NOT EXISTS \
                 (SELECT 1 FROM interpretations i WHERE i.id = t.interpretation_id AND \
                 i.deleted_at IS NULL AND i.practice_id = t.practice_id) \
                 AND NOT {}",
                Self::parked_newer("interpretations", "t.interpretation_id")
            ),
            "tracks",
            "interpretation_id",
        )?);
        // A track's routing bus must be a live fx_bus of its OWN practice;
        // otherwise null it (playback would silently reroute to master on a
        // dangling pointer).
        out.extend(self.scalar(
            repair,
            "track-bus",
            &format!(
                "SELECT t.id AS id, t.hlc_packed AS h FROM tracks t WHERE \
                 t.deleted_at IS NULL AND t.bus_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM fx_buses b WHERE \
                 b.id = t.bus_id AND b.deleted_at IS NULL AND b.practice_id = \
                 t.practice_id) \
                 AND NOT {}",
                Self::parked_newer("fx_buses", "t.bus_id")
            ),
            "tracks",
            "bus_id",
        )?);
        // A bus->bus output route must point at a live fx_bus of the same
        // practice; otherwise null it (falls back to master, same as an
        // absent route). An absent target is pending, not dangling (see
        // above); playback already routes a route it cannot resolve to
        // master meanwhile.
        out.extend(self.scalar(
            repair,
            "bus-output",
            &format!(
                "SELECT b.id AS id, b.hlc_packed AS h FROM fx_buses b WHERE \
                 b.deleted_at IS NULL AND b.output_bus_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM fx_buses o \
                 WHERE o.id = b.output_bus_id AND o.deleted_at IS NULL AND \
                 o.practice_id = b.practice_id) \
                 AND EXISTS (SELECT 1 FROM fx_buses o WHERE o.id = b.output_bus_id) \
                 AND NOT {}",
                Self::parked_newer("fx_buses", "b.output_bus_id")
            ),
            "fx_buses",
            "output_bus_id",
        )?);

        // 2b. Bus routing must stay ACYCLIC. Every device refuses to create a
        //     cycle locally, but a merge can still synthesise one out of two
        //     individually legal edits: A→B on one device and B→A on
        //     another, offline, both applied. Nothing above catches it — each
        //     edge points at a live bus of the same practice, so `bus-output`
        //     is satisfied — and the native engine then REJECTS the whole
        //     graph, which means the database and the UI show the merged
        //     routing while playback keeps running the last accepted graph
        //     (or, on a fresh open, has no graph at all).
        out.extend(self.bus_cycles(repair)?);

        // 3. Single-slot LWW: at most one live accompaniment / reference per
        //    practice. Keep the highest HLC, tombstone the losers.
        let dupes: Vec<(String, String)> = self.db.query(
            "SELECT practice_id, kind FROM tracks WHERE deleted_at IS NULL AND \
             kind IN ('accompaniment', 'reference') GROUP BY practice_id, kind \
             HAVING COUNT(*) > 1",
            [],
            |r| Ok((r.get("practice_id")?, r.get("kind")?)),
        )?;
        for (practice_id, kind) in dupes {
            let rows: Vec<(String, String)> = self.db.query(
                "SELECT id, hlc_packed FROM tracks WHERE deleted_at IS NULL AND \
                 practice_id = ? AND kind = ? ORDER BY hlc_packed DESC",
                [&practice_id, &kind],
                |r| Ok((r.get("id")?, r.get("hlc_packed")?)),
            )?;
            for (id, h) in rows.into_iter().skip(1) {
                if repair {
                    self.tombstone("tracks", &id, &h)?;
                }
                out.push(IntegrityViolation::new(
                    "slot-dup",
                    "tracks",
                    id,
                    format!("duplicate live {kind} slot"),
                ));
            }
        }

        Ok(out)
    }

    fn scalar(
        &self,
        repair: bool,
        invariant: &str,
        sql: &str,
        table: &str,
        column: &str,
    ) -> Result<Vec<IntegrityViolation>> {
        let rows = self.ids_and_stamps(sql)?;
        let mut out = Vec::new();
        for (id, h) in rows {
            if repair {
                self.null_column(table, column, &id, &h)?;
            }
            out.push(IntegrityViolation::new(
                invariant,
                table,
                id,
                format!("{column} dangling"),
            ));
        }
        Ok(out)
    }

    /// Buses whose `output_bus_id` chain reaches themselves. `UNION` (not
    /// `UNION ALL`) dedupes the frontier, so the walk terminates ON a cycle
    /// instead of spinning in it.
    ///
    /// Repair breaks each cycle at ONE edge — the member with the lowest
    /// (hlc, id) drops its route — rather than clearing every route on the
    /// ring: the loser is the older of the two edits that collided, which
    /// matches the LWW rule the rest of the merge uses, and the newer edit
    /// survives. The outer repair fixpoint loop comes back for any further
    /// cycle. Every replica sees the same rows here, so every replica picks
    /// the same victim.
    ///
    /// A ring through a bus whose newer state is still parked
    /// (parked_newer) is broken AT that bus instead. Its route is pending
    /// and the parked state may well not close the ring, so cutting another
    /// member would null a peer's route fleet-wide for a cycle that never
    /// existed; but the ring cannot be left standing either — the engine
    /// refuses a cyclic graph, so the practice would play nothing (or never
    /// take another structural edit) for as long as the park lasts. The
    /// parked bus's own parked state already out-ranks the derived repair
    /// stamp, so the cut lasts exactly until that state lands, and a ring it
    /// really closes is judged again then.
    fn bus_cycles(&self, repair: bool) -> Result<Vec<IntegrityViolation>> {
        let rows: Vec<(String, String, i64)> = self.db.query(
            &format!(
                "WITH RECURSIVE edge(src, dst) AS (\
                   SELECT b.id, b.output_bus_id FROM fx_buses b \
                   WHERE b.deleted_at IS NULL AND b.output_bus_id IS NOT NULL \
                     AND EXISTS (SELECT 1 FROM fx_buses o WHERE o.id = b.output_bus_id \
                       AND o.deleted_at IS NULL AND o.practice_id = b.practice_id)\
                 ), reach(start, node) AS (\
                   SELECT src, dst FROM edge \
                   UNION \
                   SELECT r.start, e.dst FROM reach r JOIN edge e ON e.src = r.node\
                 ) \
                 SELECT b.id AS id, b.hlc_packed AS h, \
                   {} AS parked \
                 FROM fx_buses b \
                 WHERE EXISTS (SELECT 1 FROM reach r WHERE r.start = b.id AND r.node = b.id) \
                 ORDER BY b.hlc_packed ASC, b.id ASC",
                Self::parked_newer("fx_buses", "b.id")
            ),
            [],
            |r| Ok((r.get("id")?, r.get("h")?, r.get("parked")?)),
        )?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        if repair {
            let victim = rows
                .iter()
                .find(|(_, _, parked)| *parked != 0)
                .unwrap_or(&rows[0]);
            self.null_column("fx_buses", "output_bus_id", &victim.0, &victim.1)?;
        }
        Ok(rows
            .into_iter()
            .map(|(id, _, _)| {
                IntegrityViolation::new(
                    "bus-cycle",
                    "fx_buses",
                    id,
                    "output_bus_id chain returns to itself".to_string(),
                )
            })
            .collect())
    }

    /// Nulls one column on one row as a repair: derived stamp, journalled
    /// like any other edit so peers converge on the same fix.
    fn null_column(&self, table: &str, column: &str, id: &str, row_hlc_packed: &str) -> Result<()> {
        let s = self.repair_stamp(row_hlc_packed)?;
        self.db.execute(
            &format!(
                "UPDATE {table} SET {column} = NULL, updated_at = ?, hlc_wall = ?, \
                 hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?"
            ),
            rusqlite::params![
                s.at(),
                s.hlc.wall_ms,
                s.hlc.counter,
                s.hlc.node,
                s.hlc.packed(),
                id
            ],
        )?;
        self.writer.journal(
            table,
            id,
            op::UPDATE,
            &s.hlc,
            &format!("integrity:{column}"),
            None,
        )
    }

    fn tombstone(&self, table: &str, id: &str, row_hlc_packed: &str) -> Result<()> {
        let s = self.repair_stamp(row_hlc_packed)?;
        self.db.execute(
            &format!(
                "UPDATE {table} SET deleted_at = ?, updated_at = ?, hlc_wall = ?, \
                 hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?"
            ),
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
        self.writer
            .journal(table, id, op::TOMBSTONE, &s.hlc, "", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::rows::{
        AssetRow, ClipRow, FxBusRow, InterpretationRow, OplogRow, PracticeRow, TrackRow,
    };
    use crate::error::Error;
    use crate::sync::quarantine_store::QuarantineEntry;
    use vocara_model::util::id::new_id;

    struct Fixture {
        db: Arc<AppDatabase>,
        writer: Arc<SyncWriter>,
        checker: IntegrityChecker,
    }

    /// What the repository's createPractice + one-take save leave behind: a
    /// practice whose active interpretation is its one interpretation, one
    /// vocal take in it, one asset, one clip of it.
    struct Seeded {
        practice_id: String,
        interpretation_id: String,
    }

    fn fixture() -> Fixture {
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        let clock = Arc::new(HlcClock::new(format!("{:0<26}", "NODE")));
        let writer = Arc::new(SyncWriter::new(db.clone(), clock));
        Fixture {
            checker: IntegrityChecker::new(db.clone(), writer.clone()),
            db,
            writer,
        }
    }

    macro_rules! stamped {
        ($f:expr, $row:ident { $($field:ident : $value:expr),* $(,)? }) => {{
            let s = $f.writer.stamp();
            let row = $row {
                created_at: s.at(),
                updated_at: s.at(),
                hlc_wall: s.hlc.wall_ms,
                hlc_ctr: s.hlc.counter,
                hlc_node: s.hlc.node.clone(),
                hlc_packed: s.hlc.packed(),
                $($field: $value,)*
                ..Default::default()
            };
            $f.db.insert(&row).unwrap();
            row
        }};
    }

    impl Fixture {
        fn seed(&self) -> Seeded {
            let practice_id = new_id();
            let interpretation_id = new_id();
            stamped!(
                self,
                PracticeRow {
                    id: practice_id.clone(),
                    title: "S".into()
                }
            );
            stamped!(
                self,
                InterpretationRow {
                    id: interpretation_id.clone(),
                    practice_id: practice_id.clone(),
                    name: "Take".into(),
                }
            );
            self.db
                .update_by_id(
                    "practices",
                    &practice_id,
                    &[("active_interpretation_id", &interpretation_id)],
                )
                .unwrap();
            let track_id = new_id();
            stamped!(
                self,
                TrackRow {
                    id: track_id.clone(),
                    practice_id: practice_id.clone(),
                    kind: "vocal".into(),
                    interpretation_id: Some(interpretation_id.clone()),
                    display_name: "Take".into(),
                }
            );
            let asset_id = new_id();
            stamped!(
                self,
                AssetRow {
                    id: asset_id.clone(),
                    content_hash: "ab".repeat(32),
                    byte_size: 3,
                    ext: "wav".into(),
                }
            );
            stamped!(
                self,
                ClipRow {
                    id: new_id(),
                    track_id: track_id,
                    asset_id: asset_id,
                    duration_ms: 1000,
                }
            );
            Seeded {
                practice_id,
                interpretation_id,
            }
        }

        fn bus(&self, practice_id: &str, name: &str) -> String {
            let id = new_id();
            stamped!(
                self,
                FxBusRow {
                    id: id.clone(),
                    practice_id: practice_id.to_string(),
                    name: name.into(),
                    fx_json: Some("{}".into()),
                }
            );
            id
        }

        fn raw_tombstone(&self, table: &str, id: &str) {
            self.db
                .execute(
                    &format!("UPDATE {table} SET deleted_at = 999 WHERE id = ?"),
                    [id],
                )
                .unwrap();
        }

        fn first_id(&self, table: &str) -> String {
            self.db
                .query_row(
                    &format!("SELECT id FROM {table} WHERE deleted_at IS NULL LIMIT 1"),
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        }

        /// Parks a remote state for (table, id) in the sync quarantine,
        /// stamped [delta_ms] away from the row's CURRENT stamp.
        fn park(&self, table: &str, id: &str, delta_ms: i64, reason: &str) {
            let held: String = self
                .db
                .query_row(
                    &format!("SELECT hlc_packed FROM {table} WHERE id = ?"),
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            let base = Hlc::parse(&held).unwrap();
            let packed = Hlc::new(base.wall_ms + delta_ms, 0, format!("{:0<26}", "PEER")).packed();
            QuarantineStore::new(self.db.clone())
                .put(&QuarantineEntry {
                    table,
                    id,
                    hlc_packed: &packed,
                    schema_v: 1,
                    payload: "{}",
                    reason,
                    quarantined_at: 0,
                })
                .unwrap();
        }

        fn oplog_count(&self) -> usize {
            self.db.select_all::<OplogRow>().unwrap().len()
        }

        fn invariants(&self) -> Vec<String> {
            self.checker
                .check()
                .unwrap()
                .into_iter()
                .map(|v| v.invariant)
                .collect()
        }

        fn live_clips(&self) -> i64 {
            self.db
                .query_row(
                    "SELECT COUNT(*) AS n FROM clips WHERE deleted_at IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        }

        fn active_interp(&self, practice_id: &str) -> Option<String> {
            self.db
                .query_row(
                    "SELECT active_interpretation_id AS a FROM practices WHERE id = ?",
                    [practice_id],
                    |r| r.get(0),
                )
                .unwrap()
        }

        fn route_of(&self, bus_id: &str) -> Option<String> {
            self.db
                .query_row(
                    "SELECT output_bus_id AS o FROM fx_buses WHERE id = ?",
                    [bus_id],
                    |r| r.get(0),
                )
                .unwrap()
        }

        fn route(&self, from: &str, to: &str, hlc_packed: Option<&str>) {
            match hlc_packed {
                Some(h) => self.db.execute(
                    "UPDATE fx_buses SET output_bus_id = ?, hlc_packed = ? WHERE id = ?",
                    [to, h, from],
                ),
                None => self.db.execute(
                    "UPDATE fx_buses SET output_bus_id = ? WHERE id = ?",
                    [to, from],
                ),
            }
            .unwrap();
        }
    }

    fn peer_stamp(wall: i64) -> String {
        Hlc::new(wall, 0, format!("{:0<26}", "PEER")).packed()
    }

    #[test]
    fn a_well_formed_practice_has_no_violations() {
        let f = fixture();
        f.seed();
        assert!(f.checker.check().unwrap().is_empty());
    }

    #[test]
    fn a_live_clip_under_a_tombstoned_track_is_detected_and_cascade_repaired() {
        let f = fixture();
        f.seed();
        f.raw_tombstone("tracks", &f.first_id("tracks"));

        assert!(f.invariants().contains(&"clip-track".to_string()));
        assert!(f.checker.repair().unwrap().is_empty());
        // The orphaned clip is now tombstoned too.
        assert_eq!(f.live_clips(), 0);
    }

    #[test]
    fn a_dangling_active_interpretation_id_is_nulled() {
        let f = fixture();
        let s = f.seed();
        f.raw_tombstone("interpretations", &s.interpretation_id);

        assert!(f.invariants().contains(&"active-interp".to_string()));
        f.checker.repair().unwrap();
        assert_eq!(f.active_interp(&s.practice_id), None);
        assert!(f.checker.check().unwrap().is_empty());
    }

    #[test]
    fn an_active_interpretation_id_whose_target_is_absent_is_pending() {
        // A half-uploaded push: the practice reached this device one request
        // before the interpretation it points at. Absent is not dangling.
        let f = fixture();
        let s = f.seed();
        f.db.execute(
            "UPDATE practices SET active_interpretation_id = ? WHERE id = ?",
            [new_id(), s.practice_id.clone()],
        )
        .unwrap();
        let ops_before = f.oplog_count();

        assert!(f.checker.check().unwrap().is_empty());
        assert!(f.db.transaction(|| f.checker.repair()).unwrap().is_empty());
        assert_eq!(f.oplog_count(), ops_before);
    }

    #[test]
    fn a_bus_route_is_nulled_when_its_target_is_tombstoned_or_foreign_and_left_while_absent() {
        let f = fixture();
        let s = f.seed();
        let a = f.bus(&s.practice_id, "A");
        let b = f.bus(&s.practice_id, "B");

        // Absent: pending.
        let absent = new_id();
        f.route(&a, &absent, None);
        assert!(f.checker.check().unwrap().is_empty());
        f.checker.repair().unwrap();
        assert_eq!(f.route_of(&a), Some(absent));

        // Tombstoned: dangling.
        f.route(&a, &b, None);
        f.raw_tombstone("fx_buses", &b);
        assert!(f.invariants().contains(&"bus-output".to_string()));
        assert!(f.checker.repair().unwrap().is_empty());
        assert_eq!(f.route_of(&a), None);

        // Live, but another practice's bus: dangling.
        let other = new_id();
        stamped!(
            f,
            PracticeRow {
                id: other.clone(),
                title: "Other".into()
            }
        );
        let foreign = f.bus(&other, "F");
        f.route(&a, &foreign, None);
        assert!(f.invariants().contains(&"bus-output".to_string()));
        assert!(f.checker.repair().unwrap().is_empty());
        assert_eq!(f.route_of(&a), None);
    }

    #[test]
    fn a_clip_whose_asset_was_tombstoned_is_repaired() {
        let f = fixture();
        f.seed();
        f.raw_tombstone("assets", &f.first_id("assets"));
        assert!(f.invariants().contains(&"clip-asset".to_string()));
        assert!(f.checker.repair().unwrap().is_empty());
    }

    #[test]
    fn a_bus_routing_cycle_is_detected_and_broken_at_its_oldest_edge() {
        let f = fixture();
        let s = f.seed();
        let a = f.bus(&s.practice_id, "A");
        let b = f.bus(&s.practice_id, "B");
        // A is the older edit (lower HLC), so it is the one that must lose
        // its route: the newer B→A survives.
        f.route(&a, &b, Some(&peer_stamp(1000)));
        f.route(&b, &a, Some(&peer_stamp(2000)));

        let found = f.checker.check().unwrap();
        assert!(found.iter().all(|v| v.invariant == "bus-cycle"));
        let mut ids: Vec<&str> = found.iter().map(|v| v.id.as_str()).collect();
        ids.sort();
        let mut expected = vec![a.as_str(), b.as_str()];
        expected.sort();
        assert_eq!(ids, expected);

        assert!(f.checker.repair().unwrap().is_empty());
        assert_eq!(f.route_of(&a), None, "the older edge is dropped");
        assert_eq!(f.route_of(&b), Some(a.clone()), "the newer edge survives");
    }

    #[test]
    fn a_self_route_is_a_cycle_of_one_and_is_repaired() {
        let f = fixture();
        let s = f.seed();
        let a = f.bus(&s.practice_id, "A");
        f.db.execute("UPDATE fx_buses SET output_bus_id = id WHERE id = ?", [&a])
            .unwrap();
        assert!(f.invariants().contains(&"bus-cycle".to_string()));
        assert!(f.checker.repair().unwrap().is_empty());
    }

    #[test]
    fn repair_composes_into_an_enclosing_transaction() {
        let f = fixture();
        f.seed();
        f.raw_tombstone("tracks", &f.first_id("tracks"));
        assert!(!f.checker.check().unwrap().is_empty());
        let ops_before = f.oplog_count();

        let failed: Result<()> = f.db.transaction(|| {
            f.checker.repair()?;
            Err(Error::State("injected post-repair crash".to_string()))
        });
        assert!(failed.is_err());

        // The violations are all still present and not one journal entry
        // leaked.
        assert!(!f.checker.check().unwrap().is_empty());
        assert_eq!(f.oplog_count(), ops_before);

        // The same repair inside a COMMITTED transaction resolves everything
        // and journals it.
        f.db.transaction(|| f.checker.repair()).unwrap();
        assert!(f.checker.check().unwrap().is_empty());
        assert!(f.oplog_count() > ops_before);
    }

    #[test]
    fn repairs_are_stamped_derived_from_the_rows_own_hlc_and_journaled() {
        let f = fixture();
        f.seed();
        let track = f.first_id("tracks");
        let clip: ClipRow = f.db.select("1", []).unwrap().pop().unwrap();
        f.raw_tombstone("tracks", &track);
        f.checker.repair().unwrap();

        let repaired: ClipRow = f.db.find(&clip.id).unwrap().unwrap();
        let base = Hlc::parse(&clip.hlc_packed).unwrap();
        let derived = HlcClock::derived(&base, f.writer.clock().node());
        assert_eq!(repaired.hlc_packed, derived.packed());
        assert_eq!(repaired.deleted_at, Some(derived.wall_ms));
        let last = f.db.select_all::<OplogRow>().unwrap().pop().unwrap();
        assert_eq!(last.entity_id, clip.id);
        assert_eq!(last.op, "tombstone");
        assert_eq!(last.hlc_packed, derived.packed());
    }

    #[test]
    fn duplicate_live_slot_tracks_keep_the_highest_hlc() {
        let f = fixture();
        let s = f.seed();
        let older = new_id();
        let newer = new_id();
        // The partial unique index forbids two live slots: land one
        // tombstoned, then revive it with raw SQL, as a merge could.
        stamped!(
            f,
            TrackRow {
                id: older.clone(),
                practice_id: s.practice_id.clone(),
                kind: "accompaniment".into(),
                deleted_at: Some(1),
            }
        );
        stamped!(
            f,
            TrackRow {
                id: newer.clone(),
                practice_id: s.practice_id.clone(),
                kind: "accompaniment".into(),
            }
        );
        f.db.execute("DROP INDEX ux_track_slot", []).unwrap();
        f.db.execute("UPDATE tracks SET deleted_at = NULL WHERE id = ?", [&older])
            .unwrap();

        let found = f.checker.check().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].invariant, "slot-dup");
        assert_eq!(found[0].id, older);
        assert_eq!(
            found[0].to_string(),
            format!("[slot-dup] tracks/{older}: duplicate live accompaniment slot")
        );
        assert!(f.checker.repair().unwrap().is_empty());
        assert!(f
            .db
            .find::<TrackRow>(&older)
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some());
        assert!(f
            .db
            .find::<TrackRow>(&newer)
            .unwrap()
            .unwrap()
            .deleted_at
            .is_none());
    }

    // --- a parked NEWER state of the referenced row defers the verdict ---

    #[test]
    fn a_live_clip_under_a_tombstoned_track_whose_newer_state_is_parked_is_left_alone() {
        let f = fixture();
        f.seed();
        let track = f.first_id("tracks");
        f.raw_tombstone("tracks", &track);
        f.park("tracks", &track, 1000, "hlc-drift: parked by the test");
        let ops_before = f.oplog_count();

        assert!(f.checker.check().unwrap().is_empty());
        assert!(f.db.transaction(|| f.checker.repair()).unwrap().is_empty());
        assert_eq!(f.oplog_count(), ops_before, "no repair was journaled");
        assert_eq!(f.live_clips(), 1);
    }

    #[test]
    fn only_a_replayable_park_strictly_newer_than_the_local_row_defers() {
        let f = fixture();
        f.seed();
        let track = f.first_id("tracks");
        f.raw_tombstone("tracks", &track);

        // Can never land (a pure function of the payload): defers nothing.
        f.park("tracks", &track, 1000, "invalid-value: parked by the test");
        assert!(f.invariants().contains(&"clip-track".to_string()));
        // Older than what the table holds: it would settle as an LWW skip.
        QuarantineStore::new(f.db.clone())
            .delete("tracks", &track)
            .unwrap();
        f.park("tracks", &track, -1000, "hlc-drift: parked by the test");
        assert!(f.invariants().contains(&"clip-track".to_string()));
        assert!(f.checker.repair().unwrap().is_empty());
    }

    #[test]
    fn a_scalar_heal_is_deferred_while_its_targets_newer_state_is_parked() {
        let f = fixture();
        let s = f.seed();
        f.raw_tombstone("interpretations", &s.interpretation_id);
        f.park(
            "interpretations",
            &s.interpretation_id,
            1000,
            "hlc-drift: parked by the test",
        );

        assert!(f.checker.check().unwrap().is_empty());
        f.checker.repair().unwrap();
        assert_eq!(
            f.active_interp(&s.practice_id),
            Some(s.interpretation_id.clone()),
            "the pointer must survive until the parked state settles"
        );

        // The park settles (here: discarded) — the verdict is judged again.
        QuarantineStore::new(f.db.clone())
            .delete("interpretations", &s.interpretation_id)
            .unwrap();
        assert!(f.invariants().contains(&"active-interp".to_string()));
    }

    #[test]
    fn a_bus_cycle_through_a_bus_whose_newer_state_is_parked_is_broken_at_that_bus() {
        let f = fixture();
        let s = f.seed();
        let a = f.bus(&s.practice_id, "A");
        let b = f.bus(&s.practice_id, "B");
        f.route(&a, &b, Some(&peer_stamp(1000)));
        f.route(&b, &a, Some(&peer_stamp(2000)));
        // B (not the would-be victim A) has a newer state parked.
        f.park("fx_buses", &b, 1000, "hlc-drift: parked by the test");

        let found = f.checker.check().unwrap();
        assert!(!found.is_empty() && found.iter().all(|v| v.invariant == "bus-cycle"));
        f.checker.repair().unwrap();
        assert_eq!(f.route_of(&a), Some(b.clone()), "A's route is not nulled");
        assert_eq!(f.route_of(&b), None, "the ring is cut at the pending bus");
        assert!(f.checker.check().unwrap().is_empty());
        // The cut ranks below B's parked state, so it lasts only until that
        // state lands.
        let parked: String =
            f.db.query_row(
                "SELECT remote_hlc_packed AS h FROM sync_quarantine WHERE id = ?",
                [&b],
                |r| r.get(0),
            )
            .unwrap();
        let cut: FxBusRow = f.db.find(&b).unwrap().unwrap();
        assert!(cut.hlc_packed < parked);
    }
}
