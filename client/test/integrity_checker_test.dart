import 'dart:io';

import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late SongPracticeRepositoryDrift repo;
  late IntegrityChecker checker;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_integrity');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    final assets = AssetStore(db: db, root: tmp, writer: writer);
    repo = SongPracticeRepositoryDrift(db: db, writer: writer, assets: assets);
    checker = IntegrityChecker(db, writer);
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  /// Seeds a practice with one vocal take (one clip) and returns it.
  Future<SongPractice> seed() async {
    final base = (await repo.createPractice('S') as Ok<SongPractice>).value;
    final f = File(p.join(tmp.path, 'take.wav'));
    await f.writeAsBytes([1, 2, 3]);
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      createdAt: DateTime(2026),
      versionId: base.versions.single.id,
      clips: [AudioClip(id: newId(), audioFilePath: f.path, durationMs: 1000)],
    );
    return (await repo.savePractice(
              SongPractice(
                id: base.id,
                title: base.title,
                createdAt: base.createdAt,
                updatedAt: base.updatedAt,
                vocalTracks: [vocal],
                versions: base.versions,
                activeVersionId: base.versions.single.id,
              ),
            )
            as Ok<SongPractice>)
        .value;
  }

  Future<void> rawTombstone(String table, String id) => db.customUpdate(
    'UPDATE $table SET deleted_at = 999 WHERE id = ?',
    variables: [Variable.withString(id)],
  );

  Future<String> firstId(String table) async {
    final r = await db
        .customSelect('SELECT id FROM $table WHERE deleted_at IS NULL LIMIT 1')
        .getSingle();
    return r.read<String>('id');
  }

  test('a well-formed practice has no violations', () async {
    await seed();
    expect(await checker.check(), isEmpty);
  });

  test(
    'a live clip under a tombstoned track is detected + cascade-repaired',
    () async {
      await seed();
      await rawTombstone('tracks', await firstId('tracks'));

      final found = await checker.check();
      expect(found.map((v) => v.invariant), contains('clip-track'));

      expect(await checker.repair(), isEmpty);
      // The orphaned clip is now tombstoned too.
      final liveClips = await db
          .customSelect(
            'SELECT COUNT(*) AS n FROM clips WHERE deleted_at IS NULL',
          )
          .getSingle();
      expect(liveClips.read<int>('n'), 0);
    },
  );

  test('a dangling active_interpretation_id is nulled', () async {
    final pr = await seed();
    await rawTombstone('interpretations', pr.versions.single.id);

    final found = await checker.check();
    expect(found.map((v) => v.invariant), contains('active-interp'));

    await checker.repair();
    final row = await db
        .customSelect(
          'SELECT active_interpretation_id AS a FROM practices WHERE id = ?',
          variables: [Variable.withString(pr.id)],
        )
        .getSingle();
    expect(row.data['a'], isNull);
    expect(await checker.check(), isEmpty);
  });

  test('a clip whose asset was tombstoned is repaired', () async {
    await seed();
    await rawTombstone('assets', await firstId('assets'));

    expect(
      (await checker.check()).map((v) => v.invariant),
      contains('clip-asset'),
    );
    expect(await checker.repair(), isEmpty);
  });

  test('repair composes into an enclosing transaction: a rollback reverts '
      'both the row mutations and their oplog entries', () async {
    // Persistence.repairLibrary (like the sync engine's end-of-pull repair)
    // wraps repair() in ONE db.transaction so each repair's row UPDATE and
    // its journal entry can never commit separately. This proves the repair
    // internals genuinely compose into a caller transaction: everything a
    // full repair pass wrote is undone atomically by a rollback.
    await seed();
    await rawTombstone('tracks', await firstId('tracks'));
    expect(await checker.check(), isNotEmpty);
    final opsBefore = (await db.select(db.oplog).get()).length;

    await expectLater(
      db.transaction(() async {
        await checker.repair();
        throw StateError('injected post-repair crash');
      }),
      throwsStateError,
    );

    // The violations are all still present and not one journal entry leaked.
    expect(await checker.check(), isNotEmpty);
    expect((await db.select(db.oplog).get()).length, opsBefore);

    // The same repair inside a COMMITTED transaction resolves everything and
    // journals it.
    await db.transaction(() => checker.repair());
    expect(await checker.check(), isEmpty);
    expect((await db.select(db.oplog).get()).length, greaterThan(opsBefore));
  });
}
