// The take spectrogram image: its pixels, the column cap that keeps a long
// take inside a GPU texture, and the cache that renders each take once.
//
// One column per analysis frame is ~94 a second, so a take past ~3 minutes
// used to decode wider than the 16384-px texture limit, and the engine scaled
// the whole image down to fit — uniformly on the default Skia backend, which
// cut a 10-minute take's frequency rows from 215 to 62.

import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/ui/core/widgets/spectrogram_painter.dart';

Spectrogram _spec(int frames, int bins, double Function(int f, int b) db) {
  final values = Float32List(frames * bins);
  var maxDb = double.negativeInfinity;
  for (var f = 0; f < frames; f++) {
    for (var b = 0; b < bins; b++) {
      final v = db(f, b);
      values[f * bins + b] = v;
      if (v > maxDb) maxDb = v;
    }
  }
  return Spectrogram(
    frames: frames,
    bins: bins,
    maxHz: 5000,
    sampleRate: 48000,
    fftSize: 2048,
    hop: 512,
    db: values,
    minDb: -12,
    maxDb: maxDb,
  );
}

/// The RGB of pixel ([x], [y]) in an image [width] wide.
List<int> _rgb(Uint8List px, int width, int x, int y) {
  final i = (y * width + x) * 4;
  return [px[i], px[i + 1], px[i + 2]];
}

// With a 60 dB peak the 72 dB window runs from -12 (bottom colour) to 60.
const _bottom = [14, 14, 18];
const _top = [255, 255, 255];

void main() {
  test('one column per frame paints the heat map, low frequency at the '
      'bottom', () {
    final values = <double>[60, 24, -12, double.nan, -40, 60];
    final s = _spec(2, 3, (f, b) => values[f * 3 + b]);
    final px = spectrogramPixels(s);
    expect((px.width, px.height), (2, 3));
    // Frame 0: bin 0 (bottom row) at the peak, bin 1 mid-scale, bin 2 floor.
    expect(_rgb(px.pixels, 2, 0, 2), _top);
    expect(_rgb(px.pixels, 2, 0, 1), [158, 81, 109]);
    expect(_rgb(px.pixels, 2, 0, 0), _bottom);
    // Frame 1: NaN and below-the-window read as the bottom colour.
    expect(_rgb(px.pixels, 2, 1, 2), _bottom);
    expect(_rgb(px.pixels, 2, 1, 1), _bottom);
    expect(_rgb(px.pixels, 2, 1, 0), _top);
  });

  test('a take within the cap keeps one column per frame', () {
    final r = math.Random(3);
    final s = _spec(400, 5, (_, _) => r.nextDouble() * 70 - 10);
    final capped = spectrogramPixels(s, maxColumns: 400);
    final uncapped = spectrogramPixels(s, maxColumns: 1 << 30);
    expect(capped.width, 400);
    expect(capped.pixels, uncapped.pixels);
  });

  test('past the cap, columns max-pool equal shares of the take', () {
    // One hot bin in frame 200 of 307, pooled to 100 columns: frames
    // [199, 202) form column 65.
    final s = _spec(
      307,
      4,
      (f, b) =>
          f == 200 && b == 2 ? 60 : (f == 10 && b == 0 ? double.nan : -12),
    );
    final px = spectrogramPixels(s, maxColumns: 100);
    expect((px.width, px.height), (100, 4));
    for (var c = 0; c < 100; c++) {
      for (var row = 0; row < 4; row++) {
        final hot = c == 65 && row == 4 - 1 - 2;
        expect(
          _rgb(px.pixels, 100, c, row),
          hot ? _top : _bottom,
          reason: 'column $c row $row',
        );
      }
    }
  });

  test('a take longer than the texture limit is capped to it', () {
    final s = _spec(kMaxSpectrogramImageColumns + 1000, 1, (f, _) => f % 60.0);
    final px = spectrogramPixels(s);
    expect(px.width, kMaxSpectrogramImageColumns);
    expect(px.height, 1);
    expect(kMaxSpectrogramImageColumns, lessThanOrEqualTo(16384));
  });

  testWidgets('a take is rendered once, and its handles outlive eviction', (
    tester,
  ) async {
    final r = math.Random(5);
    Spectrogram take() => _spec(50, 8, (_, _) => r.nextDouble() * 60);
    final first = take();
    await tester.runAsync(() async {
      final a = await spectrogramImageFor(first);
      final b = await spectrogramImageFor(first);
      expect(a.isCloneOf(b), isTrue, reason: 'one render, shared');
      expect((a.width, a.height), (50, 8));

      final others = [for (var i = 0; i < 5; i++) take()];
      final handles = <ui.Image>[];
      for (final s in others) {
        final h = await spectrogramImageFor(s);
        expect(h.isCloneOf(a), isFalse);
        handles.add(h);
      }
      // [first] has been evicted (the cache is smaller than six takes): the
      // handles already out still hold the image...
      expect(a.debugDisposed, isFalse);
      expect(a.width, 50);
      // ...and asking again renders it afresh.
      final c = await spectrogramImageFor(first);
      expect(c.isCloneOf(a), isFalse);

      for (final h in [a, b, c, ...handles]) {
        h.dispose();
      }
    });
  });
}
