// What one playhead tick repaints on the detail screen. The playhead moves at
// 60 Hz while the transport runs; every painter it drags along is paid for
// sixty times a second. Before the playheads got their own layers, a tick
// repainted the whole screen — ~1800 render objects with 30 lanes, and with a
// 10-minute take in the dock a formant overlay of ~225k dots, every frame.
// These pins hold a tick to the playheads themselves (and the transport
// clock's one paragraph), across the screen's states.

import 'dart:async';
import 'dart:convert';
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
import 'package:vocara/data/services/recording_finalizer.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/core/widgets/spectrogram_painter.dart';
import 'package:vocara/ui/core/widgets/waveform_painter.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/bottom_dock.dart';
import 'package:vocara/ui/song_practice_detail/widgets/fx_panel.dart';
import 'package:vocara/ui/song_practice_detail/widgets/ruler_bar.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo extends Fake implements SongPracticeRepository {
  _FakeRepo(this.stored, {this.reread = false});

  SongPractice stored;

  /// Answers a save as the real store does: with a re-read, every object in
  /// it new.
  final bool reread;
  int saves = 0;

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.ok(Loaded(stored, const {}));

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    saves++;
    stored = practice;
    if (!reread) return Result.ok(Loaded(stored, const {}));
    final copy = SongPractice.fromJson(
      jsonDecode(jsonEncode(practice.toJson())) as Map<String, dynamic>,
    );
    return Result.ok(Loaded(copy, const {}));
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
/// [effects] makes it the native engine's stand-in: the inspector's FX
/// panel, the mixer's MIC strip and buses show.
class _FakeEngine extends Fake implements PlaybackEngine {
  _FakeEngine({this.effects = false});

