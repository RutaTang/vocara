// The four inline per-lane analysis strips, rendered against analysis the REAL
// DSP produced — the same composition that surfaced the NaN crash in
// spectrogram_painter, applied to the lanes rather than the dock tabs.
//
// These take plain data (a track, a span, metrics, a spectrogram), so the
// interesting inputs are the degenerate SPANS as much as the degenerate audio:
// a lane whose timeline length is zero, a viewport scrolled past the end, a
// clip whose analysis is shorter than the lane it sits in. Each of those is a
// divisor or an index in the lane painters' time mapping.

import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart' show PointerDeviceKind;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/widgets/analysis_lanes.dart';
import 'package:vocara/ui/song_practice_detail/widgets/pitch_lane.dart';

Float32List _sine(int n, double f, int sr) {
  final s = Float32List(n);
  for (var i = 0; i < n; i++) {
    s[i] = 0.3 * math.sin(2 * math.pi * f * i / sr);
  }
  return s;
}

AudioTrack _track({int startMs = 0, int durationMs = 1000}) => AudioTrack(
  id: 't1',
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  audioFilePath: '/x.wav',
  durationMs: durationMs,
  startMs: startMs,
  createdAt: DateTime(2026),
);

typedef _Analysis = ({VoiceMetrics? metrics, Spectrogram? spectrogram});

_Analysis _analyzed(Float32List samples, {int sr = 48000}) => (
  metrics: computeVoiceMetrics(samples, sr),
  spectrogram: computeSpectrogram(samples, sr),
);

