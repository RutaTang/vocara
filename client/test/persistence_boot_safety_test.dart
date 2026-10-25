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

import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:sqlite3/sqlite3.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/oplog_drain.dart';
import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/data/sync/sync_state_store.dart';
import 'package:vocara/data/sync/sync_transport.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

import 'support/fake_sync_transport.dart';

/// Holds a pull run between two pages: fetch number [pauseAfter] + 1 signals
/// [paused] and waits on [resume] before serving its page.
class _PausingTransport extends FakeSyncTransport {
  _PausingTransport(super.cloud);

  int? pauseAfter;
  final paused = Completer<void>();
  final resume = Completer<void>();

  @override
  Future<FetchPage> fetchChanges(String? sinceToken) async {
    if (pauseAfter != null && fetchCalls == pauseAfter) {
      pauseAfter = null;
      paused.complete();
      await resume.future;
    }
    return super.fetchChanges(sinceToken);
  }
}

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
      // Settle the asset's journal entry as a push ack would (the ack
      // hard-deletes): the sweep holds back any asset whose entry is still
      // pending, since a pushed-but-unacked row may already be server-side.
      await persistence.db.customStatement(
        "DELETE FROM oplog WHERE entity_table = 'assets'",
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
    final practice =
        (await persistence.songRepo.createPractice('Song') as Ok<SongPractice>)
            .value;
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
      final id = await persistence.assets.importFile(src.path);
      // Age the fresh row past the sweep's 24 h grace window. Without this
      // the age guard alone kept it live and the window checks below proved
      // nothing about the gate: now ONLY the gate stands between this
      // unreferenced row and the sweep.
      await persistence.db.customStatement(
        'UPDATE assets SET created_at = 1, updated_at = 1',
      );
      assetId = id;
      events.add('imported');
      await holdSave.future;
      // The rows referencing the import commit last (nested shared section;
      // the gate is reader-preferring, so the queued repair cannot block it).
      final pooled = (await persistence.assets.resolve(id))!;
      final saved = await persistence.songRepo.savePractice(
        practice.copyWith(
          vocalTracks: [
            AudioTrack(
              id: newId(),
              kind: AudioTrackKind.vocal,
              displayName: 'Take 1',
              audioFilePath: pooled,
              durationMs: 4,
              createdAt: DateTime(2026),
              versionId: practice.activeVersionId,
            ),
          ],
        ),
      );
      expect(saved, isA<Ok<SongPractice>>());
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
    // ...so the imported, not-yet-referenced (and aged) asset is untouched.
    expect(await assetLive(assetId!), isTrue);

    holdSave.complete();
    await save;
    await repair;
    expect(events, ['imported', 'save-committed', 'repair-done']);
    // Once the repair does run, the committed clip row references the asset.
    expect(await assetLive(assetId!), isTrue);
  });

  test('repairLibrary started between two pages of a pull run waits for the '
      'run, so it never repairs a half-delivered aggregate', () async {
    final persistence = Persistence.memory();
    final transport = _PausingTransport(FakeCloud())..pageSize = 1;
    persistence.attachSync(transport);
    addTearDown(() async {
      persistence.sync!.dispose();
      await persistence.db.close();
      final dir = persistence.assets.root;
      if (await dir.exists()) await dir.delete(recursive: true);
    });
    final peer = 'PEER'.padRight(26, '0');
    final t = DateTime.now().millisecondsSinceEpoch - 100000;
    Map<String, Object?> cols(int wall, {required bool live}) => {
      'created_at': t,
      'updated_at': wall,
      'deleted_at': live ? null : wall,
      'hlc_wall': wall,
      'hlc_ctr': 0,
      'hlc_node': peer,
      'hlc_packed': Hlc(wallMs: wall, counter: 0, node: peer).packed,
      'schema_v': 1,
    };
    final practiceId = newId();
    final interpId = newId();
    SyncRow practice(int wall, {required bool live}) => SyncRow(
      table: 'practices',
      fields: {
        'id': practiceId,
        'title': 'song',
        'timeline_duration_ms': 0,
        'master_volume': 1.0,
        'active_interpretation_id': null,
        'separation_asset_id': null,
        'reverb_json': null,
        'lyrics_json': null,
        ...cols(wall, live: live),
      },
    );
    SyncRow interp(int wall, {required bool live}) => SyncRow(
      table: 'interpretations',
      fields: {
        'id': interpId,
        'practice_id': practiceId,
        'name': 'take',
        'order_index': 0,
        ...cols(wall, live: live),
      },
    );
    Future<Object?> interpDeletedAt() async =>
        (await persistence.db
                .customSelect(
                  'SELECT deleted_at AS d FROM interpretations WHERE id = ?',
                  variables: [Variable.withString(interpId)],
                )
                .getSingle())
            .data['d'];

    // Shared history: the practice and its interpretation, then deleted.
    final cloud = transport.cloud;
    cloud.accept(practice(t + 1, live: true));
    cloud.accept(interp(t + 2, live: true));
    cloud.accept(practice(t + 3, live: false));
    cloud.accept(interp(t + 4, live: false));
    expect((await persistence.sync!.pumpPull()).failed, isFalse);

    // A peer revives both. The interpretation's revive lands on one page,
    // the practice's on the next — between them the store holds a live
    // interpretation under a tombstoned practice, which is pending, not
    // broken.
    cloud.accept(interp(t + 5, live: true));
    cloud.accept(practice(t + 6, live: true));
    final events = <String>[];
    transport.pauseAfter = transport.fetchCalls + 1;
    final pull = persistence.sync!.pumpPull().whenComplete(
      () => events.add('pull-done'),
    );
    await transport.paused.future;
    expect(await interpDeletedAt(), isNull, reason: 'page 1 landed');

    final repair = persistence.repairLibrary().whenComplete(
      () => events.add('repair-done'),
    );
    await Future<void>.delayed(const Duration(milliseconds: 25));
    expect(events, isEmpty, reason: 'the repair must wait for the run');

    transport.resume.complete();
    expect((await pull).failed, isFalse);
    await repair;
    expect(events, ['pull-done', 'repair-done']);
    expect(
      await interpDeletedAt(),
      isNull,
      reason:
          'the revived interpretation must not be tombstoned on a verdict '
          'about a half-delivered aggregate',
    );
    expect(await persistence.db.select(persistence.db.oplog).get(), isEmpty);
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

  group('oplog compaction', () {
    late Persistence persistence;

    setUp(() {
      persistence = Persistence.memory();
      addTearDown(() async {
        await persistence.db.close();
        final dir = persistence.assets.root;
        if (await dir.exists()) await dir.delete(recursive: true);
      });
    });

    /// Journals one entry through the production chokepoint (no row mutation
    /// needed — compaction is about the ledger alone).
    Future<Hlc> journal(
      String table,
      String id,
      String op, {
      String? dependsOnAssetId,
    }) async {
      final s = persistence.writer.stamp();
      await persistence.writer.journal(
        table: table,
        id: id,
        op: op,
        hlc: s.hlc,
        payloadHash: op == 'tombstone' ? '' : 'h',
        dependsOnAssetId: dependsOnAssetId,
      );
      return s.hlc;
    }

    Future<int> oplogCount() async =>
        (await persistence.db.select(persistence.db.oplog).get()).length;

    test('keeps only the latest op per entity; the drain ships exactly what '
        'its own collapse would have shipped', () async {
      final id1 = 'P1'.padRight(26, '0');
      final id2 = 'P2'.padRight(26, '0');
      await journal('practices', id1, 'insert');
      await journal('practices', id1, 'update');
      await journal('practices', id2, 'insert');
      final tomb = await journal('practices', id1, 'tombstone');
      expect(await oplogCount(), 4);

      expect(await persistence.compactOplog(), 2);
      expect(await oplogCount(), 2);

      // The drain result is IDENTICAL to an uncompacted drain: latest op per
      // entity, trailing tombstone preserved (its collapse rule is exactly
      // what compaction enforced in place).
      final batch = await OplogDrain(persistence.db).readPending();
      expect(batch.entries, hasLength(2));
      expect(batch.entries[0].id, id2);
      expect(batch.entries[0].op, 'insert');
      expect(batch.entries[1].id, id1);
      expect(batch.entries[1].op, 'tombstone');
      expect(batch.entries[1].hlcPacked, tomb.packed);
      expect(batch.maxSeq, 4, reason: 'surviving seqs keep their values');
    });

    test('carries the latest non-null dependsOnAssetId onto the surviving '
        'row (a content-only update must not shed the asset gate)', () async {
      final id = 'C1'.padRight(26, '0');
      final asset = 'AX'.padRight(26, '0');
      await journal('clips', id, 'insert', dependsOnAssetId: asset);
      await journal('clips', id, 'update'); // no dependency of its own

      expect(await persistence.compactOplog(), 1);
      final batch = await OplogDrain(persistence.db).readPending();
      expect(batch.entries.single.op, 'update');
      expect(
        batch.entries.single.dependsOnAssetId,
        asset,
        reason:
            'the gate must survive compaction exactly as it survives '
            'the drain collapse',
      );
    });

    test('is idempotent and a no-op on an already-compact ledger', () async {
      final id = 'P1'.padRight(26, '0');
      await journal('practices', id, 'insert');
      expect(await persistence.compactOplog(), 0);
      expect(await persistence.compactOplog(), 0);
      expect(await oplogCount(), 1);
    });

    test('the boot maintenance pass compacts superseded history', () async {
      // Boot #1: a real practice (several entities, one entry each) plus a
      // superseded extra op for the practice row itself.
      final first = await boot();
      final res = await first.songRepo.createPractice('Song');
      final id = (res as Ok<SongPractice>).value.id;
      final s = first.writer.stamp();
      await first.writer.journal(
        table: 'practices',
        id: id,
        op: 'update',
        hlc: s.hlc,
        payloadHash: 'h2',
      );
      final before = await first.db
          .customSelect(
            "SELECT COUNT(*) AS n FROM oplog WHERE entity_table = 'practices' "
            'AND entity_id = ?',
            variables: [Variable.withString(id)],
          )
          .getSingle();
      expect(before.read<int>('n'), 2);
      await first.db.close();

      // Boot #2 runs the gated maintenance pass, which compacts.
      final second = await boot();
      addTearDown(second.db.close);
      final after = await second.db
          .customSelect(
            'SELECT COUNT(*) AS n, MAX(op) AS op FROM oplog WHERE '
            "entity_table = 'practices' AND entity_id = ?",
            variables: [Variable.withString(id)],
          )
          .getSingle();
      expect(after.read<int>('n'), 1);
      expect(after.read<String>('op'), 'update', reason: 'the LATEST op won');
    });
  });

  group('boot clock seed', () {
    /// A stamp from a peer, ahead of this device's wall clock but well inside
    /// the plausibility window the applier's drift gate allows.
    Hlc peerStamp(int aheadMs) => Hlc(
      wallMs: DateTime.now().millisecondsSinceEpoch + aheadMs,
      counter: 0,
      node: 'REMOTE'.padRight(26, '0'),
    );

    test('a verified remote stamp whose record was PARKED still seeds the '
        'clock after a restart', () async {
      // The applier merges the stamps of records the FK sweep parks — HLC
      // semantics advance on any received message — but a parked record
      // leaves no row behind, and sync_quarantine's stamp column is
      // deliberately not named hlc_packed (it also holds suspect far-future
      // stamps, which must never seed a clock). So that legitimate high-water
      // mark used to live only in memory: after a restart a local edit could
      // take a LOWER stamp, and the quarantine replay would then win LWW and
      // silently overwrite the user's newer edit.
      final first = await boot();
      final parked = peerStamp(120000);
      await SyncStateStore(first.db).raiseHlcHighWater(parked.packed);
      // Nothing in the tables carries that stamp — the seed's other source.
      await first.db.close();

      final second = await boot();
      addTearDown(second.db.close);
      final local = second.writer.stamp().hlc;
      expect(
        local > parked,
        isTrue,
        reason:
            'a local edit after the restart must out-stamp what this '
            'device already accepted: got ${local.packed} vs ${parked.packed}',
      );
    });

    test('the durable mark only ever moves up', () async {
      final persistence = await boot();
      addTearDown(persistence.db.close);
      final state = SyncStateStore(persistence.db);
      final high = peerStamp(120000);
      final low = peerStamp(1000);

      await state.raiseHlcHighWater(high.packed);
      await state.raiseHlcHighWater(low.packed);
      expect(await state.hlcHighWater(), high.packed);
    });
  });
}
