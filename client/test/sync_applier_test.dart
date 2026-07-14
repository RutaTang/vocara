import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_row.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

void main() {
  late AppDatabase db;
  late SyncApplier applier;
  final remoteNode = _id('REMOTE');

  setUp(() {
    db = AppDatabase(NativeDatabase.memory());
    applier = SyncApplier(db, HlcClock(node: _id('LOCAL'), now: () => 1000));
  });
  tearDown(() => db.close());

  Hlc hlc(int wall, [int ctr = 0]) =>
      Hlc(wallMs: wall, counter: ctr, node: remoteNode);

  RemoteRecord practice(
    String id,
    String title,
    Hlc h, {
    int? deletedAt,
    String? separationAssetId,
  }) => RemoteRecord(
    table: 'practices',
    columns: {
      'id': _id(id),
      'title': title,
      'timeline_duration_ms': 0,
      'master_volume': 1.0,
      if (separationAssetId != null)
        'separation_asset_id': _id(separationAssetId),
      'created_at': h.wallMs,
      'updated_at': h.wallMs,
      'deleted_at': deletedAt,
      'hlc_wall': h.wallMs,
      'hlc_ctr': h.counter,
      'hlc_node': h.node,
      'hlc_packed': h.packed,
      'schema_v': 1,
    },
  );

  RemoteRecord track(String id, String practiceId, Hlc h) => RemoteRecord(
    table: 'tracks',
    columns: {
      'id': _id(id),
      'practice_id': _id(practiceId),
      'kind': 'vocal',
      'display_name': '',
      'muted': 0,
      'soloed': 0,
      'volume': 1.0,
      'pan': 0.0,
      'order_index': 0,
      'created_at': h.wallMs,
      'updated_at': h.wallMs,
      'deleted_at': null,
      'hlc_wall': h.wallMs,
      'hlc_ctr': h.counter,
      'hlc_node': h.node,
      'hlc_packed': h.packed,
      'schema_v': 1,
    },
  );

  RemoteRecord clip(String id, String trackId, String assetId, Hlc h) =>
      RemoteRecord(
        table: 'clips',
        columns: {
          'id': _id(id),
          'track_id': _id(trackId),
          'asset_id': _id(assetId),
          'start_ms': 0,
          'duration_ms': 0,
          'order_index': 0,
          'created_at': h.wallMs,
          'updated_at': h.wallMs,
          'deleted_at': null,
          'hlc_wall': h.wallMs,
          'hlc_ctr': h.counter,
          'hlc_node': h.node,
          'hlc_packed': h.packed,
          'schema_v': 1,
        },
      );

  Future<Map<String, Object?>?> row(String id) async {
    final r = await db
        .customSelect(
          'SELECT title, deleted_at, separation_asset_id '
          'FROM practices WHERE id = ?',
          variables: [Variable.withString(_id(id))],
        )
        .getSingleOrNull();
    return r?.data;
  }

  Future<Map<String, Object?>?> trackRow(String id) async {
    final r = await db
        .customSelect(
          'SELECT id, deleted_at FROM tracks WHERE id = ?',
          variables: [Variable.withString(_id(id))],
        )
        .getSingleOrNull();
    return r?.data;
  }

  test('a new record is inserted', () async {
    final res = await applier.apply([practice('P', 'Hello', hlc(900))]);
    expect(res.applied, 1);
    expect((await row('P'))!['title'], 'Hello');
  });

  test('a higher-HLC update wins; a lower-HLC update is skipped', () async {
    await applier.apply([practice('P', 'v1', hlc(900))]);

    final win = await applier.apply([practice('P', 'v2', hlc(950))]);
    expect(win.applied, 1);
    expect((await row('P'))!['title'], 'v2');

    final lose = await applier.apply([practice('P', 'stale', hlc(920))]);
    expect(lose.skipped, 1);
    expect(lose.applied, 0);
    expect((await row('P'))!['title'], 'v2'); // unchanged
  });

  test('a tombstone arrives as a higher-HLC update', () async {
    await applier.apply([practice('P', 'v1', hlc(900))]);
    final res = await applier.apply([
      practice('P', 'v1', hlc(970), deletedAt: 970),
    ]);
    expect(res.applied, 1);
    expect((await row('P'))!['deleted_at'], isNotNull);
  });

  test('an implausibly-future record is quarantined, not applied', () async {
    // now=1000, default maxDrift=5min -> wall 5min+1 ahead is rejected.
    final res = await applier.apply([
      practice('FUT', 'from the future', hlc(1000 + 5 * 60 * 1000 + 1)),
    ]);
    expect(res.quarantined, 1);
    expect(res.applied, 0);
    expect(await row('FUT'), isNull);
  });

  test('a malformed record is quarantined without discarding the batch', () async {
    final good = practice('GOOD', 'kept', hlc(900));
    // A record carrying a column that does not exist locally (e.g. from a newer
    // schema_v) fails at INSERT — a NON-drift error. It must roll back only itself
    // (its SAVEPOINT), not the whole batch, so the already-applied good record stays.
    final bad = RemoteRecord(
      table: 'practices',
      columns: {
        ...practice('BAD', 'dropped', hlc(910)).columns,
        'column_from_the_future': 1,
      },
    );
    final res = await applier.apply([good, bad]);
    expect(res.applied, 1);
    expect(res.failed, 1);
    expect((await row('GOOD'))!['title'], 'kept'); // survived the later failure
    expect(await row('BAD'), isNull); // rolled back to its savepoint
  });

  test('applying remote changes writes NO oplog (no ping-pong)', () async {
    await applier.apply([
      practice('A', 'a', hlc(900)),
      practice('B', 'b', hlc(910)),
    ]);
    await applier.apply([practice('A', 'a2', hlc(950))]);
    expect(await db.select(db.oplog).get(), isEmpty);
  });

  test('the local clock advances past an applied remote stamp', () async {
    await applier.apply([practice('P', 'x', hlc(2000))]);
    // A subsequent local tick must be strictly after the remote stamp we saw.
    final next = applier.clock.tick();
    expect(next.wallMs >= 2000, isTrue);
  });

  group('FK sweep', () {
    test(
      'a child arriving BEFORE its parent within a batch applies fine',
      () async {
        final res = await applier.apply([
          track('T', 'P', hlc(900)), // child first
          practice('P', 'parent', hlc(901)), // parent later in the same batch
        ]);
        expect(res.applied, 2);
        expect(res.failed, 0);
        expect((await row('P'))!['title'], 'parent');
        expect(await trackRow('T'), isNotNull);
      },
    );

    test(
      'a true orphan whose parent never arrives is quarantined ALONE',
      () async {
        final res = await applier.apply([
          practice('P', 'kept', hlc(900)),
          track('T', 'NEVER', hlc(910)), // parent not in this batch, not local
        ]);
        expect(res.applied, 1); // the good record stuck
        expect(res.failed, 1); // the orphan did not
        expect((await row('P'))!['title'], 'kept');
        expect(await trackRow('T'), isNull); // hard-deleted; re-arrives later
      },
    );

    test(
      'an orphan violating TWO foreign keys is deleted + counted once',
      () async {
        final res = await applier.apply([
          practice('P', 'kept', hlc(900)),
          // Both track_id and asset_id dangle -> two foreign_key_check entries
          // for one row. It must decrement applied / increment failed only once.
          clip('C', 'NOTRACK', 'NOASSET', hlc(910)),
        ]);
        expect(res.applied, 1);
        expect(res.failed, 1);
        expect((await row('P'))!['title'], 'kept');
      },
    );

    test('a remote update pointing a PRE-EXISTING row at a not-yet-arrived '
        'asset nulls the pointer; the row and its subtree survive', () async {
      // Seed an earlier page: a practice with a child track (all FKs intact).
      await applier.apply([
        practice('P', 'v1', hlc(900)),
        track('T', 'P', hlc(901)),
      ]);

      // A later page updates the practice, referencing an asset record that
      // has not arrived yet. The OLD sweep hard-deleted the practice and then
      // cascaded through its whole pre-existing subtree.
      final res = await applier.apply([
        practice('P', 'v2', hlc(950), separationAssetId: 'FUTUREASSET'),
      ]);
      expect(res.applied, 1);
      expect(res.failed, 0);

      final p = await row('P');
      expect(p, isNotNull); // the practice row survived
      expect(p!['title'], 'v2'); // the update itself stuck
      expect(p['separation_asset_id'], isNull); // the dangler was healed
      expect(await trackRow('T'), isNotNull); // subtree intact
    });
  });

  group('per-device asset columns', () {
    RemoteRecord asset(
      String id,
      Hlc h, {
      String ext = 'wav',
      Map<String, Object?> extra = const {},
    }) => RemoteRecord(
      table: 'assets',
      columns: {
        'id': _id(id),
        'content_hash': 'HASH_$id',
        'byte_size': 8,
        'ext': ext,
        'kind': 'audio',
        'created_at': h.wallMs,
        'updated_at': h.wallMs,
        'deleted_at': null,
        'hlc_wall': h.wallMs,
        'hlc_ctr': h.counter,
        'hlc_node': h.node,
        'hlc_packed': h.packed,
        'schema_v': 1,
        ...extra,
      },
    );

    Future<Map<String, Object?>> assetRow(String id) async {
      final r = await db
          .customSelect(
            'SELECT ext, local_rel_path, record_sync_state, '
            'bytes_sync_state, remote_ref FROM assets WHERE id = ?',
            variables: [Variable.withString(_id(id))],
          )
          .getSingle();
      return r.data;
    }

    test('a remote UPDATE that still carries per-device columns (older-build '
        'peer) wins LWW without clobbering local bookkeeping', () async {
      // Local state: bytes materialized HERE as an .mp3 under this device's
      // pool path, with its own sync-state bookkeeping.
      await applier.apply([asset('A', hlc(900), ext: 'mp3')]);
      await db.customStatement(
        "UPDATE assets SET local_rel_path = 'assets/HA/SH/HASH_A.mp3', "
        "record_sync_state = 'synced', bytes_sync_state = 'present', "
        "remote_ref = 'blob://HASH_A' WHERE id = ?",
        [_id('A')],
      );

      // A newer remote version arrives carrying the SENDER's per-device
      // values (a .wav rel path that does not exist here, localOnly states).
      final res = await applier.apply([
        asset(
          'A',
          hlc(950),
          extra: const {
            'local_rel_path': 'assets/HA/SH/HASH_A.wav',
            'record_sync_state': 'localOnly',
            'bytes_sync_state': 'localOnly',
            'remote_ref': null,
          },
        ),
      ]);
      expect(res.applied, 1);

      final row = await assetRow('A');
      expect(row['ext'], 'wav', reason: 'the synced content applied');
      // The per-device bookkeeping survived the winning remote apply.
      expect(row['local_rel_path'], 'assets/HA/SH/HASH_A.mp3');
      expect(row['record_sync_state'], 'synced');
      expect(row['bytes_sync_state'], 'present');
      expect(row['remote_ref'], 'blob://HASH_A');
    });

    test('a remote INSERT with per-device columns lands on schema defaults '
        '(no adopted foreign bookkeeping)', () async {
      final res = await applier.apply([
        asset(
          'B',
          hlc(900),
          extra: const {
            'local_rel_path': 'assets/XX/YY/HASH_B.wav',
            'record_sync_state': 'synced',
            'bytes_sync_state': 'present',
            'remote_ref': 'blob://HASH_B',
          },
        ),
      ]);
      expect(res.applied, 1);

      final row = await assetRow('B');
      expect(row['local_rel_path'], isNull, reason: 'no bytes here yet');
      expect(row['record_sync_state'], 'localOnly');
      expect(row['bytes_sync_state'], 'localOnly');
      expect(row['remote_ref'], isNull);
    });
  });

  group('quarantine persistence', () {
    late QuarantineStore store;

    setUp(() {
      store = QuarantineStore(db);
      // Same applier wiring plus the optional collaborator under test.
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        quarantine: store,
      );
    });

    test('an HLC-drift record is parked with its full wire payload', () async {
      final future = hlc(1000 + 5 * 60 * 1000 + 1);
      final rec = practice('FUT', 'from the future', future);
      final res = await applier.apply([rec]);
      expect(res.quarantined, 1);
      expect(await row('FUT'), isNull); // still NOT applied

      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.tbl, 'practices');
      expect(parked.single.id, _id('FUT'));
      expect(parked.single.remoteHlcPacked, future.packed);
      expect(parked.single.schemaV, 1);
      expect(parked.single.reason, startsWith('hlc-drift'));
      // The payload is the SyncRow wire format: replayable later, verbatim.
      final replay = SyncRow.decode(parked.single.payload);
      expect(replay.table, 'practices');
      expect(replay.fields, equals(rec.columns));
    });

    test('a forward-incompatible record is parked with reason apply-failed '
        'and the good record still applies', () async {
      final good = practice('GOOD', 'kept', hlc(900));
      final bad = RemoteRecord(
        table: 'practices',
        columns: {
          ...practice('BAD', 'dropped', hlc(910)).columns,
          'column_from_the_future': 1,
        },
      );
      final res = await applier.apply([good, bad]);
      expect(res.applied, 1);
      expect(res.failed, 1);
      expect((await row('GOOD'))!['title'], 'kept');
      expect(await row('BAD'), isNull);

      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.id, _id('BAD'));
      expect(parked.single.reason, startsWith('apply-failed'));
      // The unknown column rides along in the parked payload for a future
      // (schema-upgraded) re-apply.
      final replay = SyncRow.decode(parked.single.payload);
      expect(replay.fields['column_from_the_future'], 1);
    });

    test('re-quarantining the same row keeps only the latest state', () async {
      final drift1 = hlc(1000 + 6 * 60 * 1000);
      final drift2 = hlc(1000 + 7 * 60 * 1000);
      await applier.apply([practice('FUT', 'v1', drift1)]);
      await applier.apply([practice('FUT', 'v2', drift2)]);

      final parked = await store.list();
      expect(parked, hasLength(1)); // latest per (tbl, id), not a pile-up
      expect(parked.single.remoteHlcPacked, drift2.packed);
      expect(SyncRow.decode(parked.single.payload).fields['title'], 'v2');
    });

    test('quarantine rows commit atomically with the batch (same outer '
        'transaction, no oplog echo)', () async {
      await applier.apply([
        practice('OK', 'applied', hlc(900)),
        practice('FUT', 'parked', hlc(1000 + 6 * 60 * 1000)),
      ]);
      // Both the applied row and the parked row landed...
      expect((await row('OK'))!['title'], 'applied');
      expect(await store.list(), hasLength(1));
      // ...and neither the apply nor the quarantine journaled anything.
      expect(await db.select(db.oplog).get(), isEmpty);
    });
  });
}
