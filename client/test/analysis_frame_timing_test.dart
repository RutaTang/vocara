// Where an analysis frame sits in time. Frame f analyses the fftSize-sample
// window starting at sample f·hop, so what it measures is the audio around
// the window's CENTRE — f·hop + fftSize/2. Placing frames at their window
// START made every analysis view lead the waveform by half a window (~21 ms
// at 2048 / 48 kHz): a note began early on the pitch lane, the loudness swell
// and the spectrogram ridge arrived before the audio, and the playhead
// readouts showed a frame two hops ahead of the sound under it.
//
// So these pin each layer to the centre: the shared mapping itself, a real
// tone burst's onset as the DSP sees it, the lanes that draw it, the dock
// spectrogram's columns and dots and the seek they share an axis with, and
// the readouts under the playhead.

import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/monitor_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/core/widgets/spectrogram_painter.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_source.dart';
import 'package:vocara/ui/song_practice_detail/widgets/analysis_lanes.dart';
import 'package:vocara/ui/song_practice_detail/widgets/pitch_lane.dart';
import 'package:vocara/ui/song_practice_detail/widgets/voiceprint_view.dart';

const int _sr = 48000;
const int _hop = 512;
const int _fft = 2048;

/// One hop in ms — the grid every onset is quantized to.
const double _hopMs = _hop / _sr * 1000;

/// The tone burst's onset, and the take's length.
const int _onsetMs = 500;
const int _takeMs = 1200;

/// Silence, then a steady 220 Hz tone from exactly [_onsetMs].
Float32List _burst() {
  final n = _takeMs * _sr ~/ 1000;
  final onset = _onsetMs * _sr ~/ 1000;
  final s = Float32List(n);
  for (var i = onset; i < n; i++) {
    s[i] = 0.3 * math.sin(2 * math.pi * 220 * (i - onset) / _sr);
  }
  return s;
}

/// The window-centre time of frame [f] for the production framing, spelled
/// out rather than read back through the mapping under test.
double _centreSeconds(int f) => (f * _hop + _fft / 2) / _sr;

/// Records the draw calls the lane painters make, so their geometry can be
/// read without rasterizing.
class _RecordingCanvas implements Canvas {
  final rrects = <RRect>[];
  final lines = <(Offset, Offset)>[];
  final paths = <(Path, Paint)>[];
  final dots = <(Offset, double radius)>[];
  final imageRects = <(Rect src, Rect dst)>[];

  @override
  void drawRRect(RRect rrect, Paint paint) => rrects.add(rrect);

  @override
  void drawLine(Offset p1, Offset p2, Paint paint) => lines.add((p1, p2));

  @override
  void drawPath(Path path, Paint paint) => paths.add((path, paint));

  // The formant overlay draws each track as one batch of round points, a
  // strokeWidth wide.
  @override
  void drawRawPoints(ui.PointMode mode, Float32List points, Paint paint) {
    for (var i = 0; i + 1 < points.length; i += 2) {
      dots.add((Offset(points[i], points[i + 1]), paint.strokeWidth / 2));
    }
  }

  @override
  void drawImageRect(ui.Image image, Rect src, Rect dst, Paint paint) =>
      imageRects.add((src, dst));

  // Text labels, grid rects, … are irrelevant to the time axis.
  @override
  dynamic noSuchMethod(Invocation invocation) => null;
}

/// The first x along [path] at or above the height [y] (smaller y = higher),
/// walking it in quarter-pixel steps.
double _firstXAtOrAbove(Path path, double y) {
  for (final metric in path.computeMetrics()) {
    for (var d = 0.0; d <= metric.length; d += 0.25) {
      final p = metric.getTangentForOffset(d)!.position;
      if (p.dy <= y) return p.dx;
    }
  }
  return double.nan;
}

AudioTrack _track(int durationMs) => AudioTrack(
  id: 't1',
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  audioFilePath: '/x.wav',
  durationMs: durationMs,
  createdAt: DateTime(2026),
);

class _Source extends ChangeNotifier implements AnalysisSource {
  _Source({required this.metrics, this.spectrogram, required this.durationMs});

