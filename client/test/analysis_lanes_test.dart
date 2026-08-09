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
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/domain/models/audio_track.dart';
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
}