void main() {
  final position = ValueNotifier(const Duration(milliseconds: 250));

  setUp(() {
    final view =
        TestWidgetsFlutterBinding.instance.platformDispatcher.views.first;
    view.physicalSize = const Size(1440, 900);
    view.devicePixelRatio = 1.0;
    addTearDown(() {
      view.resetPhysicalSize();
      view.resetDevicePixelRatio();
    });
  });

  /// All four lanes over one (track, span, analysis) combination.
  Future<void> pumpLanes(
    WidgetTester tester, {
    required AudioTrack track,
    required int timelineMs,
    required _Analysis analysis,
    int viewStartMs = 0,
    bool loading = false,
  }) async {
    final lanes = <String, Widget>{
      'Pitch': PitchLane(
        track: track,
        timelineDurationMs: timelineMs,
        viewStartMs: viewStartMs,
        position: position,
        headerWidth: 180,
        loading: loading,
        metrics: analysis.metrics,
      ),
      'Spectrogram': SpectrogramLane(
        track: track,
        timelineDurationMs: timelineMs,
        viewStartMs: viewStartMs,
        position: position,
        loading: loading,
        spectrogram: analysis.spectrogram,
      ),
      'Power': PowerLane(
        track: track,
        position: position,
        loading: loading,
        spectrogram: analysis.spectrogram,
        metrics: analysis.metrics,
      ),
      'Loudness': LoudnessLane(
        track: track,
        timelineDurationMs: timelineMs,
        viewStartMs: viewStartMs,
        position: position,
        loading: loading,
        metrics: analysis.metrics,
      ),
    };
    for (final entry in lanes.entries) {
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(body: Center(child: entry.value)),
        ),
      );
      await tester.pump(const Duration(milliseconds: 32));
      expect(tester.takeException(), isNull, reason: '${entry.key} lane threw');
    }
  }

  testWidgets('a normal take renders in every lane', (tester) async {
    await pumpLanes(
      tester,
      track: _track(durationMs: 1000),
      timelineMs: 20000,
      analysis: _analyzed(_sine(48000, 220, 48000)),
    );
  });

  testWidgets('a silent take renders in every lane', (tester) async {
    await pumpLanes(
      tester,
      track: _track(durationMs: 1000),
      timelineMs: 20000,
      analysis: _analyzed(Float32List(48000)),
    );
  });

  testWidgets('audio carrying non-finite samples renders in every lane', (
    tester,
  ) async {
    final data = _sine(48000, 220, 48000);
    data[100] = double.nan;
    data[200] = double.infinity;
    data[300] = double.negativeInfinity;
    await pumpLanes(
      tester,
      track: _track(durationMs: 1000),
      timelineMs: 20000,
      analysis: _analyzed(data),
    );
  });

  testWidgets('a take stopped after one buffer renders in every lane', (
    tester,
  ) async {
    await pumpLanes(
      tester,
      track: _track(durationMs: 5),
      timelineMs: 20000,
      analysis: _analyzed(_sine(256, 220, 48000)),
    );
  });

  testWidgets('no analysis yet renders in every lane', (tester) async {
    await pumpLanes(
      tester,
      track: _track(),
      timelineMs: 20000,
      analysis: (metrics: null, spectrogram: null),
      loading: true,
    );
  });

  group('degenerate spans', () {
    final analysis = _analyzed(_sine(48000, 220, 48000));

    testWidgets('a zero-length timeline is not divided by', (tester) async {
      await pumpLanes(
        tester,
        track: _track(durationMs: 1000),
        timelineMs: 0,
        analysis: analysis,
      );
    });

    testWidgets('a zero-length TAKE is not divided by', (tester) async {
      await pumpLanes(
        tester,
        track: _track(durationMs: 0),
        timelineMs: 20000,
        analysis: analysis,
      );
    });

    testWidgets('a viewport scrolled past the end renders', (tester) async {
      await pumpLanes(
        tester,
        track: _track(durationMs: 1000),
        timelineMs: 20000,
        viewStartMs: 999999,
        analysis: analysis,
      );
    });

    testWidgets('a take starting beyond the timeline renders', (tester) async {
      await pumpLanes(
        tester,
        track: _track(startMs: 999999, durationMs: 1000),
        timelineMs: 20000,
        analysis: analysis,
      );
    });

    testWidgets('the playhead far outside the take renders', (tester) async {
      position.value = const Duration(days: 1);
      addTearDown(() => position.value = const Duration(milliseconds: 250));
      await pumpLanes(
        tester,
        track: _track(durationMs: 1000),
        timelineMs: 20000,
        analysis: analysis,
      );
    });
  });

  // The Notes / Techniques lanes add a range by dragging across them. The
  // drag used to be held in PIXELS and turned into times only on release, at
  // whatever viewStart the lane had THEN — so a follow-cursor page flip while
  // the drag was held committed a range the user never touched — and a
  // right-to-left drag handed the editors start > end, whose header then
  // read as a point ("Note at …") for a range.
  group('a range drag on a marker lane', () {
    /// A NotesLane whose view start can be moved mid-gesture, as
    /// follow-cursor does during playback.
    Future<(ValueNotifier<int>, List<(int, int?)>)> pumpNotes(
      WidgetTester tester,
    ) async {
      final viewStart = ValueNotifier(0);
      addTearDown(viewStart.dispose);
      final added = <(int, int?)>[];
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: ValueListenableBuilder<int>(
              valueListenable: viewStart,
              builder: (_, vs, _) => NotesLane(
                track: _track(durationMs: 60000),
                timelineDurationMs: 10000,
                viewStartMs: vs,
                position: position,
                onAddNote: (s, e) => added.add((s, e)),
                onEditNote: (PracticeNote _) {},
              ),
            ),
          ),
        ),
      );
      return (viewStart, added);
    }

    Future<void> drag(
      WidgetTester tester,
      Offset from,
      Offset by, {
      VoidCallback? beforeRelease,
    }) async {
      final g = await tester.startGesture(from);
      await g.moveBy(by / 2);
      await tester.pump();
      await g.moveBy(by / 2);
      await tester.pump();
      beforeRelease?.call();
      await tester.pump();
      await g.up();
      await tester.pumpAndSettle();
    }

    testWidgets('a right-to-left drag commits an ordered range', (
      tester,
    ) async {
      final (_, added) = await pumpNotes(tester);
      final lane = tester.getRect(find.byType(NotesLane));
      await drag(
        tester,
        Offset(lane.right - 100, lane.center.dy),
        const Offset(-400, 0),
      );
      final (start, end) = added.single;
      expect(end, isNotNull);
      expect(start, lessThan(end!));
    });

    testWidgets('a page flip while the drag is held commits the range that '
        'was dragged over', (tester) async {
      final (viewStart, added) = await pumpNotes(tester);
      final lane = tester.getRect(find.byType(NotesLane));
      final from = Offset(lane.center.dx - 200, lane.center.dy);
      await drag(tester, from, const Offset(240, 0));
      final dragged = added.single;

      added.clear();
      await drag(
        tester,
        from,
        const Offset(240, 0),
        // The playhead reached the window's end: follow-cursor pages on.
        beforeRelease: () => viewStart.value = 7800,
      );
      expect(added.single, dragged);
    });

    // A two-finger trackpad swipe pans the timeline. Over a marker lane it
    // reached the range drag as a pan-zoom gesture and, on lift, opened the
    // add-note (or technique) editor for a range nobody drew.
    testWidgets('a two-finger trackpad swipe adds nothing', (tester) async {
      final (_, added) = await pumpNotes(tester);
      final at = tester.getCenter(find.byType(NotesLane));
      final g = await tester.createGesture(kind: PointerDeviceKind.trackpad);
      await g.panZoomStart(at);
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(-60, 0));
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(-240, 0));
      await tester.pump();
      await g.panZoomEnd();
      await tester.pumpAndSettle();
      expect(added, isEmpty);
    });

    testWidgets('a mouse drag still adds its range', (tester) async {
      final (_, added) = await pumpNotes(tester);
      final at = tester.getCenter(find.byType(NotesLane));
      final g = await tester.startGesture(at, kind: PointerDeviceKind.mouse);
      await g.moveBy(const Offset(120, 0));
      await tester.pump();
      await g.moveBy(const Offset(120, 0));
      await tester.pump();
      await g.up();
      await tester.pumpAndSettle();
      final (start, end) = added.single;
      expect(end, greaterThan(start));
    });
  });
}
