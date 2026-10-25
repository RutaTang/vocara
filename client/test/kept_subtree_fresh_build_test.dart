// The detail screen keeps unchanged subtrees across rebuilds (StableSubtree,
// the mixer's envelope cache): a kept row, strip or panel skips its build.
// Each keep is only sound if its comparator sees every input the subtree
// shows or acts on. These walks change the screen one edit at a time and, at
// each step, compare the kept tree with a fresh build of the same state (both
// memoizations switched off by their test flags): pixel for pixel and text
// for text. A comparator that misses an input shows up as a stale step.

import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/gestures.dart';
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
import 'package:vocara/ui/core/widgets/stable_subtree.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/ui/song_practice_detail/widgets/track_row.dart';
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
  @override
  void setBusFx(String busId, TrackFx fx) {}
  @override
  void setBusPan(String busId, double pan) {}
  @override
  double busGainReductionDb(String busId) => 0;
  @override
  void setTrackSendLevel(String trackId, int slot, double levelDb) {}
  @override
  void setBusSendLevel(String busId, int slot, double levelDb) {}
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
    return [for (var i = 0; i < 400; i++) r.nextDouble()];
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
    reference: AudioTrack(
      id: 'ref',
      kind: AudioTrackKind.reference,
      displayName: 'Original',
      audioFilePath: '/pool/ref.wav',
      durationMs: _takeMs,
      waveformPeaks: peaks(99),
      createdAt: created,
    ),
    vocalTracks: [
      for (var i = 0; i < lanes; i++)
        AudioTrack(
          id: 't$i',
          kind: AudioTrackKind.vocal,
          displayName: 'Take ${i + 1}',
          clips: [
            AudioClip(
              id: 't$i-c0',
              audioFilePath: '/pool/t$i.wav',
              durationMs: 20000,
              startMs: 0,
              waveformPeaks: peaks(i + 1),
            ),
            AudioClip(
              id: 't$i-c1',
              audioFilePath: '/pool/t${i}b.wav',
              durationMs: 15000,
              startMs: 30000,
              waveformPeaks: i == lanes - 1 ? const [] : peaks(i + 50),
            ),
          ],
          createdAt: created,
        ),
    ],
  );
}

final _root = GlobalKey();

