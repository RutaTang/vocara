// What one playhead tick repaints on the detail screen. The playhead moves at
// 60 Hz while the transport runs; every painter it drags along is paid for
// sixty times a second. Before the playheads got their own layers, a tick
// repainted the whole screen — ~1800 render objects with 30 lanes, and with a
// 10-minute take in the dock a formant overlay of ~225k dots, every frame.
// These pins hold a tick to the playheads themselves (and the transport
// clock's one paragraph), across the screen's states.

import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_edit_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/core/widgets/waveform_painter.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo extends Fake implements SongPracticeRepository {
  _FakeRepo(this.stored);

  SongPractice stored;

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.ok(Loaded(stored, const {}));

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    stored = practice;
    return Result.ok(Loaded(stored, const {}));
  }
}

class _FakeTechRepo extends Fake implements TechniqueRepository {}

class _FakeFiles extends Fake implements AudioFileService {}

class _FakeWaveforms extends Fake implements WaveformService {}

class _FakeEdits extends Fake implements AudioEditService {}

class _FakeSeparation extends Fake implements SourceSeparationService {
  @override
  Future<bool> isAvailable() async => false;
}

/// Just enough engine for the controllers to wire up without native audio.
class _FakeEngine extends Fake implements PlaybackEngine {
  @override
  Stream<Duration> get position => const Stream.empty();
  @override
  Stream<bool> get playing => const Stream.empty();
  @override
  Duration get currentPosition => Duration.zero;
  @override
  bool get isPlaying => false;
  @override
  bool get supportsEffects => false;
  @override
  void setLoop(bool enabled, int startMs, int endMs) {}
  @override
  Future<void> sync(
    List<TrackPlayback> tracks,
    List<BusPlayback> buses,
  ) async {}
  @override
  Future<void> play() async {}
  @override
  Future<void> pause() async {}
  @override
  Future<void> seek(Duration position) async {}
  @override
  Future<void> setVolume(
    String id,
    double volume, {
    bool muted = false,
  }) async {}
  @override
  void setTrackPan(String id, double pan) {}
  @override
  void setMasterVolume(double gain) {}
  @override
  void setTrackFx(String trackId, TrackFx fx) {}
  @override
  void setReverb(ReverbSettings reverb) {}
  @override
  Future<void> dispose() async {}
}

const int _takeMs = 60000;
const int _sr = 48000, _hop = 512;
const int _frames = _takeMs * _sr ~/ 1000 ~/ _hop;
const int _bins = 215;

/// A minute of synthetic analysis: a note every ~0.5 s, mostly voiced.
VoiceMetrics _metrics() {
  Float32List curve(double Function(int) f) =>
      Float32List.fromList([for (var i = 0; i < _frames; i++) f(i)]);
  bool voiced(int i) => i % 47 < 33;
  double hz(int i) => voiced(i)
      ? 220 * math.pow(2, (i ~/ 47) % 12 / 12) * (1 + 0.01 * math.sin(i / 3))
      : double.nan;
  return VoiceMetrics(
    frames: _frames,
    hop: _hop,
    sampleRate: _sr,
    ring: curve((i) => math.sin(i / 50)),
    tilt: curve((i) => math.cos(i / 70)),
    cpp: curve((i) => 10 + math.sin(i / 30)),
    loudness: curve((i) => -30 + 10 * math.sin(i / 40)),
    f0: curve(hz),
    f1: curve((i) => voiced(i) ? 500 + 100 * math.sin(i / 20) : double.nan),
    f2: curve((i) => voiced(i) ? 1500 + 300 * math.sin(i / 25) : double.nan),
    f3: curve((i) => voiced(i) ? 2600 + 200 * math.sin(i / 35) : double.nan),
    h1h2: curve((i) => voiced(i) ? 5 * math.sin(i / 60) : double.nan),
    active: Uint8List.fromList([
      for (var i = 0; i < _frames; i++) voiced(i) ? 1 : 0,
    ]),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 10,
  );
}

Spectrogram _spectrogram() {
  final r = math.Random(1);
  return Spectrogram(
    frames: _frames,
    bins: _bins,
    maxHz: 5000,
    sampleRate: _sr,
    fftSize: 2048,
    hop: _hop,
    db: Float32List.fromList([
      for (var i = 0; i < _frames * _bins; i++) r.nextDouble() * 60,
    ]),
    minDb: 0,
    maxDb: 60,
  );
}

class _FakeSpectrograms extends Fake implements SpectrogramService {
  final spectrogram = _spectrogram();
  final metrics = _metrics();

  @override
  Future<TakeAnalysis?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async => (spectrogram: spectrogram, metrics: metrics);

  @override
  Future<VoiceMetrics?> computeMetricsFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
  }) async => metrics;

  @override
  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async => spectrogram;
}

