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

/// Repo that mirrors the drift repository's hydrate: a successful save
/// re-reads the aggregate with every media file resolved through the
/// content-addressed asset pool, so a clip staged from the sandbox comes back
/// under a DIFFERENT (pool) path while its id stays stable.
class _PoolRewriteRepo implements TechniqueRepository {
  _PoolRewriteRepo(this.technique, this.poolRoot);

  Technique technique;
  final String poolRoot;

  TechniqueMedia _hydrated(TechniqueMedia m) => TechniqueMedia(
    id: m.id,
    path: p.join(poolRoot, '${m.id}.wav'),
    kind: m.kind,
    label: m.label,
    addedAt: m.addedAt,
  );

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
    technique = t.copyWith(media: [for (final m in t.media) _hydrated(m)]);
    return Result.ok(Loaded(technique, const {}));
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

/// Analysis fake that always returns the SAME canned instance, so a test can
/// prove a cached analysis was carried over (identity) rather than recomputed.
class _FixedAnalysis extends Fake implements MediaAnalysisService {
  _FixedAnalysis(this.result);

  final TechniqueAnalysis result;
  bool failing = false;
  int analyzeCalls = 0;

  /// While set, every call parks on it (a slow DSP pass).
  Completer<void>? gate;

  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async {
    analyzeCalls++;
    await gate?.future;
    return failing ? null : result;
  }
}

/// A [LocalStorageService] rooted at a test temp dir, so importMedia works
/// without path_provider / platform channels.
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

/// Recorder fake: [start] writes a WAV stub into its own temp dir and [stop]
/// reports a clean take.
class _FakeRecorder extends Fake implements MicRecordingController {
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
  bool get feedingMonitor => false;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    final dir = await Directory.systemTemp.createTemp('vocara_fake_take');
    final path = p.join(dir.path, 'take.wav');
    await File(path).writeAsBytes(const [1, 2, 3]);
    _path = path;
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

const _techniqueId = 'T1000000000000000000000000';

void main() {
  late Directory tmp;
  late _PoolRewriteRepo repo;
  late _FixedAnalysis analysis;
  late TechniqueDetailViewModel vm;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_save_selection');
    final now = DateTime(2026, 6, 20);
    final poolRoot = p.join(tmp.path, 'assets');
    repo = _PoolRewriteRepo(
      Technique(
        id: _techniqueId,
        name: 'Twang',
        createdAt: now,
        updatedAt: now,
        media: [
          // A pre-existing persisted example, already under its pool path —
          // the fallback the selection silently jumped to.
          TechniqueMedia(
            id: 'M1',
            path: p.join(poolRoot, 'M1.wav'),
            kind: TechniqueMediaKind.audio,
            addedAt: now,
          ),
        ],
      ),
      poolRoot,
    );
    analysis = _FixedAnalysis(_analysis());
    vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: _TempStorage(tmp)),
      techniqueId: _techniqueId,
      analysisService: analysis,
      recorder: _FakeRecorder(),
    );
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);
  });

  tearDown(() async {
    vm.dispose();
    // dispose() fire-and-forgets deletion of staged draft files under [tmp],
    // which can race this recursive delete. Retry briefly until drained.
    for (var i = 0; i < 50; i++) {
      try {
        if (await tmp.exists()) await tmp.delete(recursive: true);
        break;
      } on FileSystemException {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
    }
  });

  test('save keeps the just-recorded example selected and carries its '
      'analysis across the sandbox-to-pool path rewrite', () async {
    vm.beginMediaDraft();
    expect(await vm.startExampleRecording(), isNull);
    await vm.stopExampleRecording();

    // The just-recorded clip is selected + expanded, and its analysis is
    // computed under the sandbox path it was staged at.
    final recorded = vm.effectiveMedia.last;
    expect(vm.referenceMedia!.id, recorded.id);
    await vm.ensureReferenceAnalysis();
    final computed = vm.referenceAnalysis;
    expect(computed, isNotNull);
    final analyzeCallsBeforeSave = analysis.analyzeCalls;

    expect(await vm.save(vm.technique!), isTrue);

    // The commit rewrote the clip's path to its pool path; the selection and
    // the expansion must follow the clip (by id), not fall back to the first
    // example, and the analysis computed under the sandbox path must remain
    // reachable under the new one.
    expect(vm.referenceMedia!.id, recorded.id);
    expect(vm.referenceMedia!.path, isNot(recorded.path));
    expect(vm.expandedExampleId, vm.referenceMedia!.id);
    expect(identical(vm.referenceAnalysis, computed), isTrue);
    await vm.ensureReferenceAnalysis();
    expect(analysis.analyzeCalls, analyzeCallsBeforeSave);
  });

  test('save carries the analysis-failed latch across the path rewrite '
      '(the pool copy is byte-identical, so a recompute is doomed)', () async {
    analysis.failing = true;
    vm.beginMediaDraft();
    expect(await vm.startExampleRecording(), isNull);
    await vm.stopExampleRecording();
    await vm.ensureReferenceAnalysis();
    expect(vm.referenceAnalysisFailed, isTrue);
    final analyzeCallsBeforeSave = analysis.analyzeCalls;

    expect(await vm.save(vm.technique!), isTrue);

    expect(vm.referenceAnalysisFailed, isTrue);
    await vm.ensureReferenceAnalysis();
    expect(analysis.analyzeCalls, analyzeCallsBeforeSave);
  });

  test('a save landing while the staged clip is still analyzed does not '
      'start a second analysis of it', () async {
    // Regression: the in-flight analysis kept its completion keyed on the
    // staged (sandbox) path, so the saved example read as not loading under
    // its pool path — the view started a second full decode of the same
    // bytes, and the first one parked a dead entry in the shared cache.
    vm.beginMediaDraft();
    expect(await vm.startExampleRecording(), isNull);
    await vm.stopExampleRecording();
    analysis.gate = Completer<void>();
    final analyzing = vm.ensureReferenceAnalysis();
    expect(vm.referenceAnalysisLoading, isTrue);

    expect(await vm.save(vm.technique!), isTrue);
    expect(vm.referenceAnalysisLoading, isTrue, reason: 'the same analysis');
    unawaited(vm.ensureReferenceAnalysis()); // the screen's post-frame call
    expect(analysis.analyzeCalls, 1);

    analysis.gate!.complete();
    await analyzing;
    expect(vm.referenceAnalysisLoading, isFalse);
    expect(identical(vm.referenceAnalysis, analysis.result), isTrue);
    await vm.ensureReferenceAnalysis();
    expect(analysis.analyzeCalls, 1);
  });
}
