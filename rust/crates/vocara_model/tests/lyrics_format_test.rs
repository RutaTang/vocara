//! Port of `client/test/lyrics_format_test.dart`.

use std::time::{Duration, Instant};

use vocara_model::models::lyric_line::LyricLine;
use vocara_model::util::lyrics_format::*;
use vocara_model::Error;

fn align(prev: &[&str], next: &[&str]) -> Vec<Option<usize>> {
    align_lines_default(prev, next)
}

fn texts(lines: &[LyricLine]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

fn times(lines: &[LyricLine]) -> Vec<Option<i64>> {
    lines.iter().map(|l| l.time_ms).collect()
}

// --- alignLines ---

#[test]
fn a_repeated_line_maps_to_its_own_earlier_self() {
    // A typo fixed in the first chorus: the second chorus is still the
    // second one, not the first.
    assert_eq!(
        align(
            &["chorus", "verse", "chorus"],
            &["chorus!", "verse", "chorus"]
        ),
        [None, Some(1), Some(2)]
    );
    // The first chorus deleted.
    assert_eq!(
        align(&["chorus", "verse", "chorus"], &["verse", "chorus"]),
        [Some(1), Some(2)]
    );
}

#[test]
fn insertions_are_new_a_moved_line_keeps_the_order_of_the_rest() {
    assert_eq!(
        align(&["a", "b", "a"], &["a", "x", "b", "a"]),
        [Some(0), None, Some(1), Some(2)]
    );
    assert_eq!(
        align(&["a", "b", "c"], &["c", "a", "b"]),
        [None, Some(0), Some(1)]
    );
    assert_eq!(
        align(&["a", "", "b"], &["a", "", "b"]),
        [Some(0), Some(1), Some(2)]
    );
}

#[test]
fn a_line_inserted_into_a_long_text_keeps_every_other_line_matched() {
    // Past the cell limit the forward match jumped ahead to the first equal
    // line anywhere: one blank line added at the top of a 2500-line song
    // kept times on only 1750 of its lines.
    let prev: Vec<String> = (0..2500)
        .map(|k| {
            if k % 10 == 9 {
                String::new()
            } else {
                format!("line {k}")
            }
        })
        .collect();
    let mut next = vec![String::new()];
    next.extend(prev.iter().cloned());
    let out = align_lines_default(&prev, &next);
    assert_eq!(out[0], None);
    assert_eq!(out[1..], (0..2500).map(Some).collect::<Vec<_>>()[..]);
}

#[test]
fn a_long_cut_between_edited_ends_keeps_every_line_left() {
    // Edited at both ends, nothing is shared at the edges, so the whole
    // text is the middle: past the old cell limit a windowed guess lost
    // every line after a cut longer than its window, or matched them to a
    // chorus or blank line far back (that line's time handed on).
    let line = |k: usize| match k % 8 {
        3 | 7 => String::new(),
        5 | 6 => format!("chorus {}", k % 8),
        _ => format!("verse {k}"),
    };
    let prev: Vec<String> = (0..2600).map(line).collect();
    let kept: Vec<usize> = (1..2599).filter(|k| *k < 1200 || *k >= 1300).collect();
    let mut next = vec!["edited top".to_string()];
    next.extend(kept.iter().map(|k| prev[*k].clone()));
    next.push("edited end".to_string());
    let out = align_lines_default(&prev, &next);
    assert_eq!(out[0], None);
    assert_eq!(out[out.len() - 1], None);
    let mid = &out[1..out.len() - 1];
    assert_eq!(mid.iter().flatten().count(), kept.len());
    for j in 0..kept.len() {
        let m = mid[j].unwrap();
        assert_eq!(prev[m], next[j + 1]);
        if j > 0 {
            assert!(m > mid[j - 1].unwrap());
        }
        // A verse line is unique: it can only be its own line.
        if next[j + 1].starts_with("verse") {
            assert_eq!(m, kept[j]);
        }
    }
}

#[test]
fn past_the_cell_limit_the_changed_middle_is_new_the_edges_kept() {
    assert_eq!(
        align_lines(
            &["intro", "chorus", "verse", "bridge", "outro"],
            &["intro", "verse", "chorus", "outro"],
            1
        ),
        [Some(0), None, None, Some(4)]
    );
    assert_eq!(align(&[], &["a"]), [None]);
}

#[test]
fn a_5000_line_middle_aligns_exactly_in_good_time() {
    // Both ends edited and every seventh line rewritten: all middle, at the
    // cell limit's edge, every untouched line still found.
    let prev: Vec<String> = (0..5000).map(|k| format!("line {k}")).collect();
    let next: Vec<String> = (0..5000)
        .map(|k| {
            if k == 0 || k == 4999 || k % 7 == 3 {
                format!("new {k}")
            } else {
                format!("line {k}")
            }
        })
        .collect();
    let watch = Instant::now();
    let out = align_lines_default(&prev, &next);
    // Generous for an unoptimized build: the point is no quadratic blow-up
    // past the table.
    assert!(watch.elapsed() < Duration::from_secs(30));
    for k in 0..5000 {
        let expected = if next[k].starts_with("new") {
            None
        } else {
            Some(k)
        };
        assert_eq!(out[k], expected);
    }
}

#[test]
fn parse_plain_lyrics_keeps_lines_and_trims_trailing_blanks() {
    let lines = parse_plain_lyrics("Line one\nLine two\n\n");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "Line one");
    assert_eq!(lines[1].time_ms, None);
}

