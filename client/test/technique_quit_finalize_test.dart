import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
import 'package:vocara/data/services/recording_finalizer.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo implements TechniqueRepository {
  _FakeRepo(this.technique);

  Technique technique;
  int baseSaves = 0;

  /// When set, every base-hash save parks on it — a commit still in flight.
  Completer<void>? saveGate;

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
    await saveGate?.future;
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

  /// Faithful to the real guard where the view model depends on it: the
  /// post-stop work is registered with the quit finalizer while in flight, so
  /// a test can drive a quit against the stop-to-commit window.
  final _inFlightTails = <Future<void>>{};

  @override
  Future<void> guardStopFinalize(Future<void> Function() work) async {
    RecordingFinalizer.instance.register(_holdQuit);
    final tail = work();
    _inFlightTails.add(tail);
    try {
      await tail;
    } finally {
      _inFlightTails.remove(tail);
      if (_inFlightTails.isEmpty) {
        RecordingFinalizer.instance.unregister(_holdQuit);
      }
    }
  }

  Future<void> _holdQuit() => Future.wait(_inFlightTails.toList());

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

/// Alternates real time (the fake take's file IO only completes on the real
/// event loop) with pumps of the test's fake zone until [ready] (bounded).
Future<void> _settleIo(WidgetTester tester, bool Function() ready) async {
  for (var i = 0; i < 200 && !ready(); i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 5)),
    );
    await tester.pump();
  }
}

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

  testWidgets('a quit while Save is finalizing a recording example waits for '
      'the take to commit', (tester) async {
    // Regression: Save stopped the take — which unregisters the armed-window
    // quit finalizer — and only then imported and committed it, with nothing
    // holding the quit. A quit in that window exited mid-save, and the take
    // survived only as a temp WAV that boot recovery never scans.
    tester.view.physicalSize = const Size(1200, 2600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    repo.saveGate = Completer<void>();
    await tester.pumpWidget(
      MaterialApp(
        home: ChangeNotifierProvider.value(
          value: vm,
          child: const TechniqueDetailScreen(),
        ),
      ),
    );
    await tester.pump();
    await tester.tap(find.text('Edit').first);
    await tester.pump();
    // The fake take and its import are real file IO.
    await tester.runAsync(vm.startExampleRecording);
    await tester.pump();
    expect(vm.isRecordingExample, isTrue);

    await tester.tap(find.text('Save'));
    await tester.pump();
    await _settleIo(tester, () => repo.baseSaves == 1);
    expect(repo.baseSaves, 1, reason: 'imported; the commit is in flight');

    var quitDone = false;
    final quit = RecordingFinalizer.instance.finalizeAll().whenComplete(
      () => quitDone = true,
    );
    await tester.pump();
    expect(
      quitDone,
      isFalse,
      reason: 'the quit gate must cover the stop-to-commit window',
    );

    repo.saveGate!.complete();
    await tester.pump();
    await tester.pump();
    expect(quitDone, isTrue);
    await quit;
    expect(repo.technique.media, hasLength(1), reason: 'the take committed');
    expect(File(repo.technique.media.single.path).existsSync(), isTrue);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    expect(tester.takeException(), isNull);
  });
}
