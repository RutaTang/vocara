// How a technique's examples are named once they cross the sandbox →
// asset-pool hand-off, end to end on the real drift repository.
//
// An imported clip is copied into the sandbox under a generated id and
// pooled under its content hash, and an uncaptioned example falls back to its
// file's basename — so a dropped "My Twang Demo.wav" read as
// "01M5Q7…T68.wav" while staged and as a 64-hex hash once saved.

import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// A [LocalStorageService] rooted at a test temp dir (the SAME root the
/// [AssetStore] uses), so importMedia works without path_provider.
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

/// Analysis fake that decodes nothing (the real one needs the native engine).
class _NullAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(String path) async => null;
}

/// Recorder fake: [start] writes a distinct WAV stub into its own temp dir
/// and [stop] reports a clean take.
class _FakeRecorder extends Fake implements MicRecordingController {
  bool recording = false;
  int _takes = 0;

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
    await File(path).writeAsBytes([1, 2, 3, ++_takes]);
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

void main() {
  late AppDatabase db;
  late Directory tmp;
  late TechniqueRepositoryDrift repo;
  late String techniqueId;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_example_identity');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    repo = TechniqueRepositoryDrift(
      db: db,
      writer: writer,
      assets: AssetStore(db: db, root: tmp, writer: writer),
    );
    techniqueId = newId();
    final now = DateTime(2026, 7);
    await repo.saveTechnique(
      Technique(id: techniqueId, name: 'Twang', createdAt: now, updatedAt: now),
    );
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<String> source(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, 'src', name));
    await f.parent.create(recursive: true);
    await f.writeAsBytes(bytes);
    return f.path;
  }

  TechniqueDetailViewModel newVm() => TechniqueDetailViewModel(
    repository: repo,
    mediaService: TechniqueMediaService(storage: _TempStorage(tmp)),
    techniqueId: techniqueId,
    analysisService: _NullAnalysis(),
    recorder: _FakeRecorder(),
  );

  Future<void> loaded(TechniqueDetailViewModel vm) async {
    for (var i = 0; i < 400 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 2));
    }
    expect(vm.technique, isNotNull);
  }

  test('a dropped clip keeps its file name through the save', () async {
    final vm = newVm();
    addTearDown(vm.dispose);
    await loaded(vm);
    vm.beginEdit();
    vm.beginMediaDraft();
    await vm.addMedia([
      await source('My Twang Demo.wav', [1, 2, 3]),
    ]);

    expect(vm.effectiveMedia.single.displayLabel, 'My Twang Demo');
    expect(await vm.save(vm.technique!), isTrue);
    expect(vm.effectiveMedia.single.displayLabel, 'My Twang Demo');

    final stored =
        (await repo.getTechnique(techniqueId) as Ok<Technique>).value;
    expect(stored.media.single.label, 'My Twang Demo');
  });

  test('recorded examples are captioned "Recording N", counting up across '
      'edit sessions', () async {
    final vm = newVm();
    addTearDown(vm.dispose);
    await loaded(vm);

    Future<void> recordOne() async {
      expect(await vm.startExampleRecording(), isNull);
      expect(await vm.stopExampleRecording(), isNotNull);
    }

    vm.beginEdit();
    vm.beginMediaDraft();
    await recordOne();
    await recordOne();
    expect(await vm.save(vm.technique!), isTrue);
    vm.endEdit();

    vm.beginEdit();
    vm.beginMediaDraft();
    await recordOne();
    expect(vm.effectiveMedia.map((m) => m.displayLabel), [
      'Recording 1',
      'Recording 2',
      'Recording 3',
    ]);
  });
}
