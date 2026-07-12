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

/// The registered schema upgrade steps, keyed by the version each upgrades
/// FROM (step N transforms a vN copy into a v(N+1) schema). Steps are raw SQL
/// on the migration COPY and must produce EXACTLY the schema drift creates for
/// the target version (safe_migrator_test compares column-for-column against a
/// fresh drift-created database).
final Map<int, MigrationStep> registeredMigrationSteps = {1: _migrateV1ToV2};

/// v1 -> v2: adds the three LOCAL-ONLY sync bookkeeping tables. Additive only —
/// no existing table is touched, so user data passes through unchanged.
///
/// These tables deliberately carry NO SyncCols: they are per-device engine
/// state, never journaled to the oplog and excluded from every synced-table
/// enumeration (the sync_quarantine stamp column is named remote_hlc_packed,
/// not hlc_packed, so Persistence._seedClock's hlc_packed scan and the
/// syncedTableRegistry structural scan both skip it — see tables.dart).
void _migrateV1ToV2(Database copy) {
  copy.execute(
    'CREATE TABLE IF NOT EXISTS "sync_state" ('
    '"key" TEXT NOT NULL, '
    '"value" TEXT NOT NULL, '
    'PRIMARY KEY ("key"))',
  );
  copy.execute(
    'CREATE TABLE IF NOT EXISTS "sync_quarantine" ('
    '"tbl" TEXT NOT NULL, '
    '"id" TEXT NOT NULL, '
    '"remote_hlc_packed" TEXT NOT NULL, '
    '"schema_v" INTEGER NOT NULL, '
    '"payload" TEXT NOT NULL, '
    '"reason" TEXT NOT NULL, '
    '"quarantined_at" INTEGER NOT NULL, '
    'PRIMARY KEY ("tbl", "id"))',
  );
  copy.execute(
    'CREATE TABLE IF NOT EXISTS "asset_sync" ('
    '"hash" TEXT NOT NULL, '
    '"uploaded_at" INTEGER NOT NULL, '
    'PRIMARY KEY ("hash"))',
  );
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
    // SQLite's own syncs do not flush the drive's write cache (no F_FULLFSYNC
    // by default): force the migrated copy's BYTES to permanent storage before
    // it is renamed over the live db, or a power cut right after the swap
    // could leave a truncated/corrupt file as the only database.
    await fs.fsyncFile(copyPath);

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

  /// Brings the on-disk database at [dbPath] up to [targetVersion] by running
  /// the [registeredMigrationSteps] ONE version at a time, each through the
  /// full copy/migrate/validate/swap protocol (so a crash between two steps
  /// leaves a consistent, recoverable intermediate version).
  ///
  /// Deliberate no-ops, chosen as the safest behavior:
  ///  * the file does not exist — a fresh install; drift's onCreate builds the
  ///    current schema directly (opening it here would CREATE an empty file);
  ///  * `user_version` is 0 — the file exists but drift never created a schema
  ///    in it (an empty just-created file); drift's onCreate handles it;
  ///  * `user_version` >= [targetVersion] — already current (a NEWER on-disk
  ///    version is left untouched too: never modify data from a newer app;
  ///    drift's own version check will refuse to open it).
  ///
  /// Call AFTER [recoverIfNeeded] and BEFORE drift opens the file.
  Future<void> upgradeIfNeeded({
    required String dbPath,
    required int targetVersion,
    Map<int, MigrationStep>? steps,
  }) async {
    if (!await File(dbPath).exists()) return;
    final registry = steps ?? registeredMigrationSteps;
    var current = readUserVersion(dbPath);
    if (current == 0) return;
    while (current < targetVersion) {
      final step = registry[current];
      if (step == null) {
        throw MigrationException(
          'no registered migration step from v$current (target v$targetVersion)',
        );
      }
      await migrate(dbPath: dbPath, targetVersion: current + 1, step: step);
      final next = readUserVersion(dbPath);
      if (next <= current) {
        // Defensive: a step that fails to advance must not spin forever.
        throw MigrationException(
          'migration step from v$current left user_version at $next',
        );
      }
      current = next;
    }
  }

  /// Reads the schema `user_version` of the SQLite file at [dbPath] without
  /// ever creating or modifying it (read-only open). Throws if the file is
  /// not a readable SQLite database.
  ///
  /// Public because Persistence.open uses it to REFUSE a library written by a
  /// newer app (user_version above the code's schema) before building the
  /// store, and the diagnostics exporter includes it in bug-report bundles.
  int readUserVersion(String dbPath) {
    final raw = sqlite3.open(dbPath, mode: OpenMode.readOnly);
    try {
      return raw.select('PRAGMA user_version').first.values.first as int;
    } finally {
      raw.close();
    }
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
          // Flush its bytes first — the crash may have hit before migrate()'s
          // own fsync, and promoting an unflushed copy reopens the window
          // where power loss leaves a truncated live db.
          await fs.fsyncFile(copyPath);
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
      throw MigrationException(
        'integrity_check: ${integ.map((r) => r.values).toList()}',
      );
    }
    // foreign_key_check is a standalone checker, independent of the FK pragma.
    final fk = copy.select('PRAGMA foreign_key_check');
    if (fk.isNotEmpty) {
      throw MigrationException(
        'foreign_key_check found ${fk.length} violations',
      );
    }
    final uv = copy.select('PRAGMA user_version').first.values.first as int;
    if (uv != target) {
      throw MigrationException('user_version is $uv, expected $target');
    }
  }

  Future<void> _writeMarker(File marker, String state, int target) async {
    await marker.writeAsString('{"state":"$state","to":$target}');
    await fs.fsyncFile(marker.path);
    // A newly CREATED marker is only durable once its directory entry is also
    // flushed (fsyncFile covers the bytes, not the dirent). Without this, a
    // crash mid-swap could land the renames on disk with NO marker, so
    // recoverIfNeeded would see "nothing to do" while the only database sits
    // half-swapped (e.g. stranded in .bak).
    await fs.fsyncDir(marker.parent.path);
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
