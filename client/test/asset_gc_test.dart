import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late AssetStore assets;
  late SongPracticeRepositoryDrift repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_gc');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    assets = AssetStore(db: db, root: tmp, writer: writer);
    repo = SongPracticeRepositoryDrift(db: db, writer: writer, assets: assets);
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

  test('sweep tombstones + deletes a wholly-unreferenced asset', () async {
    final id = await assets.importFile(await makeSrc('lone.wav', [1, 2, 3]));
    final path = (await assets.resolve(id))!;
    expect(await File(path).exists(), isTrue);

    expect(await assets.sweepUnreferenced(), 1);
    expect(await assets.resolve(id), isNull); // tombstoned
    expect(await File(path).exists(), isFalse); // file reclaimed
  });

  test(
    'sweepOrphanPoolFiles reclaims a file left after a tombstone (crash)',
    () async {
      // A second, LIVE asset must exist: with zero live rows the sweep's
      // lost-database fuse (deliberately) protects the whole pool instead.
      await assets.importFile(await makeSrc('live.wav', [1]));
      final id = await assets.importFile(await makeSrc('crash.wav', [5, 6, 7]));
      final path = (await assets.resolve(id))!;
      expect(await File(path).exists(), isTrue);
      // Simulate a crash between the tombstone commit and the file reclaim in
      // sweepUnreferenced: the row is tombstoned but the pooled file is still on disk.
      // sweepUnreferenced never revisits it (it iterates only live rows), so the
      // orphan sweep — trusting only LIVE hashes — must reclaim it.
      await db.customStatement(
        'UPDATE assets SET deleted_at = 1 WHERE id = ?',
        [id],
      );
      expect(await File(path).exists(), isTrue);
      expect(await assets.sweepOrphanPoolFiles(), 1);
      expect(await File(path).exists(), isFalse); // gone from the pool...
      // ...but parked in the trash, not unlinked.
      final trashed = (await assets.trashDir.list().toList()).whereType<File>();
      expect(
        trashed.where((f) => p.basename(f.path).endsWith(p.basename(path))),
        hasLength(1),
      );
    },
  );

  test('orphan-sweep fuse: an empty database with a populated pool touches '
      'nothing (database lost, protect the bytes)', () async {
    // Zero asset rows at all (a wiped/replaced DB file) while the pool still
    // holds content-addressed bytes — the only legitimate zero-live-rows
    // state is a fresh install with NO pool, so the sweep must skip.
    final hash = 'a' * 64;
    final orphan = File(p.join(tmp.path, 'assets', 'aa', 'aa', '$hash.wav'));
    await orphan.create(recursive: true);
    await orphan.writeAsBytes([9, 9, 9]);

    expect(await assets.sweepOrphanPoolFiles(), 0);
    expect(await orphan.exists(), isTrue); // bytes protected
    expect(await assets.trashDir.exists(), isFalse); // nothing trashed
  });

  test(
    'a GC-reclaimed pool file lands in the trash and is recoverable',
    () async {
      final id = await assets.importFile(await makeSrc('gone.wav', [1, 2, 3]));
      final path = (await assets.resolve(id))!;

      expect(await assets.sweepUnreferenced(), 1);
      expect(await File(path).exists(), isFalse); // gone from the pool...

      // ...and sitting in trash under `<epochMs>-<originalName>`, bytes intact.
      final trashed = (await assets.trashDir.list().toList())
          .whereType<File>()
          .toList();
      expect(trashed, hasLength(1));
      expect(p.basename(trashed.single.path), endsWith(p.basename(path)));
      expect(await trashed.single.readAsBytes(), [1, 2, 3]);
    },
  );

  test('pruneTrash removes only entries older than the retention', () async {
    final trash = assets.trashDir;
    await trash.create(recursive: true);
    final nowMs = DateTime.now().millisecondsSinceEpoch;
    const dayMs = 24 * 3600 * 1000;

    final expired = File(p.join(trash.path, '${nowMs - 31 * dayMs}-old.wav'));
    final fresh = File(p.join(trash.path, '$nowMs-fresh.wav'));
    // No parseable epoch prefix: never aged out (don't delete undatable bytes).
    final undated = File(p.join(trash.path, 'nostamp.wav'));
    for (final f in [expired, fresh, undated]) {
      await f.writeAsBytes([1]);
    }
    // A swept staging TREE ages out as one entry, recursively.
    final expiredTree = Directory(
      p.join(trash.path, '${nowMs - 40 * dayMs}-audio'),
    );
    final treeFile = File(p.join(expiredTree.path, 'p1', 'take.wav'));
    await treeFile.create(recursive: true);
    await treeFile.writeAsBytes([2]);

    expect(await assets.pruneTrash(), 2);
    expect(await expired.exists(), isFalse);
    expect(await expiredTree.exists(), isFalse);
    expect(await fresh.exists(), isTrue);
    expect(await undated.exists(), isTrue);
  });

  test(
    'assetRefColumns covers every schema column referencing assets',
    () async {
      // Derive the asset-referencing columns STRUCTURALLY from the live schema:
      // every foreign key into assets. If a new `.references(Assets)` column
      // ever ships without being added to AssetStore.assetRefColumns, the GC
      // would treat its assets as unreferenced and reclaim their bytes — fail
      // here instead.
      final fromSchema = <String>{};
      for (final table in db.allTables) {
        final name = table.actualTableName;
        final fks = await db
            .customSelect('PRAGMA foreign_key_list($name)')
            .get();
        for (final fk in fks) {
          if (fk.data['table'] == 'assets') {
            fromSchema.add('$name.${fk.data['from']}');
          }
        }
      }
      expect(fromSchema, isNotEmpty); // the derivation itself must keep working
      final declared = {
        for (final ref in AssetStore.assetRefColumns)
          '${ref.table}.${ref.column}',
      };
      expect(
        declared,
        fromSchema,
        reason:
            'AssetStore.assetRefColumns must list exactly the schema columns '
            'that reference assets: a missing one makes the GC delete live '
            'bytes; a stale one breaks the sweep query.',
      );
    },
  );

  test(
    'sweep deletes every pooled file for a hash (same bytes, two extensions)',
    () async {
      final id = await assets.importFile(await makeSrc('dup.wav', [7, 7, 7]));
      final wavPath = (await assets.resolve(id))!;
      // Simulate identical bytes pooled under a different extension by another device
      // (same hash, `<hash>.wav` AND `<hash>.mp3`).
      final mp3Path = wavPath.replaceAll(RegExp(r'\.wav$'), '.mp3');
      await File(wavPath).copy(mp3Path);
      expect(await File(mp3Path).exists(), isTrue);
      // The asset is unreferenced -> sweep tombstones it and deletes ALL files for the
      // hash, not just the current row's `.wav`.
      expect(await assets.sweepUnreferenced(), 1);
      expect(await File(wavPath).exists(), isFalse);
      expect(await File(mp3Path).exists(), isFalse);
    },
  );

  test(
    'a referenced asset survives sweep, then is collectable once unused',
    () async {
      final base = (await repo.createPractice('S') as Ok<SongPractice>).value;
      final clipPath = await makeSrc('take.wav', [9, 9, 9]);
      final vocal = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take',
        createdAt: DateTime(2026),
        versionId: base.versions.single.id,
        clips: [
          AudioClip(id: newId(), audioFilePath: clipPath, durationMs: 1000),
        ],
      );
      final withClip = SongPractice(
        id: base.id,
        title: base.title,
        createdAt: base.createdAt,
        updatedAt: base.updatedAt,
        vocalTracks: [vocal],
        versions: base.versions,
        activeVersionId: base.versions.single.id,
      );
      (await repo.savePractice(withClip) as Ok<SongPractice>).value;

      // One referenced asset + one standalone unreferenced asset.
      await assets.importFile(await makeSrc('orphan.wav', [4, 4]));
      expect(await db.select(db.assets).get(), hasLength(2));

      // Sweep removes only the orphan; the clip's asset survives.
      expect(await assets.sweepUnreferenced(), 1);
      final liveAssets = await (db.select(
        db.assets,
      )..where((a) => a.deletedAt.isNull())).get();
      expect(liveAssets, hasLength(1));

      // Drop the clip -> its asset is now unreferenced -> a later sweep reclaims it.
      (await repo.savePractice(
                SongPractice(
                  id: base.id,
                  title: base.title,
                  createdAt: base.createdAt,
                  updatedAt: base.updatedAt,
                  versions: base.versions,
                  activeVersionId: base.versions.single.id,
                ),
              )
              as Ok<SongPractice>)
          .value;
      expect(await assets.sweepUnreferenced(), 1);
      final remaining = await (db.select(
        db.assets,
      )..where((a) => a.deletedAt.isNull())).get();
      expect(remaining, isEmpty);
    },
  );
}