  final VoiceMetrics metrics;
  final Spectrogram? spectrogram;
  final int durationMs;

  @override
  AudioTrack? get selectedTrack => _track(durationMs);

  @override
  int get analysisStartMs => 0;

  @override
  int get analysisDurationMs => durationMs;

  @override
  VoiceMetrics? get selectedMetrics => metrics;

  @override
  Spectrogram? get selectedSpectrogram => spectrogram;

  @override
  VoiceProfile? get selectedProfile => analyzeVoiceProfile(metrics);

  @override
  Ltas? get selectedLtas => null;

  @override
  RegisterMap? get selectedRegister => analyzeRegister(metrics);

  @override
  bool get spectrogramLoading => false;

  @override
  bool get liveMonitor => false;

  @override
  MonitorService get monitor => throw StateError('not live');

  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  @override
  Future<void> seek(Duration to) async => position.value = to;
}

/// Hand-built metrics on the production framing whose readouts change at a
/// frame boundary: frames 0..19 sing A3 (F1 300 Hz), frames 20..39 sing C4
/// (F1 750 Hz). A playhead two hops early or late reads the wrong side.
VoiceMetrics _stepMetrics() {
  const n = 40;
  Float32List curve(double lo, double hi) =>
      Float32List.fromList([for (var f = 0; f < n; f++) f < 20 ? lo : hi]);
  final nan = Float32List(n)..fillRange(0, n, double.nan);
  return VoiceMetrics(
    frames: n,
    hop: _hop,
    fftSize: _fft,
    sampleRate: _sr,
    ring: curve(-10, -10),
    tilt: curve(-5, -5),
    cpp: curve(10, 10),
    loudness: curve(-20, -20),
    f0: curve(220, 261.6256),
    f1: curve(300, 750),
    f2: curve(2300, 1150),
    f3: nan,
    h1h2: nan,
    active: Uint8List(n)..fillRange(0, n, 1),
    ringAvg: -10,
    tiltAvg: -5,
    cppAvg: 10,
  );
}

/// The first frame at which [hit] holds.
int _firstFrame(int frames, bool Function(int f) hit) {
  for (var f = 0; f < frames; f++) {
    if (hit(f)) return f;
  }
  return -1;
}

/// Lets the take image render (on a worker isolate, then a decode — both off
/// the fake clock) and the [SpectrogramImage] rebuild with it.
Future<void> _untilImageShows(WidgetTester tester) async {
  final painted = find.descendant(
    of: find.byType(SpectrogramImage),
    matching: find.byType(CustomPaint),
  );
  for (var i = 0; i < 100 && painted.evaluate().isEmpty; i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 20)),
    );
    await tester.pump();
  }
  expect(painted, findsOneWidget, reason: 'the take image rendered');
}

/// The dock is a wide, short strip; lay the widget tests out on a realistic
/// window rather than the 800x600 default the tabs overflow.
void _wideWindow() {
  final view =
      TestWidgetsFlutterBinding.instance.platformDispatcher.views.first;
  view.physicalSize = const Size(1440, 900);
  view.devicePixelRatio = 1.0;
  addTearDown(() {
    view.resetPhysicalSize();
    view.resetDevicePixelRatio();
  });
}

