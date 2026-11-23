//! Replays the oplog vectors the Lean model computed
//! (`verification/vectors/oplog.json`, theorems in
//! `verification/VocaraVerify/Oplog.lean`) through the real ledger: entries
//! journaled by `SyncWriter::journal`, read by `OplogDrain::read_pending`,
//! acked by `OplogDrain::ack` and compacted by `Persistence::compact_oplog`.
//! Every read must collapse to the rows, sequences and resolved asset
//! dependencies the model gives, and every ack and compaction must leave
//! exactly the model's ledger, carried dependencies included. So the proofs
//! (an ack or a compaction never changes what a read resolves a surviving
//! row's gate to) are about this SQL.

use std::fs;

use serde_json::Value;
use vocara_services::persistence::Persistence;
use vocara_store::sync::oplog_drain::OplogDrain;

fn vectors() -> Vec<Value> {
    let path = format!(
        "{}/../../verification/vectors/oplog.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).expect("oplog.json")
}

fn pad(s: String) -> String {
    format!("{s:0<26}")
}

fn row_id(row: u64) -> String {
    pad(format!("ROW{row}"))
}

fn asset_id(asset: u64) -> String {
    pad(format!("ASSET{asset}"))
}

/// An entry as the vectors write it: (seq, row, tombstone, dependency).
type Entry = (i64, String, bool, Option<String>);

fn entry_of(v: &Value) -> Entry {
    (
        v["seq"].as_i64().unwrap(),
        row_id(v["row"].as_u64().unwrap()),
        v["tomb"].as_bool().unwrap(),
        v["dep"].as_u64().map(asset_id),
    )
}

fn ledger(p: &Persistence) -> Vec<Entry> {
    p.db()
        .query(
            "SELECT seq, entity_id, op, depends_on_asset_id FROM oplog ORDER BY seq",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)? == "tombstone",
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .unwrap()
}

#[test]
fn the_ledger_carries_and_collapses_as_the_verified_model() {
    let cases = vectors();
    assert!(cases.len() >= 60);
    let (mut reads, mut acks, mut compactions) = (0, 0, 0);
    for case in cases {
        let p = Persistence::memory().unwrap();
        let root = p.assets().root().to_path_buf();
        let drain = OplogDrain::new(p.db().clone());
        for step in case.as_array().unwrap() {
            if let Some(append) = step.get("append") {
                let (seq, id, tomb, dep) = entry_of(append);
                let hlc = p.writer().stamp().hlc;
                let (op, payload) = if tomb {
                    ("tombstone", "")
                } else {
                    ("update", "h")
                };
                p.db()
                    .transaction(|| {
                        p.writer()
                            .journal("clips", &id, op, &hlc, payload, dep.as_deref())
                    })
                    .unwrap();
                assert_eq!(ledger(&p).last().map(|e| e.0), Some(seq), "{step}");
            } else if let Some(m) = step.get("read") {
                let batch = drain.read_pending(m.as_i64().unwrap()).unwrap();
                let got: Vec<Entry> = batch
                    .entries
                    .iter()
                    .map(|e| {
                        (
                            e.seq,
                            e.id.clone(),
                            e.op == "tombstone",
                            e.depends_on_asset_id.clone(),
                        )
                    })
                    .collect();
                let want: Vec<Entry> = step["expect"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(entry_of)
                    .collect();
                assert_eq!(got, want, "{step}");
                reads += 1;
            } else {
                if let Some(w) = step.get("ack") {
                    drain.ack(w.as_i64().unwrap()).unwrap();
                    acks += 1;
                } else {
                    p.compact_oplog().unwrap();
                    compactions += 1;
                }
                let want: Vec<Entry> = step["ledger"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(entry_of)
                    .collect();
                assert_eq!(ledger(&p), want, "{step}");
            }
        }
        drop(drain);
        drop(p);
        let _ = fs::remove_dir_all(&root);
    }
    assert!(reads > 300 && acks > 200 && compactions > 50);
}
