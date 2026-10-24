// The timeline's analysis lane painters (pitch notes + contour, loudness
// envelope) map the whole take onto the lane through the zoomed viewport, so
// most of a long take lands outside the box. These pin what that leaves on
// screen: nothing outside the lane, and — now that the painters walk only the
// frames that can reach the box — exactly the pixels the whole-take walk drew.
// The same for the dock's formant dots, drawn as point batches instead of one
// drawCircle per frame.

import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/core/themes/studio.dart';
import 'package:vocara/ui/core/widgets/spectrogram_painter.dart';
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
    f1: curve((i) => voiced(i) ? 500 + 100 * math.sin(i / 20) : nan),
    f2: curve((i) => voiced(i) ? 1500 + 300 * math.sin(i / 25) : nan),
    f3: curve((i) => voiced(i) ? 2600 + 200 * math.sin(i / 35) : nan),
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

/// The pitch lane exactly as it painted before windowing (clip included):
/// every note and every contour frame of the take, as a frozen reference.
class _WholeTakePitch extends CustomPainter {
  _WholeTakePitch(this.p);

  final PitchLanePainter p;

  static double _hzToMidi(double hz) =>
      69 + 12 * (math.log(hz / 440) / math.ln2);

  static String _centsStr(double cents) {
    final c = cents.round();
    return '${c >= 0 ? '+' : '−'}${c.abs()}¢';
  }

  void _label(Canvas canvas, String text, double x, double y, Color color) {
    final tp = TextPainter(
      text: TextSpan(
        text: text,
        style: TextStyle(color: color, fontSize: 9),
      ),
      textDirection: TextDirection.ltr,
    )..layout();
    tp.paint(canvas, Offset(x, y - tp.height / 2));
  }

  @override
  void paint(Canvas canvas, Size size) {
    final dur = p.timelineDurationMs;
    if (dur <= 0 || p.hiSemi <= p.loSemi) return;
    canvas.clipRect(Offset.zero & size);
    final span = (p.hiSemi - p.loSemi).toDouble();
    double yOf(double semi) => (1 - (semi - p.loSemi) / span) * size.height;
    double xOf(double ms) => (ms - p.viewStartMs) / dur * size.width;
    double xOfFrame(int fr) =>
        xOf(p.startMs + p.metrics.timeOfFrame(fr) * 1000);
    final grid = Paint()..strokeWidth = 1;
    for (var s = p.loSemi; s <= p.hiSemi; s++) {
      final isC = s % 12 == 0;
      grid.color = isC ? Studio.gridStrong : Studio.grid.withValues(alpha: 0.5);
      final y = yOf(s.toDouble());
      canvas.drawLine(Offset(0, y), Offset(size.width, y), grid);
      if (isC) _label(canvas, 'C${(s ~/ 12) - 1}', 3, y, Studio.textFaint);
    }
    final blockH = size.height / span * 0.62;
    for (var i = 0; i < p.notes.length; i++) {
      final n = p.notes[i];
      final x0 = xOfFrame(n.startFrame);
      final x1 = math.max(xOfFrame(n.endFrame), x0 + 2);
      final yc = yOf(n.semitone.toDouble());
      final rrect = RRect.fromRectAndRadius(
        Rect.fromLTRB(x0, yc - blockH / 2, x1, yc + blockH / 2),
        const Radius.circular(3),
      );
      final col = Color.lerp(
        Studio.inTune,
        Studio.record,
        (n.medianCents.abs() / 50).clamp(0.0, 1.0),
      )!;
      canvas.drawRRect(rrect, Paint()..color = col.withValues(alpha: 0.26));
      final isWorst = p.worst.contains(i);
      canvas.drawRRect(
        rrect,
        Paint()
          ..color = isWorst ? Studio.record : col.withValues(alpha: 0.7)
          ..style = PaintingStyle.stroke
          ..strokeWidth = isWorst ? 1.6 : 1,
      );
      if (isWorst) {
        var ly = yc - blockH / 2 - 11;
        if (ly < 1) ly = yc + blockH / 2 + 2;
        _label(canvas, _centsStr(n.medianCents), x0 + 2, ly + 5, Studio.record);
      }
    }
    final cline = Paint()
      ..color = Studio.textPrimary.withValues(alpha: 0.85)
      ..strokeWidth = 1.3
      ..strokeCap = StrokeCap.round;
    for (final n in p.notes) {
      double? px, py;
      for (var fr = n.startFrame; fr <= n.endFrame; fr++) {
        final hz = p.metrics.f0[fr];
        if (hz.isNaN || hz <= 0) {
          px = null;
          continue;
        }
        final x = xOfFrame(fr);
        final y = yOf(_hzToMidi(hz));
        if (px != null) canvas.drawLine(Offset(px, py!), Offset(x, y), cline);
        px = x;
        py = y;
      }
    }
  }