void main() {
  final samples = _burst();
  final m = computeVoiceMetrics(samples, _sr);
  final spec = computeSpectrogram(samples, _sr);

  group('the shared frame↔time mapping', () {
    test('a frame sits at its window centre, not its start', () {
      expect(m.fftSize, _fft);
      for (final a in <AnalysisFraming>[m, spec]) {
        expect(a.timeOfFrame(0), closeTo(_centreSeconds(0), 1e-12));
        expect(a.timeOfFrame(45), closeTo(_centreSeconds(45), 1e-12));
        // A frame's cell is hop-wide and centred on it.
        expect(
          a.cellStart(45),
          closeTo(_centreSeconds(45) - _hop / 2 / _sr, 1e-12),
        );
        expect(
          a.cellEnd(45),
          closeTo(_centreSeconds(45) + _hop / 2 / _sr, 1e-12),
        );
      }
    });

    test('a playhead at a frame centre reads that frame', () {
      final bins = spec.bins;
      for (var f = 0; f < m.frames; f++) {
        final t = _centreSeconds(f);
        expect(m.frameAtTime(t), f);
        expect(spec.frameAtTime(t), f);
        expect(m.sampleAt(m.loudness, t), m.loudness[f]);
        expect(
          spec.columnAtTime(t),
          orderedEquals(spec.db.sublist(f * bins, f * bins + bins)),
        );
      }
      // Anywhere inside the cell still reads it; times off either end clamp.
      expect(m.frameAtTime(_centreSeconds(30) + 0.4 * _hop / _sr), 30);
      expect(m.frameAtTime(_centreSeconds(30) - 0.4 * _hop / _sr), 30);
      expect(m.frameAtTime(0), 0);
      expect(m.frameAtTime(100), m.frames - 1);
    });

    test(
      'a tone burst at 500 ms reads its onset at 500 ms in every analysis',
      () {
        // Pitch: the first voiced frame.
        final voiced = _firstFrame(m.frames, (f) => m.f0[f].isFinite);
        // Loudness: the first frame within 3 dB of the steady tone — the frame
        // whose window is at least half tone, i.e. whose CENTRE is past onset.
        final steady = m.loudness[m.frames ~/ 2 + 20];
        final loud = _firstFrame(
          m.frames,
          (f) => m.loudness[f] >= steady - 3.0103,
        );
        // Spectrogram: the first column within 6 dB of the steady tone at its
        // bin (a Hann window half-filled with tone carries half the amplitude).
        final bin = (220 / spec.binHz).round();
        final steadyDb = spec.valueAt(spec.frames ~/ 2 + 20, bin);
        final col = _firstFrame(
          spec.frames,
          (f) => spec.valueAt(f, bin) >= steadyDb - 6.0206,
        );
        for (final (what, ms) in [
          ('pitch', m.timeOfFrame(voiced) * 1000),
          ('loudness', m.timeOfFrame(loud) * 1000),
          ('spectrogram', spec.timeOfFrame(col) * 1000),
        ]) {
          expect(
            ms,
            closeTo(_onsetMs, _hopMs),
            reason: '$what onset at $ms ms',
          );
        }
      },
    );

    test('a merged lane keeps every clip frame at its own window centre', () {
      // The same burst as a second clip 1.5 s into the lane: each of its frames
      // reads, on the lane axis, its clip offset plus its clip-local centre.
      final merged = mergeVoiceMetrics([
        (offsetMs: 0, metrics: m),
        (offsetMs: 1500, metrics: m),
      ])!;
      final mergedSpec = mergeSpectrograms([
        (offsetMs: 0, spectrogram: spec),
        (offsetMs: 1500, spectrogram: spec),
      ])!;
      final off = mergeFrameOf(1500, _sr, _hop);
      for (final f in [0, 45, m.frames - 1]) {
        final lane = 1.5 + m.timeOfFrame(f);
        // Within the merge's own half-hop offset quantization.
        expect(merged.timeOfFrame(off + f), closeTo(lane, _hop / 2 / _sr));
        expect(mergedSpec.timeOfFrame(off + f), merged.timeOfFrame(off + f));
        expect(merged.loudness[off + f], m.loudness[f]);
      }
    });
  });

  group('the views draw frames at their centres', () {
    // 1 px per ms: x reads directly as a time.
    const lane = Size(_takeMs + 0.0, 96);

    test('the pitch lane starts the note — block and contour — at 500 ms', () {
      final canvas = _RecordingCanvas();
      final notes = segmentPitchNotes(m);
      expect(notes, hasLength(1));
      PitchLanePainter(
        metrics: m,
        notes: notes,
        worst: const {},
        timelineDurationMs: _takeMs,
        startMs: 0,
        loSemi: 50,
        hiSemi: 62,
      ).paint(canvas, lane);
      expect(canvas.rrects.first.left, closeTo(_onsetMs, _hopMs));
      expect(
        canvas.rrects.first.left,
        closeTo(m.timeOfFrame(notes.single.startFrame) * 1000, 1e-6),
      );
      // The contour's first segment begins on the same frame, and its last
      // ends on the last voiced frame's centre — not half a window early.
      // (The semitone grid lines all start at x = 0.)
      final contour = canvas.lines.where((l) => l.$1.dx > 0);
      expect(contour.first.$1.dx, closeTo(_onsetMs, _hopMs));
      expect(
        contour.last.$2.dx,
        closeTo(m.timeOfFrame(notes.single.endFrame) * 1000, 1e-6),
      );
    });

    test('the loudness lane swells at 500 ms', () {
      final canvas = _RecordingCanvas();
      LoudnessLanePainter(
        metrics: m,
        timelineDurationMs: _takeMs,
        startMs: 0,
      ).paint(canvas, lane);
      final line = canvas.paths
          .firstWhere((p) => p.$2.style == PaintingStyle.stroke)
          .$1;
      // The painter auto-ranges 48 dB under the take's peak (the steady tone);
      // find where the envelope first comes within 3 dB of it.
      final y3 = 3.0103 / 48 * lane.height;
      expect(_firstXAtOrAbove(line, y3), closeTo(_onsetMs, _hopMs));
    });
  });

  group('the dock spectrogram', () {
    setUp(_wideWindow);

    testWidgets('columns, dots, the seek and the readout share one axis', (
      tester,
    ) async {
      final src = _Source(metrics: m, spectrogram: spec, durationMs: _takeMs);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: SizedBox(height: 300, child: SpectrogramTab(vm: src)),
          ),
        ),
      );
      await _untilImageShows(tester);

      final imageFinder = find.byType(SpectrogramImage);
      final box = tester.renderObject<RenderBox>(imageFinder);
      final w = box.size.width;
      double msOfX(double x) => x / w * _takeMs;

      // The image: column c's middle is frame c's centre on the axis.
      final imagePaint = tester.widget<CustomPaint>(
        find.descendant(of: imageFinder, matching: find.byType(CustomPaint)),
      );
      final canvas = _RecordingCanvas();
      imagePaint.painter!.paint(canvas, box.size);
      final dst = canvas.imageRects.single.$2;
      final colW = dst.width / spec.frames;
      double columnMidMs(int c) => msOfX(dst.left + (c + 0.5) * colW);
      for (final c in [0, 45, spec.frames - 1]) {
        expect(
          columnMidMs(c),
          closeTo(spec.timeOfFrame(c) * 1000, 0.01),
          reason: 'column $c',
        );
      }

      // The overlay: every F0 dot sits on its frame's column, the first on
      // the burst's onset.
      final overlay = tester
          .widgetList<CustomPaint>(find.byType(CustomPaint))
          .map((p) => p.painter)
          .whereType<FormantTrackPainter>()
          .single;
      final dots = _RecordingCanvas();
      overlay.paint(dots, box.size);
      // F0 dots are the larger ones (1.4 px, formants 1.1 px).
      final f0Dots = [
        for (final d in dots.dots)
          if ((d.$2 - 1.4).abs() < 1e-6) d.$1.dx,
      ];
      final voiced = [
        for (var f = 0; f < m.frames; f++)
          if (m.f0[f].isFinite && m.f0[f] > 0) f,
      ];
      expect(f0Dots, hasLength(voiced.length));
      expect(msOfX(f0Dots.first), closeTo(_onsetMs, _hopMs));
      for (var i = 0; i < voiced.length; i++) {
        expect(msOfX(f0Dots[i]), closeTo(columnMidMs(voiced[i]), 0.01));
      }

      // The seek: tapping a column's middle lands the playhead on that
      // column's frame, and the readouts read it back.
      final x = dst.left + (45 + 0.5) * colW;
      await tester.tapAt(box.localToGlobal(Offset(x, box.size.height / 2)));
      await tester.pump();
      final at = src.position.value.inMilliseconds / 1000;
      expect(at, closeTo(spec.timeOfFrame(45), 0.001));
      expect(spec.frameAtTime(at), 45);
      expect(m.frameAtTime(at), 45);
    });

    testWidgets('a max-pooled column sits over the frames it pools', (
      tester,
    ) async {
      // Past the texture cap, image column c holds frames
      // [c·frames/w, (c+1)·frames/w) — and must be drawn where THOSE frames'
      // centres fall on the axis, like an unpooled column.
      const frames = 20000, hot = 60;
      final db = Float32List(frames * 2)..fillRange(0, frames * 2, -12);
      db[hot * 2] = 60;
      final s = Spectrogram(
        frames: frames,
        bins: 2,
        maxHz: 5000,
        sampleRate: _sr,
        fftSize: _fft,
        hop: _hop,
        db: db,
        minDb: -12,
        maxDb: 60,
      );
      // The take the analyzer gets [frames] frames from.
      const takeMs = ((frames - 1) * _hop + _fft) * 1000 ~/ _sr;
      await tester.pumpWidget(
        Center(
          child: SizedBox(
            width: 700,
            height: 40,
            child: SpectrogramImage(spectrogram: s, timelineDurationMs: takeMs),
          ),
        ),
      );
      await _untilImageShows(tester);

      final imageFinder = find.byType(SpectrogramImage);
      final box = tester.renderObject<RenderBox>(imageFinder);
      final canvas = _RecordingCanvas();
      tester
          .widget<CustomPaint>(
            find.descendant(
              of: imageFinder,
              matching: find.byType(CustomPaint),
            ),
          )
          .painter!
          .paint(canvas, box.size);
      final (src, dst) = canvas.imageRects.single;
      final w = src.width.toInt();
      expect(w, kMaxSpectrogramImageColumns);
      final c = [
        for (var c = 0; c < w; c++)
          if (c * frames ~/ w <= hot && hot < (c + 1) * frames ~/ w) c,
      ].single;
      double msOf(double x) => x / box.size.width * takeMs;
      final colW = dst.width / w;
      // The pooled column spans its frames' cells, give or take the half
      // frame its integer frame bounds round off: the hot frame's window
      // centre falls inside it.
      expect(
        _centreSeconds(hot) * 1000,
        inInclusiveRange(
          msOf(dst.left + c * colW) - _hopMs / 2,
          msOf(dst.left + (c + 1) * colW),
        ),
      );
    });
  });

  group('the readouts under the playhead', () {
    setUp(_wideWindow);
    final step = _stepMetrics();
    // The last A3 frame's centre (224 ms): a start-anchored mapping reads two
    // hops ahead, into the C4 frames.
    final atLastA3 = Duration(
      milliseconds: (_centreSeconds(19) * 1000).round(),
    );

    testWidgets('the pitch lane names the note the frame under it sings', (
      tester,
    ) async {
      final position = ValueNotifier(atLastA3);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: PitchLane(
              track: _track(500),
              timelineDurationMs: 500,
              position: position,
              headerWidth: 180,
              loading: false,
              metrics: step,
            ),
          ),
        ),
      );
      expect(find.text('A3 +0¢'), findsOneWidget);
      // Inside the LAST frame's cell, past its centre, is still over the take.
      position.value = Duration(
        milliseconds: (step.cellEnd(39) * 1000).floor(),
      );
      await tester.pump();
      expect(find.text('C4 +0¢'), findsOneWidget);
    });

    testWidgets('the timbre bar and the vowel chart read the same frame', (
      tester,
    ) async {
      final src = _Source(metrics: step, durationMs: 500)
        ..position.value = atLastA3;
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: SizedBox(height: 260, child: TimbreTab(vm: src)),
          ),
        ),
      );
      expect(find.text('220 Hz'), findsOneWidget);

      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: SizedBox(height: 260, child: VowelTab(vm: src)),
          ),
        ),
      );
      final vowel = tester
          .widgetList<CustomPaint>(find.byType(CustomPaint))
          .map((p) => p.painter)
          .whereType<VowelSpacePainter>()
          .single;
      expect(vowel.f1, 300);
    });
  });
}
