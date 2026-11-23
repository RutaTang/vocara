//! Every line of `golden/lyrics_format.txt` is what the Flutter app's
//! `lyrics_format.dart` made of a tricky lyrics text (written by
//! `golden/lyrics_format.gen.dart` on the Dart SDK): the Rust matchers,
//! written by hand where Dart used regular expressions, must agree on every
//! one.

use serde_json::{json, Value};
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::util::lyrics_format::*;

fn lines_json(lines: &[LyricLine]) -> Value {
    Value::Array(lines.iter().map(|l| json!([l.text, l.time_ms])).collect())
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn rust_lyrics_parsing_matches_dart_on_every_golden_text() {
    let golden = include_str!("golden/lyrics_format.txt");
    let mut checked = 0;
    for line in golden.lines().filter(|l| !l.is_empty()) {
        let j: Value = serde_json::from_str(line).unwrap();
        if let Some(input) = j.get("input").and_then(Value::as_str) {
            assert_eq!(looks_like_lrc(input), j["looksLikeLrc"], "{input:?}");
            assert_eq!(is_lrc_document(input), j["isLrcDocument"], "{input:?}");
            let plain = parse_plain_lyrics(input);
            assert_eq!(lines_json(&plain), j["plain"], "{input:?}");
            assert_eq!(lyrics_to_plain_text(&plain), j["plainText"], "{input:?}");
            match parse_lrc(input) {
                Ok(parsed) => {
                    assert_eq!(lines_json(&parsed), j["lrc"], "{input:?}");
                    assert_eq!(to_lrc(&parsed), j["toLrc"], "{input:?}");
                }
                Err(_) => assert_eq!(j["lrc"], "THROWS", "{input:?}"),
            }
        } else if let Some(verdict) = j.get("crafted") {
            let crafted = format!("{}{}", "[0:00]".repeat(40000), "x".repeat(100000));
            let actual = if parse_lrc(&crafted).is_ok() {
                "parsed"
            } else {
                "THROWS"
            };
            assert_eq!(actual, verdict);
        } else {
            let (prev, next) = (strings(&j["prev"]), strings(&j["next"]));
            let out: Vec<Option<u64>> = align_lines_default(&prev, &next)
                .into_iter()
                .map(|o| o.map(|i| i as u64))
                .collect();
            assert_eq!(json!(out), j["align"], "{prev:?} -> {next:?}");
        }
        checked += 1;
    }
    assert!(checked > 70, "only {checked} golden lines");
}
