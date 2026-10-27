import 'dart:math' as math;
import 'dart:typed_data';

import '../domain/models/lyric_line.dart';

final _lrcTimeTag = RegExp(r'\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]');

/// Aligns two texts line by line, in order: for each line of [next], the
/// index of the line of [prev] it continues, or null for a line that is new.
/// Lines match only on equal text, and only in order (a longest common
/// subsequence), so a line repeated in the song (a chorus) maps to its own
/// earlier self, never to another repeat of it.
///
/// The lines both texts share at the start and the end match as they stand
/// (an edit usually changes a few lines in the middle), so the full
/// alignment runs on the changed middle only, at one bit a pair. A middle
/// past [cellLimit] pairs (thousands of lines rewritten at once) is all new:
/// a guess over it handed lines the times of far-off repeats.
List<int?> alignLines(
  List<String> prev,
  List<String> next, {
  int cellLimit = 25000000,
}) {
  final out = List<int?>.filled(next.length, null);
  var lo = 0;
  while (lo < prev.length && lo < next.length && prev[lo] == next[lo]) {
    out[lo] = lo;
    lo++;
  }
  var hiPrev = prev.length, hiNext = next.length;
  while (hiPrev > lo && hiNext > lo && prev[hiPrev - 1] == next[hiNext - 1]) {
    hiPrev--;
    hiNext--;
    out[hiNext] = hiPrev;
  }
  final n = hiPrev - lo, m = hiNext - lo;
  if (n == 0 || m == 0 || n * m > cellLimit) return out;
  // The middles as ints, so the table compares numbers, not strings.
  final ids = <String, int>{};
  final a = Int32List(n), b = Int32List(m);
  for (var i = 0; i < n; i++) {
    a[i] = ids.putIfAbsent(prev[lo + i], () => ids.length);
  }
  for (var j = 0; j < m; j++) {
    b[j] = ids.putIfAbsent(next[lo + j], () => ids.length);
  }
  // The lengths of the longest common subsequence of the middles from i and
  // j on, a row at a time; [down] keeps, per unequal pair, whether skipping
  // prev's line keeps it longest (what the walk below needs of the table).
  final down = Uint8List((n * m + 7) >> 3);
  var below = Int32List(m + 1), row = Int32List(m + 1);
  for (var i = n - 1; i >= 0; i--) {
    final ai = a[i];
    for (var j = m - 1; j >= 0; j--) {
      if (ai == b[j]) {
        row[j] = below[j + 1] + 1;
      } else if (below[j] >= row[j + 1]) {
        row[j] = below[j];
        final k = i * m + j;
        down[k >> 3] |= 1 << (k & 7);
      } else {
        row[j] = row[j + 1];
      }
    }
    final t = below;
    below = row;
    row = t;
  }
  var i = 0, j = 0;
  while (i < n && j < m) {
    if (a[i] == b[j]) {
      out[lo + j] = lo + i;
      i++;
      j++;
    } else if (down[(i * m + j) >> 3] & (1 << ((i * m + j) & 7)) != 0) {
      i++;
    } else {
      j++;
    }
  }
  return out;
}

/// Parses plain lyrics text into (unsynced) lines, preserving blank lines as
/// section gaps but trimming trailing blanks.
List<LyricLine> parsePlainLyrics(String text) {
  final out = [
    for (final raw in text.split('\n')) LyricLine(text: raw.trimRight()),
  ];
  while (out.isNotEmpty && out.last.isBlank) {
    out.removeLast();
  }
  return out;
}

/// Whether [content] contains at least one LRC time tag.
bool looksLikeLrc(String content) => _lrcTimeTag.hasMatch(content);

/// A whole line that is nothing but an LRC metadata tag — `[ti:Song]`,
/// `[ar:Artist]`, `[offset:+500]`. Never matches a time tag (those start with
/// digits and are handled first).
final _lrcMetaTag = RegExp(r'^\[[A-Za-z#][^\]:]*:[^\]]*\]$');

/// Whether [content] is an LRC DOCUMENT, rather than plain lyrics that merely
/// happen to contain a bracketed timestamp: EVERY non-blank line must carry a
/// time tag or be a bare metadata tag.
///
/// [looksLikeLrc] asks only whether one time tag appears anywhere. That is the
/// right question for a file the user handed over as lyrics, and the wrong one
/// for text they typed themselves: [parseLrc] drops untimed lines by design,
/// so a single `[04:33]` in an otherwise plain lyric would delete every other
/// line on save.
bool isLrcDocument(String content) {
  var sawTime = false;
  for (final raw in content.split('\n')) {
    final line = raw.trim();
    if (line.isEmpty) continue;
    if (_lrcTimeTag.hasMatch(line)) {
      sawTime = true;
      continue;
    }
    if (_lrcMetaTag.hasMatch(line)) continue;
    return false;
  }
  return sawTime;
}

