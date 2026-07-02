import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late AssetStore store;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_assets_test');
    final clock = HlcClock(node: 'NODE'.padRight(26, '0'));
    store = AssetStore(db: db, root: tmp, writer: SyncWriter(db, clock));
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<File> makeSrc(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return f;
  }

  test(
    'imports a file into the content-addressed pool + journals it',
    () async {
      final src = await makeSrc('src.wav', [1, 2, 3, 4, 5]);
      final id = await store.importFile(src.path);

      expect(id, hasLength(26));
      final row = await (db.select(
        db.assets,
      )..where((a) => a.id.equals(id))).getSingle();
      expect(row.byteSize, 5);
      expect(row.contentHash, hasLength(64)); // sha256 hex
      expect(row.localRelPath, startsWith('assets${p.separator}'));
      expect(row.bytesSyncState, 'localOnly');

      final abs = await store.resolve(id);
      expect(abs, isNotNull);
      expect(await File(abs!).readAsBytes(), [1, 2, 3, 4, 5]);
      // The file lives under the sharded pool, not at the source path.
      expect(p.isWithin(p.join(tmp.path, 'assets'), abs), isTrue);

      final ops = await db.select(db.oplog).get();
      expect(
        ops.where((o) => o.entityTable == 'assets' && o.op == 'insert'),
        hasLength(1),
      );
    },
  );

  test('dedups identical bytes to one asset + one file', () async {
    final a = await makeSrc('a.wav', [9, 9, 9]);
    final b = await makeSrc('b.wav', [9, 9, 9]); // same bytes, different source
    final id1 = await store.importFile(a.path);
    final id2 = await store.importFile(b.path);

    expect(id1, id2);
    expect(await db.select(db.assets).get(), hasLength(1));
  });

  test('different bytes produce different assets', () async {
    final id1 = await store.importFile((await makeSrc('a.wav', [1])).path);
    final id2 = await store.importFile((await makeSrc('b.wav', [2])).path);
    expect(id1, isNot(id2));
    expect(await db.select(db.assets).get(), hasLength(2));
  });

  test(
    'resolve re-materializes a deduped asset whose file went missing',
    () async {
      final src = await makeSrc('src.wav', [7, 7, 7, 7]);
      final id = await store.importFile(src.path);
      final abs = (await store.resolve(id))!;
      await File(abs).delete(); // simulate local GC of the bytes
      expect(await store.resolve(id), isNull);
      // Re-importing the same bytes reuses the row AND re-places the file.
      final id2 = await store.importFile(
        (await makeSrc('again.wav', [7, 7, 7, 7])).path,
      );
      expect(id2, id);
      expect(await store.resolve(id), isNotNull);
    },
  );

  test('importFile tolerates two LIVE assets sharing a content hash (the '
      'cross-device steady state) instead of throwing', () async {
    final idA = await store.importFile(
      (await makeSrc('dup.wav', [3, 1, 4, 1, 5])).path,
    );
    final hash = (await (db.select(
      db.assets,
    )..where((a) => a.id.equals(idA))).getSingle()).contentHash;
    // Simulate a synced remote asset: identical bytes under a different id (the
    // schema keeps content_hash NON-unique on purpose).
    const idB = 'BBBBBBBBBBBBBBBBBBBBBBBBBB';
    await db.customStatement(
      'INSERT INTO assets (id, content_hash, byte_size, created_at, updated_at, '
      'hlc_wall, hlc_ctr, hlc_node, hlc_packed) '
      "VALUES ('$idB', '$hash', 5, 1, 1, 1, 0, 'NNNNNNNNNNNNNNNNNNNNNNNNNN', 'z')",
    );

    // Two live rows now share the hash — importFile must resolve, not throw.
    final again = await store.importFile(
      (await makeSrc('dup2.wav', [3, 1, 4, 1, 5])).path,
    );
    expect([idA, idB], contains(again));
  });

  test(
    'sweepStaging clears audio/ and technique_media/ but never the pool',
    () async {
      // A pooled asset (under assets/) plus stray staging files.
      final id = await store.importFile(
        (await makeSrc('keep.wav', [4, 2])).path,
      );
      final pooled = (await store.resolve(id))!;

      final staged = <File>[
        File(p.join(tmp.path, 'audio', 'p1', 'take.wav')),
        File(p.join(tmp.path, 'technique_media', 't1', 'ex.wav')),
      ];
      for (final f in staged) {
        await f.parent.create(recursive: true);
        await f.writeAsBytes([0]);
      }

      await store.sweepStaging();

      for (final f in staged) {
        expect(await f.exists(), isFalse); // staging reclaimed
      }
      expect(await Directory(p.join(tmp.path, 'audio')).exists(), isFalse);
      expect(await File(pooled).exists(), isTrue); // pool untouched
      expect(await store.resolve(id), isNotNull);
    },
  );
}
