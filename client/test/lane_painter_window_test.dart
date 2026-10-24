// The timeline's analysis lane painters (pitch notes + contour, loudness
// envelope) map the whole take onto the lane through the zoomed viewport, so
// most of a long take lands outside the box. These pin what that leaves on
// screen: nothing outside the lane.

import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/widgets/analysis_lanes.dart';
import 'package:vocara/ui/song_practice_detail/widgets/pitch_lane.dart';

const int _sr = 48000, _hop = 512;

/// [frames] of synthetic analysis: a note every ~0.5 s with vibrato, mostly
/// voiced, and a NaN gap (as between the clips of a multi-clip take) in the
/// loudness curve.
VoiceMetrics _metrics(int frames) {
  Float32List curve(double Function(int) f) =>
      Float32List.fromList([for (var i = 0; i < frames; i++) f(i)]);
  bool voiced(int i) => i % 47 < 33;
  final nan = double.nan;
  return VoiceMetrics(
    frames: frames,
    hop: _hop,
    sampleRate: _sr,
    ring: curve((_) => 0),
    tilt: curve((_) => 0),
    cpp: curve((_) => 0),
    loudness: curve((i) => i % 900 > 860 ? nan : -30 + 10 * math.sin(i / 40)),
    f0: curve(
      (i) => voiced(i)
          ? 220 * math.pow(2, (i ~/ 47) % 12 / 12) * (1 + 0.01 * math.sin(i))
          : nan,
    ),
    f1: curve((_) => nan),
    f2: curve((_) => nan),
    f3: curve((_) => nan),
    h1h2: curve((_) => nan),
    active: Uint8List.fromList([
      for (var i = 0; i < frames; i++) voiced(i) ? 1 : 0,
    ]),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
  );
}

/// Rasterizes [painter] at [size], drawn [inset] px in from each side of a
/// wider canvas, and returns the RGBA bytes.
Future<({ByteData bytes, int width})> _raster(
  WidgetTester tester,
  CustomPainter painter,
  Size size, {
  double inset = 0,
}) async {
  final width = (size.width + 2 * inset).ceil();
  final rec = ui.PictureRecorder();
  final canvas = Canvas(rec);
  canvas.translate(inset, 0);
  painter.paint(canvas, size);
  final picture = rec.endRecording();
  late ByteData bytes;
  await tester.runAsync(() async {
    final img = await picture.toImage(width, size.height.ceil());
    bytes = (await img.toByteData())!;
    img.dispose();
  });
  picture.dispose();
  return (bytes: bytes, width: width);
}

void main() {
  final m = _metrics(5625); // one minute at 48 kHz / 512
  final notes = segmentPitchNotes(m);
  final range = pitchSemitoneRange(m);
  const size = Size(300, 96);

  // A 5-second window in the middle of the minute.
  PitchLanePainter pitch() => PitchLanePainter(
    metrics: m,
    notes: notes,
    worst: const {},
    timelineDurationMs: 5000,
    startMs: 0,
    viewStartMs: 25000,
    loSemi: range.lo,
    hiSemi: range.hi,
  );
  LoudnessLanePainter loudness() => LoudnessLanePainter(
    metrics: m,
    timelineDurationMs: 5000,
    startMs: 0,
    viewStartMs: 25000,
  );

  for (final (name, make) in [
    ('pitch', () => pitch() as CustomPainter),
    ('loudness', () => loudness() as CustomPainter),
  ]) {
    testWidgets('a zoomed $name lane paints nothing outside its box', (
      tester,
    ) async {
      const inset = 100.0;
      final r = await _raster(tester, make(), size, inset: inset);
      var inside = 0, outside = 0;
      for (var y = 0; y < size.height; y++) {
        for (var x = 0; x < r.width; x++) {
          final alpha = r.bytes.getUint8((y * r.width + x) * 4 + 3);
          if (alpha == 0) continue;
          if (x < inset || x >= inset + size.width) {
            outside++;
          } else {
            inside++;
          }
        }
      }
      expect(inside, greaterThan(0), reason: 'the window shows the take');
      expect(outside, 0, reason: 'the rest of the take stays off screen');
    });
  }
}
