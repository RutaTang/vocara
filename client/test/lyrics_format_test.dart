import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/utils/lyrics_format.dart';

void main() {
  group('alignLines', () {
    test('a repeated line maps to its own earlier self', () {
      // A typo fixed in the first chorus: the second chorus is still the
      // second one, not the first.
      expect(
        alignLines(
          ['chorus', 'verse', 'chorus'],
          ['chorus!', 'verse', 'chorus'],
        ),
        [null, 1, 2],
      );
      // The first chorus deleted.
      expect(alignLines(['chorus', 'verse', 'chorus'], ['verse', 'chorus']), [
        1,
        2,
      ]);
    });

    test('insertions are new; a moved line keeps the order of the rest', () {
      expect(alignLines(['a', 'b', 'a'], ['a', 'x', 'b', 'a']), [
        0,
        null,
        1,
        2,
      ]);
      expect(alignLines(['a', 'b', 'c'], ['c', 'a', 'b']), [null, 0, 1]);
      expect(alignLines(['a', '', 'b'], ['a', '', 'b']), [0, 1, 2]);
    });

    test('a line inserted into a long text keeps every other line matched', () {
      // Past the cell limit the forward match jumped ahead to the first equal
      // line anywhere: one blank line added at the top of a 2500-line song
      // kept times on only 1750 of its lines.
      final prev = [
        for (var k = 0; k < 2500; k++) k % 10 == 9 ? '' : 'line $k',
      ];
      final next = ['', ...prev];
      final out = alignLines(prev, next);
      expect(out.first, isNull);
      expect(out.skip(1), [for (var k = 0; k < 2500; k++) k]);
    });

    test('a long cut between edited ends keeps every line left', () {
      // Edited at both ends, nothing is shared at the edges, so the whole
      // text is the middle: past the old cell limit a windowed guess lost
      // every line after a cut longer than its window, or matched them to a
      // chorus or blank line far back (that line's time handed on).
      String line(int k) => switch (k % 8) {
        3 || 7 => '',
        5 || 6 => 'chorus ${k % 8}',
        _ => 'verse $k',
      };
      final prev = [for (var k = 0; k < 2600; k++) line(k)];
      final kept = [
        for (var k = 1; k < 2599; k++)
          if (k < 1200 || k >= 1300) k,
      ];
      final next = ['edited top', for (final k in kept) prev[k], 'edited end'];
      final out = alignLines(prev, next);
      expect(out.first, isNull);
      expect(out.last, isNull);
      final mid = out.sublist(1, out.length - 1);
      expect(mid.whereType<int>().length, kept.length);
      for (var j = 0; j < kept.length; j++) {
        expect(prev[mid[j]!], next[j + 1]);
        if (j > 0) expect(mid[j]!, greaterThan(mid[j - 1]!));
        // A verse line is unique: it can only be its own line.
        if (next[j + 1].startsWith('verse')) expect(mid[j], kept[j]);
      }
    });

    test('past the cell limit the changed middle is new, the edges kept', () {
      expect(
        alignLines(
          ['intro', 'chorus', 'verse', 'bridge', 'outro'],
          ['intro', 'verse', 'chorus', 'outro'],
          cellLimit: 1,
        ),
        [0, null, null, 4],
      );
      expect(alignLines(const [], ['a']), [null]);
    });

    test('a wholly rewritten long text aligns in good time', () {
      final prev = [for (var k = 0; k < 5000; k++) 'old $k'];
      final next = [for (var k = 0; k < 5000; k++) 'new $k'];
      final watch = Stopwatch()..start();
      expect(alignLines(prev, next).every((i) => i == null), isTrue);
      expect(watch.elapsed, lessThan(const Duration(seconds: 5)));
    });
  });

  test('parsePlainLyrics keeps lines and trims trailing blanks', () {
    final lines = parsePlainLyrics('Line one\nLine two\n\n');
    expect(lines.length, 2);
    expect(lines[0].text, 'Line one');
    expect(lines[1].timeMs, isNull);
  });

  test('parseLrc reads timestamps (centiseconds + milliseconds)', () {
    final lines = parseLrc(
      '[ti:Song]\n[00:12.50]hello\n[01:03.250]world\nuntimed',
    );
    expect(lines.length, 2, reason: 'metadata + untimed lines are skipped');
    expect(lines[0].text, 'hello');
    expect(lines[0].timeMs, 12500);
    expect(lines[1].text, 'world');
    expect(lines[1].timeMs, 63250);
  });

  test('parseLrc sorts by time and expands multi-timestamp lines', () {
    final lines = parseLrc(
      '[00:30.00]chorus\n[00:05.00]intro\n[00:10.00][00:20.00]ah',
    );
    expect(lines.map((l) => l.timeMs).toList(), [5000, 10000, 20000, 30000]);
    expect(lines[1].text, 'ah');
    expect(lines[2].text, 'ah');
  });

  test(
    'parseLrc reads tenths / hundredths / milliseconds fractions correctly',
    () {
      // .5 == 500 ms (tenths), .05 == 50 ms (hundredths), .005 == 5 ms (millis).
      expect(parseLrc('[00:01.5]a').single.timeMs, 1500);
      expect(parseLrc('[00:01.05]a').single.timeMs, 1050);
      expect(parseLrc('[00:01.50]a').single.timeMs, 1500);
      expect(parseLrc('[00:01.005]a').single.timeMs, 1005);
      expect(parseLrc('[00:01.500]a').single.timeMs, 1500);
    },
  );

  test('looksLikeLrc distinguishes lrc from plain text', () {
    expect(looksLikeLrc('[00:01.00]hi'), isTrue);
    expect(looksLikeLrc('just words\nmore words'), isFalse);
  });

  test('isLrcDocument only accepts text that is LRC all the way down', () {
    // The editor routes on this: parseLrc drops untimed lines, so plain lyrics
    // carrying one bracketed timestamp must NOT be treated as a document.
    expect(
      isLrcDocument('[ti:Song]\n[00:12.50]hello\n[01:03.25]world'),
      isTrue,
    );
    expect(isLrcDocument('[00:12.50]hello\n\n[01:03.25]world'), isTrue);
    expect(
      isLrcDocument('I waited [04:33] for you\nand you came'),
      isFalse,
      reason: 'a stray timestamp inside plain lyrics is not an LRC document',
    );
    expect(
      isLrcDocument('[00:12.50]hello\nlyrics by someone'),
      isFalse,
      reason: 'one untimed lyric line means parseLrc would drop it',
    );
    expect(isLrcDocument('just words\nmore words'), isFalse);
    expect(isLrcDocument('[ti:Song]\n[ar:Artist]'), isFalse);
  });

  test('toLrc round-trips synced lines', () {
    final lrc = '[00:12.50]hello\n[01:03.25]world\n';
    final round = toLrc(parseLrc(lrc));
    expect(round, lrc);
  });
}
