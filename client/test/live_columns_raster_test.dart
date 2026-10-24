// The live spectrogram's incremental raster: across a monitor session —
// filling the window, scrolling, the loudest column arriving and leaving,
// voicing changes, a restart — every update must be exactly the full-window
// colouring, while columns already on screen are not coloured again.

import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/ui/core/widgets/spectrogram_painter.dart';

const _bins = 24;
const _maxColumns = 60;

void main() {
  final r = math.Random(11);
  Float32List column({double loud = 0}) => Float32List.fromList([
    for (var b = 0; b < _bins; b++) r.nextDouble() * 40 - 10 + loud,
  ]);

  test('every update is exactly the full-window colouring', () {
    final raster = LiveColumnsRaster();
    final cols = <Float32List>[];
    final active = <int>[];
    var steps = 0;
    void push(int n, {double loud = 0}) {
      for (var i = 0; i < n; i++) {
        cols.add(column(loud: loud));
        active.add(r.nextInt(3) == 0 ? 0 : 1);
        if (cols.length > _maxColumns) {
          cols.removeAt(0);
          active.removeAt(0);
        }
      }
      final got = raster.update(List.of(cols), _bins, active: List.of(active));
      final want = liveColumnsPixels(cols, _bins, active: active);
      expect(got, want, reason: 'step ${steps++} (${cols.length} columns)');
    }

    // Filling the window (it grows), then scrolling.
    for (var i = 0; i < 40; i++) {
      push(1 + r.nextInt(4));
    }
    // A loud column raises the ceiling; scrolling on until it leaves lowers
    // it again.
    push(1, loud: 40);
    for (var i = 0; i < 30; i++) {
      push(1 + r.nextInt(3));
    }
    // Several updates' worth of columns at once, and none at all.
    push(25);
    push(0);
    // The monitor restarted: a window with nothing in common with the last.
    cols.clear();
    active.clear();
    push(5);
    push(3);
  });

  test('a column already on screen is not coloured again', () {
    // Pins that the raster reuses the previous pixels: change an on-screen
    // column's bins in place (the monitor never does) without moving the
    // ceiling, and only a full repaint would notice.
    final raster = LiveColumnsRaster();
    final cols = [for (var i = 0; i < 10; i++) column()];
    final loud = column(loud: 30); // holds the ceiling
    cols.add(loud);
    raster.update(List.of(cols), _bins);
    final before = liveColumnsPixels(cols, _bins);
    cols[3].fillRange(0, _bins, -10);
    cols.add(column());
    final got = raster.update(List.of(cols), _bins);
    expect(got, isNot(liveColumnsPixels(cols, _bins)));
    // ...while the columns it did colour match.
    final w = cols.length;
    for (var row = 0; row < _bins; row++) {
      for (var ch = 0; ch < 4; ch++) {
        expect(
          got[(row * w + 3) * 4 + ch],
          before[(row * (w - 1) + 3) * 4 + ch],
        );
        final last = (row * w + w - 1) * 4 + ch;
        expect(got[last], liveColumnsPixels(cols, _bins)[last]);
      }
    }
  });

  test('the live colouring is the take colouring of the same bins', () {
    // Voiced columns use the take renderer's scale and colour map: the same
    // bins as a spectrogram (whose peak is the window's) paint identically.
    final cols = [for (var i = 0; i < 30; i++) column(loud: i == 7 ? 30 : 0)];
    var peak = double.negativeInfinity;
    final db = Float32List(cols.length * _bins);
    for (var f = 0; f < cols.length; f++) {
      for (var b = 0; b < _bins; b++) {
        db[f * _bins + b] = cols[f][b];
        peak = math.max(peak, cols[f][b]);
      }
    }
    final spec = Spectrogram(
      frames: cols.length,
      bins: _bins,
      maxHz: 5000,
      sampleRate: 48000,
      fftSize: 2048,
      hop: 512,
      db: db,
      minDb: -10,
      maxDb: peak,
    );
    expect(liveColumnsPixels(cols, _bins), spectrogramPixels(spec).pixels);
  });
}
