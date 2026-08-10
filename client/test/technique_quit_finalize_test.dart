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
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo implements TechniqueRepository {
  _FakeRepo(this.technique);

  Technique technique;
  int baseSaves = 0;

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
    baseSaves++;
    technique = t;
    return Result.ok(Loaded(technique, const {}));
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

/// Analysis fake that decodes nothing: the quit path under test must persist
/// the recorded file regardless of any analysis outcome.
class _NullAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(String path) async => null;
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

const _techniqueId = 'T1000000000000000000000000';

Technique _technique() {
  final now = DateTime(2026, 6, 20);
  return Technique(
    id: _techniqueId,
    name: 'Twang',
    createdAt: now,
    updatedAt: now,
  );
}

void main() {
  late Directory tmp;
  late _FakeRepo repo;
  late TechniqueDetailViewModel vm;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_quit_finalize');
    repo = _FakeRepo(_technique());
    vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: _TempStorage(tmp)),
      techniqueId: _techniqueId,
      analysisService: _NullAnalysis(),
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

  test('quit finalize PERSISTS an in-flight example recording (the draft is '
      'in-memory and dies with the process)', () async {
    // Regression: the quit path imported the clip and staged it only into
    // _draftAdds, so after the process exit no row referenced the file and
    // the next boot's staging sweep trashed the only copy.
    vm.beginMediaDraft();
    expect(await vm.startExampleRecording(), isNull);
    expect(vm.isRecordingExample, isTrue);

    await vm.quitFinalize();

    expect(vm.isRecordingExample, isFalse);
    expect(
      repo.technique.media,
      hasLength(1),
      reason: 'the captured clip must be committed, not just staged',
    );
    final persisted = repo.technique.media.single;
    expect(persisted.kind, TechniqueMediaKind.audio);
    expect(File(persisted.path).existsSync(), isTrue);
    // The commit adopted the clip: it is no longer double-listed as both a
    // persisted row and a draft addition.
    expect(vm.effectiveMedia, hasLength(1));
    expect(vm.referenceMedia!.id, persisted.id);
  });

  test('quit finalize of a practice take persists nothing (ephemeral by '
      'design)', () async {
    expect(await vm.startPractice(), isNull);
    expect(vm.isRecordingPractice, isTrue);

    await vm.quitFinalize();

    expect(vm.isRecordingPractice, isFalse);
    expect(repo.baseSaves, 0);
    expect(repo.technique.media, isEmpty);
  });
}