/// An `[offset:+500]` metadata line: milliseconds to show every line sooner.
final _lrcOffsetTag = RegExp(
  r'^\[offset:\s*([+-]?\d+)\s*\]$',
  caseSensitive: false,
);

/// Parses an `.lrc` document into timed lines, sorted by time. A line with
/// several timestamps emits one entry per timestamp; metadata tags (`[ti:]`,
/// `[ar:]`, …) and untimed lines are skipped, but an `[offset:]` moves every
/// time by it (ignored, a file with one landed that much off). Throws a
/// [FormatException] for a document that would expand past a few times its
/// own size.
List<LyricLine> parseLrc(String content) {
  var offsetMs = 0;
  for (final raw in content.split('\n')) {
    final m = _lrcOffsetTag.firstMatch(raw.trim());
    // An offset too long to be a number of milliseconds is no offset, and
    // one past a day is held to it: subtracted as is, it overflowed and the
    // lines landed anywhere.
    final ms = m == null ? null : int.tryParse(m.group(1)!);
    if (ms != null) offsetMs = ms.clamp(-86400000, 86400000);
  }
  final out = <LyricLine>[];
  // A line's text is repeated once per time tag on it: half a megabyte of
  // tags before one long text came to billions of characters, and the save
  // ran out of memory encoding them.
  final budget = math.max(1 << 20, content.length * 4);
  var emitted = 0;
  for (final raw in content.split('\n')) {
    final line = raw.trim();
    if (line.isEmpty) continue;
    final tags = _lrcTimeTag.allMatches(line).toList();
    if (tags.isEmpty) continue;
    final text = line.replaceAll(_lrcTimeTag, '').trim();
    emitted += (text.length + 1) * tags.length;
    if (emitted > budget) {
      throw const FormatException(
        'These lyrics repeat their lines too many times to load.',
      );
    }
    for (final m in tags) {
      final minutes = int.parse(m.group(1)!);
      final seconds = int.parse(m.group(2)!);
      final frac = m.group(3);
      var ms = (minutes * 60 + seconds) * 1000;
      if (frac != null) {
        // Tenths (1 digit), hundredths (2 digits), or milliseconds (3+ digits).
        // A single digit is tenths (.5 == 500 ms), so it scales by 100, not 10.
        ms += frac.length >= 3
            ? int.parse(frac.substring(0, 3))
            : int.parse(frac) * (frac.length == 1 ? 100 : 10);
      }
      out.add(LyricLine(text: text, timeMs: math.max(0, ms - offsetMs)));
    }
  }
  // List.sort is not stable, so pair each line with its original index and use
  // it as a total-order tiebreaker — otherwise distinct lines sharing a
  // timestamp can be reordered (same hazard handled in
  // song_practice_repository_drift).
  final indexed = [
    for (var i = 0; i < out.length; i++) (index: i, line: out[i]),
  ];
  indexed.sort((a, b) {
    final c = (a.line.timeMs ?? 0).compareTo(b.line.timeMs ?? 0);
    return c != 0 ? c : a.index.compareTo(b.index);
  });
  return [for (final e in indexed) e.line];
}

/// Serializes [lines] to an `.lrc` document: synced lines get a `[mm:ss.cc]`
/// tag; unsynced lines are written as bare text.
String toLrc(List<LyricLine> lines) {
  final b = StringBuffer();
  for (final l in lines) {
    final t = l.timeMs;
    if (t != null) {
      final mm = (t ~/ 60000).toString().padLeft(2, '0');
      final ss = ((t ~/ 1000) % 60).toString().padLeft(2, '0');
      final cs = ((t % 1000) ~/ 10).toString().padLeft(2, '0');
      b.writeln('[$mm:$ss.$cs]${l.text}');
    } else {
      b.writeln(l.text);
    }
  }
  return b.toString();
}

/// Renders [lines] back to plain editable text (one line each, no timestamps).
String lyricsToPlainText(List<LyricLine> lines) =>
    lines.map((l) => l.text).join('\n');
