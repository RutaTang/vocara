import 'dart:convert';
import 'dart:io';

import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_state_store.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late AssetStore assets;
  late SongPracticeRepositoryDrift repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_gc');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    assets = AssetStore(db: db, root: tmp, writer: writer);
    repo = SongPracticeRepositoryDrift(db: db, writer: writer, assets: assets);
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<String> makeSrc(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return f.path;
  }

  /// Settles the durability ledger, exactly as a push ack would
  /// (OplogDrain.ack hard-DELETES the covered entries). sweepUnreferenced
  /// holds back any asset whose journal entry is still pending — a
  /// pushed-but-unacked row may already be server-side — so tests exercising
  /// the sweep's OTHER mechanics drain the ledger first; the pending-journal
  /// guard has its own test below.
  Future<void> ackJournal() => db.delete(db.oplog).go();

  test('sweep tombstones + deletes a wholly-unreferenced asset', () async {
    final id = await assets.importFile(await makeSrc('lone.wav', [1, 2, 3]));
    final path = (await assets.resolve(id))!;
    expect(await File(path).exists(), isTrue);
    await ackJournal();

    // minAge zero: these mechanics tests exercise the reclaim itself; the
    // default age grace window has its own test below.
    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
    expect(await assets.resolve(id), isNull); // tombstoned
    expect(await File(path).exists(), isFalse); // file reclaimed
  });

  test('sweepUnreferenced skips assets younger than the age grace window '
      '(a transiently-unreferenced row is in flight, not garbage)', () async {
    final id = await assets.importFile(await makeSrc('young.wav', [1, 2]));
    await ackJournal();
    // Default grace: a brand-new unreferenced row survives the sweep (its
    // referencing clip/media rows may simply not have committed yet, and a
    // journaled tombstone would propagate fleet-wide once sync ships).
    expect(await assets.sweepUnreferenced(), 0);
    expect(await assets.resolve(id), isNotNull);
    // Aged past the window it is ordinary garbage again.
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?',
      [id],
    );
    expect(await assets.sweepUnreferenced(), 1);
    expect(await assets.resolve(id), isNull);
  });

  test('sweepUnreferenced treats assets referenced by quarantined payloads '
      'as reachable (a parked page must not become a tombstone)', () async {
    final id = await assets.importFile(await makeSrc('parked.wav', [3, 4, 5]));
    await ackJournal();
    const clipId = 'CLIPCLIPCLIPCLIPCLIPCLIP01';
    // A rejected remote clip referencing the asset, parked for review: until
    // an upgrade or manual review re-applies it, the parked payload may hold
    // the ONLY pointer at these bytes.
    await QuarantineStore(db).put(
      table: 'clips',
      id: clipId,
      hlcPacked: 'z',
      schemaV: 1,
      payload: jsonEncode({
        'table': 'clips',
        'id': clipId,
        'schemaV': 1,
        'hlcPacked': 'z',
        'fields': {'id': clipId, 'asset_id': id},
        'fieldClocks': null,
      }),
      reason: 'apply-failed',
      quarantinedAt: DateTime.now().millisecondsSinceEpoch,
    );

    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 0);
    expect(await assets.resolve(id), isNotNull);

    // Unparked (re-applied or discarded): the asset is sweepable again.
    await QuarantineStore(db).delete('clips', clipId);
    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
    expect(await assets.resolve(id), isNull);
  });

  test('sweepUnreferenced never touches an asset that has EVER been synced '
      '(local reachability is not knowledge for a synced asset; a future '
      'server-side refcount owns that lifecycle)', () async {
    // Three unreferenced assets, aged past the grace window below: one whose
    // RECORD crossed the wire, one whose BYTES did, one fully local.
    final syncedRecord = await assets.importFile(
      await makeSrc('synced_record.wav', [1, 1]),
    );
    final syncedBytes = await assets.importFile(
      await makeSrc('synced_bytes.wav', [2, 2]),
    );
    final localOnly = await assets.importFile(
      await makeSrc('local_only.wav', [3, 3]),
    );
    await db.customStatement(
      "UPDATE assets SET record_sync_state = 'synced' WHERE id = ?",
      [syncedRecord],
    );
    await db.customStatement(
      "UPDATE assets SET bytes_sync_state = 'uploaded' WHERE id = ?",
      [syncedBytes],
    );
    await ackJournal();

    // Only the never-synced asset is swept: a device behind on pull cannot
    // tell a remotely-referenced synced asset from garbage, and the journaled
    // tombstone would propagate as a fleet-wide delete.
    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
    expect(await assets.resolve(localOnly), isNull); // tombstoned
    expect(await assets.resolve(syncedRecord), isNotNull); // left alone
    expect(await assets.resolve(syncedBytes), isNotNull); // left alone
  });

  test('sweepUnreferenced holds back an asset whose journal entry is still '
      'PENDING (a pushed-but-unacked row may already be server-side); once '
      'the entry is acked it is sweepable again', () async {
    // The pending-journal guard engages only once a transport has ever
    // existed (the durable marker the engine writes before its first wire
    // call): before that, no server can hold anything.
    await SyncStateStore(db).markTransportSeen();
    final id = await assets.importFile(await makeSrc('inflight.wav', [8, 8]));
    // Aged past the grace window, unreferenced, both sync states 'localOnly'
    // — sweepable on every other guard. But importFile's journal entry is
    // still pending, and a crash between the server durably persisting a push
    // and the ack landing looks EXACTLY like this: record_sync_state never
    // advanced, yet peers may already have pulled the row. Sweeping it would
    // journal a tombstone that broadcasts a fleet-wide delete.
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?',
      [id],
    );
    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 0);
    expect(await assets.resolve(id), isNotNull, reason: 'fate not decidable');

    // The ack deletes the journal entry (simulated as the bare delete — on
    // the happy path the same round also marks the record synced, but the
    // crash-window row this guard protects is exactly the one that marking
    // missed). With the ledger settled the row is ordinary garbage again.
    await ackJournal();
    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
    expect(await assets.resolve(id), isNull);
  });

  test('tombstone-time re-verification: a first push landing between the '
      'candidate scan and the tombstone (transport marker + pending journal '
      'entry) aborts the tombstone — nothing journaled, bytes kept', () async {
    final id = await assets.importFile(await makeSrc('race.wav', [6, 6]));
    await ackJournal(); // scan-time state: clean and sweepable
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?',
      [id],
    );
    final path = (await assets.resolve(id))!;

    var fired = 0;
    final removed = await assets.sweepUnreferenced(
      minAge: Duration.zero,
      debugBeforeTombstone: (assetId) async {
        fired++;
        expect(assetId, id);
        // The first-ever push races the sweep: the engine durably marks the
        // transport BEFORE its first wire call, and the asset's row is
        // journaled for the wire — the exact crash-window shape the
        // pending-journal guard exists for, appearing AFTER the candidate
        // scan already read clean state. The conditional tombstone must
        // re-see both atomically and stand down.
        await SyncStateStore(db).markTransportSeen();
        await db
            .into(db.oplog)
            .insert(
              OplogCompanion.insert(
                entityTable: 'assets',
                entityId: assetId,
                op: 'insert',
                hlcPacked: 'x',
                payloadHash: '',
                createdAt: 1,
              ),
            );
      },
    );

    expect(fired, 1);
    expect(removed, 0, reason: 'the conditional tombstone matched 0 rows');
    expect(await assets.resolve(id), isNotNull, reason: 'row still live');
    expect(await File(path).exists(), isTrue, reason: 'bytes untouched');
    final tombstones = await db
        .customSelect("SELECT 1 AS x FROM oplog WHERE op = 'tombstone'")
        .get();
    expect(tombstones, isEmpty, reason: 'no fleet-wide delete journaled');
  });

  test('tombstone-time re-verification: record_sync_state advancing after '
      'the scan (a concurrent push ack marking the record synced) also '
      'aborts the tombstone', () async {
    final id = await assets.importFile(await makeSrc('marked.wav', [4, 2]));
    await ackJournal();
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?',
      [id],
    );

    final removed = await assets.sweepUnreferenced(
      minAge: Duration.zero,
      debugBeforeTombstone: (assetId) async {
        await db.customStatement(
          "UPDATE assets SET record_sync_state = 'synced' WHERE id = ?",
          [assetId],
        );
      },
    );
    expect(removed, 0);
    expect(await assets.resolve(id), isNotNull);
    expect(
      await db.select(db.oplog).get(),
      isEmpty,
      reason: 'no tombstone journaled for a row the UPDATE refused',
    );
  });

  test('tombstone-time re-verification changes nothing when no race occurs: '
      'a clean candidate still tombstones, journals and reclaims', () async {
    final id = await assets.importFile(await makeSrc('clean.wav', [5, 5]));
    await ackJournal();
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?',
      [id],
    );
    final path = (await assets.resolve(id))!;

    final removed = await assets.sweepUnreferenced(
      minAge: Duration.zero,
      debugBeforeTombstone: (_) async {}, // seam armed, state untouched
    );
    expect(removed, 1);
    expect(await assets.resolve(id), isNull);
    expect(await File(path).exists(), isFalse);
    final tombstones = await db
        .customSelect("SELECT 1 AS x FROM oplog WHERE op = 'tombstone'")
        .get();
    expect(tombstones, hasLength(1), reason: 'the delete still propagates');
  });

  test('with NO transport ever seen the pending-journal guard stands down: '
      'a journaled local asset still reclaims (production has no transport '
      'and nothing ever acks, so the guard would otherwise hold everything '
      'forever)', () async {
    final id = await assets.importFile(await makeSrc('offline.wav', [7, 7]));
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?',
      [id],
    );
    // The journal entry is deliberately NOT acked — today's production shape.
    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
    expect(await assets.resolve(id), isNull, reason: 'local reclaim survives');
  });

  test('an asset row that arrived over the wire is marked record-synced by '
      'the applier and the sweep skips it; a never-synced local asset stays '
      'sweepable', () async {
    // Local, never-synced garbage for contrast — aged past the grace window.
    final localId = await assets.importFile(await makeSrc('local.wav', [9]));
    await db.customStatement(
      'UPDATE assets SET created_at = 1, updated_at = 1 WHERE id = ?',
      [localId],
    );
    await ackJournal();

    // A remote asset record lands through the applier (no local bytes at
    // all): unreferenced and ancient, i.e. exactly what the sweep would eat
    // if the applier left the row on the 'localOnly' drift default. Another
    // device may reference it — local reachability is not knowledge here.
    const remoteId = 'ASSETASSETASSETASSETASSET1';
    final h = Hlc(wallMs: 1, counter: 0, node: 'REMOTE'.padRight(26, '0'));
    final applier = SyncApplier(db, HlcClock(node: 'GC'.padRight(26, '0')));
    final res = await applier.apply([
      RemoteRecord(
        table: 'assets',
        columns: {
          'id': remoteId,
          'content_hash': 'HASH_REMOTE',
          'byte_size': 8,
          'ext': 'wav',
          'kind': 'audio',
          'created_at': h.wallMs,
          'updated_at': h.wallMs,
          'deleted_at': null,
          'hlc_wall': h.wallMs,
          'hlc_ctr': h.counter,
          'hlc_node': h.node,
          'hlc_packed': h.packed,
          'schema_v': 1,
        },
      ),
    ]);
    expect(res.applied, 1);

    // Only the local never-synced asset is swept; the wire-arrived row is
    // protected by the record_sync_state the applier stamped on it.
    expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
    expect(await assets.resolve(localId), isNull, reason: 'still sweepable');
    final remote = await db
        .customSelect(
          'SELECT deleted_at, record_sync_state FROM assets WHERE id = ?',
          variables: [Variable.withString(remoteId)],
        )
        .getSingle();
    expect(remote.data['record_sync_state'], 'synced');
    expect(remote.data['deleted_at'], isNull, reason: 'never tombstoned');
  });

  test(
    'sweepOrphanPoolFiles reclaims a file left after a tombstone (crash)',
    () async {
      // A second, LIVE asset must exist: with zero live rows the sweep's
      // lost-database fuse (deliberately) protects the whole pool instead.
      await assets.importFile(await makeSrc('live.wav', [1]));
      final id = await assets.importFile(await makeSrc('crash.wav', [5, 6, 7]));
      final path = (await assets.resolve(id))!;
      expect(await File(path).exists(), isTrue);
      // Simulate a crash between the tombstone commit and the file reclaim in
      // sweepUnreferenced: the row is tombstoned but the pooled file is still on disk.
      // sweepUnreferenced never revisits it (it iterates only live rows), so the
      // orphan sweep — trusting only LIVE hashes — must reclaim it.
      await db.customStatement(
        'UPDATE assets SET deleted_at = 1 WHERE id = ?',
        [id],
      );
      expect(await File(path).exists(), isTrue);
      expect(await assets.sweepOrphanPoolFiles(), 1);
      expect(await File(path).exists(), isFalse); // gone from the pool...
      // ...but parked in the trash, not unlinked.
      final trashed = (await assets.trashDir.list().toList()).whereType<File>();
      expect(
        trashed.where((f) => p.basename(f.path).endsWith(p.basename(path))),
        hasLength(1),
      );
    },
  );

  test('orphan-sweep fuse: an empty database with a populated pool touches '
      'nothing (database lost, protect the bytes)', () async {
    // Zero asset rows at all (a wiped/replaced DB file) while the pool still
    // holds content-addressed bytes — the only legitimate zero-live-rows
    // state is a fresh install with NO pool, so the sweep must skip.
    final hash = 'a' * 64;
    final orphan = File(p.join(tmp.path, 'assets', 'aa', 'aa', '$hash.wav'));
    await orphan.create(recursive: true);
    await orphan.writeAsBytes([9, 9, 9]);

    expect(await assets.sweepOrphanPoolFiles(), 0);
    expect(await orphan.exists(), isTrue); // bytes protected
    expect(await assets.trashDir.exists(), isFalse); // nothing trashed
  });

  test(
    'a GC-reclaimed pool file lands in the trash and is recoverable',
    () async {
      final id = await assets.importFile(await makeSrc('gone.wav', [1, 2, 3]));
      final path = (await assets.resolve(id))!;
      await ackJournal();

      expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
      expect(await File(path).exists(), isFalse); // gone from the pool...

      // ...and sitting in trash under `<epochMs>-<originalName>`, bytes intact.
      final trashed = (await assets.trashDir.list().toList())
          .whereType<File>()
          .toList();
      expect(trashed, hasLength(1));
      expect(p.basename(trashed.single.path), endsWith(p.basename(path)));
      expect(await trashed.single.readAsBytes(), [1, 2, 3]);
    },
  );

  test('pruneTrash removes only entries older than the retention', () async {
    final trash = assets.trashDir;
    await trash.create(recursive: true);
    final nowMs = DateTime.now().millisecondsSinceEpoch;
    const dayMs = 24 * 3600 * 1000;

    final expired = File(p.join(trash.path, '${nowMs - 31 * dayMs}-old.wav'));
    final fresh = File(p.join(trash.path, '$nowMs-fresh.wav'));
    // No parseable epoch prefix: never aged out (don't delete undatable bytes).
    final undated = File(p.join(trash.path, 'nostamp.wav'));
    for (final f in [expired, fresh, undated]) {
      await f.writeAsBytes([1]);
    }
    // A swept staging TREE ages out as one entry, recursively.
    final expiredTree = Directory(
      p.join(trash.path, '${nowMs - 40 * dayMs}-audio'),
    );
    final treeFile = File(p.join(expiredTree.path, 'p1', 'take.wav'));
    await treeFile.create(recursive: true);
    await treeFile.writeAsBytes([2]);

    expect(await assets.pruneTrash(), 2);
    expect(await expired.exists(), isFalse);
    expect(await expiredTree.exists(), isFalse);
    expect(await fresh.exists(), isTrue);
    expect(await undated.exists(), isTrue);
  });

  test(
    'assetRefColumns covers every schema column referencing assets',
    () async {
      // Derive the asset-referencing columns STRUCTURALLY from the live schema:
      // every foreign key into assets. If a new `.references(Assets)` column
      // ever ships without being added to AssetStore.assetRefColumns, the GC
      // would treat its assets as unreferenced and reclaim their bytes — fail
      // here instead.
      final fromSchema = <String>{};
      for (final table in db.allTables) {
        final name = table.actualTableName;
        final fks = await db
            .customSelect('PRAGMA foreign_key_list($name)')
            .get();
        for (final fk in fks) {
          if (fk.data['table'] == 'assets') {
            fromSchema.add('$name.${fk.data['from']}');
          }
        }
      }
      expect(fromSchema, isNotEmpty); // the derivation itself must keep working
      final declared = {
        for (final ref in AssetStore.assetRefColumns)
          '${ref.table}.${ref.column}',
      };
      expect(
        declared,
        fromSchema,
        reason:
            'AssetStore.assetRefColumns must list exactly the schema columns '
            'that reference assets: a missing one makes the GC delete live '
            'bytes; a stale one breaks the sweep query.',
      );
    },
  );

  test(
    'sweep deletes every pooled file for a hash (same bytes, two extensions)',
    () async {
      final id = await assets.importFile(await makeSrc('dup.wav', [7, 7, 7]));
      final wavPath = (await assets.resolve(id))!;
      // Simulate identical bytes pooled under a different extension by another device
      // (same hash, `<hash>.wav` AND `<hash>.mp3`).
      final mp3Path = wavPath.replaceAll(RegExp(r'\.wav$'), '.mp3');
      await File(wavPath).copy(mp3Path);
      expect(await File(mp3Path).exists(), isTrue);
      await ackJournal();
      // The asset is unreferenced -> sweep tombstones it and deletes ALL files for the
      // hash, not just the current row's `.wav`.
      expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
      expect(await File(wavPath).exists(), isFalse);
      expect(await File(mp3Path).exists(), isFalse);
    },
  );

  test(
    'a referenced asset survives sweep, then is collectable once unused',
    () async {
      final base = (await repo.createPractice('S') as Ok<SongPractice>).value;
      final clipPath = await makeSrc('take.wav', [9, 9, 9]);
      final vocal = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take',
        createdAt: DateTime(2026),
        versionId: base.versions.single.id,
        clips: [
          AudioClip(id: newId(), audioFilePath: clipPath, durationMs: 1000),
        ],
      );
      final withClip = SongPractice(
        id: base.id,
        title: base.title,
        createdAt: base.createdAt,
        updatedAt: base.updatedAt,
        vocalTracks: [vocal],
        versions: base.versions,
        activeVersionId: base.versions.single.id,
      );
      (await repo.savePractice(withClip) as Ok<SongPractice>).value;

      // One referenced asset + one standalone unreferenced asset.
      await assets.importFile(await makeSrc('orphan.wav', [4, 4]));
      expect(await db.select(db.assets).get(), hasLength(2));
      await ackJournal();

      // Sweep removes only the orphan; the clip's asset survives.
      expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
      final liveAssets = await (db.select(
        db.assets,
      )..where((a) => a.deletedAt.isNull())).get();
      expect(liveAssets, hasLength(1));

      // Drop the clip -> its asset is now unreferenced -> a later sweep
      // (past the age grace window) reclaims it.
      (await repo.savePractice(
                SongPractice(
                  id: base.id,
                  title: base.title,
                  createdAt: base.createdAt,
                  updatedAt: base.updatedAt,
                  versions: base.versions,
                  activeVersionId: base.versions.single.id,
                ),
              )
              as Ok<SongPractice>)
          .value;
      await ackJournal();
      expect(await assets.sweepUnreferenced(minAge: Duration.zero), 1);
      final remaining = await (db.select(
        db.assets,
      )..where((a) => a.deletedAt.isNull())).get();
      expect(remaining, isEmpty);
    },
  );
}
