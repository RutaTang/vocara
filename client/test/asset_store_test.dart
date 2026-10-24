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

  test('six concurrent imports of the same bytes all succeed and dedupe to '
      'one asset (the old shared .part name tore each other apart)', () async {
    final sources = <File>[];
    for (var i = 0; i < 6; i++) {
      // Big enough that the streaming copies genuinely interleave.
      sources.add(await makeSrc('c$i.wav', List.filled(256 * 1024, 7)));
    }
    final ids = await Future.wait(sources.map((s) => store.importFile(s.path)));
    expect(ids.toSet(), hasLength(1));
    expect(await db.select(db.assets).get(), hasLength(1));
    final abs = await store.resolve(ids.first);
    expect(abs, isNotNull);
    expect(await File(abs!).length(), 256 * 1024);
    // No staging leftovers: every .part was promoted or cleaned up.
    final staging = Directory(p.join(tmp.path, 'assets', '.staging'));
    if (await staging.exists()) {
      expect(await staging.list().toList(), isEmpty);
    }
  });

  test(
    'a truncated pool file left by a crash is replaced, not trusted',
    () async {
      final src = await makeSrc('src.wav', [1, 2, 3, 4, 5, 6, 7, 8]);
      final id = await store.importFile(src.path);
      final abs = (await store.resolve(id))!;
      // Simulate a crash leftover: the pool path exists but holds fewer bytes.
      await File(abs).writeAsBytes([1, 2, 3]);
      final id2 = await store.importFile(src.path);
      expect(id2, id);
      expect(
        await File(abs).length(),
        8,
        reason: 'existence alone must not win',
      );
    },
  );

  test('resolveOrImport of a pool path a live asset owns returns that asset '
      'without staging a copy or re-hashing the bytes', () async {
    final id = await store.importFile(
      (await makeSrc('take.wav', [1, 2, 3, 4])).path,
    );
    // What hydration hands every clip, and so what every later save passes.
    final pooled = (await store.resolve(id))!;
    final opsBefore = (await db.select(db.oplog).get()).length;

    // Bytes changed under the content address after pooling (rot / an outside
    // edit — the case verifyPoolIntegrity reports and leaves alone)...
    await File(pooled).writeAsBytes([1, 2, 3, 5], flush: true);
    // ...and no room to stage anything: a file where the staging dir goes.
    final staging = Directory(p.join(tmp.path, 'assets', '.staging'));
    if (await staging.exists()) await staging.delete(recursive: true);
    await File(staging.path).writeAsBytes([0]);
    // (A genuine import really is blocked now — the case below isn't vacuous.)
    await expectLater(
      store.importFile((await makeSrc('other.wav', [9])).path),
      throwsA(isA<FileSystemException>()),
    );

    expect(await store.resolveOrImport(pooled), id);
    // No new asset minted for the changed bytes, nothing journaled, and the
    // pooled file is still exactly what was there.
    expect(await db.select(db.assets).get(), hasLength(1));
    expect(await db.select(db.oplog).get(), hasLength(opsBefore));
    expect(await File(pooled).readAsBytes(), [1, 2, 3, 5]);
  });

  test('resolveOrImport still imports + dedups a file that is not a live '
      "asset's pool path", () async {
    final id = await store.importFile(
      (await makeSrc('take.wav', [6, 6, 6])).path,
    );
    // An external copy of the same bytes dedups to the existing asset...
    expect(
      await store.resolveOrImport((await makeSrc('copy.wav', [6, 6, 6])).path),
      id,
    );
    // ...and new bytes, even placed inside the pool tree, are imported.
    final stray = File(p.join(tmp.path, 'assets', 'stray.wav'));
    await stray.writeAsBytes([8, 8]);
    final strayId = await store.resolveOrImport(stray.path);
    expect(strayId, isNot(id));
    expect(await db.select(db.assets).get(), hasLength(2));
    expect(await File((await store.resolve(strayId))!).readAsBytes(), [8, 8]);
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
