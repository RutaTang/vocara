import 'dart:ui' as ui;

import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/waveform_painter.dart';

void main() {
  _burstTests();

  group('visiblePeakSlice', () {
    // 1000 buckets over 180 000 ms: one bucket = 180 ms, the shape of the
    // master/bus mix envelopes (mixer_controller builds n = 1000 over the
    // whole extent) and of a take-lane clip's peak list.
    final peaks = List<double>.generate(1000, (i) => i / 1000);

    test('a window not aligned to a bucket keeps the discarded fraction as '
        'indexOffset, so the painter can anchor the slice to the ruler', () {
      // Viewport 4500..9500 over span 0..180000: 4500 ms = bucket 25.0; use
      // 4590 ms = bucket 25.5 to land mid-bucket.
      final cut = visiblePeakSlice(
        peaks,
        spanStartMs: 0,
        spanDurMs: 180000,
        visibleStartMs: 4590,
        visibleEndMs: 5590,
      );
      // floor(25.5) = 25 .. ceil(31.05...) = 32
      expect(cut.peaks.first, peaks[25]);
      expect(cut.indexOffset, closeTo(0.5, 1e-9));
      // 1000 ms window = 1000/180 buckets wide.
      expect(cut.indexSpan, closeTo(1000 / 180, 1e-9));
    });

    test('a bucket-aligned window has zero offset', () {
      final cut = visiblePeakSlice(
        peaks,
        spanStartMs: 0,
        spanDurMs: 180000,
        visibleStartMs: 1800, // exactly bucket 10
        visibleEndMs: 3600, // exactly bucket 20
      );
      expect(cut.indexOffset, closeTo(0.0, 1e-9));
      // Buckets 10..19 are in view, plus bucket 20: the painter draws each
      // peak as a vertex at its bucket's START, so the last in-view bucket
      // only gets a right edge to draw to from the vertex at or past the
      // window's end. Stopping at 19 left the last bucket blank.
      expect(cut.peaks.length, 11);
      expect(cut.indexSpan, closeTo(10.0, 1e-9));
    });

    test('a span not starting at zero re-bases the window first', () {
      // Clip spanning 5000..10000 ms with 100 peaks (50 ms each). Viewport
      // intersection 5075..6075: starts 1.5 buckets in.
      final clip = List<double>.generate(100, (i) => i / 100);
      final cut = visiblePeakSlice(
        clip,
        spanStartMs: 5000,
        spanDurMs: 5000,
        visibleStartMs: 5075,
        visibleEndMs: 6075,
      );
      expect(cut.peaks.first, clip[1]);
      expect(cut.indexOffset, closeTo(0.5, 1e-9));
      expect(cut.indexSpan, closeTo(20.0, 1e-9));
    });

    test('the window clamped at the span end still covers the last bucket', () {
      final cut = visiblePeakSlice(
        peaks,
        spanStartMs: 0,
        spanDurMs: 180000,
        visibleStartMs: 179000,
        visibleEndMs: 180000,
      );
      expect(cut.peaks.last, peaks.last);
      expect(cut.indexOffset + cut.indexSpan, lessThanOrEqualTo(1000.0 + 1e-9));
    });

    test('degenerate inputs yield an empty, harmless slice', () {
      for (final cut in [
        visiblePeakSlice(
          const [],
          spanStartMs: 0,
          spanDurMs: 1000,
          visibleStartMs: 0,
          visibleEndMs: 500,
        ),
        visiblePeakSlice(
          peaks,
          spanStartMs: 0,
          spanDurMs: 0,
          visibleStartMs: 0,
          visibleEndMs: 500,
        ),
        visiblePeakSlice(
          peaks,
          spanStartMs: 0,
          spanDurMs: 1000,
          visibleStartMs: 500,
          visibleEndMs: 500,
        ),
      ]) {
        expect(cut.peaks, isEmpty);
        expect(cut.indexOffset, 0.0);
        expect(cut.indexSpan, 1.0);
      }
    });
  });

  // What the slice is FOR: a full-amplitude clip must paint the whole box it
  // is visible in. The cut stopped one peak short of the window's right edge
  // and the painter drew nothing past the last vertex, so the strip after it
  // stayed blank — the clip's final bucket never drew, and a window inside
  // one bucket (a long take at max zoom) drew nothing at all.
  group('a sliced clip paints out to the box edges', () {
    final full = List<double>.filled(1000, 1.0);
    const size = Size(500, 40);

    Future<(int, int)> painted(
      WidgetTester tester, {
      required int spanDurMs,
      required int visibleStartMs,
      required int visibleEndMs,
    }) {
      final cut = visiblePeakSlice(
        full,
        spanStartMs: 0,
        spanDurMs: spanDurMs,
        visibleStartMs: visibleStartMs,
        visibleEndMs: visibleEndMs,
      );
      return _paintedRange(
        tester,
        WaveformPainter(
          peaks: cut.peaks,
          color: const Color(0xFFFFFFFF),
          indexOffset: cut.indexOffset,
          indexSpan: cut.indexSpan,
        ),
        size,
      );
    }

    testWidgets('a window ending mid-clip', (tester) async {
      // 180 ms buckets; 4590..5750 ms = buckets 25.5 .. 31.94.
      final (lo, hi) = await painted(
        tester,
        spanDurMs: 180000,
        visibleStartMs: 4590,
        visibleEndMs: 5750,
      );
      expect(lo, 0);
      expect(hi, size.width - 1);
    });

    testWidgets('a bucket-aligned window', (tester) async {
      final (_, hi) = await painted(
        tester,
        spanDurMs: 180000,
        visibleStartMs: 1800,
        visibleEndMs: 3600,
      );
      expect(hi, size.width - 1);
    });

    testWidgets('a window ending at the clip end draws the final bucket', (
      tester,
    ) async {
      // No peak past the clip's last one to draw to: the painter holds the
      // last peak out to the edge instead.
      final (_, hi) = await painted(
        tester,
        spanDurMs: 180000,
        visibleStartMs: 179000,
        visibleEndMs: 180000,
      );
      expect(hi, size.width - 1);
    });

    testWidgets('a window inside one bucket (a long take at max zoom)', (
      tester,
    ) async {
      // A 20-minute clip: 1200 ms buckets, viewed through a 1 s window.
      final (lo, hi) = await painted(
        tester,
        spanDurMs: 1200000,
        visibleStartMs: 600100,
        visibleEndMs: 601100,
      );
      expect(lo, 0);
      expect(hi, size.width - 1);
    });
  });
}

