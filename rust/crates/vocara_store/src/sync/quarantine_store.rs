//! The sync quarantine's parking lot (port of
//! `lib/data/sync/quarantine_store.dart`).

use std::sync::Arc;

use crate::db::rows::SyncQuarantineRow;
use crate::db::AppDatabase;
use crate::error::Result;

/// One state to park: the arguments of [`QuarantineStore::put`] (Dart's
/// named parameters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarantineEntry<'a> {
    /// The synced table the rejected record targets.
    pub table: &'a str,
    /// The record's row id (or its slot key).
    pub id: &'a str,
    /// The record's packed HLC as received.
    pub hlc_packed: &'a str,
    /// The schema_v the record claims.
    pub schema_v: i64,
    /// The full record in the SyncRow wire format (JSON).
    pub payload: &'a str,
    /// Why it was parked (a stable prefix, see
    /// [`QuarantineStore::is_replayable`]).
    pub reason: &'a str,
    /// Wall time (epoch ms) of the park.
    pub quarantined_at: i64,
}

/// Persistence for the LOCAL-ONLY `sync_quarantine` parking lot: the NEWEST
/// (by HLC) rejected remote state per (table, id) — see `SyncQuarantineRow`
/// for the schema and the remote_hlc_packed naming rationale. A row has up
/// to TWO entries: the door's, keyed by its id ([`QuarantineStore::put`]),
/// and the FK sweep's, keyed by [`QuarantineStore::sweep_slot_id`]
/// ([`QuarantineStore::put_swept`]) — two replay obligations that must never
/// displace each other.
///
/// Like `SyncStateStore`, every method is a plain statement so it joins a
/// caller-provided transaction: the sync applier persists a quarantine
/// inside the SAME outer transaction as the batch apply, keeping the parking
/// lot exactly consistent with what actually committed.
#[derive(Debug, Clone)]
pub struct QuarantineStore {
    db: Arc<AppDatabase>,
}

/// Reason prefixes of parks whose payload can NEVER re-apply: their gate's
/// verdict is a pure function of the payload alone, so no app upgrade, clock
/// recovery, or later delivery ever changes it — the replay re-parks them
/// every cycle, forever. Deliberately NOT listed: 'unknown-table',
/// 'unknown-column' and 'missing-column' (their gates re-run against the
/// LIVE schema, which an upgrade changes), 'hlc-drift' (a recovered clock
/// un-parks it), 'fk-orphan' (a delivered parent un-parks it), and
/// 'apply-failed' (a savepoint failure may be transient, and the
/// newer-schema gate parks under this prefix — an upgrade un-parks it; the
/// applier keeps the class honest by parking DETERMINISTIC savepoint
/// failures — CHECK / NOT NULL violations, pure functions of the payload —
/// under 'invalid-value:' instead, so a forever-failing entry can never sit
/// in a replayable class and displace a replayable park). The prefixes are
/// the sync applier's reason vocabulary.
const UNREPLAYABLE_PREFIXES: [&str; 4] = [
    "invalid-schema_v:",
    "invalid-id:",
    "invalid-value:",
    "dead-letter(",
];

const SWEEP_SLOT_SUFFIX: &str = "#fk-sweep";

const ORDER: &str = "ORDER BY \"quarantined_at\" ASC, \"tbl\" ASC, \"id\" ASC";

impl QuarantineStore {
    /// A store over [db].
    pub fn new(db: Arc<AppDatabase>) -> Self {
        QuarantineStore { db }
    }

