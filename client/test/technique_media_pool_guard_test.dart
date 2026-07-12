import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// A [LocalStorageService] rooted at a test temp dir (the SAME root the
/// [AssetStore] uses), so the media service's pool-root guard is exercised
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
}

void main() {
  late AppDatabase db;
  late Directory tmp;
  late AssetStore assets;
  late TechniqueRepositoryDrift repo;
  late TechniqueMediaService media;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_media_guard');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    assets = AssetStore(db: db, root: tmp, writer: writer);
    repo = TechniqueRepositoryDrift(db: db, writer: writer, assets: assets);
    media = TechniqueMediaService(storage: _TempStorage(tmp));
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<String> makeSrc(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return f.path;
  }

  Technique techniqueWith(String id, String mediaId, String path) {
    final now = DateTime(2026, 7);
    return Technique(
      id: id,
      name: 'T $id',
      media: [
        TechniqueMedia(
          id: mediaId,
          path: path,
          kind: TechniqueMediaKind.audio,
          addedAt: now,
        ),
      ],
      createdAt: now,
      updatedAt: now,
    );
  }

  test('deleteMedia refuses pool files and asset:// refs, still deletes '
      'staging copies', () async {
    final id = await assets.importFile(await makeSrc('a.wav', [1, 2, 3]));
    final pooled = (await assets.resolve(id))!;

    // Content-addressed pool bytes are owned by the hash-refcount GC.
    await media.deleteMedia(pooled);
    expect(await File(pooled).exists(), isTrue);

    // A bytes-missing-locally reference is not a path at all.
    await media.deleteMedia(AssetStore.refFor(id)); // must not throw

    // A draft staging copy (what the service itself imports) is still deleted.
    final staged = await media.importMedia(
      techniqueId: 't1',
      sourcePath: await makeSrc('b.wav', [4, 5]),
    );
    expect(await File(staged).exists(), isTrue);
    await media.deleteMedia(staged);
    expect(await File(staged).exists(), isFalse);
  });

  test('removing a persisted example never reclaims shared pool bytes '
      '(end-to-end removedPaths flow)', () async {
    // Two techniques whose examples dedup to the SAME content hash (one
    // pooled file, one asset row).
    final t1Id = newId(), m1Id = newId();
    final t2Id = newId(), m2Id = newId();
    final saved1 = await repo.saveTechnique(
      techniqueWith(t1Id, m1Id, await makeSrc('one.wav', [7, 7, 7])),
    );
    expect(saved1, isA<Ok<Technique>>());
    final saved2 = await repo.saveTechnique(
      techniqueWith(t2Id, m2Id, await makeSrc('two.wav', [7, 7, 7])),
    );
    expect(saved2, isA<Ok<Technique>>());

    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: media,
      techniqueId: t1Id,
    );
    addTearDown(vm.dispose);
    // Let the constructor's load command resolve.
    for (var i = 0; vm.technique == null && i < 200; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 1));
    }
    final poolPath = vm.technique!.media.single.path;
    expect(p.isWithin(p.join(tmp.path, 'assets'), poolPath), isTrue);

    // Remove the persisted example and save: the removedPaths cleanup calls
    // deleteMedia(poolPath) — the guard must leave the pooled bytes alone.
    await vm.removeMedia(m1Id);
    expect(await vm.save(vm.technique!), isTrue);
    expect(vm.technique!.media, isEmpty);
    expect(await File(poolPath).exists(), isTrue);

    // The second technique referencing the same hash still resolves.
    final t2 = (await repo.getTechnique(t2Id) as Ok<Technique>).value;
    expect(t2.media.single.path, poolPath);
    expect(await File(poolPath).exists(), isTrue);

    // Pool bytes are reclaimed ONLY by the hash-refcount GC: with t2 still
    // referencing the hash, a sweep keeps the file in place.
    await assets.sweepUnreferenced();
    expect(await File(poolPath).exists(), isTrue);
  });
}