/// A burst shorter than one pixel's worth of peaks still draws: decimated to
/// the peak nearest each vertex, one that fell between vertices vanished.
void _burstTests() {
  testWidgets('a burst between decimated vertices still draws', (tester) async {
    final peaks = List<double>.filled(1000, 0.0);
    for (var i = 403; i <= 407; i++) {
      peaks[i] = 1.0;
    }
    final (lo, hi) = await _paintedRange(
      tester,
      WaveformPainter(peaks: peaks, color: const Color(0xFFFFFFFF)),
      const Size(100, 40),
    );
    expect(lo, inInclusiveRange(38, 42));
    expect(hi, inInclusiveRange(38, 42));
  });
}

/// The leftmost and rightmost opaque x on a row a quarter of the way down
/// [size] — inside the body of a full-amplitude waveform, above its axis.
Future<(int, int)> _paintedRange(
  WidgetTester tester,
  CustomPainter painter,
  Size size,
) async {
  final rec = ui.PictureRecorder();
  painter.paint(Canvas(rec), size);
  final picture = rec.endRecording();
  int? lo, hi;
  await tester.runAsync(() async {
    final img = await picture.toImage(size.width.ceil(), size.height.ceil());
    final data = (await img.toByteData())!;
    final y = img.height ~/ 4;
    for (var x = 0; x < img.width; x++) {
      if (data.getUint8((y * img.width + x) * 4 + 3) > 128) {
        lo ??= x;
        hi = x;
      }
    }
    img.dispose();
  });
  expect(lo, isNotNull, reason: 'nothing painted');
  return (lo!, hi!);
}