#[test]
fn lyrics_read_lines_ended_by_a_lone_carriage_return() {
    // Classic Mac line endings, as some tag editors still write them. Split on
    // line feeds alone, such a file was one line: an LRC one gave each of its
    // tags the whole text, and plain lyrics could be timed only once.
    let lrc = parse_lrc("[00:01.00]one\r[00:02.00]two\r\n[00:03.00]three").unwrap();
    assert_eq!(texts(&lrc), ["one", "two", "three"]);
    assert!(is_lrc_document("[00:01.00]one\r[00:02.00]two"));
    assert_eq!(
        texts(&parse_plain_lyrics("one\rtwo\r\rthree")),
        ["one", "two", "", "three"]
    );
}

#[test]
fn parse_lrc_reads_timestamps_centiseconds_and_milliseconds() {
    let lines = parse_lrc("[ti:Song]\n[00:12.50]hello\n[01:03.250]world\nuntimed").unwrap();
    assert_eq!(lines.len(), 2, "metadata + untimed lines are skipped");
    assert_eq!(lines[0].text, "hello");
    assert_eq!(lines[0].time_ms, Some(12500));
    assert_eq!(lines[1].text, "world");
    assert_eq!(lines[1].time_ms, Some(63250));
}

#[test]
fn parse_lrc_sorts_by_time_and_expands_multi_timestamp_lines() {
    let lines = parse_lrc("[00:30.00]chorus\n[00:05.00]intro\n[00:10.00][00:20.00]ah").unwrap();
    assert_eq!(
        times(&lines),
        [Some(5000), Some(10000), Some(20000), Some(30000)]
    );
    assert_eq!(lines[1].text, "ah");
    assert_eq!(lines[2].text, "ah");
}

#[test]
fn parse_lrc_reads_tenths_hundredths_and_milliseconds_fractions_correctly() {
    // .5 == 500 ms (tenths), .05 == 50 ms (hundredths), .005 == 5 ms (millis).
    let one = |s: &str| parse_lrc(s).unwrap()[0].time_ms;
    assert_eq!(one("[00:01.5]a"), Some(1500));
    assert_eq!(one("[00:01.05]a"), Some(1050));
    assert_eq!(one("[00:01.50]a"), Some(1500));
    assert_eq!(one("[00:01.005]a"), Some(1005));
    assert_eq!(one("[00:01.500]a"), Some(1500));
}

#[test]
fn parse_lrc_moves_every_time_by_an_offset_tag() {
    // A positive offset shows the lines sooner. Ignored, a file carrying one
    // landed that far off.
    assert_eq!(
        times(&parse_lrc("[offset:+500]\n[00:10.00]a\n[00:00.20]b").unwrap()),
        [Some(0), Some(9500)]
    );
    assert_eq!(
        times(&parse_lrc("[offset:-250]\n[00:10.00]a").unwrap()),
        [Some(10250)]
    );
    // Too long to be a number: no offset, not a parse that throws.
    let long = format!("[offset:+{}]\n[00:10.00]a", "9".repeat(25));
    assert_eq!(times(&parse_lrc(&long).unwrap()), [Some(10000)]);
}

#[test]
fn looks_like_lrc_distinguishes_lrc_from_plain_text() {
    assert!(looks_like_lrc("[00:01.00]hi"));
    assert!(!looks_like_lrc("just words\nmore words"));
}

#[test]
fn is_lrc_document_only_accepts_text_that_is_lrc_all_the_way_down() {
    // The editor routes on this: parse_lrc drops untimed lines, so plain lyrics
    // carrying one bracketed timestamp must NOT be treated as a document.
    assert!(is_lrc_document(
        "[ti:Song]\n[00:12.50]hello\n[01:03.25]world"
    ));
    assert!(is_lrc_document("[00:12.50]hello\n\n[01:03.25]world"));
    // A stray timestamp inside plain lyrics is not an LRC document.
    assert!(!is_lrc_document("I waited [04:33] for you\nand you came"));
    // One untimed lyric line means parse_lrc would drop it.
    assert!(!is_lrc_document("[00:12.50]hello\nlyrics by someone"));
    assert!(!is_lrc_document("just words\nmore words"));
    assert!(!is_lrc_document("[ti:Song]\n[ar:Artist]"));
}

#[test]
fn to_lrc_round_trips_synced_lines() {
    let lrc = "[00:12.50]hello\n[01:03.25]world\n";
    assert_eq!(to_lrc(&parse_lrc(lrc).unwrap()), lrc);
}

#[test]
fn an_lrc_line_repeated_past_reason_is_refused_not_expanded() {
    // 40 000 tags before one long text: each tag repeats the text.
    let crafted = format!("{}{}", "[0:00]".repeat(40000), "x".repeat(100000));
    assert!(matches!(parse_lrc(&crafted), Err(Error::Format { .. })));
    // A chorus under a handful of tags is what the repetition is for.
    let chorus = parse_lrc("[00:10][01:10][02:10]la la\n[00:20]verse").unwrap();
    assert_eq!(
        times(&chorus),
        [Some(10000), Some(20000), Some(70000), Some(130000)]
    );
}

#[test]
fn an_offset_past_a_day_is_held_to_it() {
    // Subtracted as is, a 19-digit offset overflowed: lines landed anywhere.
    let lines = parse_lrc("[offset:-9223372036854775807]\n[00:01.00]a").unwrap();
    assert_eq!(times(&lines), [Some(1000 + 86_400_000)]);
}

#[test]
fn plain_text_rendering_joins_the_lines() {
    let lines = parse_plain_lyrics("a\n\nb");
    assert_eq!(lyrics_to_plain_text(&lines), "a\n\nb");
}
