import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_row.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

/// An [HlcClock] that counts its state-changing [receive] calls, so a test
/// can assert not just WHERE the clock ended up but HOW OFTEN the applier
/// advanced it (once per settled record, only after the outer commit).
class _CountingClock extends HlcClock {
  _CountingClock({required super.node, super.now});

  int receives = 0;

  @override
  Hlc receive(Hlc remote) {
    receives++;
    return super.receive(remote);
  }
}

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

  // Every helper below builds a FULL-row snapshot (all non-per-device columns
  // of its table), matching the wire contract the applier now enforces for
  // schema_v == currentRecordSchemaV records: a partial row is quarantined at
  // the missing-column gate, never partially applied.
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
      'active_interpretation_id': null,
      'separation_asset_id': separationAssetId == null
          ? null
          : _id(separationAssetId),
      'reverb_json': null,
      'lyrics_json': null,
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
      'interpretation_id': null,
      'display_name': '',
      'color_value': null,
      'muted': 0,
      'soloed': 0,
      'volume': 1.0,
      'pan': 0.0,
      'bus_id': null,
      'fx_json': null,
      'sends_json': null,
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

  test(
    'a malformed record is quarantined without discarding the batch',
    () async {
      final good = practice('GOOD', 'kept', hlc(900));
      // A record carrying a column that does not exist locally is refused at
      // the applier's structural column gate (its name must never reach the SQL
      // text). It must reject only itself, not the whole batch, so the
      // already-applied good record stays.
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
      expect(
        (await row('GOOD'))!['title'],
        'kept',
      ); // survived the later failure
      expect(await row('BAD'), isNull); // never written
    },
  );

  test('an unknown table is quarantined at the door (its name must never '
      'reach the SQL text) and the rest of the batch applies', () async {
    final good = practice('GOOD', 'kept', hlc(900));
    final bad = RemoteRecord(
      // A table name that is not in the synced registry — whether a hostile
      // payload or a table from a future schema, it must not be interpolated.
      table: 'practices; DROP TABLE practices; --',
      columns: practice('BAD', 'dropped', hlc(910)).columns,
    );
    final res = await applier.apply([good, bad]);
    expect(res.applied, 1);
    expect(res.failed, 1);
    expect((await row('GOOD'))!['title'], 'kept');
    expect(await row('BAD'), isNull);
  });

  test('a local-only table is refused even though it exists in the database '
      '(only SYNCED tables are writable through the remote door)', () async {
    final res = await applier.apply([
      RemoteRecord(
        table: 'sync_state', // real table, not in the synced registry
        columns: practice('X', 'nope', hlc(900)).columns,
      ),
    ]);
    expect(res.failed, 1);
    expect(res.applied, 0);
    expect(await db.select(db.syncState).get(), isEmpty);
  });

  test('a record whose HLC quad is mistyped is quarantined alone; the page '
      'survives and later records still apply', () async {
    // Regression: a TypeError out of rec.hlc used to escape the per-record
    // handling, fail the WHOLE batch, and (through the engine) retry the same
    // page forever — one bad record bricked sync and blocked every good
    // record on the page.
    final bad = RemoteRecord(
      table: 'practices',
      columns: {
        ...practice('BAD', 'dropped', hlc(910)).columns,
        'hlc_wall': 'not-an-int',
      },
    );
    final res = await applier.apply([
      practice('BEFORE', 'first', hlc(900)),
      bad,
      practice('AFTER', 'second', hlc(920)),
    ]);
    expect(res.applied, 2);
    expect(res.failed, 1);
    expect((await row('BEFORE'))!['title'], 'first');
    expect((await row('AFTER'))!['title'], 'second');
    expect(await row('BAD'), isNull);
  });

  group('missing-column gate (full-row wire contract)', () {
    Future<String> hlcPackedOf(String id) async {
      final r = await db
          .customSelect(
            'SELECT hlc_packed FROM practices WHERE id = ?',
            variables: [Variable.withString(_id(id))],
          )
          .getSingle();
      return r.read<String>('hlc_packed');
    }

    test('a PARTIAL row at the current schema_v is quarantined whole — '
        'neither content nor hlc_packed moves, so a later genuine full row '
        'with an intermediate HLC still wins LWW', () async {
      final store = QuarantineStore(db);
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        quarantine: store,
      );
      await applier.apply([practice('P', 'v1', hlc(900))]);

      // A partial row with the NEWEST HLC. Accepting it would write
      // hlc_packed 950 while keeping stale content for the missing column —
      // and the genuine full row below (HLC 920) would then lose LWW forever.
      final partial = Map<String, Object?>.of(
        practice('P', 'poisoner', hlc(950)).columns,
      )..remove('reverb_json');
      final res = await applier.apply([
        RemoteRecord(table: 'practices', columns: partial),
      ]);
      expect(res.failed, 1);
      expect(res.applied, 0);
      expect((await row('P'))!['title'], 'v1', reason: 'content unchanged');
      expect(
        await hlcPackedOf('P'),
        hlc(900).packed,
        reason: 'the LWW key must not advance on a rejected partial row',
      );
      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.reason, startsWith('missing-column'));
      expect(parked.single.reason, contains('reverb_json'));

      // The genuine full row between the two HLCs applies normally.
      final late = await applier.apply([practice('P', 'v2', hlc(920))]);
      expect(late.applied, 1);
      expect((await row('P'))!['title'], 'v2');
      expect(await hlcPackedOf('P'), hlc(920).packed);
    });

    test('a below-range schema_v never reaches the older-peer exemption: '
        'wire versions start at 1, so a v0/negative record is quarantined '
        'whole — with missing columns it would otherwise walk between the '
        'newer-schema and full-row gates and partially apply', () async {
      await applier.apply([practice('P', 'v1', hlc(900))]);

      // A "v0 peer" cannot exist (currentRecordSchemaV has never been below
      // 1): a subset row claiming schema_v 0 is malformed, and letting it
      // through would advance hlc_packed while silently keeping stale local
      // content for its missing columns — the exact poisoning the full-row
      // gate exists to stop.
      final h = hlc(950);
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            'id': _id('P'),
            'title': 'from a below-range build',
            'created_at': h.wallMs,
            'updated_at': h.wallMs,
            'deleted_at': null,
            'hlc_wall': h.wallMs,
            'hlc_ctr': h.counter,
            'hlc_node': h.node,
            'hlc_packed': h.packed,
            'schema_v': 0,
          },
        ),
      ]);
      expect(res.failed, 1);
      expect(res.applied, 0);
      expect((await row('P'))!['title'], 'v1', reason: 'content unchanged');
      expect(
        await hlcPackedOf('P'),
        hlc(900).packed,
        reason: 'the LWW key must not advance on a rejected record',
      );
    });
  });

  group('clock advance timing', () {
    test(
      'a record that fails inside its savepoint leaves the clock exactly '
      'where it was (the gate is check-only; receive runs post-settle)',
      () async {
        final before = applier.clock.last;
        // Passes every door gate (full columns, plausible HLC one minute
        // ahead) and then violates the byte_size CHECK inside its savepoint.
        final h = hlc(1000 + 60 * 1000);
        final res = await applier.apply([
          RemoteRecord(
            table: 'assets',
            columns: {
              'id': _id('BAD'),
              'content_hash': 'HASH_BAD',
              'byte_size': -1, // CHECK (byte_size >= 0) fires in the savepoint
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
        expect(res.failed, 1);
        expect(res.applied, 0);
        expect(
          applier.clock.last,
          before,
          reason:
              'a rolled-back record must leave NO side effect — an advanced '
              'in-memory clock included',
        );
      },
    );

    test('an applied record advances the clock past its stamp', () async {
      final h = hlc(1000 + 60 * 1000); // one minute ahead, within drift
      final res = await applier.apply([practice('P', 'applied', h)]);
      expect(res.applied, 1);
      expect(applier.clock.last.wallMs, greaterThanOrEqualTo(h.wallMs));
    });

    test('a batch whose OUTER commit fails leaves the clock untouched even '
        'though its records had settled inside the transaction', () async {
      final before = applier.clock.last;
      // The record itself is fine and settles as applied inside the loop;
      // the throwing beforeCommit hook (the engine's token-write seam) then
      // rolls the whole outer transaction back. The database forgot the
      // record — the in-memory clock must have forgotten it too.
      final res = await applier.apply([
        practice('P', 'settled then rolled back', hlc(1000 + 60 * 1000)),
      ], beforeCommit: () async => throw StateError('kill the outer commit'));
      expect(res.committed, isFalse);
      expect(await row('P'), isNull, reason: 'the batch rolled back');
      expect(
        applier.clock.last,
        before,
        reason:
            'the database rolled back the settled record; an advanced '
            'in-memory clock would be a side effect surviving the rollback',
      );
    });

    test('a committed batch advances the clock exactly once per settled '
        'record, never for rejected ones', () async {
      final clock = _CountingClock(node: _id('LOCAL'), now: () => 1000);
      applier = SyncApplier(db, clock);
      // Seed P so a stale copy below settles as a clean LWW skip at the
      // door (the pre-savepoint settle, which never advances the clock —
      // unchanged by the post-commit deferral).
      await applier.apply([practice('P', 'v1', hlc(950))]);
      clock.receives = 0;

      final res = await applier.apply([
        practice('A', 'applied', hlc(960)),
        practice('P', 'stale', hlc(900)), // clean LWW skip at the door
        practice('FUT', 'poison', hlc(1000 + 6 * 60 * 1000)), // quarantined
      ]);
      expect(res.applied, 1);
      expect(res.skipped, 1);
      expect(res.quarantined, 1);
      expect(
        clock.receives,
        1,
        reason:
            'exactly the applied record — a door-settled skip never '
            'received (before or after the deferral), and a quarantined '
            'record must leave the clock alone',
      );
    });
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

    test('a remote INSERT with per-device columns adopts no foreign '
        'bookkeeping: schema defaults, except record_sync_state which is '
        'forced to synced (the row demonstrably crossed the wire)', () async {
      final res = await applier.apply([
        asset(
          'B',
          hlc(900),
          extra: const {
            'local_rel_path': 'assets/XX/YY/HASH_B.wav',
            'record_sync_state': 'localOnly',
            'bytes_sync_state': 'present',
            'remote_ref': 'blob://HASH_B',
          },
        ),
      ]);
      expect(res.applied, 1);

      final row = await assetRow('B');
      expect(row['local_rel_path'], isNull, reason: 'no bytes here yet');
      // NOT the sender's value and NOT the drift default: a wire-arrived
      // assets row is by definition record-synced, and leaving it at
      // 'localOnly' would make it sweepable by the asset-GC's never-synced
      // guard — the exact rows the guard exists to protect.
      expect(row['record_sync_state'], 'synced');
      expect(row['bytes_sync_state'], 'localOnly', reason: 'no bytes here');
      expect(row['remote_ref'], isNull);
    });

    test(
      'a remote UPDATE of an existing localOnly asset row marks it '
      'record-synced (another device now provably holds the record)',
      () async {
        // A locally-created asset row, never synced (drift defaults).
        await db.customStatement(
          'INSERT INTO assets (id, content_hash, byte_size, ext, kind, '
          'created_at, updated_at, hlc_wall, hlc_ctr, hlc_node, hlc_packed, '
          'schema_v) VALUES (?, ?, 8, ?, ?, 900, 900, 900, 0, ?, ?, 1)',
          [_id('A'), 'HASH_A', 'wav', 'audio', remoteNode, hlc(900).packed],
        );
        expect((await assetRow('A'))['record_sync_state'], 'localOnly');

        // A newer remote version of the SAME row arrives: the update must
        // advance record_sync_state along with the content — a remote peer
        // holding (and editing) the record is proof it crossed the wire.
        final res = await applier.apply([asset('A', hlc(950), ext: 'mp3')]);
        expect(res.applied, 1);
        final row = await assetRow('A');
        expect(row['ext'], 'mp3', reason: 'the content update applied');
        expect(row['record_sync_state'], 'synced');
        expect(
          row['bytes_sync_state'],
          'localOnly',
          reason: 'blob upload state is untouched — it has no transport yet',
        );
      },
    );
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

    test('an unknown column at an accepted schema_v is parked with reason '
        'unknown-column and the good record still applies', () async {
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
      expect(parked.single.reason, startsWith('unknown-column'));
      // The unknown column rides along in the parked payload for a future
      // (schema-upgraded) re-apply.
      final replay = SyncRow.decode(parked.single.payload);
      expect(replay.fields['column_from_the_future'], 1);
    });

    test('a genuinely NEWER schema_v is parked with the apply-failed reason '
        'BEFORE the column gate (an upgrade un-parks it via replay)', () async {
      final bad = RemoteRecord(
        table: 'practices',
        columns: {
          ...practice('BAD', 'from a newer build', hlc(910)).columns,
          'schema_v': currentRecordSchemaV + 1,
          'column_from_the_future': 1,
        },
      );
      final res = await applier.apply([bad]);
      expect(res.failed, 1);
      expect(await row('BAD'), isNull);

      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.schemaV, currentRecordSchemaV + 1);
      expect(parked.single.reason, startsWith('apply-failed'));
    });

    test('a schema_v of 0 is parked with reason invalid-schema_v; the rest '
        'of the batch applies (versions start at 1, and a below-range value '
        'would bypass both schema gates)', () async {
      final res = await applier.apply([
        practice('GOOD', 'kept', hlc(900)),
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('BAD', 'dropped', hlc(910)).columns,
            'schema_v': 0,
          },
        ),
        practice('ALSO', 'behind', hlc(920)),
      ]);
      expect(res.failed, 1);
      expect(res.applied, 2, reason: 'the batch continues around it');
      expect((await row('GOOD'))!['title'], 'kept');
      expect((await row('ALSO'))!['title'], 'behind');
      expect(await row('BAD'), isNull, reason: 'never applied');

      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.id, _id('BAD'));
      expect(parked.single.reason, startsWith('invalid-schema_v'));
      expect(parked.single.reason, contains('0'));
    });

    test('a NEGATIVE schema_v is parked the same way', () async {
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('BAD', 'dropped', hlc(910)).columns,
            'schema_v': -1,
          },
        ),
      ]);
      expect(res.failed, 1);
      expect(res.applied, 0);
      expect(await row('BAD'), isNull);

      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.reason, startsWith('invalid-schema_v'));
      expect(parked.single.reason, contains('-1'));
    });

    test('an unknown table is parked with reason unknown-table', () async {
      await applier.apply([
        RemoteRecord(
          table: 'not_a_table',
          columns: practice('BAD', 'dropped', hlc(910)).columns,
        ),
      ]);
      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.tbl, 'not_a_table');
      expect(parked.single.reason, startsWith('unknown-table'));
    });

    test('a mistyped HLC quad is parked with reason apply-failed (any '
        'per-record throw quarantines that record alone)', () async {
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('BAD', 'dropped', hlc(910)).columns,
            'hlc_ctr': 'seven',
          },
        ),
      ]);
      expect(res.failed, 1);
      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.reason, startsWith('apply-failed'));
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
