// The pre-migration rollback copy (vocara.pre-migrate.bak) is the way back
// while a freshly migrated database is unproven, and a whole extra copy of
// the library once it is proven. It goes on the first boot whose readability
// probe and staged-take recovery succeed, and stays on any boot that fails
// either.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:sqlite3/sqlite3.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/db/safe_migrator.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late Directory root;

  setUp(() async {
    root = await Directory.systemTemp.createTemp('vocara_migration_backup');
  });
  tearDown(() async {
    if (await root.exists()) await root.delete(recursive: true);
  });

  String dbPath() => p.join(root.path, 'vocara.db');
  File backup() => File(p.join(root.path, 'vocara.pre-migrate.bak'));

  Future<WaveformResult> fakeAnalyze(String path) async =>
      (peaks: <double>[0.1, 0.2], durationMs: 500);

  Future<Persistence> boot() =>
      Persistence.open(dir: root, analyze: fakeAnalyze);

  /// A library one schema version behind, with one practice in it: built at
  /// the current version, then stripped back to v1 (v2 only added the three
  /// local-only sync tables).
  Future<String> seedV1Library() async {
    final persistence = await boot();
    final res = await persistence.songRepo.createPractice('Kept');
    await persistence.db.close();
    final raw = sqlite3.open(dbPath());
    try {
      raw.execute('DROP TABLE sync_state');
      raw.execute('DROP TABLE sync_quarantine');
      raw.execute('DROP TABLE asset_sync');
      raw.execute('PRAGMA user_version = 1');
    } finally {
      raw.close();
    }
    return (res as Ok<SongPractice>).value.id;
  }

  test('a boot that migrated the library and proved it readable discards '
      'the pre-migration backup', () async {
    final practiceId = await seedV1Library();

    final persistence = await boot();
    addTearDown(persistence.db.close);

    // The migration ran and the data came through.
    final check = sqlite3.open(dbPath(), mode: OpenMode.readOnly);
    expect(
      check.select('PRAGMA user_version').first.values.first,
      AppDatabase.currentSchemaVersion,
    );
    check.close();
    expect(
      await persistence.songRepo.getPractice(practiceId),
      isA<Ok<SongPractice>>(),
    );
    expect(
      await backup().exists(),
      isFalse,
      reason: 'a whole second copy of the library was left behind for good',
    );
  });

  // The probe and staged-take recovery open one gate together (a failure of
  // either closes it), so a failed probe stands for both here.
  test('a boot whose readability probe fails keeps the backup', () async {
    await seedV1Library();
    // Corrupt a table the probe reads; the migration itself still succeeds.
    final raw = sqlite3.open(dbPath());
    raw.execute('DROP TABLE clips');
    raw.close();

    final persistence = await boot();
    addTearDown(persistence.db.close);

    expect(await backup().exists(), isTrue);
  });

  test(
    'the backup is never discarded while a migration marker is pending',
    () async {
      await backup().writeAsString('rollback copy');
      final marker = File(p.join(root.path, 'migration.json'));
      await marker.writeAsString('{"state":"swapping","to":2}');

      expect(await SafeMigrator().discardBackup(dbPath()), isFalse);
      expect(await backup().exists(), isTrue);

      // A migration that finished (this boot's own) does not hold it back.
      await marker.writeAsString('{"state":"done","to":2}');
      expect(await SafeMigrator().discardBackup(dbPath()), isTrue);
      expect(await backup().exists(), isFalse);
    },
  );
}
