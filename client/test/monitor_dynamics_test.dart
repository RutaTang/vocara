// The live averages and dynamics are over voiced frames. The live analyzer
// holds a phrase "active" a few frames past its last pitched one (so its
// overlay does not flicker off), and those frames of fading breath were
// counted as voice: short notes sung at one level read a ~50 dB range.

import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/monitor_service.dart';

void main() {
  test('short notes at one level read a narrow dynamic range, their tails '
      'of breath not counted', () {
    const sr = 48000;
    final rng = Random(7);
    final pcm = <double>[];
    for (var phrase = 0; phrase < 6; phrase++) {
      for (var i = 0; i < sr * 0.4; i++) {
        final t = i / sr;
        pcm.add(
          0.3 *
              (sin(2 * pi * 220 * t) +
                  0.25 * sin(2 * pi * 440 * t) +
                  0.1 * sin(2 * pi * 660 * t)),
        );
      }
      for (var i = 0; i < sr * 0.5; i++) {
        pcm.add((rng.nextDouble() * 2 - 1) * 0.001); // about -60 dBFS
      }
    }
    final monitor = MonitorService()..start();
    addTearDown(monitor.dispose);
    final all = Float32List.fromList(pcm);
    for (var at = 0; at < all.length; at += 1024) {
      monitor.feedSamples(all.sublist(at, min(at + 1024, all.length)));
    }
    final d = monitor.liveDynamics;
    expect(d.rangeDb, lessThan(3), reason: 'soft ${d.softDb} loud ${d.loudDb}');
  });
}
