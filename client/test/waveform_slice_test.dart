import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/waveform_painter.dart';

void main() {
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
      expect(cut.peaks.length, 10);
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
}
