import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/utils/lyrics_format.dart';

void main() {
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

  test('toLrc round-trips synced lines', () {
    final lrc = '[00:12.50]hello\n[01:03.25]world\n';
    final round = toLrc(parseLrc(lrc));
    expect(round, lrc);
  });
}
