//! Replays the reconcile vectors the Lean model computed
//! (`verification/vectors/reconcile.json`, theorems in
//! `verification/VocaraVerify/Reconcile.lean`) through the real
//! `reconcile_collection`, so the proofs are about this code: after a save a
//! row holds the editor's version if the editor changed it since load, else
//! the persisted one, and a write is emitted exactly when that changes the
//! row.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use vocara_store::db::schema::AppDatabase;
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::reconcile::{
    reconcile_collection, reconcile_op_for, ReconcileCollection, ReconcileOp,
};
use vocara_store::sync::sync_writer::SyncWriter;

fn vectors() -> Vec<Value> {
    let path = format!(
        "{}/../../verification/vectors/reconcile.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).expect("reconcile.json")
}

/// A side of the model as a content hash (`null` = absent).
fn side(v: &Value) -> Option<String> {
    v.as_u64().map(|n| format!("h{n}"))
}

#[test]
fn reconcile_matches_the_verified_model() {
    let cases = vectors();
    assert!(cases.len() >= 80);
    let mut seen_ops = std::collections::BTreeSet::new();
    for case in cases {
        let rows = case.as_array().unwrap();
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        let clock = Arc::new(HlcClock::new(format!("{:0<26}", "NODE")).with_now(|| 1_000));
        let writer = SyncWriter::new(db, clock);

        let mut base = IndexMap::new();
        let mut current = IndexMap::new();
        let mut desired: Vec<(String, String)> = Vec::new();
        // The persisted rows the callbacks write: id -> hash (absent = not
        // live).
        let state: RefCell<BTreeMap<String, String>> = RefCell::new(BTreeMap::new());
        for r in rows {
            let id = r["id"].as_str().unwrap().to_string();
            if let Some(h) = side(&r["base"]) {
                base.insert(id.clone(), h);
            }
            if let Some(h) = side(&r["db"]) {
                current.insert(id.clone(), h.clone());
                state.borrow_mut().insert(id.clone(), h);
            }
            if let Some(h) = side(&r["mine"]) {
                desired.push((id.clone(), h));
            }
        }

        // Each desired row's op, as `reconcile_op_for` decides it.
        for r in rows {
            let Some(mine) = side(&r["mine"]) else {
                continue;
            };
            let op = reconcile_op_for(
                &mine,
                side(&r["db"]).as_deref(),
                side(&r["base"]).as_deref(),
            );
            let name = match op {
                ReconcileOp::Insert => "insert",
                ReconcileOp::Update => "update",
                ReconcileOp::Skip => "skip",
                ReconcileOp::Unchanged => "unchanged",
            };
            assert_eq!(Some(name), r["op"].as_str(), "{r}");
            seen_ops.insert(name);
        }

        let (mut inserted, mut updated, mut tombstoned) = (0u32, 0u32, 0u32);
        let result = reconcile_collection(ReconcileCollection {
            writer: &writer,
            table: "practice_notes",
            base_hashes: &base,
            current_hashes: &current,
            desired: &desired,
            id_of: &|item: &(String, String)| item.0.clone(),
            hash_of: &|item: &(String, String)| Ok(item.1.clone()),
            insert: &mut |item, _| {
                inserted += 1;
                state.borrow_mut().insert(item.0.clone(), item.1.clone());
                Ok(())
            },
            update: &mut |item, _| {
                updated += 1;
                state.borrow_mut().insert(item.0.clone(), item.1.clone());
                Ok(())
            },
            tombstone: &mut |id, _| {
                tombstoned += 1;
                state.borrow_mut().remove(id);
                Ok(())
            },
        })
        .unwrap();

        let count = |op: &str| rows.iter().filter(|r| r["op"].as_str() == Some(op)).count() as u32;
        assert_eq!(result.inserts, count("insert"));
        assert_eq!(result.updates, count("update"));
        assert_eq!(result.tombstones, count("tombstone"));
        assert_eq!(result.skips, count("skip") + count("keep"));
        assert_eq!(
            (inserted, updated, tombstoned),
            (result.inserts, result.updates, result.tombstones)
        );
        if count("tombstone") > 0 {
            seen_ops.insert("tombstone");
        }
        if count("keep") > 0 {
            seen_ops.insert("keep");
        }

        // The rows as persisted after the pass: `after` of the model.
        let state = state.borrow();
        for r in rows {
            let id = r["id"].as_str().unwrap();
            assert_eq!(state.get(id).cloned(), side(&r["after"]), "{r}");
        }
    }
    // The vectors reach every rule.
    assert_eq!(
        seen_ops.into_iter().collect::<Vec<_>>(),
        ["insert", "keep", "skip", "tombstone", "unchanged", "update"]
    );
}
