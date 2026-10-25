import 'dart:io';

import 'package:drift/drift.dart';
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/db/safe_migrator.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';

/// Counts what reaches the disk as a commit: every top-level transaction,
/// plus every write statement run outside one (an autocommit).
class _CommitCounter extends QueryInterceptor {
  int commits = 0;

  void _autocommit(QueryExecutor executor) {
    if (executor is! TransactionExecutor) commits++;
  }

  @override
  Future<void> commitTransaction(TransactionExecutor inner) {
    commits++;
    return super.commitTransaction(inner);
  }

  @override
  Future<int> runInsert(QueryExecutor e, String s, List<Object?> a) {
    _autocommit(e);
    return super.runInsert(e, s, a);
  }

  @override
  Future<int> runUpdate(QueryExecutor e, String s, List<Object?> a) {
    _autocommit(e);
    return super.runUpdate(e, s, a);
  }

  @override
  Future<int> runDelete(QueryExecutor e, String s, List<Object?> a) {
    _autocommit(e);
    return super.runDelete(e, s, a);
  }

  @override
  Future<void> runCustom(QueryExecutor e, String s, List<Object?> a) {
    if (!s.trimLeft().toUpperCase().startsWith('PRAGMA')) _autocommit(e);
    return super.runCustom(e, s, a);
  }
}

void main() {
  late Directory tmp;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_durability');
  });
  tearDown(() async {
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<int> pragma(AppDatabase db, String name) async =>
      (await db.customSelect('PRAGMA $name').getSingle()).data.values.first
          as int;

  test('the live store flushes every commit and checkpoint through to the '
      'drive (fullfsync on)', () async {
    // On macOS SQLite's default sync is a plain fsync, which leaves the bytes
    // in the drive's write cache: a power cut could lose saves that already
    // succeeded, or reorder a checkpoint's writes and corrupt the library.
    final persistence = await Persistence.open(
      dir: tmp,
      analyze: (_) async => (peaks: <double>[], durationMs: 0),
    );
    addTearDown(persistence.db.close);
    expect(await pragma(persistence.db, 'fullfsync'), 1);
    // The flag is per connection: the in-memory store wires the same open.
    final memory = Persistence.memory();
    addTearDown(memory.db.close);
    expect(await pragma(memory.db, 'fullfsync'), 1);
  });

  test("the migrator's own connection to the live database flushes through "
      'to the drive too', () {
    // It checkpoints the live WAL into the file and truncates it before the
    // copy is taken; without fullfsync the truncate can outlive the page
    // writes on a power cut.
    final dbPath = p.join(tmp.path, 'vocara.db');
    final live = SafeMigrator.openLive(dbPath);
    addTearDown(live.close);
    expect(live.select('PRAGMA fullfsync').first.values.first, 1);
  });

  test('a GC pass commits all of its tombstones together, not one drive '
      'flush per asset', () async {
    final counter = _CommitCounter();
    final db = AppDatabase(NativeDatabase.memory().interceptWith(counter));
    addTearDown(db.close);
    final store = AssetStore(
      db: db,
      root: tmp,
      writer: SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0'))),
    );
    const n = 6;
    final paths = <String>[];
    for (var i = 0; i < n; i++) {
      final src = File(p.join(tmp.path, 'take$i.wav'));
      await src.writeAsBytes([i, i + 1, i + 2]);
      final id = await store.importFile(src.path);
      paths.add((await store.resolve(id))!);
    }
    // Unreferenced and past the age grace window: all garbage.
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1',
    );

    counter.commits = 0;
    final removed = await store.sweepUnreferenced(minAge: Duration.zero);

    expect(removed, n);
    for (final path in paths) {
      expect(await File(path).exists(), isFalse, reason: 'bytes reclaimed');
    }
    final tombstones = await db
        .customSelect("SELECT 1 AS x FROM oplog WHERE op = 'tombstone'")
        .get();
    expect(tombstones, hasLength(n), reason: 'every delete still journaled');
    // One for the tombstones, one for the cached peaks of what was reclaimed.
    expect(counter.commits, lessThanOrEqualTo(2));
  });
}
