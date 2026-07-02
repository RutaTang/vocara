import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late TechniqueRepositoryDrift repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_tech_repo');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    repo = TechniqueRepositoryDrift(
      db: db,
      writer: writer,
      assets: AssetStore(db: db, root: tmp, writer: writer),
    );
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<TechniqueMedia> media(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return TechniqueMedia(
      id: newId(),
      path: f.path,
      kind: TechniqueMediaKind.audio,
      label: name,
      addedAt: DateTime(2026),
    );
  }

  Technique technique({
    String? id,
    String name = 'Twang',
    List<TechniqueMedia> media = const [],
  }) => Technique(
    id: id ?? newId(),
    name: name,
    tags: const ['EVT'],
    summary: 'bright edge',
    media: media,
    createdAt: DateTime(2026),
    updatedAt: DateTime(2026),
  );

  test(
    'save -> load round-trips a technique + resolves its media to a path',
    () async {
      final m = await media('ex.wav', [1, 2, 3]);
      final t = technique(media: [m]);
      final saved = (await repo.saveTechnique(t) as Ok<Technique>).value;

      expect(saved.id, t.id);
      expect(saved.name, 'Twang');
      expect(saved.tags, ['EVT']);
      expect(saved.media, hasLength(1));
      // The returned path is the pooled location, and it's readable.
      expect(
        p.isWithin(p.join(tmp.path, 'assets'), saved.media.single.path),
        isTrue,
      );
      expect(await File(saved.media.single.path).readAsBytes(), [1, 2, 3]);

      final got = (await repo.getTechnique(t.id) as Ok<Technique>).value;
      expect(got.name, 'Twang');
      expect(got.media.single.id, m.id);
    },
  );

  test(
    'editing the name bumps updatedAt; an unchanged save is a no-op',
    () async {
      final t = technique();
      final v1 = (await repo.saveTechnique(t) as Ok<Technique>).value;

      // No-op save: nothing changed -> updatedAt stays, no extra oplog rows.
      final beforeOps = (await db.select(db.oplog).get()).length;
      final v1b = (await repo.saveTechnique(v1) as Ok<Technique>).value;
      expect(v1b.updatedAt, v1.updatedAt);
      expect((await db.select(db.oplog).get()).length, beforeOps);

      // Real edit -> updatedAt advances + one more oplog row.
      final v2 =
          (await repo.saveTechnique(v1.copyWith(name: 'Belting'))
                  as Ok<Technique>)
              .value;
      expect(v2.name, 'Belting');
      expect(v2.updatedAt.isAfter(v1.updatedAt), isTrue);
      expect((await db.select(db.oplog).get()).length, beforeOps + 1);
    },
  );

  test(
    'media is added / removed via reconcile; identical bytes dedup',
    () async {
      final m1 = await media('a.wav', [1, 1]);
      var t = technique(media: [m1]);
      t = (await repo.saveTechnique(t) as Ok<Technique>).value;

      // Add a second example.
      final m2 = await media('b.wav', [2, 2]);
      t =
          (await repo.saveTechnique(t.copyWith(media: [...t.media, m2]))
                  as Ok<Technique>)
              .value;
      expect(t.media, hasLength(2));
      expect(await db.select(db.assets).get(), hasLength(2));

      // Remove the first -> domain shows one; the row is tombstoned, asset kept.
      t =
          (await repo.saveTechnique(t.copyWith(media: [t.media.last]))
                  as Ok<Technique>)
              .value;
      expect(t.media, hasLength(1));
      final liveMedia = await (db.select(
        db.techniqueMedia,
      )..where((x) => x.deletedAt.isNull())).get();
      expect(liveMedia, hasLength(1));
    },
  );

  test('list + delete (tombstone)', () async {
    final a =
        (await repo.saveTechnique(technique(name: 'A')) as Ok<Technique>).value;
    await repo.saveTechnique(technique(name: 'B'));
    expect(
      (await repo.listTechniques() as Ok<List<Technique>>).value,
      hasLength(2),
    );

    await repo.deleteTechnique(a.id);
    expect(
      (await repo.listTechniques() as Ok<List<Technique>>).value,
      hasLength(1),
    );
    expect(await repo.getTechnique(a.id), isA<Error<Technique>>());
  });
}
