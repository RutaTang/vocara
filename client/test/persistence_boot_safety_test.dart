// Boot safety of Persistence.open: the destructive-sweep gate, the refusal of
// libraries written by a newer app, and the manual repairLibrary pass.
//
// The gate is a door-closer for unrecoverable data loss: sweepStaging deletes
// the staging tree outright, and a staged WAV can be the ONLY copy of a take
// (hard-kill mid-record). If boot recovery cannot run — an unreadable or
// partially corrupt database — every destructive sweep must be skipped, or
// the sweep destroys exactly the file recovery existed to save.

import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:sqlite3/sqlite3.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late Directory root;

  setUp(() async {
    root = await Directory.systemTemp.createTemp('vocara_boot_safety');
  });

  tearDown(() async {
    if (await root.exists()) await root.delete(recursive: true);
  });

  /// The analyzer a real recovery would run through WaveformService; tests
  /// stand in a fixed result (the platform decoder isn't available here).
  Future<WaveformResult> fakeAnalyze(String path) async =>
      (peaks: <double>[0.1, 0.2], durationMs: 500);

  /// Mono 16-bit PCM WAV bytes — the exact shape the recording engine writes.
  Uint8List monoWav(int samples) => writeWavMono(
    Float64List.fromList([
      for (var i = 0; i < samples; i++) 0.5 * (i % 100) / 100,
    ]),
    48000,
  );

  Future<Persistence> boot() =>
      Persistence.open(dir: root, analyze: fakeAnalyze);

  String dbPath() => p.join(root.path, 'vocara.db');

  /// Boots once to create the on-disk store, adds one practice, closes.
  Future<String> createPracticeAndClose() async {
    final persistence = await boot();
    final res = await persistence.songRepo.createPractice('Song');
    final id = (res as Ok<SongPractice>).value.id;
    await persistence.db.close();
    return id;
  }

  /// Writes an orphaned recording exactly where the engine streams takes to.
  Future<File> stageOrphan(String practiceId, List<int> bytes) async {
    final f = File(p.join(root.path, 'audio', practiceId, 'take.wav'));
    await f.parent.create(recursive: true);
    await f.writeAsBytes(bytes, flush: true);
    return f;
  }

  test('a boot whose recovery cannot run skips every destructive sweep, so '
      'the staged take survives', () async {
    final practiceId = await createPracticeAndClose();
    final staged = await stageOrphan(practiceId, monoWav(4800));

    // Partial corruption: recovery's reference derivation (and the sweep
    // gate's read probe) needs the clips table; drop it so recovery fails
    // while the database file itself still opens.
    final raw = sqlite3.open(dbPath());
    raw.execute('DROP TABLE clips');
    raw.close();

    // Launch must not be blocked (the app comes up degraded)...
    final persistence = await boot();
    addTearDown(persistence.db.close);
    // ...but no sweep may run: before the gate, sweepStaging deleted the
    // staging tree here, destroying the ONLY copy of the take.
    expect(await staged.exists(), isTrue);
  });

  test(
    'a healthy boot recovers the staged take and then clears staging',
    () async {
      final practiceId = await createPracticeAndClose();
      final staged = await stageOrphan(practiceId, monoWav(4800));

      final persistence = await boot();
      addTearDown(persistence.db.close);
      final res = await persistence.songRepo.getPractice(practiceId);
      final practice = (res as Ok<SongPractice>).value;
      expect(practice.vocalTracks, hasLength(1));
      expect(practice.vocalTracks.single.displayName, 'Take 1 (recovered)');
      // Recovered into the pool, so the staging copy is gone (deleted by the
      // recovery itself or reclaimed by the sweep).
      expect(await staged.exists(), isFalse);
    },
  );

  test('a healthy boot still clears unrecoverable staging leftovers', () async {
    final practiceId = await createPracticeAndClose();
    // Not a WAV at all: never recovered, reclaimed by the staging sweep.
    final leftover = File(p.join(root.path, 'audio', practiceId, 'junk.wav'));
    await leftover.parent.create(recursive: true);
    await leftover.writeAsBytes(List<int>.generate(64, (i) => i), flush: true);

    final persistence = await boot();
    addTearDown(persistence.db.close);
    expect(await leftover.exists(), isFalse);
  });

  test('open refuses a library written by a newer Vocara and leaves it '
      'untouched', () async {
    await createPracticeAndClose();
    final newer = AppDatabase.currentSchemaVersion + 7;
    final raw = sqlite3.open(dbPath());
    raw.execute('PRAGMA user_version = $newer');
    raw.close();

    await expectLater(
      boot(),
      throwsA(
        isA<NewerLibraryException>()
            .having((e) => e.libraryVersion, 'libraryVersion', newer)
            .having(
              (e) => e.appVersion,
              'appVersion',
              AppDatabase.currentSchemaVersion,
            ),
      ),
    );
    // The refusal itself must not write: the on-disk version is unchanged
    // (a downgrade attempt would freeze users out of their own data).
    final check = sqlite3.open(dbPath(), mode: OpenMode.readOnly);
    expect(check.select('PRAGMA user_version').first.values.first, newer);
    check.close();
  });

  test(
    'repairLibrary repairs cross-row violations and reports counts',
    () async {
      final persistence = Persistence.memory();
      addTearDown(() async {
        await persistence.db.close();
        final dir = persistence.assets.root;
        if (await dir.exists()) await dir.delete(recursive: true);
      });
      final res = await persistence.songRepo.createPractice('Song');
      final practice = (res as Ok<SongPractice>).value;
      // Fabricate a violation the DB's own FK checks can't see: tombstone the
      // practice row directly, leaving its interpretation live (the repo's
      // deletePractice would cascade, so bypass it).
      await persistence.db.customStatement(
        'UPDATE practices SET deleted_at = 1 WHERE id = ?',
        [practice.id],
      );
      // An asset nothing references, for the sweep half of the pass.
      final src = File(p.join(persistence.assets.root.path, 'src.wav'));
      await src.writeAsBytes(monoWav(100), flush: true);
      await persistence.assets.importFile(src.path);

      final report = await persistence.repairLibrary();
      expect(report.violationsFound, greaterThan(0));
      expect(report.violationsRepaired, report.violationsFound);
      expect(report.assetsSwept, greaterThanOrEqualTo(1));

      // A second pass finds a clean library.
      final again = await persistence.repairLibrary();
      expect(again.violationsFound, 0);
      expect(again.violationsRepaired, 0);
    },
  );
}
