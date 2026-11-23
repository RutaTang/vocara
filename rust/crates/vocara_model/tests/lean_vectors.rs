//! Replays the vectors the Lean model computed (`verification/vectors`, made
//! by `lake exe vectors`) against the Rust code, so the theorems in
//! `verification/VocaraVerify/Ulid.lean` are about this encoder.

use serde_json::Value;
use vocara_model::util::id;

fn vectors(name: &str) -> Vec<Value> {
    let path = format!(
        "{}/../../verification/vectors/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str::<Vec<Value>>(&text).expect(name)
}

#[test]
fn the_ulid_encoder_matches_the_verified_model() {
    let cases = vectors("ulid.json");
    assert!(cases.len() > 40);
    for case in cases {
        let value: u128 = case["value"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            id::encode(value),
            case["encoded"].as_str().unwrap(),
            "{value}"
        );
    }
}
