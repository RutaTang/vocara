//! Every line of `golden/dart_json_encode.txt` is Dart's `jsonEncode` output
//! (written by `golden/dart_json_encode.gen.dart` on the Dart SDK): parsing a
//! line and encoding it again must give the line back, byte for byte.

use vocara_model::util::dart_json;

#[test]
fn rust_encodes_every_golden_value_exactly_as_dart() {
    let golden = include_str!("golden/dart_json_encode.txt");
    let mut checked = 0;
    for line in golden.lines().filter(|l| !l.is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line).expect(line);
        assert_eq!(dart_json::encode(&value).unwrap(), line);
        checked += 1;
    }
    assert!(checked > 350, "only {checked} golden lines");
}
