import 'dart:io';

import 'package:drift/drift.dart'
    show ApplyInterceptor, QueryExecutor, QueryInterceptor;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// Counts the reads a repository call issues.
class _Reads extends QueryInterceptor {
  int count = 0;

  @override
  Future<List<Map<String, Object?>>> runSelect(
    QueryExecutor executor,
    String statement,
    List<Object?> args,
  ) {
    count++;
    return super.runSelect(executor, statement, args);
  }
}

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

  test(
    'EVERY technique field round-trips (no column silently dropped)',
    () async {
      // The counterpart of the practice repository's field-by-field round-trip:
      // every user-editable field on the technique and on each media row set to a
      // distinct non-default value, so a column the writer forgets — or the
      // hydrator never reads — surfaces here instead of as a lost edit.
      final a = await media('a.wav', [1, 2, 3]);
      final b = await media('b.wav', [4, 5, 6]);
      final t = Technique(
        id: newId(),
        name: 'Belt',
        aliases: const ['Chest mix', 'Yell'],
        tags: const ['CVT', 'EVT', 'loud'],
        summary: 'a summary',
        description: 'a description',
        cues: 'some cues',
        exercises: 'some exercises',
        acousticSignature: 'high SPL, strong 2-4 kHz',
        media: [
          a.copyWith(label: 'first label'),
          TechniqueMedia(
            id: b.id,
            path: b.path,
            kind: TechniqueMediaKind.video,
            label: 'second label',
            addedAt: b.addedAt,
          ),
        ],
        createdAt: DateTime(2026, 1, 2),
        updatedAt: DateTime(2026, 3, 4),
      );
      expect(await repo.saveTechnique(t), isA<Ok<Technique>>());

      final l = ((await repo.getTechnique(t.id)) as Ok<Technique>).value;
      expect(l.name, 'Belt');
      expect(l.aliases, ['Chest mix', 'Yell']);
      expect(l.tags, ['CVT', 'EVT', 'loud']);
      expect(l.summary, 'a summary');
      expect(l.description, 'a description');
      expect(l.cues, 'some cues');
      expect(l.exercises, 'some exercises');
      expect(l.acousticSignature, 'high SPL, strong 2-4 kHz');
      expect(l.media.map((m) => m.label), ['first label', 'second label']);
      expect(l.media.map((m) => m.kind), [
        TechniqueMediaKind.audio,
        TechniqueMediaKind.video,
      ]);
      // The order the user arranged them in is part of the content.
      expect(l.media.map((m) => m.id), [t.media[0].id, t.media[1].id]);
      expect(await File(l.media.first.path).readAsBytes(), [1, 2, 3]);
      expect(await File(l.media.last.path).readAsBytes(), [4, 5, 6]);
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

  test('the library lists in the same few reads however many techniques and '
      'examples it holds, each example in order with its path', () async {
    final reads = _Reads();
    final counted = AppDatabase(NativeDatabase.memory().interceptWith(reads));
    addTearDown(counted.close);
    final writer = SyncWriter(
      counted,
      HlcClock(node: 'NODE'.padRight(26, '0')),
    );
    final library = TechniqueRepositoryDrift(
      db: counted,
      writer: writer,
      assets: AssetStore(db: counted, root: tmp, writer: writer),
    );
    var made = 0;
    Future<int> listCost(int add) async {
      for (var i = 0; i < add; i++, made++) {
        await library.saveTechnique(
          technique(
            name: 'T$made',
            media: [
              await media('a$made.wav', [made, 1]),
              await media('b$made.wav', [made, 2]),
            ],
          ),
        );
      }
      reads.count = 0;
      final listed =
          (await library.listTechniques() as Ok<List<Technique>>).value;
      expect(listed, hasLength(made));
      for (final t in listed) {
        final n = t.name.substring(1);
        expect(t.media.map((m) => m.label), ['a$n.wav', 'b$n.wav']);
        for (final m in t.media) {
          expect(await File(m.path).exists(), isTrue);
        }
      }
      return reads.count;
    }

    // techniques + their media + those media's assets. It was 1 + one per
    // technique + one per example (7 vs 31 here).
    final few = await listCost(2);
    expect(await listCost(8), few);
    expect(few, 3);
  });

  test(
    'a row whose stored JSON or kind this build reads differently is no '
    'edit: an untouched save leaves it, and keeps a concurrent rename',
    () async {
      // Hydration decodes the alias/tag JSON and folds an example kind it does
      // not know into audio, but the base hash was the stored row's: any save
      // rewrote such rows as edits nobody made, reverting a concurrent edit.
      final saved =
          (await repo.saveTechnique(
                    technique(
                      media: [
                        await media('a.wav', [1]),
                      ],
                    ),
                  )
                  as Ok<Technique>)
              .value;
      final mediaId = saved.media.single.id;
      await db.customStatement(
        'UPDATE techniques SET aliases_json = ?, tags_json = ? WHERE id = ?',
        ['["belt", "call"]', '[ "EVT" ]', saved.id],
      );
      await db.customStatement(
        "UPDATE technique_media SET kind = 'image' WHERE id = ?",
        [mediaId],
      );
      final loaded =
          (await repo.loadTechnique(saved.id) as Ok<Loaded<Technique>>).value;
      Future<int> ops() async =>
          (await db.customSelect('SELECT COUNT(*) AS n FROM oplog').getSingle())
              .read<int>('n');
      final before = await ops();
      await db.customStatement(
        "UPDATE techniques SET name = 'Remote', hlc_packed = 'zzzz' WHERE id = ?",
        [saved.id],
      );
      expect(
        await repo.saveTechniqueWithBase(
          loaded.aggregate,
          baseHashes: loaded.baseHashes,
        ),
        isA<Ok<Loaded<Technique>>>(),
      );
      final row = await (db.select(
        db.techniques,
      )..where((t) => t.id.equals(saved.id))).getSingle();
      expect(row.name, 'Remote');
      expect(row.aliasesJson, '["belt", "call"]');
      expect(row.tagsJson, '[ "EVT" ]');
      final m = await (db.select(
        db.techniqueMedia,
      )..where((x) => x.id.equals(mediaId))).getSingle();
      expect(m.kind, 'image');
      expect(await ops(), before);
    },
  );
}
