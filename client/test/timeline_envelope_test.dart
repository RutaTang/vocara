// The master and bus lanes draw one envelope bucketed over the whole timeline,
// sliced to the view. While a take runs on past the timeline's end the view
// reaches past it too (the extent follows the record head until the take is
// saved), and an envelope still spread over the whole lane put every peak of
// the song at the wrong time — stretched right, against the lanes, ruler and
// playhead above it.

import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';

/// The x-center of the topmost painted row of [painter] at [size] — with a
/// single full-amplitude spike among zero peaks, the spike's drawn position.
Future<double> _spikeX(
  WidgetTester tester,
  CustomPainter painter,
  Size size,
) async {
  final rec = ui.PictureRecorder();
  painter.paint(Canvas(rec), size);
  final picture = rec.endRecording();
  double? found;
  await tester.runAsync(() async {
    final img = await picture.toImage(size.width.ceil(), size.height.ceil());
    final data = (await img.toByteData())!;
    for (var y = 0; y < img.height && found == null; y++) {
      int? lo, hi;
      for (var x = 0; x < img.width; x++) {
        if (data.getUint8((y * img.width + x) * 4 + 3) > 128) {
          lo ??= x;
          hi = x;
        }
      }
      if (lo != null) found = (lo + hi!) / 2;
    }
    img.dispose();
  });
  expect(found, isNotNull, reason: 'nothing painted');
  return found!;
}

void main() {
  // A 20 s timeline in 1000 buckets (20 ms each), loud only at 10 s.
  const ext = 20000;
  final peaks = [for (var i = 0; i < 1000; i++) i == 500 ? 1.0 : 0.0];
  const size = Size(600, 40);

  testWidgets('the fit-all view spreads the envelope over the lane', (
    tester,
  ) async {
    final p = timelineEnvelopePainter(
      peaks,
      extentMs: ext,
      viewStartMs: 0,
      viewSpanMs: ext,
      color: Colors.white,
    );
    expect(await _spikeX(tester, p, size), closeTo(300, 3));
  });

  testWidgets('a view reaching past the timeline end keeps peaks on time', (
    tester,
  ) async {
    // Recording on past the end: the fit-all view now spans 30 s.
    final p = timelineEnvelopePainter(
      peaks,
      extentMs: ext,
      viewStartMs: 0,
      viewSpanMs: 30000,
      color: Colors.white,
    );
    expect(await _spikeX(tester, p, size), closeTo(10000 / 30000 * 600, 3));
  });

  testWidgets('a zoomed view paged past the end keeps peaks on time', (
    tester,
  ) async {
    final spiked = [for (var i = 0; i < 1000; i++) i == 850 ? 1.0 : 0.0];
    final p = timelineEnvelopePainter(
      spiked,
      extentMs: ext,
      viewStartMs: 15000,
      viewSpanMs: 10000,
      color: Colors.white,
    );
    // Bucket 850 starts at 17 000 ms: 2 s into the 10 s window.
    expect(await _spikeX(tester, p, size), closeTo(2000 / 10000 * 600, 3));
  });
}
