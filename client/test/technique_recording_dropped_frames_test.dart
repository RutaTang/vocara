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
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
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

/// Analysis fake that decodes nothing: the take's analysis outcome is
/// irrelevant to the dropped-frames notice under test.
class _NullAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async => null;
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

/// Recorder fake: [start] writes a real WAV stub into its own temp dir (the
/// VM deletes that dir after the stop, and the example path copies the file),
/// and [stop] reports whatever clean/droppedFrames outcome the test staged.
class _FakeRecorder extends Fake implements MicRecordingController {
  bool recording = false;
  bool clean = true;
  int droppedFrames = 0;

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
    return (path: _path!, clean: clean, droppedFrames: droppedFrames);
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

Technique _technique() {
  final now = DateTime(2026, 6, 20);
  return Technique(
    id: 'T1000000000000000000000000',
    name: 'Twang',
    createdAt: now,
    updatedAt: now,
  );
}

void main() {
  late Directory tmp;
  late _FakeRecorder recorder;
  late TechniqueDetailViewModel vm;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_dropped_frames');
    recorder = _FakeRecorder();
    vm = TechniqueDetailViewModel(
      repository: _FakeRepo(_technique()),
      mediaService: TechniqueMediaService(storage: _TempStorage(tmp)),
      techniqueId: 'T1000000000000000000000000',
      analysisService: _NullAnalysis(),
      recorder: recorder,
    );
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);
  });

  tearDown(() async {
    vm.dispose();
    // dispose() fire-and-forgets deletion of the staged draft files living
    // under [tmp], which can race this recursive delete (macOS then fails
    // with "Directory not empty"). Retry briefly until the unawaited deletes
    // have drained.
    for (var i = 0; i < 50; i++) {
      try {
        if (await tmp.exists()) await tmp.delete(recursive: true);
        break;
      } on FileSystemException {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
    }
  });

  test('a clean practice stop with ring overflow surfaces the dropped-frames '
      'notice', () async {
    // Regression: stop() reports droppedFrames but the VM used to read only
    // .clean — a time-compressed take (ring overflow while the writer
    // stalled) was presented as gapless.
    recorder.droppedFrames = 4800; // 100 ms at the 48 kHz recording ring
    expect(await vm.startPractice(), isNull);
    expect(vm.isRecordingPractice, isTrue);

    await vm.stopPractice();

    expect(vm.recordingNotice, contains('dropped about 100 ms'));
    expect(vm.recordingNotice, contains('small gaps'));
  });

  test('a clean practice stop without overflow surfaces no notice', () async {
    expect(await vm.startPractice(), isNull);
    await vm.stopPractice();
    expect(vm.recordingNotice, isNull);
  });

  test('a clean example stop with ring overflow surfaces the notice AND '
      'keeps the clip', () async {
    recorder.droppedFrames = 9600; // 200 ms at the 48 kHz recording ring
    vm.beginMediaDraft();
    expect(await vm.startExampleRecording(), isNull);
    expect(vm.isRecordingExample, isTrue);

    await vm.stopExampleRecording();

    expect(vm.recordingNotice, contains('dropped about 200 ms'));
    expect(vm.recordingNotice, contains('small gaps'));
    // Most of the take is fine, so it is staged as usual — the notice only
    // stops it from masquerading as gapless.
    expect(vm.effectiveMedia, hasLength(1));
  });

  test('a clean example stop without overflow surfaces no notice', () async {
    vm.beginMediaDraft();
    expect(await vm.startExampleRecording(), isNull);
    await vm.stopExampleRecording();
    expect(vm.recordingNotice, isNull);
    expect(vm.effectiveMedia, hasLength(1));
  });

  test('dispose during an airborne save never deletes the staged files the '
      'commit is persisting (real file, in-flight dispose)', () async {
    // The route can pop while save() awaits the repository. dispose() used to
    // delete every staged-addition file unconditionally — the commit then
    // landed rows pointing at nothing.
    final gated = _GatedRepo(_technique());
    final rec2 = _FakeRecorder();
    final vm2 = TechniqueDetailViewModel(
      repository: gated,
      mediaService: TechniqueMediaService(storage: _TempStorage(tmp)),
      techniqueId: 'T1000000000000000000000000',
      analysisService: _NullAnalysis(),
      recorder: rec2,
    );
    for (var i = 0; i < 200 && vm2.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm2.technique, isNotNull);

    // Stage a REAL recorded example file into the draft.
    vm2.beginMediaDraft();
    expect(await vm2.startExampleRecording(), isNull);
    await vm2.stopExampleRecording();
    final staged = vm2.effectiveMedia.single.path;
    expect(File(staged).existsSync(), isTrue);

    // Save goes airborne, then the screen is torn down mid-await.
    final saving = vm2.save(vm2.technique!.copyWith(summary: 'kept'));
    expect(vm2.saving, isTrue);
    vm2.dispose();
    // Let any (wrong) fire-and-forget deletions run before asserting.
    await Future<void>.delayed(const Duration(milliseconds: 50));
    expect(
      File(staged).existsSync(),
      isTrue,
      reason: 'dispose must not delete files the airborne commit references',
    );

    gated.gate.complete();
    expect(await saving, isTrue);
    expect(gated.technique.media.single.path, staged);
    expect(
      File(staged).existsSync(),
      isTrue,
      reason: 'the committed row must keep pointing at a real file',
    );
  });
}

/// A repo whose base-hash save parks on [gate] — models the airborne commit
/// window the dispose test tears the screen down inside of.
class _GatedRepo extends _FakeRepo {
  _GatedRepo(super.technique);

  final gate = Completer<void>();

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async {
    await gate.future;
    return super.saveTechniqueWithBase(t, baseHashes: baseHashes);
  }
}
