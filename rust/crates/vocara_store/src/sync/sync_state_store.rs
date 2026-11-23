//! Typed access to the sync engine's key-value state (port of
//! `lib/data/sync/sync_state_store.dart`).

use std::sync::Arc;

use crate::db::rows::SyncStateRow;
use crate::db::AppDatabase;
use crate::error::Result;

/// Typed access to the LOCAL-ONLY `sync_state` key-value table: the server
/// change token, pagination cursors, and misc sync-engine state.
///
/// Every method is a plain statement on the shared [`AppDatabase`], so it
/// COMPOSES into a caller-provided `db.transaction(...)`. That composability
/// is load-bearing: the change token must commit atomically WITH the batch
/// of remote records it acknowledges — persisting it outside that
/// transaction could acknowledge records the crash rolled back (lost
/// updates) or re-apply a page forever.
#[derive(Debug, Clone)]
pub struct SyncStateStore {
    db: Arc<AppDatabase>,
}

impl SyncStateStore {
    /// The server change token (opaque, transport-encoded string).
    pub const CHANGE_TOKEN_KEY: &'static str = "change_token";

    /// Set (durably, BEFORE the first outbound wire call ever made) once a
    /// sync transport has existed for this library. Its absence is a proof:
    /// no push was ever even attempted, so no server can possibly hold any
    /// of this device's rows — which is what lets the asset GC's
    /// pending-journal guard stand down on never-synced libraries instead of
    /// deferring local disk reclaim forever.
    pub const TRANSPORT_SEEN_KEY: &'static str = "transport_seen";

    /// The highest packed HLC this device has ever ACCEPTED from a peer —
    /// including stamps whose record never became a row.
    ///
    /// The boot clock seed restores monotonicity from the max hlc_packed
    /// held in the tables, which covers every stamp that landed as data. It
    /// does not cover a stamp the device verified and merged into its live
    /// clock but whose record was then parked: an FK-orphan's snapshot lives
    /// on in `sync_quarantine`, whose stamp column is deliberately NOT named
    /// hlc_packed (a quarantine also holds implausibly-future stamps that
    /// must never seed a clock). So the one legitimate high-water mark those
    /// parks contribute survived only in memory, and a restart lost it: a
    /// local edit could then take a LOWER stamp than the parked record's,
    /// and the quarantine replay would silently overwrite the user's newer
    /// edit under LWW. Recording the mark here — from stamps that already
    /// passed the plausibility gate, and only ever upward — closes that
    /// window without letting a suspect stamp anywhere near the clock.
    pub const HLC_HIGH_WATER_KEY: &'static str = "hlc_high_water";

    /// A store over [db].
    pub fn new(db: Arc<AppDatabase>) -> Self {
        SyncStateStore { db }
    }

    /// Whether a sync transport has ever been active for this library.
    pub fn transport_seen(&self) -> Result<bool> {
        Ok(self.get(Self::TRANSPORT_SEEN_KEY)?.is_some())
    }

    /// Records that a transport exists. Must complete BEFORE the first wire
    /// call, so a crash mid-push can never leave a server-held row in a
    /// library that still claims it never had a transport.
    pub fn mark_transport_seen(&self) -> Result<()> {
        self.set(Self::TRANSPORT_SEEN_KEY, "1")
    }

    /// Raises the durable high-water mark to [packed] if it is higher.
    ///
    /// Composes into the caller's transaction like everything else here,
    /// and the guard lives in the SQL so concurrent writers cannot lose an
    /// update through a read-modify-write gap. Packed HLCs are fixed-width
    /// and lexicographically ordered, so a string comparison IS the HLC
    /// ordering.
    pub fn raise_hlc_high_water(&self, packed: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO sync_state (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value \
             WHERE excluded.value > sync_state.value",
            [Self::HLC_HIGH_WATER_KEY, packed],
        )?;
        Ok(())
    }

    /// The durable high-water mark, or None before any remote stamp was
    /// accepted (see [`SyncStateStore::HLC_HIGH_WATER_KEY`]).
    pub fn hlc_high_water(&self) -> Result<Option<String>> {
        self.get(Self::HLC_HIGH_WATER_KEY)
    }