SongPractice _practice(int lanes) {
  final created = DateTime(2026);
  List<double> peaks(int seed) {
    final r = math.Random(seed);
    return [for (var i = 0; i < 1000; i++) r.nextDouble()];
  }

  return SongPractice(
    id: 'p1',
    title: 'Song',
    createdAt: created,
    updatedAt: created,
    accompaniment: AudioTrack(
      id: 'acc',
      kind: AudioTrackKind.accompaniment,
      displayName: 'Backing',
      audioFilePath: '/pool/acc.wav',
      durationMs: _takeMs,
      waveformPeaks: peaks(0),
      createdAt: created,
    ),
    vocalTracks: [
      for (var i = 0; i < lanes; i++)
        AudioTrack(
          id: 't$i',
          kind: AudioTrackKind.vocal,
          displayName: 'Take ${i + 1}',
          audioFilePath: '/pool/t$i.wav',
          durationMs: _takeMs,
          waveformPeaks: peaks(i + 1),
          createdAt: created,
        ),
    ],
  );
}

/// Pumps the real detail screen over fakes and returns its view model.
Future<SongPracticeDetailViewModel> _pumpScreen(
  WidgetTester tester, {
  int lanes = 8,
}) async {
  tester.view.physicalSize = const Size(1800, 1100);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final vm = SongPracticeDetailViewModel(
    repository: _FakeRepo(_practice(lanes)),
    techniqueRepository: _FakeTechRepo(),
    audioFileService: _FakeFiles(),
    waveformService: _FakeWaveforms(),
    spectrogramService: _FakeSpectrograms(),
    separationService: _FakeSeparation(),
    appSettings: AppSettingsService(
      storage: LocalStorageService(),
      enumerateInputs: () => const [],
      enumerateOutputs: () => const [],
    ),
    practiceId: 'p1',
    playbackEngine: _FakeEngine(),
    editService: _FakeEdits(),
    probeDuration: (_) => null,
  );
  for (var i = 0; i < 50 && vm.practice == null; i++) {
    await tester.pump(const Duration(milliseconds: 5));
  }
  expect(vm.practice, isNotNull, reason: 'the practice should load');
  // The lyrics header overflows under the test font; the panel's content is
  // not what these pins are about.
  vm.toggleLyricsPanel();
  await tester.pumpWidget(
    MaterialApp(
      home: ChangeNotifierProvider.value(
        value: vm,
        child: const SongPracticeDetailScreen(),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 50));
  addTearDown(() async {
    await tester.pumpWidget(const SizedBox());
    vm.dispose();
  });
  return vm;
}

/// The painters and paragraphs painted while the playhead ticks [ticks]
/// times — one frame per tick, the playhead inside a stable viewport and
/// inside one second of the transport clock (its `mm:ss` changes once a
/// second, and re-laying-out that text is not a per-frame cost).
Future<({Map<Type, int> painters, int paragraphs})> _paintedPerTicks(
  WidgetTester tester,
  ValueNotifier<Duration> position, {
  int ticks = 10,
  int startMs = 20100,
}) async {
  // Settle first-time layouts and the frame the ticks start from.
  position.value = Duration(milliseconds: startMs - 16);
  await tester.pump(const Duration(milliseconds: 16));
  final painters = <Type, int>{};
  var paragraphs = 0;
  debugOnProfilePaint = (ro) {
    if (ro is RenderCustomPaint) {
      for (final p in [ro.painter, ro.foregroundPainter]) {
        if (p != null) {
          painters.update(p.runtimeType, (n) => n + 1, ifAbsent: () => 1);
        }
      }
    } else if (ro is RenderParagraph) {
      paragraphs++;
    }
  };
  try {
    for (var i = 0; i < ticks; i++) {
      position.value = Duration(milliseconds: startMs + i * 16);
      await tester.pump(const Duration(milliseconds: 16));
    }
  } finally {
    debugOnProfilePaint = null;
  }
  return (painters: painters, paragraphs: paragraphs);
}

void main() {
  testWidgets('a playhead tick repaints only the playheads (dock closed)', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester);
    final r = await _paintedPerTicks(tester, vm.position);
    expect(r.painters.keys, everyElement(PlayheadPainter));
    expect(
      r.painters[PlayheadPainter],
      greaterThan(0),
      reason: 'the ticks must actually move the playheads',
    );
    // The transport clock is the one paragraph a tick may repaint.
    expect(r.paragraphs, lessThanOrEqualTo(10));
  });

  testWidgets('a playhead tick leaves the dock spectrogram and its formant '
      'overlay alone', (tester) async {
    final vm = await _pumpScreen(tester);
    vm.selectTrack('t0');
    vm.analysis.showDockTab(DockTab.spectrogram);
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    expect(vm.selectedMetrics, isNotNull, reason: 'the take should analyze');

    final r = await _paintedPerTicks(tester, vm.position);
    expect(r.painters.keys, everyElement(PlayheadPainter));
    expect(r.paragraphs, lessThanOrEqualTo(10));
  });

  testWidgets('the Pitch and Timbre tabs repaint only their playheads per '
      'tick', (tester) async {
    final vm = await _pumpScreen(tester);
    vm.selectTrack('t0');
    vm.analysis.showDockTab(DockTab.pitch);
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    expect(vm.selectedMetrics, isNotNull);
    // The pitch header's note readout used to rebuild every tick under the
    // tab's LayoutBuilder, re-laying-out (so repainting) the whole screen.
    var r = await _paintedPerTicks(tester, vm.position);
    expect(r.painters.keys, everyElement(PlayheadPainter));

    // The metrics bar's chips are content-sized, so they re-lay-out on each
    // tick; the timbre curves (every frame of the take) must not repaint.
    vm.analysis.showDockTab(DockTab.timbre);
    await tester.pump(const Duration(milliseconds: 16));
    r = await _paintedPerTicks(tester, vm.position);
    expect(r.painters.keys, everyElement(PlayheadPainter));
  });

  testWidgets('an expanded take repaints its playheads and the power '
      'spectrum per tick, nothing else', (tester) async {
    final vm = await _pumpScreen(tester);
    vm.analysis.toggleTrackExpanded('t1');
    vm.analysis.toggleLane('t1', AnalysisLane.loudness);
    vm.analysis.toggleLane('t1', AnalysisLane.power);
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    expect(vm.analysis.metricsFor('t1'), isNotNull);
    expect(vm.analysis.spectrogramFor('t1'), isNotNull);

    final r = await _paintedPerTicks(tester, vm.position);
    expect(
      r.painters.keys.map((t) => '$t'),
      everyElement(isIn(['PlayheadPainter', '_PlayheadPowerPainter'])),
    );
    expect(
      r.painters.keys.map((t) => '$t'),
      contains('_PlayheadPowerPainter'),
      reason: 'the power spectrum follows the playhead',
    );
  });

  testWidgets('a recording tick repaints the master envelope and the '
      'playheads, nothing else', (tester) async {
    // Few lanes, so the master lane is on screen.
    final vm = await _pumpScreen(tester, lanes: 2);
    final peaks = <double>[];
    void tick(int i) {
      peaks.add(0.5);
      vm.recording.recordingPeaks.value = List.unmodifiable(peaks);
      vm.position.value = Duration(milliseconds: 20100 + i * 50);
    }

    tick(0);
    await tester.pump(const Duration(milliseconds: 50));
    final painters = <Type, int>{};
    debugOnProfilePaint = (ro) {
      if (ro is RenderCustomPaint && ro.painter != null) {
        painters.update(
          ro.painter!.runtimeType,
          (n) => n + 1,
          ifAbsent: () => 1,
        );
      }
    };
    try {
      for (var i = 1; i <= 10; i++) {
        tick(i);
        await tester.pump(const Duration(milliseconds: 50));
      }
    } finally {
      debugOnProfilePaint = null;
    }
    expect(
      painters.keys,
      everyElement(isIn([PlayheadPainter, WaveformPainter])),
    );
    // One master envelope per tick: the lanes' own waveforms stay put.
    expect(painters[WaveformPainter], 10);
  });

  testWidgets('the transport clock turning over a second repaints only the '
      'transport bar', (tester) async {
    final vm = await _pumpScreen(tester);
    final r = await _paintedPerTicks(tester, vm.position, startMs: 19950);
    expect(
      r.painters.keys.map((t) => '$t'),
      everyElement(isIn(['PlayheadPainter', '_ZoomSliderPainter'])),
    );
  });

  testWidgets('every playhead sits on its own layer', (tester) async {
    final vm = await _pumpScreen(tester);
    vm.selectTrack('t0');
    vm.analysis.showDockTab(DockTab.spectrogram);
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));

    final playheads = tester.allRenderObjects
        .whereType<RenderCustomPaint>()
        .where((r) => r.painter is PlayheadPainter)
        .toList();
    // Ruler + backing + every take lane + the dock spectrogram.
    expect(playheads.length, greaterThanOrEqualTo(4));
    for (final r in playheads) {
      RenderObject? up = r.parent;
      while (up != null && !up.isRepaintBoundary) {
        expect(
          up,
          isA<RenderIgnorePointer>(),
          reason: 'only the pointer shield may share the playhead layer',
        );
        up = up.parent;
      }
      expect(up, isNotNull);
    }
  });
}
