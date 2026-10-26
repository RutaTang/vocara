import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart'
    show MediaTooLongException;
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
import 'package:vocara/ui/technique_library/widgets/technique_analysis_section.dart';
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
  Future<Result<Technique>> saveTechnique(Technique t) async => Result.ok(t);
  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async => Result.ok(Loaded(t, const {}));
  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

/// Reference analyzes fine; the practice take (> 10 min) trips the cap.
class _Analysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async {
    if (path.contains('practice_take')) {
      throw const MediaTooLongException(Duration(minutes: 10));
    }
    return _analysis();
  }
}

class _FakeRecorder extends Fake implements MicRecordingController {
  _FakeRecorder(this.dir);
  final Directory dir;
  bool recording = false;
  final ValueNotifier<double> _level = ValueNotifier(0);
  String? _path;
  @override
  ValueNotifier<double> get level => _level;
  @override
  bool get isRecording => recording;
  @override
  bool get isStarting => false;
  @override
  bool get feedingMonitor => recording;
  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    final d = Directory(p.join(dir.path, 'practice_take'))..createSync();
    _path = p.join(d.path, 'take.wav');
    File(_path!).writeAsBytesSync(const [1, 2, 3]);
    recording = true;
    return MicRecordingStart.started(_path!);
  }

  @override
  MicRecordingStop? stop() {
    if (!recording) return null;
    recording = false;
    return (path: _path!, clean: true, droppedFrames: 0);
  }

  @override
  void invalidatePendingStart() {}
  @override
  Future<void> abort() async => recording = false;
  @override
  void dispose() => _level.dispose();
}

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

TechniqueAnalysis _analysis() {
  Float32List flat(double v) => Float32List(4)..fillRange(0, 4, v);
  final metrics = VoiceMetrics(
    frames: 4,
    hop: 512,
    fftSize: 2048,
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

void main() {
  // A take the analysis refuses (here: past the 10-minute cap) used to snap the
  // panel back to the reference, with the practice segment disabled: the cause
  // was kept but shown nowhere.
  testWidgets('a failed practice analysis is shown on its own panel', (
    tester,
  ) async {
    final tmp = Directory.systemTemp.createTempSync('vocara_pf');
    final now = DateTime(2026, 6, 20);
    late TechniqueDetailViewModel vm;
    await tester.runAsync(() async {
      vm = TechniqueDetailViewModel(
        repository: _FakeRepo(
          Technique(
            id: 'T1',
            name: 'Twang',
            createdAt: now,
            updatedAt: now,
            media: [
              TechniqueMedia(
                id: 'E1',
                path: p.join(tmp.path, 'E1.wav'),
                kind: TechniqueMediaKind.audio,
                addedAt: now,
              ),
            ],
          ),
        ),
        mediaService: TechniqueMediaService(storage: _TempStorage(tmp)),
        techniqueId: 'T1',
        analysisService: _Analysis(),
        recorder: _FakeRecorder(tmp),
      );
      for (var i = 0; i < 200 && vm.technique == null; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      await vm.ensureReferenceAnalysis();
    });
    vm.setAnalysisDim(8); // Range: no async image decode in the test
    await tester.binding.setSurfaceSize(const Size(960, 1200));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: ListenableBuilder(
              listenable: vm,
              builder: (_, _) => TechniqueExampleAnalysis(vm: vm),
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    expect(find.text('Record practice'), findsOneWidget);

    await tester.runAsync(() async {
      expect(await vm.startPractice(), isNull);
    });
    await tester.pump();
    await tester.runAsync(() async {
      await vm.stopPractice();
    });
    await tester.pump();

    // The failed take stays the subject, and its panel says why.
    expect(vm.practiceAnalysisFailed, isTrue);
    expect(vm.analysisSubject, TechniqueAnalysisSubject.practice);
    expect(find.textContaining('too long'), findsOneWidget);

    // The user can leave it and come back to read it again.
    await tester.tap(find.text('Reference'));
    await tester.pump();
    expect(vm.analysisSubject, TechniqueAnalysisSubject.reference);
    expect(find.textContaining('too long'), findsNothing);
    await tester.tap(find.text('Your practice'));
    await tester.pump();
    expect(vm.analysisSubject, TechniqueAnalysisSubject.practice);
    expect(find.textContaining('too long'), findsOneWidget);

    vm.dispose();
    tmp.deleteSync(recursive: true);
  });
}
