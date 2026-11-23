//! Replays the vectors the Lean models computed (`verification/vectors/*.json`,
//! made by `lake exe vectors`) against the Rust code, so the proofs are about
//! this code:
//!
//! * `hlc_*.json` (`VocaraVerify/Hlc.lean`) against the clock — tick and
//!   merge strictly advance, the counter keeps six digits, the packed TEXT
//!   form orders as the clock;
//! * `canonical.json` (`VocaraVerify/Canonical.lean`) against
//!   `SyncWriter::content_hash_of` — a map hashes with its keys in the
//!   model's sorted order (Dart's UTF-16 code-unit order), so two maps with
//!   the same entries hash the same whatever their insertion order.

use std::cmp::Ordering;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use vocara_model::util::dart_json;
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::sync_writer::{ContentValue, SyncWriter};

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

/// A vector's entries: `[[utf16 code units], value]` pairs.
fn entries(v: &Value) -> Vec<(String, i64)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let units: Vec<u16> = e[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| u16::try_from(u.as_u64().unwrap()).unwrap())
                .collect();
            (String::from_utf16(&units).unwrap(), e[1].as_i64().unwrap())
        })
        .collect()
}

fn content_map(es: &[(String, i64)]) -> ContentValue {
    ContentValue::Map(
        es.iter()
            .map(|(k, v)| (k.clone(), ContentValue::Int(*v)))
            .collect(),
    )
}

#[test]
fn content_hashes_use_the_verified_canonical_order() {
    let cases = vectors("canonical.json");
    assert!(cases.len() >= 60);
    let mut astral_vs_bmp = false;
    for c in cases {
        let input = entries(&c["input"]);
        let canon = entries(&c["canon"]);
        // What Dart hashes: the row's columns as a JSON array, the map's keys
        // in the model's canonical order.
        let mut object = Map::new();
        for (k, v) in &canon {
            object.insert(k.clone(), Value::from(*v));
        }
        let text = dart_json::encode(&Value::Array(vec![Value::Object(object)])).unwrap();
        let expected = hex::encode(Sha256::digest(text.as_bytes()));
        assert_eq!(
            SyncWriter::content_hash_of(&[content_map(&input)]).unwrap(),
            expected,
            "{input:?}"
        );
        // Same entries in another insertion order: same hash
        // (`canon_eq_of_same_entries`).
        let mut reversed = input.clone();
        reversed.reverse();
        assert_eq!(
            SyncWriter::content_hash_of(&[content_map(&reversed)]).unwrap(),
            expected
        );
        // The vectors reach the case where UTF-16 order and scalar order
        // disagree: a surrogate pair sorting before U+FF61.
        astral_vs_bmp |= canon.windows(2).any(|w| {
            w[0].0.chars().next().is_some_and(|c| c > '\u{FFFF}') && w[1].0.starts_with('\u{FF61}')
        });
    }
    assert!(astral_vs_bmp, "no case orders an astral key before U+FF61");
}
