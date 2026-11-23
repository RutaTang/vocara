//! Replays the LRU vectors the Lean model computed
//! (`verification/vectors/lru.json`, theorems in
//! `verification/VocaraVerify/Lru.lean`) through the real `LruBytesCache`:
//! after every insert and read, the cache holds exactly the keys the model
//! holds, in the same recency order — so the proofs (the budget fits or only
//! the inserted key is left, the inserted key is never evicted, eviction
//! takes the least recently used first, nothing goes while the budget fits)
//! are about this code.
//!
//! Each case runs twice: through one cache with a budget of its own, and
//! through three caches SHARING one budget (each key lives in cache
//! `key % 3`). The model's list is then the budget's global recency order, so
//! the second run checks the cross-cache victim choice too.

use serde_json::Value;
use vocara_model::util::lru_bytes_cache::{LruBytesBudget, LruBytesCache};

const SHARDS: u64 = 3;

fn vectors() -> Vec<Value> {
    let path = format!(
        "{}/../../verification/vectors/lru.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).expect("lru.json")
}

fn keys_of(v: &Value) -> Vec<u64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_u64().unwrap())
        .collect()
}

/// Replays every case through `caches` (made fresh per case by `make`), key
/// `k` going to cache `k % caches.len()`, and returns how many inserts
/// evicted something.
fn replay(make: impl Fn(usize) -> Vec<LruBytesCache<usize>>) -> usize {
    let cases = vectors();
    assert!(cases.len() >= 40);
    let mut evictions = 0usize;
    for case in cases {
        let max = case["max"].as_u64().unwrap() as usize;
        // The value is its own byte count.
        let caches = make(max);
        let shard = |k: u64| &caches[(k % caches.len() as u64) as usize];
        let held = |caches: &[LruBytesCache<usize>]| -> Vec<Vec<String>> {
            caches.iter().map(|c| c.keys()).collect()
        };
        let expect = |model: &[u64]| -> Vec<Vec<String>> {
            (0..caches.len() as u64)
                .map(|i| {
                    model
                        .iter()
                        .filter(|k| *k % caches.len() as u64 == i)
                        .map(|k| k.to_string())
                        .collect()
                })
                .collect()
        };
        let mut before: Vec<u64> = Vec::new();
        for step in case["steps"].as_array().unwrap() {
            let op = &step["op"];
            let model = keys_of(&step["keys"]);
            if let Some(k) = op.get("put").and_then(Value::as_u64) {
                let bytes = op["bytes"].as_u64().unwrap() as usize;
                shard(k).insert(k.to_string(), bytes);
                let unevicted: Vec<u64> = before
                    .iter()
                    .copied()
                    .filter(|x| *x != k)
                    .chain(std::iter::once(k))
                    .collect();
                if model != unevicted {
                    evictions += 1;
                }
            } else {
                let k = op["get"].as_u64().unwrap();
                let _ = shard(k).get(&k.to_string());
            }
            assert_eq!(held(&caches), expect(&model), "max {max}, after {op}");
            before = model;
        }
    }
    evictions
}

#[test]
fn the_cache_evicts_as_the_verified_model() {
    let evictions = replay(|max| vec![LruBytesCache::new(max, |bytes: &usize| *bytes)]);
    assert!(
        evictions > 100,
        "the vectors exercise eviction ({evictions})"
    );
}

#[test]
fn caches_sharing_a_budget_evict_as_the_verified_model() {
    replay(|max| {
        let budget = LruBytesBudget::new(max);
        (0..SHARDS)
            .map(|_| LruBytesCache::shared(&budget, |bytes: &usize| *bytes))
            .collect()
    });
}