    /// Upserts the quarantined state for (table, id), keeping the NEWEST
    /// state per row by hlc_packed — the store itself is Last-Writer-Wins,
    /// not last-caller-wins. The composite primary key already bounds the
    /// lot to one entry per key; the guard decides WHICH state that entry
    /// holds:
    ///
    ///  * an EQUAL hlc_packed always replaces the stored entry (a refreshed
    ///    reason/payload for the SAME record state — the fk-orphan re-park
    ///    flow re-parks the identical snapshot every replay cycle and must
    ///    keep working, and a replay whose verdict changed relabels the
    ///    entry); a re-park identical word for word is a no-op, not a
    ///    rewrite;
    ///  * between two states of the same replay class (see below), the
    ///    strictly newer hlc_packed wins and a strictly OLDER park is a
    ///    deliberate no-op. Parks are not ordered by arrival: a page can
    ///    legally deliver P@1000 (parked at the door) before an older P@950
    ///    that parks too, and the park+restore can put a live row BACK on an
    ///    older HLC that a later sweep then parks — so ordering alone can
    ///    never protect the newest state. Dropping the older park loses
    ///    nothing: under LWW the replay of the newer parked snapshot
    ///    supersedes anything the older state could have contributed. That
    ///    holds between two REFUSED states — neither has been vetted, and
    ///    the newest is the one LWW finally keeps — but not against the FK
    ///    sweep's park, a state that already passed every gate, which is why
    ///    the sweep parks under a key of its own (see
    ///    [`QuarantineStore::put_swept`]);
    ///  * ACROSS replay classes the stamp order does not decide: a state
    ///    whose gate can never pass supersedes nothing — the replay
    ///    re-rejects it every cycle, forever — so it must neither displace
    ///    nor block a replayable state, whatever the stamps say. Every
    ///    replayable park is the ONLY copy of its state (the change token is
    ///    already past the record's page, so nothing re-delivers it):
    ///    letting a newer-stamped but permanently dead payload hold the slot
    ///    turned that park into silent loss. The dropped garbage is only
    ///    evidence of a non-conforming writer; the dropped state would have
    ///    been real data.
    ///
    /// The SQL-level `ON CONFLICT ... DO UPDATE ... WHERE` makes the guard
    /// race-free inside the caller's transaction (no read-modify-write gap),
    /// and a filtered-out update is silently kept as the old row, not an
    /// error. The stored side's class is judged with LIKE, exactly as drift
    /// expressed it.
    pub fn put(&self, e: &QuarantineEntry<'_>) -> Result<()> {
        // The retention rule, expressed over the stored row (packed HLCs
        // compare lexically): within a replay class the stored state must
        // not be newer than the incoming one; across classes the replayable
        // state wins outright, and an equal stamp (same record state) always
        // refreshes — unless the re-park is the SAME entry word for word
        // (every replay cycle of a row its gate still refuses), which
        // rewrites nothing and keeps the time the entry was first parked.
        let equal = "(\"sync_quarantine\".\"remote_hlc_packed\" = ?3)";
        let not_newer = "(\"sync_quarantine\".\"remote_hlc_packed\" <= ?3)";
        let stored_unreplayable = format!(
            "({})",
            (0..UNREPLAYABLE_PREFIXES.len())
                .map(|i| format!("\"sync_quarantine\".\"reason\" LIKE ?{}", 8 + i))
                .collect::<Vec<_>>()
                .join(" OR ")
        );
        let unchanged = format!(
            "({equal} AND \"sync_quarantine\".\"payload\" = ?5 \
             AND \"sync_quarantine\".\"reason\" = ?6)"
        );
        let keep = if Self::is_replayable(e.reason) {
            format!("({not_newer} OR {stored_unreplayable})")
        } else {
            format!("({equal} OR ({not_newer} AND {stored_unreplayable}))")
        };
        let sql = format!(
            "INSERT INTO \"sync_quarantine\" (\"tbl\", \"id\", \"remote_hlc_packed\", \
             \"schema_v\", \"payload\", \"reason\", \"quarantined_at\") \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(\"tbl\", \"id\") DO UPDATE SET \"tbl\" = ?1, \"id\" = ?2, \
             \"remote_hlc_packed\" = ?3, \"schema_v\" = ?4, \"payload\" = ?5, \
             \"reason\" = ?6, \"quarantined_at\" = ?7 \
             WHERE {keep} AND NOT {unchanged}"
        );
        let patterns: Vec<String> = UNREPLAYABLE_PREFIXES
            .iter()
            .map(|p| format!("{p}%"))
            .collect();
        self.db.execute(
            &sql,
            rusqlite::params![
                e.table,
                e.id,
                e.hlc_packed,
                e.schema_v,
                e.payload,
                e.reason,
                e.quarantined_at,
                patterns[0],
                patterns[1],
                patterns[2],
                patterns[3],
            ],
        )?;
        Ok(())
    }

