//! Replays the entity-level LWW vectors the Lean model computed
//! (`verification/vectors/lww.json`, made by `lake exe vectors`; theorems in
//! `verification/VocaraVerify/Lww.lean`) through the Rust sync applier, so
//! the convergence proof — an incoming record replaces the held row only
//! when its packed stamp is strictly greater, a tie keeps the held row — is
//! about the applier that ships.
//!
//! Each case: the held row is applied to an empty table, then every
//! delivery goes through `SyncApplier::apply` on the same table, in order;
//! the row read back must carry exactly the vector's result stamp and
//! payload. The payload rides in `practices.title`, the stamp in the row's
//! HLC quad and packed key. The deliveries are replayed twice: one batch per
//! delivery, and all of them in one batch (the applier settles LWW per
//! record inside a batch as well).

use std::sync::Arc;

use serde_json::Value;
use vocara_store::db::AppDatabase;
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_row::{FieldValue, Fields, SyncRow, SyncRowBuilder};

const ROW_ID: &str = "P0000000000000000000000000";

fn vectors() -> Vec<Value> {
    let path = format!(
        "{}/../../verification/vectors/lww.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).expect("lww.json")
}

fn stamp(v: &Value) -> Hlc {
    Hlc::new(
        v["wall"].as_i64().unwrap(),
        v["ctr"].as_i64().unwrap(),
        v["node"].as_str().unwrap(),
    )
}

/// A full practices row carrying [version]'s payload as its title, stamped
/// with [version]'s stamp.
fn record(version: &Value) -> SyncRow {
    let h = stamp(&version["stamp"]);
    let title = version["payload"].as_str().unwrap();
    let mut f = Fields::new();
    let mut put = |k: &str, v: FieldValue| {
        f.insert(k.to_string(), v);
    };
    put("id", ROW_ID.into());
    put("title", title.into());
    put("timeline_duration_ms", 0i64.into());
    put("master_volume", 1.0.into());
    put("active_interpretation_id", FieldValue::Null);
    put("separation_asset_id", FieldValue::Null);
    put("reverb_json", FieldValue::Null);
    put("lyrics_json", FieldValue::Null);
    put("created_at", 0i64.into());
    put("updated_at", 0i64.into());
    put("deleted_at", FieldValue::Null);
    put("hlc_wall", h.wall_ms.into());
    put("hlc_ctr", h.counter.into());
    put("hlc_node", h.node.clone().into());
    put("hlc_packed", h.packed().into());
    put("schema_v", 1i64.into());
    SyncRow::new("practices", f)
}

/// A fresh library and an applier whose clock reads the top of the 13-digit
/// wall range, so no vector stamp trips the drift gate (the gate is not
/// what the LWW model is about).
fn fresh() -> (Arc<AppDatabase>, SyncApplier) {
    let db = Arc::new(AppDatabase::open_in_memory().unwrap());
    let clock =
        Arc::new(HlcClock::new("LWWREPLAYDEVICE00000000000").with_now(|| 9_999_999_999_999));
    let applier = SyncApplier::new(
        db.clone(),
        clock,
        Arc::new(SyncRowBuilder::new(db.clone())),
        None,
    );
    (db, applier)
}

/// The held row after the replay: (wall, ctr, node, packed, title).
fn held(db: &AppDatabase) -> (i64, i64, String, String, String) {
    db.query_row(
        "SELECT hlc_wall, hlc_ctr, hlc_node, hlc_packed, title FROM practices WHERE id = ?",
        [ROW_ID],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .unwrap()
}

fn expect(case: &Value, db: &AppDatabase, how: &str) {
    let want = stamp(&case["result"]["stamp"]);
    let payload = case["result"]["payload"].as_str().unwrap();
    assert_eq!(
        held(db),
        (
            want.wall_ms,
            want.counter,
            want.node.clone(),
            want.packed(),
            payload.to_string()
        ),
        "{how}: {case}"
    );
}

#[test]
fn the_applier_s_lww_matches_the_verified_model() {
    let cases = vectors();
    assert!(cases.len() >= 40);
    for case in &cases {
        let deliveries: Vec<SyncRow> = case["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .map(record)
            .collect();

        // One batch per delivery.
        let (db, applier) = fresh();
        let seeded = applier.apply(&[record(&case["held"])]);
        assert_eq!(seeded.applied, 1, "{case}");
        for delivery in &deliveries {
            let res = applier.apply(std::slice::from_ref(delivery));
            assert!(res.committed);
            assert_eq!(res.applied + res.skipped, 1, "{case}");
        }
        expect(case, &db, "one batch per delivery");

        // Every delivery in one batch.
        let (db, applier) = fresh();
        applier.apply(&[record(&case["held"])]);
        let res = applier.apply(&deliveries);
        assert!(res.committed);
        assert_eq!(res.applied + res.skipped, deliveries.len(), "{case}");
        expect(case, &db, "one batch");
    }
}

#[test]
fn a_tie_keeps_the_held_row() {
    // The model's tie rule, pinned directly: an equal packed stamp with a
    // different payload is a clean skip.
    let (db, applier) = fresh();
    let v = |payload: &str| serde_json::json!({"stamp": {"wall": 5, "ctr": 1, "node": "A"}, "payload": payload});
    applier.apply(&[record(&v("held"))]);
    let res = applier.apply(&[record(&v("incoming"))]);
    assert_eq!(res.skipped, 1);
    assert_eq!(held(&db).4, "held");
}
