//! Lyrics text and `.lrc` parsing, writing and line alignment (port of
//! `lib/utils/lyrics_format.dart`).
//!
//! The Dart code matches with regular expressions; their exact languages are
//! matched here by hand (Dart's `\d` is ASCII only, `\s` JavaScript's white
//! space), so a lyrics file reads the same in both apps.

use std::collections::HashMap;

use crate::models::lyric_line::LyricLine;
use crate::util::dart_string::{self, utf16_len};
use crate::{Error, Result};

/// One match of `\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]` (the LRC time
/// tag): byte span and the digit groups.
struct TimeTag<'a> {
    start: usize,
    end: usize,
    minutes: &'a str,
    seconds: &'a str,
    fraction: Option<&'a str>,
}

fn digit_run(b: &[u8], from: usize) -> usize {
    b[from.min(b.len())..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count()
}

/// The time tag starting exactly at byte [i] of [s], if any.
fn time_tag_at(s: &str, i: usize) -> Option<TimeTag<'_>> {
    let b = s.as_bytes();
    if b.get(i) != Some(&b'[') {
        return None;
    }
    // \d{1,3} then ':' — a longer digit run can never be followed by ':'.
    let m = digit_run(b, i + 1);
    if !(1..=3).contains(&m) || b.get(i + 1 + m) != Some(&b':') {
        return None;
    }
    let sec_at = i + 1 + m + 1;
    if digit_run(b, sec_at) < 2 {
        return None;
    }
    let after_sec = sec_at + 2;
    // (?:[.:](\d{1,3}))? then ']'.
    if matches!(b.get(after_sec), Some(b'.' | b':')) {
        let f = digit_run(b, after_sec + 1);
        let close = after_sec + 1 + f;
        if (1..=3).contains(&f) && b.get(close) == Some(&b']') {
            return Some(TimeTag {
                start: i,
                end: close + 1,
                minutes: &s[i + 1..i + 1 + m],
                seconds: &s[sec_at..after_sec],
                fraction: Some(&s[after_sec + 1..close]),
            });
        }
    }
    (b.get(after_sec) == Some(&b']')).then(|| TimeTag {
        start: i,
        end: after_sec + 1,
        minutes: &s[i + 1..i + 1 + m],
        seconds: &s[sec_at..after_sec],
        fraction: None,
    })
}

/// Every time tag in [s], left to right, never overlapping (`allMatches`).
fn time_tags(s: &str) -> Vec<TimeTag<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        match time_tag_at(s, i) {
            Some(tag) => {
                i = tag.end;
                out.push(tag);
            }
            None => i += 1,
        }
    }
    out
}

fn has_time_tag(s: &str) -> bool {
    (0..s.len()).any(|i| time_tag_at(s, i).is_some())
}

