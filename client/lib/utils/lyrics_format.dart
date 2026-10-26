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
/// alignment runs on the changed middle only. A middle past [cellLimit]
/// pairs (thousands of lines rewritten at once) is matched forward, each line
/// against the next [window] lines of [prev] only: a line with no match close
/// ahead is new, rather than a jump past everything up to some far repeat.
List<int?> alignLines(
  List<String> prev,
  List<String> next, {
  int cellLimit = 4000000,
  int window = 64,
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
  if (n == 0 || m == 0) return out;
  if (n * m > cellLimit) {
    var from = lo;
    for (var j = lo; j < hiNext; j++) {
      final end = from + window < hiPrev ? from + window : hiPrev;
      for (var i = from; i < end; i++) {
        if (prev[i] == next[j]) {
          out[j] = i;
          from = i + 1;
          break;
        }
      }
    }
    return out;
  }
  // lcs[i][j]: the longest common subsequence of the middles from i and j on.
  final w = m + 1;
  final lcs = List<int>.filled((n + 1) * w, 0);
  for (var i = n - 1; i >= 0; i--) {
    for (var j = m - 1; j >= 0; j--) {
      lcs[i * w + j] = prev[lo + i] == next[lo + j]
          ? lcs[(i + 1) * w + j + 1] + 1
          : (lcs[(i + 1) * w + j] > lcs[i * w + j + 1]
                ? lcs[(i + 1) * w + j]
                : lcs[i * w + j + 1]);
    }
  }
  var i = 0, j = 0;
  while (i < n && j < m) {
    if (prev[lo + i] == next[lo + j]) {
      out[lo + j] = lo + i;
      i++;
      j++;
    } else if (lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
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

/// Parses an `.lrc` document into timed lines, sorted by time. A line with
/// several timestamps emits one entry per timestamp; metadata tags (`[ti:]`,
/// `[ar:]`, …) and untimed lines are skipped.
List<LyricLine> parseLrc(String content) {
  final out = <LyricLine>[];
  for (final raw in content.split('\n')) {
    final line = raw.trim();
    if (line.isEmpty) continue;
    final tags = _lrcTimeTag.allMatches(line).toList();
    if (tags.isEmpty) continue;
    final text = line.replaceAll(_lrcTimeTag, '').trim();
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
      out.add(LyricLine(text: text, timeMs: ms));
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