    /// The parking-lot key of row [id]'s FK-SWEEP slot: the row id plus a
    /// suffix outside the wire id alphabet (SyncRow's id shape admits only
    /// `[0-9A-Za-z_-]`, and the sweep only ever parks rows that passed it),
    /// so no conforming record's door entry can share the key — a hostile id
    /// spelling it parks as 'invalid-id:', which put's class rule never lets
    /// displace the replayable sweep park. The payload still carries the
    /// real id: the replay decodes it, never this key.
    pub fn sweep_slot_id(id: &str) -> String {
        format!("{id}{SWEEP_SLOT_SUFFIX}")
    }

    /// The SQL twin of a row's keys: a predicate over the `sync_quarantine`
    /// row aliased [alias], TRUE when it is an entry OF the row whose id is
    /// the SQL expression [id_expr] — its door entry or its sweep slot. A
    /// reader asking "is a newer state of this row parked?" must match both:
    /// the sweep's park is often exactly that state (an update parked and
    /// restored over, see [`QuarantineStore::put_swept`]).
    pub fn entry_of_row_sql(alias: &str, id_expr: &str) -> String {
        format!("{alias}.id IN ({id_expr}, {id_expr} || '{SWEEP_SLOT_SUFFIX}')")
    }

    /// Parks the FK sweep's snapshot of row (table, id) in that row's SWEEP
    /// slot ([`QuarantineStore::sweep_slot_id`]) — beside the door's
    /// (table, id) entry, never in competition with it.
    ///
    /// The two hold different replay obligations, and neither can stand in
    /// for the other. A door park is a state some gate REFUSED; it replays
    /// once that gate passes (a recovered clock, an app upgrade). The
    /// sweep's park is a state that passed EVERY gate and applied, rolled
    /// back only because its parent has not arrived yet — the sole surviving
    /// copy of a row the sweep then deleted or restored to its pre-batch
    /// state, due the moment the parent lands. Sharing one slot let them
    /// displace each other by stamp: a drift-parked newer state (a peer's
    /// runaway clock) evicted the sweep's copy — or, parked first on the
    /// same page, filtered the sweep's put out — and once the parent arrived
    /// the replay found only the still-implausible state, so the row stayed
    /// deleted (or stale) for as long as the clock stayed behind, while every
    /// peer that held the parent showed the honest state. put's "the newer
    /// state supersedes it" holds only once the newer state CAN replay, and
    /// a door gate is not the sweep's gate. In separate slots each entry
    /// replays and settles on its own terms, and plain LWW at replay
    /// converges the row whichever lands first.
    ///
    /// Within the slot put's retention applies unchanged (every sweep park
    /// is replayable, so the newest wins and an equal stamp refreshes the
    /// re-park cycle). A door entry holding the SAME state (equal stamp) is
    /// folded away: that state is now accounted for here under its current
    /// verdict — the relabel a door park whose replay applied and was then
    /// swept used to get from the shared slot, and the path by which an
    /// fk-orphan entry parked under the bare row id by an older build moves
    /// over on its first replay.
    pub fn put_swept(&self, e: &QuarantineEntry<'_>) -> Result<()> {
        self.db.transaction(|| {
            let slot = Self::sweep_slot_id(e.id);
            self.put(&QuarantineEntry { id: &slot, ..*e })?;
            self.db.execute(
                "DELETE FROM \"sync_quarantine\" WHERE \"tbl\" = ?1 AND \"id\" = ?2 \
                 AND \"remote_hlc_packed\" = ?3",
                [e.table, e.id, e.hlc_packed],
            )?;
            Ok(())
        })
    }

    /// Whether a park under [reason] can ever re-apply — the replay class
    /// this store's retention rule keys on. Public so the applier classifies
    /// its own parks by the SAME rule (a replayable park's stamp must
    /// ratchet the clock).
    pub fn is_replayable(reason: &str) -> bool {
        !UNREPLAYABLE_PREFIXES.iter().any(|p| reason.starts_with(p))
    }

