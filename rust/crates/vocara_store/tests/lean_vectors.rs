//! Replays the HLC vectors the Lean model computed
//! (`verification/vectors/hlc_*.json`, made by `lake exe vectors`; theorems
//! in `verification/VocaraVerify/Hlc.lean`) against the Rust clock, so the
//! proofs — tick and merge strictly advance, the counter keeps six digits,
//! the packed TEXT form orders as the clock — are about this code.

use std::cmp::Ordering;

use serde_json::Value;
use vocara_store::sync::hlc::{Hlc, HlcClock};

fn vectors(name: &str) -> Vec<Value> {
    let path = format!(
        "{}/../../verification/vectors/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).expect(name)
}

fn hlc(v: &Value) -> Hlc {
    Hlc::new(
        v["wall"].as_i64().unwrap(),
        v["ctr"].as_i64().unwrap(),
        v["node"].as_str().unwrap(),
    )
}

#[test]
fn packed_matches_the_verified_model() {
    let cases = vectors("hlc_packed.json");
    assert!(cases.len() >= 60);
    for c in cases {
        let h = hlc(&c["hlc"]);
        let packed = c["packed"].as_str().unwrap();
        assert_eq!(h.packed(), packed);
        assert_eq!(Hlc::parse(packed).unwrap(), h);
    }
}

#[test]
fn ordering_and_packed_text_order_match_the_verified_model() {
    for c in vectors("hlc_order.json") {
        let (a, b) = (hlc(&c["a"]), hlc(&c["b"]));
        let want = match c["cmp"].as_i64().unwrap() {
            -1 => Ordering::Less,
            0 => Ordering::Equal,
            _ => Ordering::Greater,
        };
        assert_eq!(a.cmp(&b), want, "{a:?} vs {b:?}");
        // The store compares packed stamps as TEXT: the same order.
        assert_eq!(a.packed().cmp(&b.packed()), want);
    }
}

#[test]
fn tick_matches_the_verified_model() {
    for c in vectors("hlc_tick.json") {
        let now = c["now"].as_i64().unwrap();
        let last = hlc(&c["last"]);
        let clock = HlcClock::new(c["node"].as_str().unwrap())
            .with_now(move || now)
            .with_initial(last.clone());
        let got = clock.tick();
        assert_eq!(got, hlc(&c["result"]), "tick from {last:?} at {now}");
        assert!(got > last);
    }
}

#[test]
fn merge_matches_the_verified_model() {
    for c in vectors("hlc_advance.json") {
        let now = c["now"].as_i64().unwrap();
        let (last, remote) = (hlc(&c["last"]), hlc(&c["remote"]));
        let clock = HlcClock::new(c["node"].as_str().unwrap())
            .with_now(move || now)
            .with_initial(last.clone());
        let got = clock.merge_verified(&remote);
        assert_eq!(
            got,
            hlc(&c["result"]),
            "merge {remote:?} into {last:?} at {now}"
        );
        assert!(got > last && got > remote);
    }
}
