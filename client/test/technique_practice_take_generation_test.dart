import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo implements TechniqueRepository {
  _FakeRepo(this.technique);
  Technique technique;

  @override
  Future<Result<Technique>> getTechnique(String id) async =>
      Result.ok(technique);

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async =>
      Result.ok(Loaded(technique, const {}));

  @override
  Future<Result<List<Technique>>> listTechniques() async =>
      Result.ok([technique]);

  @override
  Future<Result<Technique>> saveTechnique(Technique t) async {
    technique = t;
    return Result.ok(t);
  }

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async {
    technique = t;
    return Result.ok(Loaded(t, const {}));
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

/// Analysis fake: every clip analyzes to the same canned result; the take
/// paths it was asked for are recorded, so a test can tell a take that was
/// analyzed from one that was discarded unanalyzed.
class _FixedAnalysis extends Fake implements MediaAnalysisService {
  _FixedAnalysis(this.result);

  final TechniqueAnalysis result;
  final List<String> analyzed = [];

  @override
  Future<TechniqueAnalysis?> analyze(String path) async {
    analyzed.add(path);
    return result;
  }
}

/// A [LocalStorageService] rooted at a test temp dir, so the media service
/// never touches path_provider / platform channels.
class _TempStorage extends LocalStorageService {
  _TempStorage(this.root);

  final Directory root;

  @override
  Future<Directory> assetDir(String collection) async {
    final dir = Directory(p.join(root.path, collection));
    if (!await dir.exists()) await dir.create(recursive: true);
    return dir;
  }

  @override
  Future<String> assetPath(String collection) async =>
      p.join(root.path, collection);
}

/// Recorder fake: [start] writes a WAV stub into its own temp dir and, while
/// [startGate] is set, stays PENDING ([isStarting]) until the test completes
/// the gate — the permission / temp-dir window before a take arms.
class _FakeRecorder extends Fake implements MicRecordingController {
  bool recording = false;
  bool starting = false;
  Completer<void>? startGate;

  final ValueNotifier<double> _level = ValueNotifier(0);
  String? _path;

  @override
  ValueNotifier<double> get level => _level;

  @override
  bool get isRecording => recording;

  @override
  bool get isStarting => starting;

  @override
  bool get feedingMonitor => recording;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    starting = true;
    final gate = startGate;
    if (gate != null) await gate.future;
    final dir = await Directory.systemTemp.createTemp('vocara_fake_take');
    final path = p.join(dir.path, 'take.wav');
    await File(path).writeAsBytes(const [1, 2, 3]);
    _path = path;
    starting = false;
    recording = true;
    return MicRecordingStart.started(path);
  }

  @override
  MicRecordingStop? stop() {
    if (!recording) return null;
    recording = false;
    return (path: _path!, clean: true, droppedFrames: 0);
  }

  @override
  Future<void> abort() async {
    recording = false;
  }

  @override
  void invalidatePendingStart() {}

  @override
  void dispose() {
    _level.dispose();
  }
}

TechniqueAnalysis _analysis() {
  Float32List flat(double v) => Float32List(4)..fillRange(0, 4, v);
  final metrics = VoiceMetrics(
    frames: 4,
    hop: 512,
    sampleRate: 44100,
    ring: flat(0),
    tilt: flat(0),
    cpp: flat(0),
    loudness: flat(-20),
    f0: flat(200),
    f1: flat(double.nan),
    f2: flat(double.nan),
    f3: flat(double.nan),
    h1h2: flat(double.nan),
    active: Uint8List(4),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
    peakDbfs: -6,
    clippedFraction: 0,
    snrDb: double.nan,
  );
  return TechniqueAnalysis(
    metrics: metrics,
    spectrogram: Spectrogram(
      frames: 2,
      bins: 2,
      maxHz: 5000,
      sampleRate: 44100,
      fftSize: 2048,
      hop: 512,
      db: Float32List(4),
      minDb: -80,
      maxDb: 0,
    ),
    ltas: Ltas(
      db: Float32List(4),
      binHz: 21.5,
      maxHz: 5000,
      maxDb: -20,
      alphaRatio: -12,
      singersFormantDb: 3,
      frameCount: 2,
    ),
    profile: const VoiceProfile(
      lowMidi: 48,
      highMidi: 72,
      tessLoMidi: 55,
      tessHiMidi: 65,
      passaggioMidi: null,
      passaggioJumpDb: 0,
      softDb: -30,
      loudDb: -10,
      medianDb: -20,
    ),
    register: RegisterMap(
      score: Float32List(4),
      chestPct: 50,
      mixPct: 30,
      headPct: 20,
      voicedFrames: 4,
    ),
    durationMs: 1000,
  );
}

const _techniqueId = 'T1000000000000000000000000';

void main() {
  late Directory tmp;
  late _FakeRecorder recorder;
  late _FixedAnalysis analysis;
  late TechniqueDetailViewModel vm;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_practice_generation');
    final now = DateTime(2026, 6, 20);
    TechniqueMedia example(String id) => TechniqueMedia(
      id: id,
      path: p.join(tmp.path, '$id.wav'),
      kind: TechniqueMediaKind.audio,
      addedAt: now,
    );
    recorder = _FakeRecorder();
    analysis = _FixedAnalysis(_analysis());
    vm = TechniqueDetailViewModel(
      repository: _FakeRepo(
        Technique(
          id: _techniqueId,
          name: 'Twang',
          createdAt: now,
          updatedAt: now,
          media: [example('E1'), example('E2')],
        ),
      ),
      mediaService: TechniqueMediaService(storage: _TempStorage(tmp)),
      techniqueId: _techniqueId,
      analysisService: analysis,
      recorder: recorder,
    );
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);
    expect(vm.referenceMedia!.id, 'E1');
  });

  tearDown(() async {
    vm.dispose();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  test('a reference switched WHILE the take records discards it at Stop — '
      'never paired with a reference it was not recorded against', () async {
    expect(await vm.startPractice(), isNull);
    expect(vm.isRecordingPractice, isTrue);

    // Mid-take the user opens the second example's card (the cards stay
    // live while recording): E2 becomes the reference and the practice
    // panel, Stop included, moves under it.
    vm.toggleExampleExpanded('E2');
    expect(vm.referenceMedia!.id, 'E2');
    expect(vm.isRecordingPractice, isTrue);

    await vm.stopPractice();

    // The generation used to be snapshotted at Stop, AFTER the switch: the
    // take recorded against E1 was analyzed and shown as "Your practice",
    // and the gap table paired it with E2 — while the same tap landing just
    // after Stop discarded it.
    expect(
      vm.practiceAnalysis,
      isNull,
      reason: 'the E1 take must not be paired with E2',
    );
    expect(vm.analysisSubject, TechniqueAnalysisSubject.reference);
    expect(vm.practiceAnalysisLoading, isFalse);
    expect(vm.canRecordPractice, isTrue);
    expect(
      analysis.analyzed.where((path) => path.endsWith('take.wav')),
      isEmpty,
      reason: 'a stale take is discarded without the DSP pass',
    );
    expect(vm.recordingNotice, contains('reference example changed'));
  });

  test('a take whose reference held from arm to Stop is analyzed and shown '
      '— clearing the PREVIOUS take mid-recording does not void it', () async {
    expect(await vm.startPractice(), isNull);
    await vm.stopPractice();
    expect(vm.practiceAnalysis, isNotNull);

    expect(await vm.startPractice(), isNull);
    // Discards the first take (the UI hides Clear while recording; the VM
    // must still keep the take being recorded, whose reference never
    // changed).
    vm.clearPractice();
    await vm.stopPractice();

    expect(vm.practiceAnalysis, isNotNull);
    expect(vm.analysisSubject, TechniqueAnalysisSubject.practice);
    expect(vm.recordingNotice, isNull);
  });

  test('a reference switched while the start is still PENDING is the one '
      'the take is recorded against: it is kept', () async {
    recorder.startGate = Completer<void>();
    final started = vm.startPractice();
    await pumpEventQueue();
    expect(vm.starting, isTrue);

    vm.toggleExampleExpanded('E2'); // before capture begins
    recorder.startGate!.complete();
    expect(await started, isNull);
    expect(vm.isRecordingPractice, isTrue);
    await vm.stopPractice();

    expect(vm.referenceMedia!.id, 'E2');
    expect(vm.practiceAnalysis, isNotNull);
  });

  test('a reference switched while the take is being ANALYZED still '
      'discards it (unchanged)', () async {
    expect(await vm.startPractice(), isNull);
    final stopping = vm.stopPractice();
    expect(vm.practiceAnalysisLoading, isTrue);
    vm.toggleExampleExpanded('E2');
    await stopping;

    expect(vm.practiceAnalysis, isNull);
    expect(vm.analysisSubject, TechniqueAnalysisSubject.reference);
  });
}
