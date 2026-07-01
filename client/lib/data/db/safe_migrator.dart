import 'dart:io';

import 'package:path/path.dart' as p;
import 'package:sqlite3/sqlite3.dart';

import 'durable_fs.dart';

/// A migration step run on the COPY database (raw SQL). It must transform the
/// schema to the target version; the migrator sets `user_version` and validates
/// afterwards.
typedef MigrationStep = void Function(Database copy);

/// Extra app-level validation run on the migrated copy (e.g. the
/// IntegrityChecker's invariants). Return a non-empty message to FAIL.
typedef CopyValidator = String? Function(Database copy);

class MigrationException implements Exception {
  MigrationException(this.message, [this.cause]);
  final String message;
  final Object? cause;
  @override
  String toString() =>
      'MigrationException: $message${cause == null ? '' : ' ($cause)'}';
}

/// Upgrades a SQLite database SAFELY: it never edits the live file in place.
/// Instead it makes a transactionally-consistent COPY (`VACUUM INTO`), runs the
/// migration on the copy, validates it, and only then atomically swaps the copy
/// in. On ANY failure the original file is left byte-for-byte untouched and a
/// [MigrationException] is thrown, so a bad migration can never corrupt data.
///
/// A fsync'd `migration.json` marker records the state so a crash mid-swap can
/// be recovered to a consistent file on the next launch ([recoverIfNeeded]).
///
/// The caller MUST ensure no other connection holds the live file open (the app
/// quiesces its database isolate first); the migrator opens the file itself.
class SafeMigrator {
  SafeMigrator({this.fs = const DurableFs()});

  final DurableFs fs;

  static const _copyName = 'vocara.migrating.db';
  static const _bakName = 'vocara.pre-migrate.bak';
  static const _markerName = 'migration.json';

  /// Migrates the database at [dbPath] to [targetVersion]. Throws on any
  /// failure with the original untouched.
  Future<void> migrate({
    required String dbPath,
    required int targetVersion,
    required MigrationStep step,
    CopyValidator? validate,
  }) async {
    final dir = p.dirname(dbPath);
    final copyPath = p.join(dir, _copyName);
    final bakPath = p.join(dir, _bakName);
    final marker = File(p.join(dir, _markerName));

    // A stale copy from a crashed prior attempt is disposable.
    await _deleteQuietly(copyPath);
    await _writeMarker(marker, 'copying', targetVersion);

    // 1. Checkpoint the live WAL, then VACUUM INTO a clean, consistent copy
    //    (one file, no -wal/-shm sidecars to coordinate).
    final src = sqlite3.open(dbPath);
    try {
      src.execute('PRAGMA wal_checkpoint(TRUNCATE)');
      src.execute("VACUUM INTO '${_sqlLit(copyPath)}'");
    } finally {
      src.close();
    }

    // 2. Run the migration on the COPY only, set user_version, validate.
    await _writeMarker(marker, 'migrating', targetVersion);
    final copy = sqlite3.open(copyPath);
    try {
      // Schema rebuilds (12-step table recreate) need FKs off mid-migration.
      copy.execute('PRAGMA foreign_keys = OFF');
      step(copy);
      copy.execute('PRAGMA user_version = $targetVersion');
      _validateCore(copy, targetVersion);
      final extra = validate?.call(copy);
      if (extra != null) {
        throw MigrationException('app validation failed: $extra');
      }
      copy.execute('PRAGMA wal_checkpoint(TRUNCATE)');
    } catch (e) {
      copy.close();
      await _deleteQuietly(copyPath);
      await _writeMarker(marker, 'failed', targetVersion);
      if (e is MigrationException) rethrow;
      throw MigrationException('migration failed', e);
    }
    copy.close();

    // 3. Atomic swap (marker first, then sidecar cleanup, then renames).
    await _writeMarker(marker, 'swapping', targetVersion);
    for (final s in const ['-wal', '-shm', '-journal']) {
      await _deleteQuietly('$dbPath$s');
      await _deleteQuietly('$copyPath$s');
    }
    await _deleteQuietly(bakPath);
    await File(dbPath).rename(bakPath); // keep the pre-migration db as rollback
    await File(copyPath).rename(dbPath);
    await fs.fsyncDir(dir);
    await _writeMarker(marker, 'done', targetVersion);
  }