    /// The SQL twin of [`QuarantineStore::is_replayable`], for queries that
    /// read the parking lot directly (the IntegrityChecker's parked-newer
    /// guard): a predicate over the `sync_quarantine` row aliased [alias],
    /// TRUE when its stored reason is in a replayable class. GLOB rather
    /// than LIKE so it is the same exact, case-sensitive prefix test as
    /// is_replayable (LIKE folds case, and its `_` wildcard matches inside
    /// 'invalid-schema_v:'); the prefixes carry no GLOB metacharacter or
    /// quote, so they inline safely.
    pub fn replayable_sql(alias: &str) -> String {
        UNREPLAYABLE_PREFIXES
            .iter()
            .map(|p| format!("{alias}.reason NOT GLOB '{p}*'"))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// The rows a replay can still land, in [`QuarantineStore::list`]'s
    /// order. The rest only re-park as they were, so a replay cycle leaves
    /// them, payloads and all, unread.
    pub fn list_replayable(&self) -> Result<Vec<SyncQuarantineRow>> {
        self.db.select(
            &format!("{} {ORDER}", Self::replayable_sql("sync_quarantine")),
            [],
        )
    }

    /// All quarantined rows, oldest quarantine first (stable review order).
    pub fn list(&self) -> Result<Vec<SyncQuarantineRow>> {
        self.db.select(&format!("1 {ORDER}"), [])
    }

    /// Removes one parked row (after a successful re-apply or a manual
    /// discard). A missing row is a no-op.
    pub fn delete(&self, table: &str, id: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM \"sync_quarantine\" WHERE \"tbl\" = ?1 AND \"id\" = ?2",
            [table, id],
        )?;
        Ok(())
    }

    /// Removes one parked row ONLY while it still holds EXACTLY the state
    /// that was replayed — the settle-then-delete used by the quarantine
    /// replay, which lists, applies, then deletes. A different state for the
    /// same (table, id) can be parked between the apply and the delete (a
    /// concurrent pull page), and an unconditional delete threw that state
    /// away unreplayed.
    ///
    /// The condition is the stamp AND the payload, never the stamp alone:
    /// put deliberately lets an EQUAL stamp replace the stored entry with a
    /// refreshed reason and payload (the FK-orphan re-park cycle depends on
    /// that), so a stamp match does not prove the entry is still the one
    /// that settled. Both together make this a true compare-and-swap —
    /// anything parked in the window survives for the next replay cycle,
    /// where it settles on its own terms.
    pub fn delete_if_unchanged(
        &self,
        table: &str,
        id: &str,
        hlc_packed: &str,
        payload: &str,
    ) -> Result<()> {
        self.db.execute(
            "DELETE FROM \"sync_quarantine\" WHERE \"tbl\" = ?1 AND \"id\" = ?2 \
             AND \"remote_hlc_packed\" = ?3 AND \"payload\" = ?4",
            [table, id, hlc_packed, payload],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    fn id(s: &str) -> String {
        format!("{s:0<26}")[..26].to_string()
    }

    struct Fixture {
        db: Arc<AppDatabase>,
        store: QuarantineStore,
    }

    fn fixture() -> Fixture {
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        Fixture {
            store: QuarantineStore::new(db.clone()),
            db,
        }
    }

    /// Dart's `put` test helper with its defaults.
    struct Put<'a> {
        hlc_packed: &'a str,
        reason: &'a str,
        payload: &'a str,
        at: i64,
    }

    impl Default for Put<'_> {
        fn default() -> Self {
            Put {
                hlc_packed: "0000000001000-000000-N",
                reason: "hlc-drift",
                payload: "{\"stub\":true}",
                at: 1000,
            }
        }
    }

    impl Fixture {
        fn put(&self, table: &str, seed: &str, p: Put<'_>) {
            self.store
                .put(&QuarantineEntry {
                    table,
                    id: &id(seed),
                    hlc_packed: p.hlc_packed,
                    schema_v: 1,
                    payload: p.payload,
                    reason: p.reason,
                    quarantined_at: p.at,
                })
                .unwrap();
        }

        fn put_swept(&self, seed: &str, hlc_packed: &str, at: i64) {
            self.store
                .put_swept(&QuarantineEntry {
                    table: "tracks",
                    id: &id(seed),
                    hlc_packed,
                    schema_v: 1,
                    payload: "{\"swept\":true}",
                    reason: "fk-orphan: parent practices/P missing",
                    quarantined_at: at,
                })
                .unwrap();
        }

