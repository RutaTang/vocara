//! Replays the ring traces the Lean model computed
//! (`verification/vectors/ring.json`; theorems in
//! `verification/VocaraVerify/Ring.lean`) against the engine's real SPSC ring:
//! every push must succeed or refuse, and every pop return, exactly as the
//! verified FIFO model does.

use serde_json::Value;
use vocara_engine::ring::spsc;

#[test]
fn the_engine_ring_matches_the_verified_fifo_model() {
    let path = format!(
        "{}/../../verification/vectors/ring.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let runs: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(runs.len(), 3);
    for run in runs {
        let cap = run["capacity"].as_u64().unwrap() as usize;
        let (mut prod, mut cons) = spsc::<u32>(cap);
        assert_eq!(prod.capacity(), cap);
        for op in run["ops"].as_array().unwrap() {
            if let Some(v) = op.get("push") {
                let v = v.as_u64().unwrap() as u32;
                assert_eq!(
                    prod.push(v),
                    op["ok"].as_bool().unwrap(),
                    "push {v} at cap {cap}"
                );
            } else {
                let expected = op["pop"].as_u64().map(|v| v as u32);
                assert_eq!(cons.pop(), expected, "pop at cap {cap}");
            }
        }
    }
}
