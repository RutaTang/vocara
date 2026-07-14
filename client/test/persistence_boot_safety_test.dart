// Boot safety of Persistence.open: the destructive-sweep gate, the refusal of
// libraries written by a newer app, and the manual repairLibrary pass.
//
// The gate is a door-closer for unrecoverable data loss: sweepStaging deletes
// the staging tree outright, and a staged WAV can be the ONLY copy of a take
// (hard-kill mid-record). If boot recovery cannot run — an unreadable or
// partially corrupt database — every destructive sweep must be skipped, or
// the sweep destroys exactly the file recovery existed to save.

import 'dart:async';
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
      // An asset nothing references, for the sweep half of the pass — aged
      // past the sweep's 24h grace window (a young unreferenced row is
      // deliberately treated as in-flight, not garbage).
      final src = File(p.join(persistence.assets.root.path, 'src.wav'));
      await src.writeAsBytes(monoWav(100), flush: true);
      await persistence.assets.importFile(src.path);
      await persistence.db.customStatement(
        'UPDATE assets SET created_at = 1, updated_at = 1',
      );

      final report = await persistence.repairLibrary();
      expect(report.violationsFound, greaterThan(0));
      expect(report.violationsRepaired, report.violationsFound);
      expect(report.assetsSwept, greaterThanOrEqualTo(1));

      // Atomicity floor: every repair journals in the SAME transaction as
      // its row mutation, so each repaired (tombstoned) interpretation has a
      // matching oplog entry.
      final interpRows = await persistence.db
          .customSelect(
            'SELECT id FROM interpretations WHERE deleted_at IS NOT NULL',
          )
          .get();
      expect(interpRows, isNotEmpty);
      final ops = await persistence.db
          .customSelect(
            'SELECT entity_id FROM oplog WHERE entity_table = '
            "'interpretations' AND op = 'tombstone'",
          )
          .get();
      expect(
        ops.map((r) => r.data['entity_id']).toSet(),
        containsAll(interpRows.map((r) => r.data['id'])),
      );

      // A second pass finds a clean library.
      final again = await persistence.repairLibrary();
      expect(again.violationsFound, 0);
      expect(again.violationsRepaired, 0);
    },
  );

  test('a systemic analyzer failure exempts staged takes from this boot\'s '
      'sweep, so the next boot retries and recovers them', () async {
    final practiceId = await createPracticeAndClose();
    final staged = await stageOrphan(practiceId, monoWav(4800));

    // Every recovery attempt throws (broken decoder / disk): before the
    // exemption, the sweep then trashed ALL orphans in the same boot and
    // recovery never retried (staging empty next boot).
    Future<WaveformResult> failing(String path) async =>
        throw StateError('decoder down');
    final sick = await Persistence.open(dir: root, analyze: failing);
    await sick.db.close();
    expect(await staged.exists(), isTrue); // spared for a retry

    // Next boot, decoder healthy: the take is recovered normally.
    final healthy = await boot();
    addTearDown(healthy.db.close);
    final res = await healthy.songRepo.getPractice(practiceId);
    expect((res as Ok<SongPractice>).value.vocalTracks, hasLength(1));
    expect(await staged.exists(), isFalse);
  });

  test('repairLibrary started during an in-flight save waits for it and '
      'cannot tombstone the asset the save just imported', () async {
    final persistence = Persistence.memory();
    addTearDown(() async {
      await persistence.db.close();
      final dir = persistence.assets.root;
      if (await dir.exists()) await dir.delete(recursive: true);
    });
    final src = File(p.join(persistence.assets.root.path, 'take.wav'));
    await src.writeAsBytes(monoWav(200), flush: true);

    Future<bool> assetLive(String id) async {
      final rows = await persistence.db
          .customSelect("SELECT deleted_at FROM assets WHERE id = '$id'")
          .get();
      return rows.single.data['deleted_at'] == null;
    }

    // The repository save shape under the gate's shared side: the asset
    // import lands first, the rows referencing it commit later — hold the
    // gate open across that window.
    final events = <String>[];
    final holdSave = Completer<void>();
    String? assetId;
    final save = persistence.writeGate.shared(() async {
      assetId = await persistence.assets.importFile(src.path);
      events.add('imported');
      await holdSave.future;
      events.add('save-committed');
    });
    while (assetId == null) {
      await Future<void>.delayed(const Duration(milliseconds: 1));
    }

    final repair = persistence.repairLibrary().whenComplete(
      () => events.add('repair-done'),
    );
    await Future<void>.delayed(const Duration(milliseconds: 25));
    // The exclusive pass must still be queued behind the in-flight save...
    expect(events, ['imported']);
    // ...so the freshly imported, not-yet-referenced asset is untouched.
    expect(await assetLive(assetId!), isTrue);

    holdSave.complete();
    await save;
    await repair;
    expect(events, ['imported', 'save-committed', 'repair-done']);
    // Once the repair does run, the young unreferenced row is additionally
    // protected by the sweep's age grace window.
    expect(await assetLive(assetId!), isTrue);
  });

  test('device node id survives restarts', () async {
    final p1 = await boot();
    await p1.db.close();
    final nodeFile = File(p.join(root.path, 'device_node'));
    final first = (await nodeFile.readAsString()).trim();
    expect(first, hasLength(26));

    final p2 = await boot();
    await p2.db.close();
    expect((await nodeFile.readAsString()).trim(), first);
  });

  test('an empty or corrupt device_node is treated as absent: a fresh id is '
      'minted instead of silently adopting a truncated identity', () async {
    // A power cut mid-write under the old non-durable writeAsString could
    // leave exactly these shapes on disk.
    final nodeFile = File(p.join(root.path, 'device_node'));
    await nodeFile.writeAsString('');
    final p1 = await boot();
    await p1.db.close();
    final minted = (await nodeFile.readAsString()).trim();
    expect(minted, hasLength(26));

    // A truncated id is corrupt, not a shorter identity.
    await nodeFile.writeAsString('01ABC');
    final p2 = await boot();
    await p2.db.close();
    final reminted = (await nodeFile.readAsString()).trim();
    expect(reminted, hasLength(26));
    expect(reminted, isNot('01ABC'));

    // A valid id keeps being preserved byte-for-byte.
    final p3 = await boot();
    await p3.db.close();
    expect((await nodeFile.readAsString()).trim(), reminted);
  });
}
