// Sibling paths of the technique page kept in line with each other: a quit's
// example commit held like a Save, a remote change owed to a failed start or
// a failed first load, and the practice start's pending state.
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
import 'package:vocara/data/sync/sync_engine.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/utils/result.dart';

const _id = 'T1000000000000000000000000';

class _Repo implements TechniqueRepository {
  _Repo(this.technique);
  Technique technique;
  int loads = 0;
  int baseSaves = 0;
  Completer<void>? saveGate;
  final sentMediaCounts = <int>[];

  @override
  Future<Result<Technique>> getTechnique(String id) async =>
      Result.ok(technique);
  int loadFailures = 0;
  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async {
    loads++;
    if (loadFailures > 0) {
      loadFailures--;
      return Result.error(Exception('read failed'));
    }
    return Result.ok(Loaded(technique, const {}));
  }

  @override
  Future<Result<List<Technique>>> listTechniques() async =>
      Result.ok([technique]);
  @override
  Future<Result<Technique>> saveTechnique(Technique t) async =>
      Result.ok(technique = t);
  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async {
    baseSaves++;
    sentMediaCounts.add(t.media.length);
    await saveGate?.future;
    // Like resolveOrImport: every media file must still be readable.
    for (final m in t.media) {
      if (!File(m.path).existsSync()) {
        return Result.error(Exception('missing ${m.path}'));
      }
    }
    technique = t;
    return Result.ok(Loaded(t, const {}));
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

class _NoAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async => null;
}

class _Storage extends LocalStorageService {
  _Storage(this.root);
  final Directory root;
  @override
  Future<Directory> assetDir(String c) async {
    final d = Directory(p.join(root.path, c));
    if (!await d.exists()) await d.create(recursive: true);
    return d;
  }

  @override
  Future<String> assetPath(String c) async => p.join(root.path, c);
}

class _Recorder extends Fake implements MicRecordingController {
  bool recording = false;
  bool starting = false;
  Completer<MicRecordingStart>? pending;
  bool park = false;
  String? _path;
  final ValueNotifier<double> _level = ValueNotifier(0);
  @override
  ValueNotifier<double> get level => _level;
  @override
  bool get isRecording => recording;
  @override
  bool get isStarting => starting;
  @override
  bool get feedingMonitor => false;

  /// When set, a start throws (the take's temp folder could not be made).
  bool throwOnStart = false;
  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    starting = true;
    if (throwOnStart) {
      await Future<void>.delayed(Duration.zero);
      starting = false;
      throw const FileSystemException('createTemp failed');
    }
    if (park) {
      final r = await (pending = Completer<MicRecordingStart>()).future;
      starting = false;
      recording = r.started;
      return r;
    }
    final dir = await Directory.systemTemp.createTemp('vocara_probe_take');
    _path = p.join(dir.path, 'take.wav');
    await File(_path!).writeAsBytes(const [1, 2, 3]);
    starting = false;
    recording = true;
    return MicRecordingStart.started(_path!);
  }

  @override
  MicRecordingStop? stop() {
    if (!recording) return null;
    recording = false;
    return (
      path: _path ?? '/nonexistent/take.wav',
      clean: true,
      droppedFrames: 0,
    );
  }

  @override
  Future<void> guardStopFinalize(Future<void> Function() work) => work();
  @override
  Future<void> abort() async => recording = false;
  @override
  void invalidatePendingStart() => starting = false;
  @override
  void dispose() => _level.dispose();
}

/// A media import that parks each copy on [gate], counting them.
class _GatedMedia extends TechniqueMediaService {
  _GatedMedia(Directory root) : super(storage: _Storage(root));

  Completer<void> gate = Completer<void>();
  int imports = 0;

  @override
  Future<String> importMedia({
    required String techniqueId,
    required String sourcePath,
  }) async {
    imports++;
    await gate.future;
    return '/pool/$techniqueId-$imports.wav';
  }

  @override
  Future<void> deleteMedia(String path) async {}
}

Technique _t() {
  final now = DateTime(2026, 6, 20);
  return Technique(id: _id, name: 'Twang', createdAt: now, updatedAt: now);
}