  @override
  bool shouldRepaint(_WholeTakePitch old) => true;
}

/// The loudness lane exactly as it painted before windowing (clip included).
class _WholeTakeLoudness extends CustomPainter {
  _WholeTakeLoudness(this.p);

  final LoudnessLanePainter p;

  @override
  void paint(Canvas canvas, Size size) {
    final dur = p.timelineDurationMs;
    final m = p.metrics;
    if (dur <= 0 || m.frames == 0) return;
    canvas.clipRect(Offset.zero & size);
    var peak = -1e9;
    for (final v in m.loudness) {
      if (v.isFinite && v > peak) peak = v;
    }
    if (peak <= -1e8) return;
    final floor = peak - 48;
    double xOf(double ms) => (ms - p.viewStartMs) / dur * size.width;
    double yOf(double db) =>
        (1 - ((db - floor) / 48).clamp(0.0, 1.0)) * size.height;
    final area = Path();
    final line = Path();
    var started = false;
    var any = false;
    var lastX = 0.0;
    void closeSegment() {
      if (!started) return;
      area
        ..lineTo(lastX, size.height)
        ..close();
      started = false;
    }

    for (var fr = 0; fr < m.frames; fr++) {
      final db = m.loudness[fr];
      if (!db.isFinite) {
        closeSegment();
        continue;
      }
      final x = xOf(p.startMs + m.timeOfFrame(fr) * 1000);
      final y = yOf(db);
      if (!started) {
        area.moveTo(x, size.height);
        area.lineTo(x, y);
        line.moveTo(x, y);
        started = true;
        any = true;
      } else {
        area.lineTo(x, y);
        line.lineTo(x, y);
      }
      lastX = x;
    }
    if (!any) return;
    closeSegment();
    canvas.drawPath(
      area,
      Paint()..color = Studio.accent.withValues(alpha: 0.18),
    );
    canvas.drawPath(
      line,
      Paint()
        ..color = Studio.accent.withValues(alpha: 0.85)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
  }

  @override
  bool shouldRepaint(_WholeTakeLoudness old) => true;
}

/// The formant overlay as it painted before: one drawCircle per voiced frame.
class _PerFrameFormants extends CustomPainter {
  _PerFrameFormants(this.metrics, this.maxHz);

  final VoiceMetrics metrics;
  final double maxHz;

  @override
  void paint(Canvas canvas, Size size) {
    final frames = metrics.frames;
    void track(Float32List f, Color color, double radius) {
      final paint = Paint()..color = color.withValues(alpha: 0.9);
      for (var fr = 0; fr < frames; fr++) {
        final hz = f[fr];
        if (hz.isNaN || hz <= 0 || hz > maxHz) continue;
        final x = (fr + 0.5) / frames * size.width;
        final y = (1 - hz / maxHz) * size.height;
        canvas.drawCircle(Offset(x, y), radius, paint);
      }
    }

    track(metrics.f1, formantColors[0], 1.1);
    track(metrics.f2, formantColors[1], 1.1);
    track(metrics.f3, formantColors[2], 1.1);
    track(metrics.f0, f0Color, 1.4);
  }