    /// Generic get: None when the key was never set.
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .select::<SyncStateRow>("\"key\" = ?1", [key])?
            .into_iter()
            .next()
            .map(|r| r.value))
    }

    /// Generic upsert.
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.upsert(&SyncStateRow {
            key: key.to_string(),
            value: value.to_string(),
        })
    }

    /// Generic delete (a missing key is a no-op).
    pub fn remove(&self, key: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM \"sync_state\" WHERE \"key\" = ?1", [key])?;
        Ok(())
    }

    /// The server change token.
    pub fn change_token(&self) -> Result<Option<String>> {
        self.get(Self::CHANGE_TOKEN_KEY)
    }

    /// Stores the server change token.
    pub fn set_change_token(&self, token: &str) -> Result<()> {
        self.set(Self::CHANGE_TOKEN_KEY, token)
    }

    /// Drops the token, forcing the next pull to start from scratch (the
    /// server signals an expired token this way).
    pub fn clear_change_token(&self) -> Result<()> {
        self.remove(Self::CHANGE_TOKEN_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::sync::hlc::Hlc;

    fn store() -> (Arc<AppDatabase>, SyncStateStore) {
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        (db.clone(), SyncStateStore::new(db))
    }

    #[test]
    fn a_missing_key_reads_none() {
        let (_db, store) = store();
        assert_eq!(store.get("nope").unwrap(), None);
    }

    #[test]
    fn set_get_overwrite_remove_round_trip() {
        let (_db, store) = store();
        store.set("cursor", "a").unwrap();
        assert_eq!(store.get("cursor").unwrap().as_deref(), Some("a"));

        store.set("cursor", "b").unwrap(); // upsert, not a PK violation
        assert_eq!(store.get("cursor").unwrap().as_deref(), Some("b"));

        store.remove("cursor").unwrap();
        assert_eq!(store.get("cursor").unwrap(), None);

        store.remove("cursor").unwrap(); // removing a missing key is a no-op
    }

    #[test]
    fn typed_change_token_accessors() {
        let (_db, store) = store();
        assert_eq!(store.change_token().unwrap(), None);
        store.set_change_token("tok-1").unwrap();
        assert_eq!(store.change_token().unwrap().as_deref(), Some("tok-1"));
        store.set_change_token("tok-2").unwrap();
        assert_eq!(store.change_token().unwrap().as_deref(), Some("tok-2"));
        store.clear_change_token().unwrap();
        assert_eq!(store.change_token().unwrap(), None);
    }

    #[test]
    fn writes_join_a_caller_transaction_and_roll_back_with_it() {
        let (db, store) = store();
        store.set_change_token("before").unwrap();
        let failed: Result<()> = db.transaction(|| {
            // The apply-loop shape: mutate rows, then persist the token — if
            // the commit fails, acknowledging the page would lose those
            // records.
            store.set_change_token("after")?;
            Err(Error::State("apply failed".to_string()))
        });
        assert!(failed.is_err());
        assert_eq!(store.change_token().unwrap().as_deref(), Some("before"));
    }

    #[test]
    fn writes_inside_a_committed_caller_transaction_persist() {
        let (db, store) = store();
        db.transaction(|| -> Result<()> {
            store.set_change_token("committed")?;
            store.set("cursor", "page-9")
        })
        .unwrap();
        assert_eq!(store.change_token().unwrap().as_deref(), Some("committed"));
        assert_eq!(store.get("cursor").unwrap().as_deref(), Some("page-9"));
    }

    #[test]
    fn transport_seen_is_absent_until_marked() {
        let (_db, store) = store();
        assert!(!store.transport_seen().unwrap());
        store.mark_transport_seen().unwrap();
        assert!(store.transport_seen().unwrap());
        assert_eq!(
            store
                .get(SyncStateStore::TRANSPORT_SEEN_KEY)
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn the_durable_high_water_mark_only_ever_moves_up() {
        // From persistence_boot_safety_test.dart's boot clock seed group.
        let (_db, store) = store();
        let node = format!("{:0<26}", "REMOTE");
        let high = Hlc::new(1_800_000_120_000, 0, node.clone()).packed();
        let low = Hlc::new(1_800_000_001_000, 0, node).packed();
        assert_eq!(store.hlc_high_water().unwrap(), None);
        store.raise_hlc_high_water(&high).unwrap();
        store.raise_hlc_high_water(&low).unwrap();
        assert_eq!(store.hlc_high_water().unwrap(), Some(high));
    }
}