        fn list(&self) -> Vec<SyncQuarantineRow> {
            self.store.list().unwrap()
        }
    }

    #[test]
    fn put_list_delete_round_trip() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                at: 100,
                ..Put::default()
            },
        );
        f.put(
            "tracks",
            "T1",
            Put {
                at: 50,
                ..Put::default()
            },
        );

        let rows = f.list();
        assert_eq!(rows.len(), 2);
        // Oldest quarantine first.
        assert_eq!(rows[0].tbl, "tracks");
        assert_eq!(rows[1].tbl, "practices");
        assert_eq!(rows[1].id, id("P1"));
        assert_eq!(rows[1].schema_v, 1);
        assert_eq!(rows[1].payload, "{\"stub\":true}");

        f.store.delete("practices", &id("P1")).unwrap();
        assert_eq!(f.list().len(), 1);
        assert_eq!(f.list()[0].tbl, "tracks");

        f.store.delete("practices", &id("P1")).unwrap(); // missing row: no-op
    }

    #[test]
    fn list_replayable_holds_exactly_what_is_replayable_says_can_land() {
        let f = fixture();
        let reasons = [
            "hlc-drift: ahead",
            "fk-orphan: tracks",
            "apply-failed: newer schema",
            "unknown-column: x",
            "invalid-schema_v: 0",
            "invalid-id: bad",
            "invalid-value: mistyped",
            "dead-letter(invalid-value: x)",
            // LIKE's `_` would match this; the class is the exact prefix.
            "invalid-schemaXv: 0",
        ];
        for (i, r) in reasons.iter().enumerate() {
            f.put(
                "practices",
                &format!("P{i}"),
                Put {
                    reason: r,
                    at: i as i64,
                    ..Put::default()
                },
            );
        }
        let replayable: Vec<String> = f
            .store
            .list_replayable()
            .unwrap()
            .into_iter()
            .map(|q| q.reason)
            .collect();
        let expected: Vec<String> = reasons
            .iter()
            .filter(|r| QuarantineStore::is_replayable(r))
            .map(|r| r.to_string())
            .collect();
        assert_eq!(replayable, expected);
        assert!(expected.contains(&"invalid-schemaXv: 0".to_string()));
    }

    #[test]
    fn re_quarantining_a_row_keeps_only_the_latest_state() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                reason: "hlc-drift: old",
                at: 100,
                ..Put::default()
            },
        );
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000002000-000000-N",
                reason: "apply-failed: new",
                at: 200,
                ..Put::default()
            },
        );
        let rows = f.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, "apply-failed: new");
        assert_eq!(rows[0].remote_hlc_packed, "0000000002000-000000-N");
        assert_eq!(rows[0].quarantined_at, 200);
    }

    #[test]
    fn a_strictly_older_park_is_a_no_op() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000002000-000000-N",
                reason: "hlc-drift: newest state, parked at the door",
                at: 100,
                ..Put::default()
            },
        );
        // The end-of-batch FK sweep can legally park an OLDER db-row state
        // of the same row later in the same page.
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000001000-000000-N",
                reason: "fk-orphan: stale sweep park",
                at: 200,
                ..Put::default()
            },
        );
        let rows = f.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].remote_hlc_packed, "0000000002000-000000-N");
        assert_eq!(
            rows[0].reason,
            "hlc-drift: newest state, parked at the door"
        );
        assert_eq!(rows[0].payload, "{\"stub\":true}");
        assert_eq!(rows[0].quarantined_at, 100, "entirely untouched");
    }

    #[test]
    fn an_equal_hlc_re_park_still_refreshes_reason_and_payload() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                reason: "fk-orphan: first pass",
                at: 100,
                ..Put::default()
            },
        );
        f.put(
            "practices",
            "P1",
            Put {
                reason: "fk-orphan: second pass",
                at: 200,
                ..Put::default()
            },
        );
        let rows = f.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, "fk-orphan: second pass");
        assert_eq!(rows[0].quarantined_at, 200);
    }

    #[test]
    fn an_identical_re_park_rewrites_nothing() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                reason: "fk-orphan: parent missing",
                at: 100,
                ..Put::default()
            },
        );
        f.put(
            "practices",
            "P1",
            Put {
                reason: "fk-orphan: parent missing",
                at: 200,
                ..Put::default()
            },
        );
        let changed: i64 =
            f.db.query_row("SELECT changes() AS n", [], |r| r.get(0))
                .unwrap();
        assert_eq!(changed, 0, "no row was rewritten");
        assert_eq!(f.list()[0].quarantined_at, 100);
    }

    #[test]
    fn a_newer_unreplayable_park_does_not_displace_a_stored_fk_orphan_park() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000001000-000000-N",
                reason: "fk-orphan: parent assets/A missing",
                payload: "{\"honest\":true}",
                at: 100,
            },
        );
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000002000-000000-N",
                reason: "invalid-value: lyrics_json is JSON but not the shape",
                payload: "{\"garbage\":true}",
                at: 200,
            },
        );
        let rows = f.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].remote_hlc_packed, "0000000001000-000000-N");
        assert!(rows[0].reason.starts_with("fk-orphan"));
        assert_eq!(rows[0].payload, "{\"honest\":true}");
    }

    #[test]
    fn an_fk_orphan_park_replaces_stored_unreplayable_garbage_even_when_older() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000002000-000000-N",
                reason: "invalid-value: lyrics_json is JSON but not the shape",
                payload: "{\"garbage\":true}",
                at: 100,
            },
        );
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000001000-000000-N",
                reason: "fk-orphan: parent assets/A missing",
                payload: "{\"honest\":true}",
                at: 200,
            },
        );
        let rows = f.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].remote_hlc_packed, "0000000001000-000000-N");
        assert!(rows[0].reason.starts_with("fk-orphan"));
        assert_eq!(rows[0].payload, "{\"honest\":true}");
    }

    #[test]
    fn between_two_unreplayable_parks_the_newest_still_wins() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000001000-000000-N",
                reason: "invalid-id: separator in id",
                at: 100,
                ..Put::default()
            },
        );
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000002000-000000-N",
                reason: "invalid-value: mistyped column",
                at: 200,
                ..Put::default()
            },
        );
        assert_eq!(f.list()[0].remote_hlc_packed, "0000000002000-000000-N");

        // And the older direction stays a no-op, like within any class.
        f.put(
            "practices",
            "P1",
            Put {
                hlc_packed: "0000000000500-000000-N",
                reason: "invalid-value: older garbage",
                at: 300,
                ..Put::default()
            },
        );
        assert_eq!(f.list()[0].remote_hlc_packed, "0000000002000-000000-N");
    }

    #[test]
    fn an_equal_stamp_replaces_across_classes_too() {
        let f = fixture();
        f.put(
            "practices",
            "P1",
            Put {
                reason: "hlc-drift: 6 minutes ahead",
                at: 100,
                ..Put::default()
            },
        );
        f.put(
            "practices",
            "P1",
            Put {
                reason: "invalid-value: mistyped column",
                at: 200,
                ..Put::default()
            },
        );
        let rows = f.list();
        assert!(rows[0].reason.starts_with("invalid-value"));
        assert_eq!(rows[0].quarantined_at, 200);
    }

    #[test]
    fn a_door_park_neither_displaces_nor_filters_the_sweeps_park_in_either_order() {
        let f = fixture();
        f.put_swept("T1", "0000000001000-000000-N", 100);
        f.put(
            "tracks",
            "T1",
            Put {
                hlc_packed: "0000000009000-000000-N",
                reason: "hlc-drift: 6 minutes ahead",
                at: 200,
                ..Put::default()
            },
        );
        // The other order, on a second row: door first, then the sweep.
        f.put(
            "tracks",
            "T2",
            Put {
                hlc_packed: "0000000009000-000000-N",
                reason: "hlc-drift: 6 minutes ahead",
                at: 300,
                ..Put::default()
            },
        );
        f.put_swept("T2", "0000000001000-000000-N", 400);

        let rows: Vec<(String, String)> = f
            .list()
            .into_iter()
            .map(|q| (q.id, q.remote_hlc_packed))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    QuarantineStore::sweep_slot_id(&id("T1")),
                    "0000000001000-000000-N".to_string()
                ),
                (id("T1"), "0000000009000-000000-N".to_string()),
                (id("T2"), "0000000009000-000000-N".to_string()),
                (
                    QuarantineStore::sweep_slot_id(&id("T2")),
                    "0000000001000-000000-N".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_door_entry_holding_the_same_state_is_folded_into_the_sweep_slot() {
        let f = fixture();
        // A pre-split build parked the sweep's state under the bare row id.
        f.put(
            "tracks",
            "T1",
            Put {
                hlc_packed: "0000000001000-000000-N",
                reason: "fk-orphan: parent practices/P missing",
                at: 100,
                ..Put::default()
            },
        );
        f.put_swept("T1", "0000000001000-000000-N", 200);
        let rows = f.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, QuarantineStore::sweep_slot_id(&id("T1")));
        assert_eq!(rows[0].quarantined_at, 200);
    }

    #[test]
    fn the_same_id_under_two_tables_parks_two_rows() {
        let f = fixture();
        f.put("practices", "X", Put::default());
        f.put("tracks", "X", Put::default());
        assert_eq!(f.list().len(), 2);
    }

    #[test]
    fn writes_join_a_caller_transaction() {
        let f = fixture();
        let failed: Result<()> = f.db.transaction(|| {
            f.put("practices", "P1", Put::default());
            Err(Error::State("outer apply failed".to_string()))
        });
        assert!(failed.is_err());
        assert!(f.list().is_empty());
    }

    #[test]
    fn delete_if_unchanged_settles_only_the_exact_replayed_state() {
        let f = fixture();
        let old = "0000000000100-000000-N";
        f.put(
            "practices",
            "P",
            Put {
                hlc_packed: old,
                ..Put::default()
            },
        );
        f.store
            .delete_if_unchanged("practices", &id("P"), old, "{\"stub\":true}")
            .unwrap();
        assert!(f.list().is_empty(), "the exact state settles");

        f.put(
            "practices",
            "P",
            Put {
                hlc_packed: old,
                ..Put::default()
            },
        );
        // The LWW put replaces the entry with a NEWER state mid-replay…
        f.put(
            "practices",
            "P",
            Put {
                hlc_packed: "0000000000200-000000-N",
                ..Put::default()
            },
        );
        // …and the settle of the OLD stamp must not touch it.
        f.store
            .delete_if_unchanged("practices", &id("P"), old, "{\"stub\":true}")
            .unwrap();
        let left = f.list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].remote_hlc_packed, "0000000000200-000000-N");
    }

    #[test]
    fn an_equal_stamp_with_a_refreshed_payload_also_survives_the_settle() {
        let f = fixture();
        let stamp = "0000000000100-000000-N";
        f.put(
            "practices",
            "P",
            Put {
                hlc_packed: stamp,
                payload: "{\"v\":1}",
                ..Put::default()
            },
        );
        f.put(
            "practices",
            "P",
            Put {
                hlc_packed: stamp,
                payload: "{\"v\":2}",
                ..Put::default()
            },
        );
        f.store
            .delete_if_unchanged("practices", &id("P"), stamp, "{\"v\":1}")
            .unwrap();
        let left = f.list();
        assert_eq!(left.len(), 1, "the re-parked payload must survive");
        assert_eq!(left[0].payload, "{\"v\":2}");
    }

    #[test]
    fn clock_poison_guard_sync_quarantine_never_carries_hlc_packed() {
        let f = fixture();
        let names: Vec<String> =
            f.db.query("PRAGMA table_info(sync_quarantine)", [], |r| r.get(1))
                .unwrap();
        assert!(!names.contains(&"hlc_packed".to_string()));
        assert!(names.contains(&"remote_hlc_packed".to_string()));
    }

    #[test]
    fn the_sql_helpers_spell_the_dart_predicates() {
        assert_eq!(
            QuarantineStore::entry_of_row_sql("q", "c.track_id"),
            "q.id IN (c.track_id, c.track_id || '#fk-sweep')"
        );
        assert_eq!(
            QuarantineStore::replayable_sql("q"),
            "q.reason NOT GLOB 'invalid-schema_v:*' AND q.reason NOT GLOB 'invalid-id:*' \
             AND q.reason NOT GLOB 'invalid-value:*' AND q.reason NOT GLOB 'dead-letter(*'"
        );
        assert_eq!(QuarantineStore::sweep_slot_id("X"), "X#fk-sweep");
    }
}
