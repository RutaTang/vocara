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

  /// Creates a GENUINE v1 database file with one practice row, then closes it.
  ///
  /// AppDatabase now creates schema v2 directly, so the v2 additions (the
  /// local-only sync tables) are stripped and user_version is reset to 1 —
  /// reconstructing exactly the on-disk layout the registered v1->v2 step
  /// upgrades from (v2 is purely additive over v1).
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
    final raw = sqlite3.open(dbPath);
    try {
      raw.execute('DROP TABLE sync_state');
      raw.execute('DROP TABLE sync_quarantine');
      raw.execute('DROP TABLE asset_sync');
      raw.execute('PRAGMA user_version = 1');
    } finally {
      raw.close();
    }
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

  group('registered v1 -> v2 step', () {
    /// PRAGMA table_info projected to the schema-relevant tuple per column.
    List<Map<String, Object?>> tableInfo(String path, String table) {
      final raw = sqlite3.open(path);
      try {
        return raw
            .select('PRAGMA table_info("$table")')
            .map(
              (r) => <String, Object?>{
                'name': r['name'],
                'type': r['type'],
                'notnull': r['notnull'],
                'dflt_value': r['dflt_value'],
                'pk': r['pk'],
              },
            )
            .toList();
      } finally {
        raw.close();
      }
    }

    test('upgradeIfNeeded brings a v1 db to v2: data preserved, sync tables '
        'match the drift-created schema, drift opens the result', () async {
      await seedV1();
      await SafeMigrator().upgradeIfNeeded(
        dbPath: dbPath,
        targetVersion: AppDatabase.currentSchemaVersion,
      );
      expect(userVersion(), AppDatabase.currentSchemaVersion);

      // Existing data passed through untouched (additive-only migration).
      final raw = sqlite3.open(dbPath);
      final row = raw.select('SELECT title FROM practices WHERE id = ?', [
        pid,
      ]).first;
      expect(row['title'], 'keepme');
      raw.close();

      // The migrated sync tables are column-for-column identical to what a
      // FRESH drift-created v2 database gets — the step's raw SQL can never
      // drift (pun intended) from the schema in tables.dart unnoticed.
      final freshPath = p.join(tmp.path, 'fresh.db');
      final fresh = AppDatabase(NativeDatabase(File(freshPath)));
      await fresh.customSelect('SELECT 1').get(); // force open/create
      await fresh.close();
      for (final table in ['sync_state', 'sync_quarantine', 'asset_sync']) {
        expect(
          tableInfo(dbPath, table),
          tableInfo(freshPath, table),
          reason: '$table schema must match the drift-created one',
        );
      }

      // Drift opens the migrated file at the current version (its onUpgrade —
      // which throws by design — must NOT fire) and the new tables work.
      final db = AppDatabase(NativeDatabase(File(dbPath)));
      final kept = await db.select(db.practices).get();
      expect(kept.single.title, 'keepme');
      await db
          .into(db.syncState)
          .insert(SyncStateCompanion.insert(key: 'k', value: 'v'));
      expect((await db.select(db.syncState).get()).single.value, 'v');
      await db.close();
    });

    test('upgradeIfNeeded is a no-op on an already-current database', () async {
      await seedV1();
      await SafeMigrator().upgradeIfNeeded(
        dbPath: dbPath,
        targetVersion: AppDatabase.currentSchemaVersion,
      );
      // Remove the rollback .bak so a second run would recreate it IF it
      // (wrongly) migrated again.
      await File(p.join(tmp.path, 'vocara.pre-migrate.bak')).delete();

      await SafeMigrator().upgradeIfNeeded(
        dbPath: dbPath,
        targetVersion: AppDatabase.currentSchemaVersion,
      );

      expect(userVersion(), AppDatabase.currentSchemaVersion);
      expect(
        await File(p.join(tmp.path, 'vocara.pre-migrate.bak')).exists(),
        isFalse, // no second migration ran
      );
    });

    test('upgradeIfNeeded is a no-op when no database file exists yet '
        '(fresh install: drift creates the current schema itself)', () async {
      await SafeMigrator().upgradeIfNeeded(
        dbPath: dbPath,
        targetVersion: AppDatabase.currentSchemaVersion,
      );
      expect(await File(dbPath).exists(), isFalse); // probing created nothing
    });

    test('upgradeIfNeeded refuses a version gap with no registered step '
        '(never bumps user_version without the schema change)', () async {
      await seedV1();
      await expectLater(
        SafeMigrator().upgradeIfNeeded(
          dbPath: dbPath,
          // One past the current version: step 1 exists, step 2 does not.
          targetVersion: AppDatabase.currentSchemaVersion + 1,
        ),
        throwsA(isA<MigrationException>()),
      );
      // The v1->v2 step it DID have ran safely; the missing step ran nothing.
      expect(userVersion(), AppDatabase.currentSchemaVersion);
    });
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