  /// Called at startup BEFORE opening the live db. If a crash left a half-done
  /// swap, finish or roll it back so the live file is always consistent.
  Future<void> recoverIfNeeded(String dbPath) async {
    final dir = p.dirname(dbPath);
    final marker = File(p.join(dir, _markerName));
    if (!await marker.exists()) return;
    final state = _markerState(await marker.readAsString());
    final copyPath = p.join(dir, _copyName);
    final bakPath = p.join(dir, _bakName);

    switch (state) {
      case 'swapping':
        // The two-rename swap (live->bak, copy->live) may have been interrupted
        // at ANY point, and with no durable directory fsync the filesystem may
        // have reordered the entry updates. Recover deterministically from
        // whichever file survived, most-progressed first, so we NEVER come up
        // with no database (the old bug fell through to "nothing to do" when
        // both live and copy entries were lost, abandoning the only data in
        // .bak — drift then created a fresh empty db).
        if (await File(dbPath).exists()) {
          // copy->live completed (or live was never moved): live db is current.
        } else if (await File(copyPath).exists()) {
          // live->bak done but copy->live not: promote the migrated copy.
          await File(copyPath).rename(dbPath);
        } else if (await File(bakPath).exists()) {
          // Both the live and copy directory entries were lost mid-swap: roll
          // back to the pre-migration original so data is never lost. The
          // migration simply re-runs next launch. (Idempotent across repeated
          // crashes: a later boot re-enters this branch until dbPath exists.)
          await File(bakPath).rename(dbPath);
        }
        // else: no db, copy, or bak survived — unrecoverable FS corruption.
        // Durably land the rename (if any) BEFORE dropping the marker, matching
        // the forward-swap ordering (fsyncDir then terminal marker); otherwise a
        // double-fault could delete the marker while the rename is still only in
        // the FS cache, abandoning the db in .bak with no marker to recover from.
        await fs.fsyncDir(dir);
        await marker.delete();
        await fs.fsyncDir(dir);
      case 'copying':
      case 'migrating':
      case 'failed':
        // The live db was never touched; drop the disposable copy.
        await _deleteQuietly(copyPath);
        await marker.delete();
      case 'done':
        await marker.delete();
      default:
        await marker.delete();
    }
    // The .bak is retained intentionally (a generous rollback window); a janitor
    // prunes it after the new db proves healthy.
    if (await File(bakPath).exists()) {
      // left in place on purpose
    }
  }

  void _validateCore(Database copy, int target) {
    final integ = copy.select('PRAGMA integrity_check');
    if (integ.isEmpty || integ.first.values.first != 'ok') {
      throw MigrationException('integrity_check: ${integ.map((r) => r.values).toList()}');
    }
    // foreign_key_check is a standalone checker, independent of the FK pragma.
    final fk = copy.select('PRAGMA foreign_key_check');
    if (fk.isNotEmpty) {
      throw MigrationException('foreign_key_check found ${fk.length} violations');
    }
    final uv = copy.select('PRAGMA user_version').first.values.first as int;
    if (uv != target) {
      throw MigrationException('user_version is $uv, expected $target');
    }
  }

  Future<void> _writeMarker(File marker, String state, int target) async {
    await marker.writeAsString('{"state":"$state","to":$target}');
    await fs.fsyncFile(marker.path);
  }

  String _markerState(String json) {
    final m = RegExp(r'"state"\s*:\s*"(\w+)"').firstMatch(json);
    return m?.group(1) ?? 'unknown';
  }

  String _sqlLit(String s) => s.replaceAll("'", "''");

  Future<void> _deleteQuietly(String path) async {
    final f = File(path);
    if (await f.exists()) await f.delete();
  }
}
