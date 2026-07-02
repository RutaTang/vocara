import 'dart:io';

import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:sqlite3/sqlite3.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/durable_fs.dart';
import 'package:vocara/data/db/safe_migrator.dart';

void main() {
  late Directory tmp;
  late String dbPath;
  final pid = 'P'.padRight(26, '0');
  final node = 'N'.padRight(26, '0');

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_mig');
    dbPath = p.join(tmp.path, 'vocara.db');
  });
  tearDown(() async {
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  /// Creates a v1 database file with one practice row, then closes it.
  Future<void> seedV1() async {
    final db = AppDatabase(NativeDatabase(File(dbPath)));
    await db
        .into(db.practices)
        .insert(
          PracticesCompanion.insert(
            id: pid,
            title: const Value('keepme'),
            createdAt: 1,
            updatedAt: 1,
            hlcWall: 1,
            hlcCtr: 0,
            hlcNode: node,
            hlcPacked: 'x',
          ),
        );
    await db.close();
  }

  int userVersion() {
    final raw = sqlite3.open(dbPath);
    try {
      return raw.select('PRAGMA user_version').first.values.first as int;
    } finally {
      raw.close();
    }
  }

  test('migrates on a copy, swaps in, keeps data + bumps version', () async {
    await seedV1();
    await SafeMigrator().migrate(
      dbPath: dbPath,
      targetVersion: 2,
      step: (copy) =>
          copy.execute('ALTER TABLE practices ADD COLUMN test_col TEXT'),
    );

    expect(userVersion(), 2);
    final raw = sqlite3.open(dbPath);
    final cols = raw
        .select('PRAGMA table_info(practices)')
        .map((r) => r['name'])
        .toList();
    expect(cols, contains('test_col'));
    final row = raw.select('SELECT title FROM practices WHERE id = ?', [
      pid,
    ]).first;
    expect(row['title'], 'keepme');
    raw.close();

    // The pre-migration db is retained as a rollback copy.
    expect(
      await File(p.join(tmp.path, 'vocara.pre-migrate.bak')).exists(),
      isTrue,
    );
  });

  test('a throwing migration leaves the original untouched', () async {
    await seedV1();
    await expectLater(
      SafeMigrator().migrate(
        dbPath: dbPath,
        targetVersion: 2,
        step: (_) => throw StateError('boom'),
      ),
      throwsA(isA<MigrationException>()),
    );

    expect(userVersion(), 1); // never advanced
    final raw = sqlite3.open(dbPath);
    expect(raw.select('SELECT COUNT(*) AS c FROM practices').first['c'], 1);
    raw.close();
    // The disposable copy is gone.
    expect(
      await File(p.join(tmp.path, 'vocara.migrating.db')).exists(),
      isFalse,
    );
  });

  test('app validation can reject a structurally-valid migration', () async {
    await seedV1();
    await expectLater(
      SafeMigrator().migrate(
        dbPath: dbPath,
        targetVersion: 2,
        step: (copy) => copy.execute('ALTER TABLE practices ADD COLUMN x TEXT'),
        validate: (_) => 'app says no',
      ),
      throwsA(isA<MigrationException>()),
    );
    expect(userVersion(), 1); // untouched
  });

  test('recovery finishes a swap interrupted after the .bak rename', () async {
    await seedV1();
    await SafeMigrator().migrate(
      dbPath: dbPath,
      targetVersion: 2,
      step: (copy) => copy.execute('ALTER TABLE practices ADD COLUMN c TEXT'),
    );
    // Re-stage "crashed mid-swap": the migrated db sits at the copy path and
    // the live path is empty (live was renamed to .bak, copy->live never ran).
    await File(dbPath).rename(p.join(tmp.path, _copy));
    await File(
      p.join(tmp.path, 'migration.json'),
    ).writeAsString('{"state":"swapping","to":2}');

    await SafeMigrator().recoverIfNeeded(dbPath);

    expect(await File(dbPath).exists(), isTrue);
    expect(userVersion(), 2); // the staged copy became live
    expect(await File(p.join(tmp.path, 'migration.json')).exists(), isFalse);
  });

  test('recovery drops a stale copy from a crash before the swap', () async {
    await seedV1(); // live db at v1, untouched
    await File(p.join(tmp.path, _copy)).writeAsString('junk');
    await File(
      p.join(tmp.path, 'migration.json'),
    ).writeAsString('{"state":"migrating","to":2}');

    await SafeMigrator().recoverIfNeeded(dbPath);

    expect(userVersion(), 1); // original intact
    expect(await File(p.join(tmp.path, _copy)).exists(), isFalse);
    expect(await File(p.join(tmp.path, 'migration.json')).exists(), isFalse);
  });

  test('recovery rolls back to the .bak when both live and copy were lost '
      'mid-swap (never comes up with no database)', () async {
    await seedV1();
    // Stage a crashed swap where ONLY the pre-migration .bak survived: the live
    // and migrating-copy directory entries were both lost (an FS reordering
    // window with no durable directory fsync).
    await File(dbPath).rename(p.join(tmp.path, 'vocara.pre-migrate.bak'));
    await File(
      p.join(tmp.path, 'migration.json'),
    ).writeAsString('{"state":"swapping","to":2}');

    await SafeMigrator().recoverIfNeeded(dbPath);

    // The original data is restored as the live db (the migration re-runs next
    // launch) rather than the user silently losing everything.
    expect(await File(dbPath).exists(), isTrue);
    expect(userVersion(), 1); // pre-migration original
    final raw = sqlite3.open(dbPath);
    expect(raw.select('SELECT COUNT(*) AS c FROM practices').first['c'], 1);
    raw.close();
    expect(await File(p.join(tmp.path, 'migration.json')).exists(), isFalse);
  });

  test('recovery is a no-op when there is no marker', () async {
    await seedV1();
    await SafeMigrator().recoverIfNeeded(dbPath);
    expect(userVersion(), 1);
  });

  test('copy bytes + swapping marker dirent are durable BEFORE the swap '
      'renames', () async {
    await seedV1();
    final fs = _RecordingFs(tmp.path);
    await SafeMigrator(fs: fs).migrate(
      dbPath: dbPath,
      targetVersion: 2,
      step: (copy) => copy.execute('ALTER TABLE practices ADD COLUMN c TEXT'),
    );

    // The migrated copy's bytes were flushed while the live db still existed,
    // i.e. before any destructive rename ran.
    final copySync = fs.log.indexOf('file:$_copy(liveExists=true)');
    expect(
      copySync,
      greaterThanOrEqualTo(0),
      reason: 'copy must be fsynced before the swap: ${fs.log}',
    );

    // The 'swapping' marker was written AFTER the copy flush and its directory
    // entry was fsync'd while no .bak existed yet — i.e. the marker is durable
    // before the first rename creates the .bak.
    final swapMarker = fs.log.indexOf('marker:swapping');
    expect(swapMarker, greaterThan(copySync));
    // Any dir fsync logged with NO .bak yet necessarily ran before the first
    // rename (live->bak); one must follow the swapping marker write.
    final preSwapDirSync = fs.log.indexOf('dir(bakExists=false)', swapMarker);
    expect(
      preSwapDirSync,
      greaterThan(swapMarker),
      reason:
          'the swapping marker dirent must be flushed before the '
          'renames: ${fs.log}',
    );

    // The post-rename directory fsync (bak now exists) still runs afterwards.
    expect(
      fs.log.indexOf('dir(bakExists=true)', preSwapDirSync),
      greaterThan(preSwapDirSync),
    );
  });

  test('recovery flushes the copy bytes before promoting it to live', () async {
    await seedV1();
    await SafeMigrator().migrate(
      dbPath: dbPath,
      targetVersion: 2,
      step: (copy) => copy.execute('ALTER TABLE practices ADD COLUMN c TEXT'),
    );
    // Stage "crashed mid-swap": migrated db at the copy path, live path empty.
    await File(dbPath).rename(p.join(tmp.path, _copy));
    await File(
      p.join(tmp.path, 'migration.json'),
    ).writeAsString('{"state":"swapping","to":2}');

    final fs = _RecordingFs(tmp.path);
    await SafeMigrator(fs: fs).recoverIfNeeded(dbPath);

    // The copy was fsynced while the live path was still empty, i.e. before
    // the copy->live promotion rename.
    expect(fs.log, contains('file:$_copy(liveExists=false)'));
    expect(userVersion(), 2); // and the promotion itself still happened
  });
}

const _copy = 'vocara.migrating.db';

/// Records every durability call, tagged with enough directory state to assert
/// ORDER relative to the swap renames: whether the live db / the .bak exist at
/// the moment of the call distinguishes "before the swap" from "after".
class _RecordingFs extends DurableFs {
  _RecordingFs(this.dir);

  final String dir;
  final List<String> log = [];

  @override
  Future<void> fsyncFile(String path) async {
    final name = p.basename(path);
    if (name == 'migration.json') {
      final state =
          RegExp(
            r'"state"\s*:\s*"(\w+)"',
          ).firstMatch(await File(path).readAsString())?.group(1) ??
          '?';
      log.add('marker:$state');
    } else {
      final live = File(p.join(dir, 'vocara.db')).existsSync();
      log.add('file:$name(liveExists=$live)');
    }
    await super.fsyncFile(path);
  }

  @override
  Future<void> fsyncDir(String path) async {
    final bak = File(p.join(dir, 'vocara.pre-migrate.bak')).existsSync();
    log.add('dir(bakExists=$bak)');
    await super.fsyncDir(path);
  }
}
