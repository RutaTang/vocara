// The ruler and lane grids pick their tick interval so labels land at least
// 72 px apart. The longest step was 10 minutes, so a multi-hour backing at
// fit-all on a narrow timeline packed its labels a few pixels apart, one
// over the next.

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/waveform_painter.dart';

void main() {
  double spacingPx(int durationMs, double width) =>
      niceTimeIntervalMs(durationMs, width) * width / durationMs;

  test('a two-hour timeline keeps its labels apart', () {
    expect(spacingPx(2 * 3600000, 800), greaterThanOrEqualTo(72));
  });

  test('a three-hour timeline on a narrow pane keeps its labels apart', () {
    expect(spacingPx(3 * 3600000, 400), greaterThanOrEqualTo(72));
  });

  test('a timeline of years keeps its labels apart too', () {
    // Held at an hour a step, 2^62 ms drew a line per hour: the screen froze.
    expect(spacingPx(1 << 62, 800), greaterThanOrEqualTo(72));
    expect(spacingPx(30 * 24 * 3600000, 800), greaterThanOrEqualTo(72));
  });

  test('a song-length timeline keeps its fine steps', () {
    expect(niceTimeIntervalMs(180000, 800), 30000);
    expect(niceTimeIntervalMs(10000, 800), 1000);
  });
}
