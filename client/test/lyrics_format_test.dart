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

    test('a long rewritten middle is matched forward within a window', () {
      final prev = [for (var k = 0; k < 3000; k++) 'old $k'];
      final next = [
        for (var k = 0; k < 3000; k++) k == 1500 ? 'old 2999' : 'new $k',
      ];
      // No line matches near where it would sit: all new, no far jump.
      expect(alignLines(prev, next).every((i) => i == null), isTrue);
    });

    test('past the cell limit, a forward greedy match', () {
      expect(
        alignLines(
          ['chorus', 'verse', 'chorus'],
          ['verse', 'chorus'],
          cellLimit: 1,
        ),
        [1, 2],
      );
      expect(alignLines(const [], ['a']), [null]);
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
