// The detail screen keeps unchanged subtrees across rebuilds. These walks
// change it one edit at a time, with the mixer dock open, and check each
// step's kept screen against a fresh build (see FreshBuildOracle); a first
// test drives kept rows' and strips' callbacks through the UI.

import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
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
import 'package:vocara/ui/core/widgets/studio_controls.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/ui/song_practice_detail/widgets/track_row.dart';
import 'package:vocara/utils/result.dart';

import 'support/fresh_build_oracle.dart';

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

Future<SongPracticeDetailViewModel> _pumpScreen(
  WidgetTester tester, {
  int lanes = 4,
  Size size = const Size(1800, 1100),
  bool effects = true,
  FreshBuildOracle? oracle,
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
  final app = MaterialApp(
    home: ChangeNotifierProvider.value(
      value: vm,
      child: const SongPracticeDetailScreen(),
    ),
  );
  await tester.pumpWidget(oracle?.wrap(app) ?? app);
  await tester.pump(const Duration(milliseconds: 50));
  addTearDown(() async {
    await tester.pumpWidget(const SizedBox());
    vm.dispose();
  });
  return vm;
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
    final oracle = FreshBuildOracle(tester);
    final vm = await _pumpScreen(tester, oracle: oracle);
    final m = vm.mixer;
    final pb = vm.playback;
    vm.analysis.showDockTab(DockTab.mixer);
    vm.analysis.setDockHeight(330);
    await tester.pump(const Duration(milliseconds: 50));
    await oracle.expectFresh('baseline');
    await oracle.step('select t1', () => vm.selectTrack('t1'));
    await oracle.step('select clip t2-c1', () => vm.selectClip('t2', 't2-c1'));
    await oracle.step('select clip t2-c0', () => vm.selectClip('t2', 't2-c0'));
    await oracle.step('mute t0', () => m.toggleMute('t0'));
    await oracle.step('solo t1', () => m.toggleSolo('t1'));
    await oracle.step('unsolo t1', () => m.toggleSolo('t1'));
    await oracle.step('fader t3', () => m.setTrackVolumeLive('t3', 0.3));
    await oracle.step('pan t3', () => m.setTrackPanLive('t3', -0.5));
    await oracle.step(
      'rename t2',
      () => vm.renameTrack('t2', 'Renamed take two'),
    );
    await oracle.step('color t2', () => vm.setTrackColor('t2', 0xFF22CC44));
    await oracle.step(
      'move clip t1-c1',
      () => vm.moveClipStartLive('t1', 't1-c1', 41000),
    );
    await oracle.step('commit move', vm.commitTrackMove);
    await oracle.step('zoom', () => pb.setTimelineZoomLive(0.5));
    await oracle.step('pan view', () => pb.panView(3000));
    await oracle.step('zoom back', () => pb.setTimelineZoomLive(1.0));
    await oracle.step('note t0', () => vm.addNote('t0', 10000, null, 'hi'));
    await oracle.step('expand t1', () => vm.analysis.toggleTrackExpanded('t1'));
    await oracle.step(
      'collapse t1',
      () => vm.analysis.toggleTrackExpanded('t1'),
    );
    await oracle.step('master', () => m.setMasterVolumeLive(0.4));
    await oracle.step('A/B on', () => m.setPlayReference(true));
    await oracle.step('A/B off', () => m.setPlayReference(false));
    await oracle.step(
      'reorder t3->t0',
      () => vm.moveVocalTrackToLaneOf('t3', 't0'),
    );
    late String bus1, bus2;
    await oracle.step(
      'create bus',
      () async => bus1 = await m.createBus('Verb'),
    );
    await oracle.step('route t0->bus', () => m.setTrackBus('t0', bus1));
    await oracle.step(
      'rename bus',
      () => m.renameBus(bus1, 'A much longer bus'),
    );
    await oracle.step('bus mute', () => m.toggleBusMute(bus1));
    await oracle.step('bus solo', () => m.toggleBusSolo(bus1));
    await oracle.step('bus unsolo', () => m.toggleBusSolo(bus1));
    await oracle.step('bus fader', () => m.setBusVolumeLive(bus1, 0.2));
    await oracle.step('bus pan', () => m.setBusPanLive(bus1, 0.7));
    await oracle.step(
      'create bus2',
      () async => bus2 = await m.createBus('Dly'),
    );
    await oracle.step('bus1 out->bus2', () => m.setBusOutput(bus1, bus2));
    await oracle.step('rename bus2', () => m.renameBus(bus2, 'Delay return'));
    await oracle.step('send t1->bus2', () => m.addTrackSend('t1', bus2));
    await oracle.step('send level', () => m.setTrackSendLevelLive('t1', 0, -3));
    await oracle.step('select bus1', () => vm.selectBus(bus1));
    await oracle.step(
      'bus fx',
      () => m.setBusFxLive(bus1, TrackFx.flat().copyWith(compOn: true)),
    );
    await oracle.step('select t0', () => vm.selectTrack('t0'));
    await oracle.step(
      'fx t0',
      () => m.setTrackFxLive('t0', TrackFx.flat().copyWith(compOn: true)),
    );
    await oracle.step('select t2', () => vm.selectTrack('t2'));
    await oracle.step('unroute t0', () => m.setTrackBus('t0', null));
    await oracle.step('delete bus2', () => m.deleteBus(bus2));
    await oracle.step('loop', pb.toggleLoop);
    await oracle.step('dock height', () => vm.analysis.setDockHeight(260));
    await oracle.step('resize window', () {
      tester.view.physicalSize = const Size(1500, 1000);
    });
    await oracle.step('inspector off', vm.toggleInspectorPanel);
    await oracle.step('inspector on', vm.toggleInspectorPanel);
    await oracle.step('delete t3', () => vm.deleteTrack('t3'));
    await oracle.step('new interpretation', () => vm.versions.create('Second'));
    await oracle.step(
      'back to first',
      () => vm.versions.setActive(vm.practice!.versions.first.id),
    );
    await oracle.step(
      'pitch tab',
      () => vm.analysis.showDockTab(DockTab.pitch),
    );
    await oracle.step('close dock', vm.analysis.toggleDock);
    await oracle.step('open dock', vm.analysis.toggleDock);
    await oracle.step(
      'mixer tab',
      () => vm.analysis.showDockTab(DockTab.mixer),
    );
    expect(oracle.failures, isEmpty);
  });
  group('a timeline fader keeps its own drag when lanes change above it', () {
    // Matched by position, a lane added or removed above the one whose fader
    // was being dragged handed that fader's state, and the rest of its drag,
    // to the next lane (or rebuilt the row, and the drag did nothing more).
    Finder byTypeName(String name) =>
        find.byWidgetPredicate((w) => w.runtimeType.toString() == name);

    Future<TestGesture> grab(WidgetTester tester, Finder fader) async {
      final g = await tester.startGesture(
        tester.getCenter(fader),
        kind: PointerDeviceKind.mouse,
      );
      await tester.pump();
      await g.moveBy(const Offset(-20, 0));
      await tester.pump();
      await g.moveBy(const Offset(-10, 0));
      await tester.pump();
      return g;
    }

    Future<void> dragOn(WidgetTester tester, TestGesture g) async {
      await tester.pump(const Duration(milliseconds: 50));
      await g.moveBy(const Offset(-30, 0));
      await tester.pump();
      await g.moveBy(const Offset(-30, 0));
      await tester.pump();
    }

    testWidgets('a bus fader, a bus deleted above it', (tester) async {
      final vm = await _pumpScreen(tester, lanes: 2);
      final m = vm.mixer;
      final a = await m.createBus('A');
      final b = await m.createBus('B');
      final c = await m.createBus('C');
      await tester.pump(const Duration(milliseconds: 50));
      double vol(String id) => m.fxBuses.firstWhere((x) => x.id == id).volume;
      final c0 = vol(c);
      final g = await grab(
        tester,
        find.descendant(
          of: byTypeName('_BusRow').at(1),
          matching: find.byType(StudioFader),
        ),
      );
      await m.deleteBus(a);
      final bMid = vol(b);
      await dragOn(tester, g);
      await g.up();
      await tester.pump(const Duration(milliseconds: 50));

      expect(vol(c), c0, reason: "C must not take the rest of B's drag");
      expect(vol(b), lessThan(bMid));
    });

    testWidgets('the master fader, a bus added above it', (tester) async {
      final vm = await _pumpScreen(tester, lanes: 2);
      final m = vm.mixer;
      await m.createBus('A');
      await tester.pump(const Duration(milliseconds: 50));
      final g = await grab(
        tester,
        find.descendant(
          of: byTypeName('_MasterRow'),
          matching: find.byType(StudioFader),
        ),
      );
      await m.createBus('B');
      final mid = m.masterVolume;
      await dragOn(tester, g);
      final after = m.masterVolume;
      await g.up();
      await tester.pump(const Duration(milliseconds: 50));

      expect(after, lessThan(mid), reason: 'the drag still moves the master');
    });

    testWidgets('a take fader, a take deleted above it', (tester) async {
      final vm = await _pumpScreen(tester, lanes: 3);
      final takes = vm.practice!.vocalTracks;
      double vol(String id) =>
          vm.practice!.vocalTracks.firstWhere((t) => t.id == id).volume;
      final last0 = vol(takes[2].id);
      final g = await grab(
        tester,
        find.descendant(
          of: find.byWidgetPredicate(
            (w) => w is TrackRow && w.key == ValueKey(takes[1].id),
          ),
          matching: find.byType(StudioFader),
        ),
      );
      await vm.deleteTrack(takes[0].id);
      final mid = vol(takes[1].id);
      await dragOn(tester, g);
      await g.up();
      await tester.pump(const Duration(milliseconds: 50));

      expect(vol(takes[2].id), last0);
      expect(vol(takes[1].id), lessThan(mid));
    });
  });
}