Future<SongPracticeDetailViewModel> _pumpScreen(
  WidgetTester tester, {
  int lanes = 4,
  Size size = const Size(1800, 1100),
  bool effects = true,
}) async {
  tester.view.physicalSize = size;
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
    playbackEngine: _FakeEngine(effects: effects),
    editService: _FakeEdits(),
    probeDuration: (_) => null,
  );
  for (var i = 0; i < 100 && vm.practice == null; i++) {
    await tester.pump(const Duration(milliseconds: 5));
  }
  expect(vm.practice, isNotNull);
  vm.toggleLyricsPanel();
  await tester.pumpWidget(
    RepaintBoundary(
      key: _root,
      child: MaterialApp(
        home: ChangeNotifierProvider.value(
          value: vm,
          child: const SongPracticeDetailScreen(),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 50));
  addTearDown(() async {
    StableSubtree.debugNeverKeep = false;
    MixerController.debugNoEnvelopeCache = false;
    await tester.pumpWidget(const SizedBox());
    vm.dispose();
  });
  return vm;
}

Future<Uint8List> _shot(WidgetTester tester) async {
  final ro = _root.currentContext!.findRenderObject()! as RenderRepaintBoundary;
  final bytes = await tester.runAsync(() async {
    final img = await ro.toImage();
    final bd = await img.toByteData(format: ui.ImageByteFormat.rawRgba);
    img.dispose();
    return bd!.buffer.asUint8List();
  });
  return bytes!;
}

List<String> _texts(WidgetTester tester) => [
  for (final p in tester.allRenderObjects.whereType<RenderParagraph>())
    p.text.toPlainText(),
];

void _rebuildAll(WidgetTester tester) {
  void mark(Element e) {
    e.markNeedsBuild();
    e.visitChildren(mark);
  }

  tester.binding.rootElement!.visitChildren(mark);
}

int _diff(Uint8List a, Uint8List b) {
  if (a.length != b.length) return -1;
  var n = 0;
  for (var i = 0; i < a.length; i += 4) {
    if (a[i] != b[i] || a[i + 1] != b[i + 1] || a[i + 2] != b[i + 2]) n++;
  }
  return n;
}

final _failures = <String>[];

Future<void> _expectFresh(WidgetTester tester, String what) async {
  for (var i = 0; i < 20; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
  // Two kept shots that agree: a screenshot runs real async, in which work
  // still in flight (a lane's analysis) can land and change the screen.
  late Uint8List kept;
  late List<String> keptTexts;
  var noise = 0;
  for (var attempt = 0; attempt < 5; attempt++) {
    _rebuildAll(tester);
    await tester.pump(const Duration(milliseconds: 16));
    final first = await _shot(tester);
    final firstTexts = _texts(tester);
    _rebuildAll(tester);
    await tester.pump(const Duration(milliseconds: 16));
    kept = await _shot(tester);
    keptTexts = _texts(tester);
    noise = _diff(first, kept);
    if (noise == 0 && firstTexts.join('|') == keptTexts.join('|')) break;
  }
  StableSubtree.debugNeverKeep = true;
  MixerController.debugNoEnvelopeCache = true;
  Uint8List fresh;
  List<String> freshTexts;
  try {
    _rebuildAll(tester);
    await tester.pump(const Duration(milliseconds: 16));
    fresh = await _shot(tester);
    freshTexts = _texts(tester);
  } finally {
    StableSubtree.debugNeverKeep = false;
    MixerController.debugNoEnvelopeCache = false;
  }
  _rebuildAll(tester);
  await tester.pump(const Duration(milliseconds: 16));
  final d = _diff(kept, fresh);
  final textsEqual = keptTexts.join('|') == freshTexts.join('|');
  if (d != 0 || !textsEqual) {
    final a = keptTexts.toSet(), b = freshTexts.toSet();
    _failures.add(
      '$what: $d pixels differ (run-to-run noise $noise); '
      'kept-only texts ${a.difference(b)}, fresh-only ${b.difference(a)}',
    );
  }
}

Future<void> _step(
  WidgetTester tester,
  String what,
  FutureOr<void> Function() change,
) async {
  final r = change();
  if (r is Future) {
    unawaited(r.catchError((Object e) => _failures.add('$what threw: $e')));
  }
  await tester.pump(const Duration(milliseconds: 50));
  await tester.pump(const Duration(milliseconds: 50));
  await _expectFresh(tester, what);
}

void main() {
  testWidgets('callbacks of kept rows and strips act on the current objects', (
    tester,
  ) async {
    final vm = await _pumpScreen(tester);
    final m = vm.mixer;
    vm.analysis.showDockTab(DockTab.mixer);
    vm.analysis.setDockHeight(330);
    await tester.pump(const Duration(milliseconds: 50));
    AudioTrack t(String id) =>
        vm.practice!.vocalTracks.firstWhere((x) => x.id == id);

    // Edits elsewhere keep t0's row; then its own M button, through the UI.
    m.setTrackVolumeLive('t3', 0.2);
    await tester.pump(const Duration(milliseconds: 16));
    m.setTrackPanLive('t2', 0.4);
    await tester.pump(const Duration(milliseconds: 16));
    final mBtn = find.descendant(
      of: find.byKey(const ValueKey('t0')),
      matching: find.byWidgetPredicate((w) => w is MsToggle && w.label == 'M'),
    );
    await tester.tap(mBtn);
    await tester.pump(const Duration(milliseconds: 50));
    expect(t('t0').muted, isTrue);
    expect(t('t3').muted, isFalse);
    expect(
      t('t3').volume,
      closeTo(0.2, 1e-9),
      reason: 'the earlier edit survived',
    );
    expect(t('t2').pan, closeTo(0.4, 1e-9));

    // A kept lane's clip drags the right clip, and commits it.
    m.setTrackVolumeLive('t3', 0.25);
    await tester.pump(const Duration(milliseconds: 16));
    final before = t('t1').clips.map((c) => c.startMs).toList();
    final clip = find.descendant(
      of: find.byKey(const ValueKey('t1')),
      matching: find.byKey(const ValueKey('t1-c0')),
    );
    await tester.drag(
      clip,
      const Offset(120, 0),
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump(const Duration(milliseconds: 50));
    final after = t('t1').clips.map((c) => c.startMs).toList();
    expect(after[0], greaterThan(before[0]));
    expect(after[1], before[1]);
    expect(t('t3').volume, closeTo(0.25, 1e-9));

    // Rename a bus, then route a KEPT channel strip through its chip menu.
    final bus = await m.createBus('Verb');
    await tester.pump(const Duration(milliseconds: 50));
    await m.renameBus(bus, 'Renamed');
    await tester.pump(const Duration(milliseconds: 50));
    m.setTrackVolumeLive('t3', 0.3); // t1's strip stays kept
    await tester.pump(const Duration(milliseconds: 50));
    final chips = find.text('→ Master');
    expect(chips, findsWidgets);
    await tester.tap(chips.at(1)); // second vocal strip (t1 or as ordered)
    await tester.pump(const Duration(milliseconds: 50));
    expect(
      find.text('Renamed'),
      findsWidgets,
      reason: 'the menu lists the current bus name',
    );
    await tester.tap(find.text('Renamed').last);
    await tester.pump(const Duration(milliseconds: 50));
    final routed = [
      for (final x in vm.practice!.vocalTracks)
        if (x.busId == bus) x.id,
    ];
    expect(routed, hasLength(1));
  });

  testWidgets('kept subtrees equal a fresh build across edits (mixer dock)', (
    tester,
  ) async {
    _failures.clear();
    final vm = await _pumpScreen(tester);
    final m = vm.mixer;
    final pb = vm.playback;
    vm.analysis.showDockTab(DockTab.mixer);
    vm.analysis.setDockHeight(330);
    await tester.pump(const Duration(milliseconds: 50));
    await _expectFresh(tester, 'baseline');
    await _step(tester, 'select t1', () => vm.selectTrack('t1'));
    await _step(
      tester,
      'select clip t2-c1',
      () => vm.selectClip('t2', 't2-c1'),
    );
    await _step(
      tester,
      'select clip t2-c0',
      () => vm.selectClip('t2', 't2-c0'),
    );
    await _step(tester, 'mute t0', () => m.toggleMute('t0'));
    await _step(tester, 'solo t1', () => m.toggleSolo('t1'));
    await _step(tester, 'unsolo t1', () => m.toggleSolo('t1'));
    await _step(tester, 'fader t3', () => m.setTrackVolumeLive('t3', 0.3));
    await _step(tester, 'pan t3', () => m.setTrackPanLive('t3', -0.5));
    await _step(
      tester,
      'rename t2',
      () => vm.renameTrack('t2', 'Renamed take two'),
    );
    await _step(tester, 'color t2', () => vm.setTrackColor('t2', 0xFF22CC44));
    await _step(
      tester,
      'move clip t1-c1',
      () => vm.moveClipStartLive('t1', 't1-c1', 41000),
    );
    await _step(tester, 'commit move', vm.commitTrackMove);
    await _step(tester, 'zoom', () => pb.setTimelineZoomLive(0.5));
    await _step(tester, 'pan view', () => pb.panView(3000));
    await _step(tester, 'zoom back', () => pb.setTimelineZoomLive(1.0));
    await _step(tester, 'note t0', () => vm.addNote('t0', 10000, null, 'hi'));
    await _step(
      tester,
      'expand t1',
      () => vm.analysis.toggleTrackExpanded('t1'),
    );
    await _step(
      tester,
      'collapse t1',
      () => vm.analysis.toggleTrackExpanded('t1'),
    );
    await _step(tester, 'master', () => m.setMasterVolumeLive(0.4));
    await _step(tester, 'A/B on', () => m.setPlayReference(true));
    await _step(tester, 'A/B off', () => m.setPlayReference(false));
    await _step(
      tester,
      'reorder t3->t0',
      () => vm.moveVocalTrackToLaneOf('t3', 't0'),
    );
    late String bus1, bus2;
    await _step(
      tester,
      'create bus',
      () async => bus1 = await m.createBus('Verb'),
    );
    await _step(tester, 'route t0->bus', () => m.setTrackBus('t0', bus1));
    await _step(
      tester,
      'rename bus',
      () => m.renameBus(bus1, 'A much longer bus'),
    );
    await _step(tester, 'bus mute', () => m.toggleBusMute(bus1));
    await _step(tester, 'bus solo', () => m.toggleBusSolo(bus1));
    await _step(tester, 'bus unsolo', () => m.toggleBusSolo(bus1));
    await _step(tester, 'bus fader', () => m.setBusVolumeLive(bus1, 0.2));
    await _step(tester, 'bus pan', () => m.setBusPanLive(bus1, 0.7));
    await _step(
      tester,
      'create bus2',
      () async => bus2 = await m.createBus('Dly'),
    );
    await _step(tester, 'bus1 out->bus2', () => m.setBusOutput(bus1, bus2));
    await _step(tester, 'rename bus2', () => m.renameBus(bus2, 'Delay return'));
    await _step(tester, 'send t1->bus2', () => m.addTrackSend('t1', bus2));
    await _step(
      tester,
      'send level',
      () => m.setTrackSendLevelLive('t1', 0, -3),
    );
    await _step(tester, 'select bus1', () => vm.selectBus(bus1));
    await _step(
      tester,
      'bus fx',
      () => m.setBusFxLive(bus1, TrackFx.flat().copyWith(compOn: true)),
    );
    await _step(tester, 'select t0', () => vm.selectTrack('t0'));
    await _step(
      tester,
      'fx t0',
      () => m.setTrackFxLive('t0', TrackFx.flat().copyWith(compOn: true)),
    );
    await _step(tester, 'select t2', () => vm.selectTrack('t2'));
    await _step(tester, 'unroute t0', () => m.setTrackBus('t0', null));
    await _step(tester, 'delete bus2', () => m.deleteBus(bus2));
    await _step(tester, 'loop', pb.toggleLoop);
    await _step(tester, 'dock height', () => vm.analysis.setDockHeight(260));
    await _step(tester, 'resize window', () {
      tester.view.physicalSize = const Size(1500, 1000);
    });
    await _step(tester, 'inspector off', vm.toggleInspectorPanel);
    await _step(tester, 'inspector on', vm.toggleInspectorPanel);
    await _step(tester, 'delete t3', () => vm.deleteTrack('t3'));
    await _step(
      tester,
      'new interpretation',
      () => vm.versions.create('Second'),
    );
    await _step(
      tester,
      'back to first',
      () => vm.versions.setActive(vm.practice!.versions.first.id),
    );
    await _step(
      tester,
      'pitch tab',
      () => vm.analysis.showDockTab(DockTab.pitch),
    );
    await _step(tester, 'close dock', vm.analysis.toggleDock);
    await _step(tester, 'open dock', vm.analysis.toggleDock);
    await _step(
      tester,
      'mixer tab',
      () => vm.analysis.showDockTab(DockTab.mixer),
    );
    expect(_failures, isEmpty);
  });
}