  final bool effects;

  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {}
  @override
  double trackGainReductionDb(String trackId) => 0;
  @override
  Stream<Duration> get position => const Stream.empty();
  @override
  Stream<bool> get playing => const Stream.empty();
  @override
  Duration get currentPosition => Duration.zero;
  @override
  bool get isPlaying => false;
  @override
  bool get supportsEffects => effects;
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
    fftSize: 2048,
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
  Future<TakeAnalysisBundle?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async =>
      (spectrogram: spectrogram, metrics: metrics, ltas: null, profile: null);

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
  Size size = const Size(1800, 1100),
  RecordingFinalizer? finalizer,
  bool effects = false,
  _FakeRepo? repository,
}) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final vm = SongPracticeDetailViewModel(
    repository: repository ?? _FakeRepo(_practice(lanes)),
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
    playbackEngine: _FakeEngine(effects: effects),
    editService: _FakeEdits(),
    probeDuration: (_) => null,
    finalizer: finalizer,
  );
  for (var i = 0; i < 100 && vm.practice == null; i++) {
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

/// How many widgets of each type the frame after [change] rebuilt.
Future<Map<String, int>> _rebuildCountsBy(
  WidgetTester tester,
  VoidCallback change,
) async {
  await tester.pump(const Duration(milliseconds: 16));
  final rebuilt = <String, int>{};
  debugOnRebuildDirtyWidget = (e, _) => rebuilt.update(
    e.widget.runtimeType.toString(),
    (n) => n + 1,
    ifAbsent: () => 1,
  );
  try {
    change();
    await tester.pump(const Duration(milliseconds: 16));
  } finally {
    debugOnRebuildDirtyWidget = null;
  }
  return rebuilt;
}

/// How many times each painter type painted in the frame after [change].
Future<Map<String, int>> _paintsBy(
  WidgetTester tester,
  VoidCallback change,
) async {
  await tester.pump(const Duration(milliseconds: 16));
  final painted = <String, int>{};
  debugOnProfilePaint = (ro) {
    if (ro is RenderCustomPaint && ro.painter != null) {
      painted.update(
        '${ro.painter.runtimeType}',
        (n) => n + 1,
        ifAbsent: () => 1,
      );
    }
  };
  try {
    change();
    await tester.pump(const Duration(milliseconds: 16));
  } finally {
    debugOnProfilePaint = null;
  }
  return painted;
}

/// The widget types rebuilt by the frame after [change].
Future<Set<String>> _rebuiltBy(WidgetTester tester, VoidCallback change) async {
  await tester.pump(const Duration(milliseconds: 16));
  final rebuilt = <String>{};
  debugOnRebuildDirtyWidget = (e, _) =>
      rebuilt.add(e.widget.runtimeType.toString());
  try {
    change();
    await tester.pump(const Duration(milliseconds: 16));
  } finally {
    debugOnRebuildDirtyWidget = null;
  }
  return rebuilt;
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

  // Every notification of the screen rebuilds the dock's tab, and its
  // LayoutBuilder's re-layout repainted up to the nearest boundary: a fader
  // tick on any lane re-recorded the formant overlay (~225k dots for a
  // 10-minute take) at pointer rate.
  testWidgets('a fader tick leaves the dock spectrogram and its formant '
      'overlay alone', (tester) async {
    final vm = await _pumpScreen(tester);
    vm.selectTrack('t0');
    vm.analysis.showDockTab(DockTab.spectrogram);
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    expect(vm.selectedMetrics, isNotNull, reason: 'the take should analyze');
    expect(find.byType(SpectrogramImage), findsOneWidget);

    var v = 0.5;
    final painted = await _paintsBy(
      tester,
      () => vm.mixer.setTrackVolumeLive('t3', v += 0.01),
    );
    expect(painted['WaveformPainter'], 1, reason: 'the tick must land');
    expect(painted, isNot(contains('FormantTrackPainter')));
    expect(painted, isNot(contains('FrequencyGridPainter')));
    expect(painted, isNot(contains('_SpectrogramImagePainter')));
  });

  // Reuses this file's screen harness; not a repaint pin. The two halves of a
  // clip's analysis are separate cache entries, and under the byte budget's
  // pressure the spectrogram (read before the metrics) went first: nothing
  // asked for it again, and the dock said the take could not be analyzed.
  testWidgets('the dock asks again for a spectrogram the cache let go', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester);
    vm.selectTrack('t0');
    vm.analysis.showDockTab(DockTab.spectrogram);
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    expect(vm.selectedSpectrogram, isNotNull);

    vm.analysis.debugDropHalf('/pool/t0.wav', metrics: false);
    // Any notification rebuilds the dock (here a fader tick on another lane).
    vm.mixer.setTrackVolumeLive('t3', 0.4);
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    expect(vm.selectedSpectrogram, isNotNull);
    expect(find.byType(SpectrogramImage), findsOneWidget);
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

  testWidgets('changing the VIEW rebuilds the timeline, not the workspace', (
    tester,
  ) async {
    // Pan, zoom, follow and loop-edge drags arrive at trackpad, drag or
    // playhead rate; forwarded through the view model, each one rebuilt the
    // whole workspace — both side panels, the dock, the transport bar and
    // every lane's header (1326 -> 549 widgets per pan over 30 lanes).
    final vm = await _pumpScreen(tester);
    final playback = vm.playback;
    const elsewhere = ['LeftPanel', 'RightPanel', 'BottomDock'];
    final changes = <String, VoidCallback>{
      'zoom': () => playback.setTimelineZoomLive(0.6),
      'pan': () => playback.panView(2000),
      'follow': playback.toggleFollowCursor,
      'loop edge': () => playback.setLoopEnd(playback.loopEndMs - 500),
    };
    for (final MapEntry(key: name, value: change) in changes.entries) {
      final rebuilt = await _rebuiltBy(tester, change);
      if (name == 'loop edge') {
        // The loop band is the timeline's, over the lanes: no lane shows
        // anything of it, so each keeps the row it built.
        expect(rebuilt, isNot(contains('TrackRow')), reason: name);
      } else {
        expect(
          rebuilt,
          contains('TrackRow'),
          reason: '$name: the lanes follow',
        );
      }
      expect(rebuilt, contains('RulerBar'), reason: '$name: the ruler follows');
      // Nor the lane headers: nothing they show depends on the view.
      for (final w in [...elsewhere, 'DetailTransportBar', '_TrackHeader']) {
        expect(rebuilt, isNot(contains(w)), reason: '$name rebuilt $w');
      }
    }
    // A page-flip of the follow-cursor scroll, driven by the playhead.
    final span = playback.viewSpanMs;
    final flip = await _rebuiltBy(
      tester,
      () => playback.setRecordingPosition(
        Duration(milliseconds: playback.viewStartMs + span),
      ),
    );
    expect(flip, contains('TrackRow'));
    for (final w in elsewhere) {
      expect(flip, isNot(contains(w)), reason: 'a follow page-flip rebuilt $w');
    }
  });

  // A live edit — a fader, pan-knob or FX tick, a clip drag — notifies the
  // view model at pointer rate, and every notification rebuilds the
  // workspace. Each one used to rebuild every lane, header and body, and
  // repaint every lane's waveforms and the ruler's labels: 913 widgets and
  // 1857 render objects per tick over 12 three-clip lanes. Only what the edit
  // changed may now rebuild or repaint.
  testWidgets('a fader tick rebuilds and repaints its own lane alone', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester);
    var v = 0.5;
    void fader() => vm.mixer.setTrackVolumeLive('t3', v += 0.01);
    final rebuilt = await _rebuildCountsBy(tester, fader);
    expect(rebuilt['TrackRow'], 1, reason: 'the other lanes are kept');
    expect(rebuilt['_TrackHeader'], 1, reason: 'its fader moved');
    expect(rebuilt['_ClipRegion'], 1, reason: 'its body follows its track');
    final painted = await _paintsBy(tester, fader);
    expect(
      painted['WaveformPainter'],
      2,
      reason: "the lane's clip, and the master envelope its level scales",
    );
    expect(painted, isNot(contains('TimeRulerPainter')));
    expect(painted['TimelineGridPainter'], 1, reason: 'its own lane');
    // The edit itself still lands on screen.
    expect(vm.practice!.vocalTracks[3].volume, closeTo(v, 1e-9));
  });

  // A save is answered with a re-read of the whole practice, every object in
  // it new; taking that in rebuilt every lane and repainted every waveform
  // after each fader release, mute or rename. What the re-read reproduces
  // keeps the objects it was sent as.
  testWidgets("a save's re-read rebuilds and repaints only what the edit "
      'changed', (tester) async {
    SongPractice roundTrip(SongPractice p) => SongPractice.fromJson(
      jsonDecode(jsonEncode(p.toJson())) as Map<String, dynamic>,
    );
    // Through JSON once first: that gives the takes their interpretation.
    final repo = _FakeRepo(roundTrip(_practice(8)), reread: true);
    expect(
      roundTrip(repo.stored),
      repo.stored,
      reason: 'the stand-in re-read must reproduce what it was sent',
    );
    final vm = await _pumpScreen(tester, repository: repo);
    final untouched = vm.practice!.vocalTracks[1];

    final rebuilt = await _rebuildCountsBy(
      tester,
      () => unawaited(vm.mixer.toggleMute('t3')),
    );
    expect(repo.saves, 1, reason: 'the save must have landed');
    expect(vm.practice!.vocalTracks[3].muted, isTrue);
    expect(rebuilt['TrackRow'], 1, reason: 'the other lanes are kept');
    expect(identical(vm.practice!.vocalTracks[1], untouched), isTrue);

    final painted = await _paintsBy(
      tester,
      () => unawaited(vm.mixer.toggleMute('t3')),
    );
    expect(repo.saves, 2);
    expect(
      painted['WaveformPainter'],
      lessThanOrEqualTo(2),
      reason: "the lane's clip, and the master envelope its mute changes",
    );
  });

  testWidgets('a pan repaints the lanes\' waveforms, not their headers', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester);
    vm.playback.setTimelineZoomLive(0.6);
    vm.playback.panView(5000);
    await tester.pump(const Duration(milliseconds: 16));
    final painted = await _paintsBy(tester, () => vm.playback.panView(300));
    expect(painted['WaveformPainter'], greaterThan(1), reason: 'they moved');
    expect(painted, isNot(contains('_FaderPainter')), reason: 'they did not');
  });

  testWidgets('a mixer fader tick rebuilds its own channel strip alone', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester);
    vm.analysis.showDockTab(DockTab.mixer);
    await tester.pump(const Duration(milliseconds: 50));
    var v = 0.5;
    final rebuilt = await _rebuildCountsBy(
      tester,
      () => vm.mixer.setTrackVolumeLive('t3', v += 0.01),
    );
    expect(rebuilt['_Channel'], 1);
    expect(rebuilt['_VerticalFader'], 1, reason: 'nor the master strip');
    expect(rebuilt, isNot(contains('_Tab')), reason: 'the tab strip is kept');
  });

  testWidgets('the inspector keeps its FX panel through another lane\'s '
      'edits, and follows its own', (tester) async {
    final vm = await _pumpScreen(tester, effects: true);
    vm.selectTrack('t0');
    await tester.pump(const Duration(milliseconds: 50));
    expect(find.byType(FxPanel), findsOneWidget);
    var v = 0.5;
    var rebuilt = await _rebuildCountsBy(
      tester,
      () => vm.mixer.setTrackVolumeLive('t3', v += 0.01),
    );
    expect(rebuilt, isNot(contains('FxPanel')));
    expect(rebuilt, isNot(contains('_MicInputChannel')));

    final fx = TrackFx.flat();
    rebuilt = await _rebuildCountsBy(
      tester,
      () => vm.mixer.setTrackFxLive('t0', fx.copyWith(compOn: true)),
    );
    expect(rebuilt['FxPanel'], 1, reason: 'its own take changed');
  });

  // The dock's height was clamped to a fixed range, not to the window: on a
  // window shorter than the dock at its tallest (any laptop screen), dragging
  // the dock up — or shrinking the window under a tall dock — overflowed the
  // workspace, collapsed the timeline to nothing and cut off the dock's
  // bottom.
  testWidgets('a dock taller than the window leaves the timeline whole', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester, size: const Size(1800, 700));
    vm.analysis.showDockTab(DockTab.analysisSummary);
    vm.analysis.setDockHeight(760);
    await tester.pump(const Duration(milliseconds: 50));

    expect(tester.takeException(), isNull);
    expect(
      tester.getSize(find.byType(RulerBar)).height,
      greaterThan(0),
      reason: 'the timeline keeps its place',
    );
    final dock = tester.getRect(find.byType(BottomDock));
    expect(dock.bottom, lessThanOrEqualTo(700));
    expect(dock.top, greaterThan(tester.getRect(find.byType(RulerBar)).bottom));
  });

  testWidgets('a dock resize counts every move, even several in one frame', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester);
    vm.analysis.showDockTab(DockTab.analysisSummary);
    await tester.pump(const Duration(milliseconds: 50));
    final grabber = find.byWidgetPredicate(
      (w) => w is MouseRegion && w.cursor == SystemMouseCursors.resizeUpDown,
    );
    final g = await tester.startGesture(tester.getCenter(grabber));
    await g.moveBy(const Offset(0, -30)); // wins the gesture
    await tester.pump();
    final before = vm.analysis.dockHeight;

    // A fast mouse: two moves land before the dock rebuilds.
    await g.moveBy(const Offset(0, -10));
    await g.moveBy(const Offset(0, -10));
    await tester.pump();
    await g.up();
    expect(vm.analysis.dockHeight, before + 20);
  });

  // Reuses this file's screen harness; not a repaint pin.
  testWidgets('a reopen behind a landing save shows the workspace but takes '
      'no input until it lands', (tester) async {
    final finalizer = RecordingFinalizer.forTesting();
    final save = Completer<void>();
    unawaited(finalizer.landing('p1', () => save.future));
    final vm = await _pumpScreen(tester, finalizer: finalizer);
    bool absorbing() => tester
        .widget<AbsorbPointer>(
          find
              .ancestor(
                of: find.byType(BottomDock),
                matching: find.byType(AbsorbPointer),
              )
              .first,
        )
        .absorbing;

    expect(vm.catchingUp, isTrue);
    expect(find.byType(BottomDock), findsOneWidget, reason: 'shown at once');
    expect(absorbing(), isTrue, reason: 'an edit here would lose a save');
    expect(find.text('Finishing your last save…'), findsOneWidget);

    save.complete();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(vm.catchingUp, isFalse);
    expect(absorbing(), isFalse);
    expect(find.text('Finishing your last save…'), findsNothing);
  });
}
