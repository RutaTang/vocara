//! Replays the lyrics vectors the Lean models computed
//! (`verification/vectors/lrc_*.json` and `align.json`, theorems in
//! `verification/VocaraVerify/Lrc.lean` and `Align.lean`) through the real
//! `to_lrc`, `parse_lrc` and `align_lines`, so the proofs are about this code:
//!
//! * the document `to_lrc` writes is the model's, character for character,
//!   and `parse_lrc` reads it back as the model does — for writable lines,
//!   the timed ones below 1000 minutes, trimmed, rounded down to the
//!   centisecond and sorted stably by time;
//! * `parse_lrc` reads every tag syntax (`[m:ss]`, tenths, hundredths,
//!   milliseconds, a colon before the fraction, three-digit minutes), the
//!   tags it rejects, offsets and metadata as the model does, and refuses a
//!   document at exactly the model's size limit;
//! * `align_lines` gives the model's alignment, entry for entry: the same
//!   longest common subsequence with the same tie-breaks, and the same
//!   fallback past the cell limit.

use serde_json::Value;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::util::lyrics_format::{align_lines, parse_lrc, to_lrc};

fn vectors(name: &str) -> Vec<Value> {
    let path = format!(
        "{}/../../verification/vectors/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).expect(name)
}

fn lines_of(v: &Value) -> Vec<LyricLine> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|l| LyricLine::new(l["text"].as_str().unwrap(), l["time"].as_i64()))
        .collect()
}

/// The model's parse: the lines, or `None` for the size limit's error.
fn parsed_of(v: &Value) -> Option<Vec<LyricLine>> {
    (!v.is_null()).then(|| lines_of(v))
}

#[test]
fn to_lrc_then_parse_lrc_matches_the_verified_model() {
    let cases = vectors("lrc_roundtrip.json");
    assert!(cases.len() >= 150);
    let (mut writable, mut lines_seen) = (0, 0);
    for case in &cases {
        let lines = lines_of(&case["lines"]);
        lines_seen += lines.len();
        let doc = to_lrc(&lines);
        assert_eq!(doc, case["doc"].as_str().unwrap(), "to_lrc({lines:?})");
        assert_eq!(
            parse_lrc(&doc).ok(),
            parsed_of(&case["parsed"]),
            "parse_lrc({doc:?})"
        );
        if case["writable"].as_bool().unwrap() {
            writable += 1;
        }
    }
    assert!(lines_seen > 500, "the vectors hold lines ({lines_seen})");
    assert!(
        writable > 60,
        "the vectors exercise the round trip theorem ({writable})"
    );
}

#[test]
fn parse_lrc_reads_every_tag_syntax_as_the_verified_model() {
    let cases = vectors("lrc_parse.json");
    assert!(cases.len() >= 150);
    let mut entries = 0;
    for case in &cases {
        let doc = case["doc"].as_str().unwrap();
        let expect = parsed_of(&case["parsed"]);
        entries += expect.as_ref().map_or(0, Vec::len);
        assert_eq!(parse_lrc(doc).ok(), expect, "parse_lrc({doc:?})");
    }
    assert!(entries > 400, "the vectors parse entries ({entries})");
}

#[test]
fn parse_lrc_refuses_at_the_verified_size_limit() {
    let cases = vectors("lrc_budget.json");
    assert!(cases.len() >= 5);
    for case in &cases {
        let tags = case["tags"].as_u64().unwrap() as usize;
        let text = case["text"].as_u64().unwrap() as usize;
        let doc = "[00:00]".repeat(tags) + &case["char"].as_str().unwrap().repeat(text);
        let parsed = parse_lrc(&doc);
        assert_eq!(
            parsed.is_ok(),
            case["ok"].as_bool().unwrap(),
            "{tags} tags, {text} characters"
        );
        assert_eq!(
            parsed.map_or(0, |l| l.len()),
            case["count"].as_u64().unwrap() as usize
        );
    }
}

#[test]
fn align_lines_matches_the_verified_model() {
    let cases = vectors("align.json");
    assert!(cases.len() >= 200);
    let (mut fallbacks, mut matched) = (0, 0);
    for case in &cases {
        let strs = |v: &Value| -> Vec<String> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        };
        let (prev, next) = (strs(&case["prev"]), strs(&case["next"]));
        let limit = case["cell_limit"].as_u64().unwrap() as usize;
        let expect: Vec<Option<usize>> = case["out"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_u64().map(|i| i as usize))
            .collect();
        assert_eq!(
            align_lines(&prev, &next, limit),
            expect,
            "align_lines({prev:?}, {next:?}, {limit})"
        );
        // Past the cell limit the middle is left new: fewer lines match
        // than without a limit.
        let count = expect.iter().flatten().count();
        if align_lines(&prev, &next, usize::MAX)
            .iter()
            .flatten()
            .count()
            > count
        {
            fallbacks += 1;
        }
        matched += count;
    }
    assert!(
        fallbacks > 10,
        "the vectors exercise the cell limit ({fallbacks})"
    );
    assert!(matched > 1000, "the vectors match lines ({matched})");
}