void main() {
  late Directory tmp;
  setUp(
    () async => tmp = await Directory.systemTemp.createTemp('vocara_probe'),
  );
  tearDown(() async {
    try {
      await tmp.delete(recursive: true);
    } catch (_) {}
  });

  Future<
    (
      TechniqueDetailViewModel,
      _Repo,
      _Recorder,
      StreamController<RemoteApplied>,
    )
  >
  make() async {
    final repo = _Repo(_t());
    final rec = _Recorder();
    final remote = StreamController<RemoteApplied>.broadcast();
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: _Storage(tmp)),
      techniqueId: _id,
      analysisService: _NoAnalysis(),
      recorder: rec,
      remoteApplied: remote.stream,
    );
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);
    return (vm, repo, rec, remote);
  }

  RemoteApplied ev() => RemoteApplied(
    rows: const [],
    practiceIds: const {},
    techniqueIds: const {_id},
  );

  test('a failed practice start replays a remote change it held off', () async {
    // The reload owed was never run: the page kept the old text, and an edit
    // seeded from it saved over the change.
    final (vm, repo, rec, remote) = await make();
    addTearDown(remote.close);
    addTearDown(vm.dispose);
    rec.park = true;
    final starting = vm.startPractice();
    await pumpEventQueue();
    expect(vm.startingPractice, isTrue);
    final before = repo.loads;
    remote.add(ev());
    await pumpEventQueue();
    expect(repo.loads, before, reason: 'held while the start pends');
    rec.pending!.complete(
      const MicRecordingStart.failed(MicRecordingFailure.permissionDenied),
    );
    expect(await starting, isNotNull);
    await pumpEventQueue();
    expect(repo.loads, before + 1);
    expect(vm.startingPractice, isFalse);
  });

  test('a quit committing an example is held like a Save', () async {
    // Neither Save nor Cancel was refused meanwhile: Save ran a second commit
    // on the same base, and Cancel deleted the clip being committed.
    final (vm, repo, rec, remote) = await make();
    addTearDown(remote.close);
    addTearDown(vm.dispose);
    vm.beginMediaDraft();
    vm.beginEdit();
    expect(await vm.startExampleRecording(), isNull);
    repo.saveGate = Completer<void>();
    final quitting = vm.quitFinalize();
    for (var i = 0; i < 200 && repo.baseSaves == 0; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(repo.baseSaves, 1, reason: 'the quit-time commit is in flight');
    expect(vm.saving, isTrue);
    final staged = vm.effectiveMedia.single;

    expect(await vm.save(vm.technique!.copyWith(summary: 'typed')), isFalse);
    // Held off, and says so: the screen keeps the edit open on the draft.
    expect(await vm.discardMediaDraft(), isFalse);
    expect(repo.baseSaves, 1, reason: 'no second commit');
    expect(File(staged.path).existsSync(), isTrue);

    repo.saveGate!.complete();
    await quitting;
    expect(vm.saving, isFalse);
    expect(repo.technique.media, hasLength(1));
  });

  test('a remote change after a failed first load reads again', () async {
    // It was owed to the load, which had already ended: the page stayed on
    // "could not be loaded" through the change.
    final repo = _Repo(_t())..loadFailures = 1;
    final remote = StreamController<RemoteApplied>.broadcast();
    addTearDown(remote.close);
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: _Storage(tmp)),
      techniqueId: _id,
      analysisService: _NoAnalysis(),
      recorder: _Recorder(),
      remoteApplied: remote.stream,
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 200 && vm.load.running; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNull);
    remote.add(ev());
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique?.name, 'Twang');
  });

  test('an example added in this edit reads as unsaved', () async {
    // Its Remove said "Cancel the edit to keep it", but it is deleted at once.
    final (vm, repo, rec, remote) = await make();
    addTearDown(remote.close);
    addTearDown(vm.dispose);
    vm.beginMediaDraft();
    vm.beginEdit();
    expect(await vm.startExampleRecording(), isNull);
    await vm.stopExampleRecording();
    final added = vm.effectiveMedia.single;
    expect(vm.isUnsavedMedia(added.id), isTrue);
    expect(await vm.save(vm.technique!), isTrue);
    expect(vm.isUnsavedMedia(vm.technique!.media.single.id), isFalse);
  });

  test(
    'a practice start that throws is said as a failure, the button let go',
    () async {
      // Nothing was told after the throw: the button stayed on "Starting the
      // microphone…", disabled, and the error went unhandled.
      final (vm, repo, rec, remote) = await make();
      addTearDown(remote.close);
      addTearDown(vm.dispose);
      rec.throwOnStart = true;
      final seen = <bool>[];
      vm.addListener(() => seen.add(vm.startingPractice));
      expect(
        await vm.startPractice(),
        "Couldn't create the recording file. Check your free disk space.",
      );
      expect(seen.last, isFalse);
      expect(vm.canRecordPractice, isTrue);
    },
  );

  test('a cancelled import copies no more of its files', () async {
    // Cancelled during the first of four, all four were still copied (each
    // up to gigabytes), only to be deleted.
    final repo = _Repo(_t());
    final media = _GatedMedia(tmp);
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: media,
      techniqueId: _id,
      analysisService: _NoAnalysis(),
      recorder: _Recorder(),
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    vm.beginMediaDraft();
    vm.beginEdit();
    final adding = vm.addMedia([
      '/a/1.wav',
      '/a/2.wav',
      '/a/3.wav',
      '/a/4.wav',
    ]);
    await pumpEventQueue();
    expect(media.imports, 1);
    await vm.discardMediaDraft();
    media.gate.complete();
    await adding;
    expect(media.imports, 1);
    expect(vm.effectiveMedia, isEmpty);
  });
}