/// Aligns two texts line by line, in order: for each line of [next], the
/// index of the line of [prev] it continues, or `None` for a line that is new.
/// Lines match only on equal text, and only in order (a longest common
/// subsequence), so a line repeated in the song (a chorus) maps to its own
/// earlier self, never to another repeat of it.
///
/// The lines both texts share at the start and the end match as they stand
/// (an edit usually changes a few lines in the middle), so the full
/// alignment runs on the changed middle only, at one bit a pair. A middle
/// past [cell_limit] pairs (thousands of lines rewritten at once) is all new:
/// a guess over it handed lines the times of far-off repeats.
pub fn align_lines<S: AsRef<str>>(prev: &[S], next: &[S], cell_limit: usize) -> Vec<Option<usize>> {
    let mut out = vec![None; next.len()];
    let mut lo = 0;
    while lo < prev.len() && lo < next.len() && prev[lo].as_ref() == next[lo].as_ref() {
        out[lo] = Some(lo);
        lo += 1;
    }
    let (mut hi_prev, mut hi_next) = (prev.len(), next.len());
    while hi_prev > lo && hi_next > lo && prev[hi_prev - 1].as_ref() == next[hi_next - 1].as_ref() {
        hi_prev -= 1;
        hi_next -= 1;
        out[hi_next] = Some(hi_prev);
    }
    let (n, m) = (hi_prev - lo, hi_next - lo);
    if n == 0 || m == 0 || n.saturating_mul(m) > cell_limit {
        return out;
    }
    // The middles as ints, so the table compares numbers, not strings.
    let mut ids: HashMap<&str, i32> = HashMap::new();
    let mut a = Vec::with_capacity(n);
    for line in &prev[lo..lo + n] {
        let next_id = ids.len() as i32;
        a.push(*ids.entry(line.as_ref()).or_insert(next_id));
    }
    let mut b = Vec::with_capacity(m);
    for line in &next[lo..lo + m] {
        let next_id = ids.len() as i32;
        b.push(*ids.entry(line.as_ref()).or_insert(next_id));
    }
    // The lengths of the longest common subsequence of the middles from i and
    // j on, a row at a time; [down] keeps, per unequal pair, whether skipping
    // prev's line keeps it longest (what the walk below needs of the table).
    let mut down = vec![0u8; (n * m + 7) >> 3];
    let mut below = vec![0i32; m + 1];
    let mut row = vec![0i32; m + 1];
    for i in (0..n).rev() {
        let ai = a[i];
        for j in (0..m).rev() {
            if ai == b[j] {
                row[j] = below[j + 1] + 1;
            } else if below[j] >= row[j + 1] {
                row[j] = below[j];
                let k = i * m + j;
                down[k >> 3] |= 1 << (k & 7);
            } else {
                row[j] = row[j + 1];
            }
        }
        std::mem::swap(&mut below, &mut row);
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out[lo + j] = Some(lo + i);
            i += 1;
            j += 1;
        } else if down[(i * m + j) >> 3] & (1 << ((i * m + j) & 7)) != 0 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The default `cellLimit` of Dart's `alignLines`.
pub const ALIGN_CELL_LIMIT: usize = 25_000_000;

/// [`align_lines`] with the default cell limit ([`ALIGN_CELL_LIMIT`]).
pub fn align_lines_default<S: AsRef<str>>(prev: &[S], next: &[S]) -> Vec<Option<usize>> {
    align_lines(prev, next, ALIGN_CELL_LIMIT)
}

/// Splits on the ends of lines as any lyrics file writes them: a carriage
/// return and line feed, a line feed, or a lone carriage return (classic Mac,
/// some tag editors). Split on line feeds alone, such a file was one line: an
/// LRC one gave each of its tags the whole text, and plain lyrics a single
/// line. (Dart's `split(RegExp(r'\r\n|\r|\n'))`.)
fn split_lines(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                out.push(&text[start..i]);
                i += if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                start = i;
            }
            b'\n' => {
                out.push(&text[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push(&text[start..]);
    out
}

/// Parses plain lyrics text into (unsynced) lines, preserving blank lines as
/// section gaps but trimming trailing blanks.
pub fn parse_plain_lyrics(text: &str) -> Vec<LyricLine> {
    let mut out: Vec<LyricLine> = split_lines(text)
        .into_iter()
        .map(|raw| LyricLine::new(dart_string::trim_right(raw), None))
        .collect();
    while out.last().is_some_and(LyricLine::is_blank) {
        out.pop();
    }
    out
}

/// Whether [content] contains at least one LRC time tag.
pub fn looks_like_lrc(content: &str) -> bool {
    has_time_tag(content)
}

/// A whole line that is nothing but an LRC metadata tag — `[ti:Song]`,
/// `[ar:Artist]`, `[offset:+500]` (`^\[[A-Za-z#][^\]:]*:[^\]]*\]$`). Never
/// matches a time tag (those start with digits and are handled first).
fn is_meta_tag(line: &str) -> bool {
    let b = line.as_bytes();
    if b.len() < 4 || b[0] != b'[' || !(b[1].is_ascii_alphabetic() || b[1] == b'#') {
        return false;
    }
    // [^\]:]* up to the first ':' (a ']' first fails the match).
    let Some(colon) = b[2..].iter().position(|c| *c == b':' || *c == b']') else {
        return false;
    };
    let colon = colon + 2;
    if b[colon] != b':' {
        return false;
    }
    // [^\]]*\]$: the rest holds exactly one ']', at the very end.
    let rest = &b[colon + 1..];
    rest.last() == Some(&b']') && !rest[..rest.len() - 1].contains(&b']')
}

/// Whether [content] is an LRC DOCUMENT, rather than plain lyrics that merely
/// happen to contain a bracketed timestamp: EVERY non-blank line must carry a
/// time tag or be a bare metadata tag.
///
/// [`looks_like_lrc`] asks only whether one time tag appears anywhere. That is the
/// right question for a file the user handed over as lyrics, and the wrong one
/// for text they typed themselves: [`parse_lrc`] drops untimed lines by design,
/// so a single `[04:33]` in an otherwise plain lyric would delete every other
/// line on save.
pub fn is_lrc_document(content: &str) -> bool {
    let mut saw_time = false;
    for raw in split_lines(content) {
        let line = dart_string::trim(raw);
        if line.is_empty() {
            continue;
        }
        if has_time_tag(line) {
            saw_time = true;
            continue;
        }
        if is_meta_tag(line) {
            continue;
        }
        return false;
    }
    saw_time
}

/// JavaScript's `\s` (what Dart's `RegExp` means by it).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{B}' | '\u{C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// The number of an `[offset:+500]` metadata line (`^\[offset:\s*([+-]?\d+)
/// \s*\]$`, case-insensitive): milliseconds to show every line sooner.
fn offset_tag(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let head = rest.get(..7)?;
    if !head.eq_ignore_ascii_case("offset:") {
        return None;
    }
    let rest = rest[7..].trim_start_matches(is_js_space);
    let sign = usize::from(rest.starts_with(['+', '-']));
    let digits = digit_run(rest.as_bytes(), sign);
    if digits == 0 {
        return None;
    }
    let number = &rest[..sign + digits];
    let tail = rest[sign + digits..].trim_start_matches(is_js_space);
    (tail == "]").then_some(number)
}

/// Parses an `.lrc` document into timed lines, sorted by time. A line with
/// several timestamps emits one entry per timestamp; metadata tags (`[ti:]`,
/// `[ar:]`, …) and untimed lines are skipped, but an `[offset:]` moves every
/// time by it (ignored, a file with one landed that much off). Fails with a
/// [`Error::Format`] for a document that would expand past a few times its
/// own size.
pub fn parse_lrc(content: &str) -> Result<Vec<LyricLine>> {
    let mut offset_ms: i64 = 0;
    for raw in split_lines(content) {
        // An offset too long to be a number of milliseconds is no offset, and
        // one past a day is held to it: subtracted as is, it overflowed and the
        // lines landed anywhere.
        if let Some(ms) = offset_tag(dart_string::trim(raw)).and_then(|n| n.parse::<i64>().ok()) {
            offset_ms = ms.clamp(-86_400_000, 86_400_000);
        }
    }
    let mut out: Vec<LyricLine> = Vec::new();
    // A line's text is repeated once per time tag on it: half a megabyte of
    // tags before one long text came to billions of characters, and the save
    // ran out of memory encoding them.
    let budget = (1usize << 20).max(utf16_len(content) * 4);
    let mut emitted = 0usize;
    for raw in split_lines(content) {
        let line = dart_string::trim(raw);
        if line.is_empty() {
            continue;
        }
        let tags = time_tags(line);
        if tags.is_empty() {
            continue;
        }
        let mut untagged = String::with_capacity(line.len());
        let mut at = 0;
        for tag in &tags {
            untagged.push_str(&line[at..tag.start]);
            at = tag.end;
        }
        untagged.push_str(&line[at..]);
        let text = dart_string::trim(&untagged);
        emitted = emitted.saturating_add((utf16_len(text) + 1).saturating_mul(tags.len()));
        if emitted > budget {
            return Err(Error::Format {
                message: "These lyrics repeat their lines too many times to load.".into(),
                input: None,
            });
        }
        for tag in &tags {
            // At most 3 + 2 + 3 ASCII digits: these never fail.
            let minutes: i64 = tag.minutes.parse().unwrap_or(0);
            let seconds: i64 = tag.seconds.parse().unwrap_or(0);
            let mut ms = (minutes * 60 + seconds) * 1000;
            if let Some(frac) = tag.fraction {
                // Tenths (1 digit), hundredths (2 digits), or milliseconds (3 digits).
                // A single digit is tenths (.5 == 500 ms), so it scales by 100, not 10.
                let value: i64 = frac.parse().unwrap_or(0);
                ms += match frac.len() {
                    1 => value * 100,
                    2 => value * 10,
                    _ => value,
                };
            }
            out.push(LyricLine::new(text, Some((ms - offset_ms).max(0))));
        }
    }
    // A stable sort, so distinct lines sharing a timestamp keep their order
    // (Dart pairs each line with its index as a tiebreaker for the same
    // reason: its List.sort is not stable).
    out.sort_by_key(|l| l.time_ms.unwrap_or(0));
    Ok(out)
}

/// Serializes [lines] to an `.lrc` document: synced lines get a `[mm:ss.cc]`
/// tag; unsynced lines are written as bare text. Every line ends with `\n`.
pub fn to_lrc(lines: &[LyricLine]) -> String {
    let pad2 = |n: i64| format!("{:0>2}", n.to_string());
    let mut b = String::new();
    for l in lines {
        match l.time_ms {
            Some(t) => {
                let mm = pad2(t / 60000);
                let ss = pad2((t / 1000).rem_euclid(60));
                let cs = pad2(t.rem_euclid(1000) / 10);
                b.push_str(&format!("[{mm}:{ss}.{cs}]{}\n", l.text));
            }
            None => {
                b.push_str(&l.text);
                b.push('\n');
            }
        }
    }
    b
}

/// Renders [lines] back to plain editable text (one line each, no timestamps).
pub fn lyrics_to_plain_text(lines: &[LyricLine]) -> String {
    lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
