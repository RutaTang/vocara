// The ten AnalysisSource-driven dock views, rendered against analysis the
// REAL DSP
// produced from degenerate audio.
//
// analysis_edge_inputs_test already pins that the DSP survives a take stopped
// after one buffer, a muted input, a DC-offset interface and non-finite
// samples. What it cannot say is whether the views survive what the DSP hands
// back for those: no voiced frame at all, one frame, an all-NaN f0 curve, a
// spectrogram with a single column. Those are the inputs that divide by
// frames-1, index [0], or normalise by a range of zero.
//
// So this composes the two layers rather than hand-building fixtures: the
// metrics here are exactly the ones a user would get from a silent take.

import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/monitor_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_source.dart';
import 'package:vocara/ui/song_practice_detail/widgets/voice_profile_view.dart';
import 'package:vocara/ui/song_practice_detail/widgets/voiceprint_view.dart';

class _Source extends ChangeNotifier implements AnalysisSource {
  _Source({
    this.track,
    this.metrics,
    this.spectrogram,
    this.profile,
    this.ltas,
    this.register,
    this.loading = false,
    this.durationMs = 1000,
  });

  final AudioTrack? track;
  final VoiceMetrics? metrics;
  final Spectrogram? spectrogram;
  final VoiceProfile? profile;
  final Ltas? ltas;
  final RegisterMap? register;
  final bool loading;
  final int durationMs;

  @override
  AudioTrack? get selectedTrack => track;

  @override
  int get analysisStartMs => 0;

  @override
  int get analysisDurationMs => durationMs;

  @override
  VoiceMetrics? get selectedMetrics => metrics;

  @override
  Spectrogram? get selectedSpectrogram => spectrogram;

  @override
  VoiceProfile? get selectedProfile => profile;

  @override
  Ltas? get selectedLtas => ltas;

  @override
  RegisterMap? get selectedRegister => register;

  @override
  bool get spectrogramLoading => loading;

  @override
  bool get liveMonitor => false;

  @override
  MonitorService get monitor =>
      throw StateError('a non-live source must never reach the monitor');

  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  @override
  Future<void> seek(Duration to) async => position.value = to;
}

Float32List _sine(int n, double f, int sr) {
  final s = Float32List(n);
  for (var i = 0; i < n; i++) {
    s[i] = 0.3 * math.sin(2 * math.pi * f * i / sr);
  }
  return s;
}

AudioTrack _track(int durationMs) => AudioTrack(
  id: 't1',
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  audioFilePath: '/x.wav',
  durationMs: durationMs,
  createdAt: DateTime(2026),
);

/// A source carrying the analysis the real DSP produces for [samples].
_Source _analyzed(Float32List samples, {int sr = 48000}) {
  final metrics = computeVoiceMetrics(samples, sr);
  final spec = computeSpectrogram(samples, sr);
  final ms = (samples.length / sr * 1000).round();
  return _Source(
    track: _track(ms),
    metrics: metrics,
    spectrogram: spec,
    profile: analyzeVoiceProfile(metrics),
    ltas: computeLtas(spec, metrics),
    register: analyzeRegister(metrics),
    durationMs: ms,
  );
}

void main() {
  setUp(() {
    // The dock is a wide, short strip; give it a realistic one so the tabs
    // lay out the way they ship rather than overflowing a 800x600 default.
    final view =
        TestWidgetsFlutterBinding.instance.platformDispatcher.views.first;
    view.physicalSize = const Size(1440, 900);
    view.devicePixelRatio = 1.0;
    addTearDown(() {
      view.resetPhysicalSize();
      view.resetDevicePixelRatio();
    });
  });

  Future<void> pumpTabs(WidgetTester tester, _Source src) async {
    // Every tab, one at a time: a tab that throws must not be masked by a
    // sibling, and the error text names which one.
    final tabs = <String, Widget Function()>{
      'Spectrogram': () => SpectrogramTab(vm: src),
      'Power': () => PowerTab(vm: src),
      'Vowel': () => VowelTab(vm: src),
      'Pitch': () => PitchTab(vm: src),
      'Timbre': () => TimbreTab(vm: src),
      'LTAS': () => LtasTab(vm: src),
      'Vibrato': () => VibratoTab(vm: src),
      'Register': () => RegisterTab(vm: src),
      // Same AnalysisSource contract, hosted by the same dock (and by the
      // technique screen's analysis section).
      'Range': () => RangeView(vm: src),
      'Dynamics': () => DynamicsView(vm: src),
    };
    for (final entry in tabs.entries) {
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(body: SizedBox(height: 260, child: entry.value())),
        ),
      );
      await tester.pump(const Duration(milliseconds: 32));
      expect(tester.takeException(), isNull, reason: '${entry.key} tab threw');
    }
  }

  testWidgets('every tab renders a normal take', (tester) async {
    await pumpTabs(tester, _analyzed(_sine(48000, 220, 48000)));
  });

  testWidgets('every tab renders a SILENT take (no voiced frame)', (
    tester,
  ) async {
    // The muted-input case: metrics exist, but nothing is voiced, so every
    // per-frame curve is empty of usable values and the aggregates are NaN.
    final src = _analyzed(Float32List(48000));
    expect(src.metrics!.frames, greaterThan(0));
    await pumpTabs(tester, src);
  });

  testWidgets('every tab renders a take stopped after one buffer', (
    tester,
  ) async {
    await pumpTabs(tester, _analyzed(_sine(256, 220, 48000)));
  });

  testWidgets('every tab renders a single-sample take', (tester) async {
    await pumpTabs(tester, _analyzed(_sine(1, 220, 48000)));
  });

  testWidgets('every tab renders a DC take', (tester) async {
    await pumpTabs(
      tester,
      _analyzed(Float32List.fromList(List.filled(48000, 1.0))),
    );
  });

  testWidgets('every tab renders audio carrying non-finite samples', (
    tester,
  ) async {
    final data = _sine(48000, 220, 48000);
    data[100] = double.nan;
    data[200] = double.infinity;
    data[300] = double.negativeInfinity;
    await pumpTabs(tester, _analyzed(data));
  });

  testWidgets('every tab renders with NOTHING selected', (tester) async {
    await pumpTabs(tester, _Source());
  });

  testWidgets('every tab renders while the analysis is still computing', (
    tester,
  ) async {
    await pumpTabs(tester, _Source(track: _track(1000), loading: true));
  });

  testWidgets('a subject with a zero-length span does not divide by it', (
    tester,
  ) async {
    // The views map the playhead onto the subject by dividing by its span.
    final base = _analyzed(_sine(4800, 220, 48000));
    await pumpTabs(
      tester,
      _Source(
        track: _track(0),
        metrics: base.metrics,
        spectrogram: base.spectrogram,
        profile: base.profile,
        ltas: base.ltas,
        register: base.register,
        durationMs: 0,
      ),
    );
  });
}