  @override
  bool shouldRepaint(_PerFrameFormants old) => true;
}

/// How many bytes of two equally-sized rasters differ.
int _diff(ByteData a, ByteData b) {
  expect(a.lengthInBytes, b.lengthInBytes);
  var n = 0;
  for (var i = 0; i < a.lengthInBytes; i++) {
    if (a.getUint8(i) != b.getUint8(i)) n++;
  }
  return n;
}

void main() {
  final m = _metrics(5625); // one minute at 48 kHz / 512
  final notes = segmentPitchNotes(m);
  final range = pitchSemitoneRange(m);
  const size = Size(300, 96);

  // The flagged notes carry a cents label right of their start.
  final worst = {for (var i = 0; i < notes.length; i += 5) i};

  // A 5-second window in the middle of the minute by default.
  PitchLanePainter pitch({
    int durMs = 5000,
    int viewStartMs = 25000,
    int startMs = 0,
  }) => PitchLanePainter(
    metrics: m,
    notes: notes,
    worst: worst,
    timelineDurationMs: durMs,
    startMs: startMs,
    viewStartMs: viewStartMs,
    loSemi: range.lo,
    hiSemi: range.hi,
  );
  LoudnessLanePainter loudness({
    int durMs = 5000,
    int viewStartMs = 25000,
    int startMs = 0,
  }) => LoudnessLanePainter(
    metrics: m,
    timelineDurationMs: durMs,
    startMs: startMs,
    viewStartMs: viewStartMs,
  );

  // Windows across the take: the whole of it, deep zoom (a 1-second window),
  // either end, a window cutting a note and a loudness gap, a take placed
  // later on the timeline, and a window past the take's end.
  const windows = [
    (durMs: 70000, viewStartMs: 0, startMs: 0),
    (durMs: 1000, viewStartMs: 30017, startMs: 0),
    (durMs: 5000, viewStartMs: 0, startMs: 0),
    (durMs: 4000, viewStartMs: 57000, startMs: 0),
    (durMs: 2300, viewStartMs: 9411, startMs: 0),
    (durMs: 6000, viewStartMs: 41000, startMs: 12000),
    (durMs: 5000, viewStartMs: 64000, startMs: 0),
  ];

  for (final w in windows) {
    testWidgets('the windowed pitch lane draws exactly the whole-take pixels '
        '($w)', (tester) async {
      final painter = pitch(
        durMs: w.durMs,
        viewStartMs: w.viewStartMs,
        startMs: w.startMs,
      );
      final got = await _raster(tester, painter, size);
      final want = await _raster(tester, _WholeTakePitch(painter), size);
      expect(_diff(got.bytes, want.bytes), 0);
    });

    testWidgets('the windowed loudness lane draws exactly the whole-take '
        'pixels ($w)', (tester) async {
      final painter = loudness(
        durMs: w.durMs,
        viewStartMs: w.viewStartMs,
        startMs: w.startMs,
      );
      final got = await _raster(tester, painter, size);
      final want = await _raster(tester, _WholeTakeLoudness(painter), size);
      expect(_diff(got.bytes, want.bytes), 0);
    });
  }

  testWidgets('the formant dots are the same pixels as one circle per frame', (
    tester,
  ) async {
    const dock = Size(700, 220);
    final got = await _raster(
      tester,
      FormantTrackPainter(metrics: m, maxHz: 5000),
      dock,
    );
    final want = await _raster(tester, _PerFrameFormants(m, 5000), dock);
    var painted = 0;
    for (var i = 3; i < want.bytes.lengthInBytes; i += 4) {
      if (want.bytes.getUint8(i) != 0) painted++;
    }
    expect(painted, greaterThan(1000), reason: 'the overlay draws dots');
    expect(_diff(got.bytes, want.bytes), 0);
  });

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
