import '../domain/models/lyric_line.dart';

final _lrcTimeTag = RegExp(r'\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]');

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
  out.sort((a, b) => (a.timeMs ?? 0).compareTo(b.timeMs ?? 0));
  return out;
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
