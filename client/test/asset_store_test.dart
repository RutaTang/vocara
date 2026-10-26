import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:drift/drift.dart' show Value;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/db/durable_fs.dart';
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

  // The copy was promoted before the dedup lookup, and the orphan sweep keeps
  // every file of a live hash: it stayed for the asset's whole life.
  test('the same bytes under a second extension keep one pool file', () async {
    final a = await makeSrc('a.wav', List.filled(1000, 3));
    final b = await makeSrc('b.m4a', List.filled(1000, 3));
    final id1 = await store.importFile(a.path);
    final id2 = await store.importFile(b.path);
    expect(id2, id1);

    final pooled = [
      await for (final e in Directory(
        p.join(tmp.path, 'assets'),
      ).list(recursive: true))
        if (e is File && !p.basename(e.path).endsWith('.part'))
          p.basename(e.path),
    ];
    expect(pooled, hasLength(1), reason: '$pooled');
    expect(pooled.single, endsWith('.wav'), reason: 'the row\'s file stays');
    expect(File((await store.resolve(id1))!).existsSync(), isTrue);
  });

  test(
    'another name of a row\'s own file is not trashed as a duplicate',
    () async {
      // On a case-insensitive volume (macOS's default) `<hash>.WAV` is the
      // row's `<hash>.wav`: importing the same bytes as x.WAV reused that file,
      // found no row on the upper-case path, and moved the row's only copy to
      // the trash. Where names are case-sensitive, a hard link stands in.
      final a = await makeSrc('a.wav', List.filled(1000, 3));
      final b = await makeSrc('b.WAV', List.filled(1000, 3));
      final id = await store.importFile(a.path);
      final own = (await store.resolve(id))!;
      final alias = p.join(
        p.dirname(own),
        '${p.basenameWithoutExtension(own)}.WAV',
      );
      if (!File(alias).existsSync()) {
        final ln = await Process.run('ln', [own, alias]);
        expect(ln.exitCode, 0, reason: '${ln.stderr}');
      }

      expect(await store.importFile(b.path), id);
      expect(File(own).existsSync(), isTrue, reason: 'the row lost its bytes');
      expect(File(alias).existsSync(), isTrue, reason: 'moved to the trash');
    },
  );

  test(
    'a copy another import claims meanwhile is not trashed under it',
    () async {
      // Imports share the write gate. This one deduped its .m4a copy to a .wav
      // row and was about to move the copy to the trash; meanwhile that row was
      // deleted, and another import of the same bytes as .m4a promoted onto the
      // copy and went on to point a new row at it. The move left that asset
      // with no bytes.
      final a = await makeSrc('a.wav', List.filled(1000, 3));
      final b = await makeSrc('b.m4a', List.filled(1000, 3));
      final c = await makeSrc('c.m4a', List.filled(1000, 3));
      final id1 = await store.importFile(a.path);
      final promoted = Completer<void>();
      final release = Completer<void>();
      Future<String>? other;
      store.debugBeforeTrashingDuplicate = () async {
        store.debugBeforeTrashingDuplicate = null;
        await (db.update(db.assets)..where((r) => r.id.equals(id1))).write(
          const AssetsCompanion(deletedAt: Value(1)),
        );
        store.debugAfterPromote = (_) async {
          store.debugAfterPromote = null;
          promoted.complete();
          await release.future;
        };
        other = store.importFile(c.path);
        // Let it promote, where its turn allows.
        await Future.any([
          promoted.future,
          Future<void>.delayed(const Duration(milliseconds: 500)),
        ]);
      };
      expect(await store.importFile(b.path), id1);
      release.complete();
      final claimed = await other!;
      expect(claimed, isNot(id1));
      final file = await store.resolve(claimed);
      expect(file, isNotNull, reason: 'the claiming asset lost its bytes');
      expect(File(file!).existsSync(), isTrue);
    },
  );

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

  test('the first import into an empty pool flushes every directory it '
      'creates into its parent, not just the file into its shard', () async {
    final fs = _DirFlushLog();
    final logged = AssetStore(
      db: db,
      root: tmp,
      writer: SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0'))),
      fs: fs,
    );
    final id = await logged.importFile(
      (await makeSrc('take.wav', [1, 2, 3])).path,
    );
    final rel = (await (db.select(
      db.assets,
    )..where((a) => a.id.equals(id))).getSingle()).localRelPath!;

    // assets/<ab>/<cd>/<hash>.wav: this import created assets/, <ab> and
    // <cd> as well as the file, so a power cut after the row commits must
    // find every link of that chain — each new directory flushed into its
    // parent (root, assets/, <ab>), not only the file into <cd>.
    final shard = p.dirname(rel);
    final flushed = fs.dirs.map((d) => p.relative(d, from: tmp.path));
    expect(flushed, containsAll(['.', 'assets', p.dirname(shard), shard]));
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

  test('resolveOrImportAll answers each reference exactly as resolveOrImport '
      'would at that point of a sequential walk', () async {
    final a = await store.importFile((await makeSrc('a.wav', [1, 1])).path);
    final pooledA = (await store.resolve(a))!;
    // A pool file whose only asset row is tombstoned has no live owner, so
    // its first reference IMPORTS it — minting the live owner that a later
    // reference to the same path must then resolve to (an owner map read
    // once up front would miss it and import a second time).
    final t = await store.importFile((await makeSrc('t.wav', [2, 2])).path);
    final pooledT = (await store.resolve(t))!;
    await (db.update(db.assets)..where((x) => x.id.equals(t))).write(
      const AssetsCompanion(deletedAt: Value(1)),
    );
    final fresh = (await makeSrc('fresh.wav', [3, 3])).path;
    final missing = 'M'.padRight(26, '0');

    final paths = [
      pooledA,
      AssetStore.refFor(missing),
      fresh,
      pooledT,
      pooledT,
      fresh,
      pooledA,
    ];
    final ids = await store.resolveOrImportAll(paths);

    expect(ids[0], a);
    expect(ids[1], missing); // an asset:// ref keeps its embedded id
    expect(ids[2], isNot(anyOf(a, t))); // imported
    expect(ids[3], isNot(anyOf(a, t, ids[2]))); // re-imported, new live row
    expect(ids[4], ids[3]); // the owner that import just minted
    expect(ids[5], ids[2]); // deduped by content
    expect(ids[6], a);
    expect(await db.select(db.assets).get(), hasLength(4));
    // One at a time, on the store this left, every answer is the same.
    for (var i = 0; i < paths.length; i++) {
      expect(await store.resolveOrImport(paths[i]), ids[i], reason: '#$i');
    }
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

  test('a kept staging file keeps its sidecars, and only those', () async {
    final kept = File(p.join(tmp.path, 'audio', 'p1', 'take.wav'));
    final sidecars = [File('${kept.path}.sha256'), File('${kept.path}.retry')];
    // A dot in a folder name is not a sidecar's: nothing in it is kept.
    final inFolder = File(p.join('${kept.path}.d', 'other'));
    final other = File(p.join(tmp.path, 'audio', 'p1', 'other.wav'));
    for (final f in [kept, ...sidecars, inFolder, other]) {
      await f.parent.create(recursive: true);
      await f.writeAsBytes([0]);
    }

    await store.sweepStaging(keep: {kept.path});

    expect(await kept.exists(), isTrue);
    for (final f in sidecars) {
      expect(await f.exists(), isTrue, reason: f.path);
    }
    expect(await inFolder.exists(), isFalse);
    expect(await other.exists(), isFalse);
  });

  test('a landed import leaves nothing counted as still copying', () async {
    // A quit waits on a Save that is still pooling a large file, judged by
    // this count moving (see RecordingFinalizer): a staging file left in it
    // would read as a copy in progress for the rest of the session.
    expect(AssetStore.copiesInFlight, 0);
    final src = await makeSrc('big.wav', List<int>.filled(512 * 1024, 7));
    await store.importFile(src.path);
    // The set itself, not the byte count: a path left in it names a file
    // already renamed into the pool, which reads as 0 bytes either way.
    expect(AssetStore.copiesInFlight, 0);
  });

  test('a copy in flight is counted while its bytes land', () async {
    // The quit's only evidence that a Save past its budget is still working.
    final src = await makeSrc(
      'huge.wav',
      Uint8List(32 << 20)..fillRange(0, 32 << 20, 7),
    );
    var copying = false;
    var seenBytes = 0;
    final import = store.importFile(src.path);
    while (!copying || seenBytes == 0) {
      final done = await import
          .then((_) => true)
          .timeout(const Duration(milliseconds: 1), onTimeout: () => false);
      if (done) break;
      copying |= AssetStore.copiesInFlight == 1;
      final bytes = await AssetStore.bytesBeingCopied();
      if (bytes > seenBytes) seenBytes = bytes;
    }
    await import;
    expect(copying, isTrue, reason: 'the copy was never listed');
    expect(seenBytes, greaterThan(0), reason: 'its bytes were never counted');
    expect(AssetStore.copiesInFlight, 0);
  });

  group('a staged recording remembers the hash its import measured', () {
    // Boot recovery judges each staged take by its content hash; a graceful
    // stop leaves the session's takes in staging after the save pooled them,
    // and the next boot re-read every one in full to hash it again.
    Future<File> staged(String name, List<int> bytes) async {
      final f = File(p.join(tmp.path, 'audio', 'practice1', name));
      await f.parent.create(recursive: true);
      return f..writeAsBytesSync(bytes);
    }

    test('and reports it while the file is unchanged', () async {
      final f = await staged('take.wav', List.generate(4096, (i) => i & 0xff));
      await store.importFile(f.path);
      expect(
        await AssetStore.knownHashOf(f),
        await AssetStore.sha256OfFile(f.path),
      );
      f.writeAsBytesSync([1, 2, 3], mode: FileMode.append);
      expect(await AssetStore.knownHashOf(f), isNull, reason: 'it changed');
    });

    test('only for files in the staging tree', () async {
      final elsewhere = File(p.join(tmp.path, 'import.wav'))
        ..writeAsBytesSync(List.filled(64, 7));
      await store.importFile(elsewhere.path);
      expect(await AssetStore.knownHashOf(elsewhere), isNull);
      expect(File('${elsewhere.path}.sha256').existsSync(), isFalse);
    });

    test('only for recordings boot recovery looks at, not the imports and '
        'edits staged below them', () async {
      final below = await staged(
        p.join('imports', 'guide.wav'),
        List.filled(64, 9),
      );
      await store.importFile(below.path);
      expect(File('${below.path}.sha256').existsSync(), isFalse);
      expect(await AssetStore.importedHashOf(below), isNull);
    });

    test('and lands it on the drive before the import goes on', () async {
      // Boot recovery trusts this record over a staging file a power cut
      // rolled back, so it must be on disk before any row names the bytes.
      final fs = _DirFlushLog();
      final logged = AssetStore(
        db: db,
        root: tmp,
        writer: SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0'))),
        fs: fs,
      );
      final f = await staged('take.wav', List.generate(512, (i) => i & 0xff));
      await logged.importFile(f.path);
      expect(fs.dirs, contains(f.parent.path));
      // Kept whatever later happens to the file: it answers for what the
      // import pooled.
      f.writeAsBytesSync([1, 2, 3]);
      expect(await AssetStore.knownHashOf(f), isNull);
      expect(
        await AssetStore.importedHashOf(f),
        isNot(await AssetStore.sha256OfFile(f.path)),
      );
      expect(await AssetStore.importedHashOf(f), isNotNull);
    });
  });
}

/// Records every directory flush (the real flush still runs underneath).
class _DirFlushLog extends DurableFs {
  final List<String> dirs = [];

  @override
  Future<void> fsyncDir(String path) async {
    dirs.add(path);
    await super.fsyncDir(path);
  }
}
