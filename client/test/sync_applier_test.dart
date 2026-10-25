import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/data/sync/sync_state_store.dart';
import 'package:vocara/data/sync/sync_writer.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

/// An [HlcClock] that counts its state-changing advances — [mergeVerified],
/// the call the applier's post-commit loop uses (never the drift-checking
/// [receive], see the applier docs) — so a test can assert not just WHERE
/// the clock ended up but HOW OFTEN the applier advanced it (once per
/// settled record, only after the outer commit).
class _CountingClock extends HlcClock {
  _CountingClock({required super.node, super.now});

  int advances = 0;

  @override
  Hlc mergeVerified(Hlc remote) {
    advances++;
    return super.mergeVerified(remote);
  }
}

/// A [QuarantineStore] whose [put] throws while [failing] is set — the
/// injected storage failure for asserting that a record which cannot be
/// PARKED fails the whole batch (token unmoved) instead of being silently
/// lost behind an advanced change token.
class _FlakyQuarantineStore extends QuarantineStore {
  _FlakyQuarantineStore(super.db);

  bool failing = true;

  @override
  Future<void> put({
    required String table,
    required String id,
    required String hlcPacked,
    required int schemaV,
    required String payload,
    required String reason,
    required int quarantinedAt,
  }) {
    if (failing) throw StateError('quarantine storage unavailable');
    return super.put(
      table: table,
      id: id,
      hlcPacked: hlcPacked,
      schemaV: schemaV,
      payload: payload,
      reason: reason,
      quarantinedAt: quarantinedAt,
    );
  }
}

/// Records every SELECT the store runs (PRAGMA reads included).
class _Selects extends QueryInterceptor {
  final List<String> sql = [];

  @override
  Future<List<Map<String, Object?>>> runSelect(
    QueryExecutor e,
    String s,
    List<Object?> a,
  ) {
    sql.add(s);
    return super.runSelect(e, s, a);
  }
}

void main() {
  late AppDatabase db;
  late SyncApplier applier;
  final remoteNode = _id('REMOTE');

  setUp(() {
    db = AppDatabase(NativeDatabase.memory());
    applier = SyncApplier(
      db,
      HlcClock(node: _id('LOCAL'), now: () => 1000),
      builder: SyncRowBuilder(db),
    );
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

  RemoteRecord clip(
    String id,
    String trackId,
    String assetId,
    Hlc h, {
    int durationMs = 0,
  }) => RemoteRecord(
    table: 'clips',
    columns: {
      'id': _id(id),
      'track_id': _id(trackId),
      'asset_id': _id(assetId),
      'start_ms': 0,
      'duration_ms': durationMs,
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

  test('a record whose id would escape the media tree is quarantined alone '
      '(ids become path components)', () async {
    // `../../x` as a practice id would later be joined into
    // `audio/<practiceId>/` and handed to recursive deletes — a hostile sync
    // record must never get a path metacharacter into the table.
    final evil = RemoteRecord(
      table: 'practices',
      columns: {
        ...practice('EVIL', 'escape', hlc(910)).columns,
        'id': '../../escape',
      },
    );
    // An FK id column is the same primitive: technique media directories are
    // keyed by the REFERENCED id.
    final evilFk = RemoteRecord(
      table: 'practices',
      columns: {
        ...practice('EVILFK', 'fk', hlc(915)).columns,
        'separation_asset_id': '../pool',
      },
    );
    final store = QuarantineStore(db);
    final parkingApplier = SyncApplier(
      db,
      HlcClock(node: _id('LOCAL'), now: () => 1000),
      builder: SyncRowBuilder(db),
      quarantine: store,
    );
    final res = await parkingApplier.apply([
      practice('GOODA', 'first', hlc(900)),
      evil,
      evilFk,
      practice('GOODB', 'second', hlc(920)),
    ]);
    expect(res.applied, 2);
    expect(res.failed, 2);
    expect((await row('GOODA'))!['title'], 'first');
    expect((await row('GOODB'))!['title'], 'second');
    final parked = await store.list();
    expect(parked, hasLength(2));
    expect(parked.map((q) => q.reason), everyElement(startsWith('invalid-id')));
    final count = await db
        .customSelect('SELECT COUNT(*) AS n FROM practices')
        .getSingle();
    expect(count.read<int>('n'), 2, reason: 'neither hostile row may land');
  });

  test(
    'an unaddressable record parks as a DEAD LETTER instead of vanishing',
    () async {
      // A record with no usable id used to be dropped silently while its page
      // committed and advanced the change token — unrecoverable, unaccounted.
      // It now parks under a synthesized content-hash key with the raw
      // evidence; the replay's decode refuses it, so it stays for review.
      final store = QuarantineStore(db);
      final parkingApplier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        builder: SyncRowBuilder(db),
        quarantine: store,
      );
      final noId = RemoteRecord(
        table: 'practices',
        columns: {...practice('X', 'x', hlc(910)).columns, 'id': ''},
      );
      final res = await parkingApplier.apply([
        practice('GOOD', 'kept', hlc(900)),
        noId,
      ]);
      expect(res.committed, isTrue);
      expect(res.applied, 1);
      expect(res.failed, 1);
      final parked = (await store.list()).single;
      expect(parked.reason, startsWith('dead-letter('));
      expect(parked.id, startsWith('deadletter'));
      // Wide enough that two distinct dead letters cannot share a row: a
      // collision here is silent LOSS, because the quarantine upsert compares
      // hlc_packed and a dead letter's is usually empty, so the second would
      // be dropped rather than stored.
      expect(parked.id.length, greaterThan(24));
      // Never re-applied: the payload is deliberately not a SyncRow envelope.
      expect(() => SyncRow.decode(parked.payload), throwsFormatException);
    },
  );

  test('two DIFFERENT dead letters park separately', () async {
    // They key off a content digest, and the quarantine upsert resolves a
    // (table, id) clash by hlc_packed — which a dead letter usually has none
    // of, so a digest collision silently discarded the second record.
    final store = QuarantineStore(db);
    final parkingApplier = SyncApplier(
      db,
      HlcClock(node: _id('LOCAL'), now: () => 1000),
      builder: SyncRowBuilder(db),
      quarantine: store,
    );
    await parkingApplier.apply([
      RemoteRecord(
        table: 'practices',
        columns: {...practice('X', 'first', hlc(910)).columns, 'id': ''},
      ),
    ]);
    await parkingApplier.apply([
      RemoteRecord(
        table: 'practices',
        columns: {...practice('Y', 'second', hlc(911)).columns, 'id': ''},
      ),
    ]);

    final parked = await store.list();
    expect(parked, hasLength(2));
    expect(parked.map((q) => q.id).toSet(), hasLength(2));
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
        builder: SyncRowBuilder(db),
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
      applier = SyncApplier(db, clock, builder: SyncRowBuilder(db));
      // Seed P so a stale copy below settles as a clean LWW skip at the
      // door (the pre-savepoint settle, which never advances the clock —
      // unchanged by the post-commit deferral).
      await applier.apply([practice('P', 'v1', hlc(950))]);
      clock.advances = 0;

      final res = await applier.apply([
        practice('A', 'applied', hlc(960)),
        practice('P', 'stale', hlc(900)), // clean LWW skip at the door
        practice('FUT', 'poison', hlc(1000 + 6 * 60 * 1000)), // quarantined
      ]);
      expect(res.applied, 1);
      expect(res.skipped, 1);
      expect(res.quarantined, 1);
      expect(
        clock.advances,
        1,
        reason:
            'exactly the applied record — a door-settled skip never '
            'merged (before or after the deferral), and a quarantined '
            'record must leave the clock alone',
      );
    });

    test('a door-parked record (schema_v newer than this build) still advances '
        'the clock AND the durable high-water mark, so a later local edit can '
        'never stamp below the parked record', () async {
      final store = QuarantineStore(db);
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        builder: SyncRowBuilder(db),
        quarantine: store,
      );
      // Plausible (one minute ahead, within drift) and from a newer build:
      // the schema gate parks it, and the replay lands it after the app
      // upgrade — the DESIGNED path for a mixed-version fleet. A local
      // edit stamped below h would then be silently reverted by that
      // replay under LWW.
      final h = hlc(1000 + 60 * 1000);
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('P', 'from a newer build', h).columns,
            'schema_v': currentRecordSchemaV + 1,
          },
        ),
      ]);
      expect(res.committed, isTrue);
      expect(res.failed, 1);
      expect((await store.list()).single.remoteHlcPacked, h.packed);

      // In-memory: the very next local stamp is causally after the parked
      // record.
      expect(
        applier.clock.tick() > h,
        isTrue,
        reason: 'a local edit must out-rank the parked record',
      );
      // Durable: the mark survives a restart even though no table row
      // carries the parked stamp for the boot seed to find.
      expect(
        await SyncStateStore(db).hlcHighWater(),
        h.packed,
        reason: 'the parked stamp must reach the durable high-water mark',
      );
    });

    test(
      'a record whose savepoint fails TRANSIENTLY (a replayable '
      'apply-failed park) advances the clock and the durable high-water '
      'mark like a door park; a deterministic CHECK failure does not',
      () async {
        final store = QuarantineStore(db);
        applier = SyncApplier(
          db,
          HlcClock(node: _id('LOCAL'), now: () => 1000),
          builder: SyncRowBuilder(db),
          quarantine: store,
        );
        // A failure no door gate can see and the CHECK / NOT NULL classifier
        // does not claim: a trigger aborting the INSERT inside the record's
        // savepoint (SQLITE_CONSTRAINT_TRIGGER) — standing in for the IO
        // pressure or racing writer the replayable apply-failed class exists
        // to retry. Its parked snapshot can win LWW at replay, so a later
        // local edit must stamp above it.
        await db.customStatement(
          'CREATE TRIGGER flaky_insert BEFORE INSERT ON practices '
          "WHEN NEW.title = 'flaky' BEGIN SELECT RAISE(ABORT, 'injected'); END",
        );
        final transient = hlc(1000 + 60 * 1000);
        // A LATER stamp whose savepoint fails deterministically: parked
        // unreplayable, it can never land, so it must ratchet nothing.
        final deterministic = hlc(1000 + 120 * 1000);
        final res = await applier.apply([
          practice('P', 'flaky', transient),
          RemoteRecord(
            table: 'assets',
            columns: {
              'id': _id('BAD'),
              'content_hash': 'HASH_BAD',
              'byte_size': -1, // CHECK (byte_size >= 0) fires in the savepoint
              'ext': 'wav',
              'kind': 'audio',
              'created_at': deterministic.wallMs,
              'updated_at': deterministic.wallMs,
              'deleted_at': null,
              'hlc_wall': deterministic.wallMs,
              'hlc_ctr': deterministic.counter,
              'hlc_node': deterministic.node,
              'hlc_packed': deterministic.packed,
              'schema_v': 1,
            },
          ),
        ]);
        expect(res.committed, isTrue);
        expect(res.failed, 2);
        final parked = {for (final q in await store.list()) q.id: q.reason};
        expect(parked[_id('P')], startsWith('apply-failed'));
        expect(parked[_id('BAD')], startsWith('invalid-value'));

        expect(
          applier.clock.tick() > transient,
          isTrue,
          reason: 'a local edit must out-rank the replayable parked record',
        );
        expect(
          applier.clock.last < deterministic,
          isTrue,
          reason: 'an unreplayable savepoint failure ratchets nothing',
        );
        expect(
          await SyncStateStore(db).hlcHighWater(),
          transient.packed,
          reason:
              'the replayable parked stamp must survive a restart, where no '
              'table row carries it for the boot seed to find',
        );
      },
    );

    test('a door park with an IMPLAUSIBLE stamp still leaves the clock alone '
        '(the drift discipline holds at every gate)', () async {
      final store = QuarantineStore(db);
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        builder: SyncRowBuilder(db),
        quarantine: store,
      );
      final before = applier.clock.last;
      // Far-future AND mistyped: the value gate parks it before the drift
      // gate ever runs, but its suspect stamp must not ratchet anything.
      final h = hlc(1000 + 6 * 60 * 1000);
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('P', 'poison', h).columns,
            'master_volume': 'loud',
          },
        ),
      ]);
      expect(res.failed, 1);
      expect((await store.list()).single.reason, startsWith('invalid-value'));
      expect(applier.clock.last, before);
      expect(await SyncStateStore(db).hlcHighWater(), isNull);
    });

    test('the post-commit advance survives a wall-clock rollback between the '
        'drift gate and the commit: the run must not report failure for a '
        'page that is already durable', () async {
      // A stamp exactly at the drift boundary: assertPlausible admits it at
      // gate time (now = 1000, reference = max(now, maxWallEverSeen) = 1000).
      var now = 1000;
      final clock = HlcClock(node: _id('LOCAL'), now: () => now);
      applier = SyncApplier(db, clock, builder: SyncRowBuilder(db));
      final stamp = hlc(1000 + HlcClock.defaultMaxDriftMs);
      final res = await applier.apply(
        [practice('P', 'vetted', stamp)],
        beforeCommit: () async {
          // The wall clock rolls BACK inside the batch's transaction — after
          // every record passed its assertPlausible gate, before the
          // post-commit advance. The check-only gate never raised
          // maxWallEverSeen, so the drift reference shrank with the clock:
          // re-running the drift check in receive() would now THROW for the
          // very stamps whose data and change token just committed.
          now = 500;
        },
      );
      expect(res.committed, isTrue, reason: 'the page is durable');
      expect(res.applied, 1);
      expect(res.failed, 0);
      expect((await row('P'))!['title'], 'vetted');
      expect(
        clock.last > stamp,
        isTrue,
        reason: 'the clock still advanced past the committed stamp',
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
    late QuarantineStore store;

    setUp(() {
      // The sweep parks orphans instead of losing them, so this group runs
      // with the persistent parking lot wired (production wiring).
      store = QuarantineStore(db);
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        builder: SyncRowBuilder(db),
        quarantine: store,
      );
    });

    test(
      'the whole-store sweep runs only for a batch that WROTE a row: a '
      'batch that only parked or skipped cannot have left one dangling',
      () async {
        final selects = _Selects();
        final counted = AppDatabase(
          NativeDatabase.memory().interceptWith(selects),
        );
        addTearDown(counted.close);
        final a = SyncApplier(
          counted,
          HlcClock(node: _id('LOCAL'), now: () => 1000),
          builder: SyncRowBuilder(counted),
          quarantine: QuarantineStore(counted),
        );
        int sweeps() =>
            selects.sql.where((s) => s.contains('foreign_key_check')).length;

        // Past the drift bound: parked, nothing written.
        final parked = await a.apply([
          practice('P', 'ahead', hlc(1000 + 6 * 60 * 1000)),
        ]);
        expect(parked.quarantined, 1);
        expect(sweeps(), 0);

        // A write keeps the sweep.
        final written = await a.apply([practice('Q', 'now', hlc(900))]);
        expect(written.applied, 1);
        expect(sweeps(), 1);

        // A clean LWW skip writes nothing either.
        final skipped = await a.apply([practice('Q', 'stale', hlc(800))]);
        expect(skipped.skipped, 1);
        expect(sweeps(), 1);
      },
    );

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
      'a true orphan whose parent never arrives is PARKED alone (fk-orphan), '
      'not lost; the rest of the batch applies',
      () async {
        final res = await applier.apply([
          practice('P', 'kept', hlc(900)),
          track('T', 'NEVER', hlc(910)), // parent not in this batch, not local
        ]);
        expect(res.applied, 1); // the good record stuck
        expect(res.failed, 1); // the orphan did not
        expect((await row('P'))!['title'], 'kept');
        // Deleted from the table (the deferred FK still requires the row
        // gone before the outer commit)...
        expect(await trackRow('T'), isNull);
        // ...but parked, never dropped: the change token advances past this
        // page, so only the quarantine replay can ever land the row again.
        final parked = await store.list();
        expect(parked, hasLength(1));
        expect(parked.single.tbl, 'tracks');
        // In the row's own sweep slot, beside (never instead of) any door
        // park of the same row — see QuarantineStore.putSwept.
        expect(parked.single.id, QuarantineStore.sweepSlotId(_id('T')));
        expect(
          parked.single.reason,
          'fk-orphan: parent practices/${_id('NEVER')} missing',
        );
      },
    );

    test('a child whose parent sits QUARANTINED (newer schema) is parked as an '
        'fk-orphan with its own HLC and full wire snapshot', () async {
      // The parent is from a newer build: the schema gate parks it, so it
      // never applies — yet the child (current schema_v) does. The child's
      // page is then behind the change token and the server never
      // re-delivers it; hard-deleting it here (the old sweep) lost it
      // forever, surfacing only after the app upgrade replayed the parent
      // from quarantine with no child to be found.
      final parent = RemoteRecord(
        table: 'practices',
        columns: {
          ...practice('P', 'from a newer build', hlc(900)).columns,
          'schema_v': currentRecordSchemaV + 1,
        },
      );
      final child = track('T', 'P', hlc(910));
      final res = await applier.apply([parent, child]);
      expect(res.applied, 0);
      expect(res.failed, 2); // the parked parent and the swept child
      expect(await row('P'), isNull);
      expect(await trackRow('T'), isNull, reason: 'deleted from the table');

      final parked = await store.list();
      expect(parked, hasLength(2));
      final parkedChild = parked.singleWhere((q) => q.tbl == 'tracks');
      expect(parkedChild.id, QuarantineStore.sweepSlotId(_id('T')));
      expect(
        parkedChild.reason,
        'fk-orphan: parent practices/${_id('P')} missing',
      );
      // The snapshot carries the record's OWN HLC (not a fresh local
      // stamp): a later, newer state of the same row must still be able to
      // settle the parked one by plain LWW.
      expect(parkedChild.remoteHlcPacked, hlc(910).packed);
      expect(parkedChild.schemaV, currentRecordSchemaV);
      // The payload is the full wire snapshot, replayable verbatim — the
      // shared outbound projection, not a hand-written second one.
      final replayable = SyncRow.decode(parkedChild.payload);
      expect(replayable.table, 'tracks');
      expect(replayable.fields, equals(child.columns));
    });

    test('a CHECK-violating payload parks under the unreplayable invalid-value '
        'class, not as a replayable apply-failed', () async {
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
      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(
        parked.single.reason,
        startsWith('invalid-value:'),
        reason:
            'a constraint verdict is a pure function of the payload — its '
            'replay re-parks forever, so it must not sit in a replayable '
            'class where it can displace the FK sweep\'s only copy of a row',
      );
    });

    test('a non-finite REAL is refused at the value gate (an invalid-value '
        'dead letter), not landed as a row no repository can hash or the wire '
        'can encode — and the page still commits', () async {
      // JSON's 1e999 decodes to Infinity. Landed, it made the practice
      // unloadable (content hashing refuses non-finite doubles) and, as an
      // FK orphan, its sweep snapshot could not be encoded — the page rolled
      // back and the same page failed every pull from then on.
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('P', 'loud', hlc(900)).columns,
            'master_volume': double.infinity,
          },
        ),
        practice('Q', 'kept', hlc(901)),
        RemoteRecord(
          table: 'tracks',
          columns: {
            ...track('T', 'NEVER', hlc(910)).columns,
            'volume': double.negativeInfinity,
          },
        ),
      ]);
      expect(res.committed, isTrue, reason: 'the page must still commit');
      expect(res.applied, 1);
      expect(res.failed, 2);
      expect(await row('P'), isNull);
      expect((await row('Q'))!['title'], 'kept');
      expect(await trackRow('T'), isNull);
      // Refused by the value gate, and parked as dead letters: the payload
      // cannot be JSON-encoded under its own id, so the evidence is kept
      // spelled out instead (unreplayable either way).
      final parked = await store.list();
      expect(parked, hasLength(2));
      for (final q in parked) {
        expect(q.reason, startsWith('dead-letter(invalid-value:'));
        expect(q.payload, contains('Infinity'));
      }
    });

    test('a CHECK-violating newer state cannot void the FK sweep\'s park — the '
        'row\'s only surviving copy', () async {
      // The audited black hole: an honest clip C@H1 (asset not yet
      // delivered) plus a CHECK-violating update C@H2 in one page. H2 fails
      // its savepoint and parks; the end-of-batch FK sweep then parks and
      // deletes C@H1. The sweep's park must survive — it holds the only copy
      // of applied data, while H2 can never re-apply anywhere.
      final h1 = hlc(910);
      final res = await applier.apply([
        practice('P', 'kept', hlc(900)),
        track('T', 'P', hlc(905)),
        clip('C', 'T', 'NOASSET', h1),
        clip('C', 'T', 'NOASSET', hlc(920), durationMs: -1),
      ]);
      expect(res.failed, greaterThanOrEqualTo(1));
      final parked = await store.list();
      final c = parked.singleWhere(
        (p) => p.id == QuarantineStore.sweepSlotId(_id('C')),
      );
      expect(
        c.reason,
        startsWith('fk-orphan'),
        reason:
            'the fk-orphan park is the only copy of the applied row; a '
            'permanently-unreplayable newer state must not hold the slot',
      );
      expect(
        c.remoteHlcPacked,
        h1.packed,
        reason: 'the preserved state must be the applied H1 content',
      );
    });

    test(
      'an orphan violating TWO foreign keys is parked + counted once',
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
        // One parked entry, not one per violated constraint (the second
        // foreign_key_check entry finds the row already deleted).
        final parked = await store.list();
        expect(parked, hasLength(1));
        expect(parked.single.id, QuarantineStore.sweepSlotId(_id('C')));
        expect(parked.single.reason, startsWith('fk-orphan'));
      },
    );

    RemoteRecord assetRecord(String id, Hlc h) => RemoteRecord(
      table: 'assets',
      columns: {
        'id': _id(id),
        'content_hash': 'HASH_$id',
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
    );

    Future<Map<String, Object?>> practiceState(String id) async {
      final r = await db
          .customSelect(
            'SELECT title, separation_asset_id, hlc_packed '
            'FROM practices WHERE id = ?',
            variables: [Variable.withString(_id(id))],
          )
          .getSingle();
      return r.data;
    }

    Future<Map<String, Object?>> clipState(String id) async {
      final r = await db
          .customSelect(
            'SELECT asset_id, hlc_packed FROM clips WHERE id = ?',
            variables: [Variable.withString(_id(id))],
          )
          .getSingle();
      return r.data;
    }

    // Mimics SyncEngine._replayQuarantine at applier level: parent tables
    // first (the same parent-before-child contract tableRanks derives), one
    // single-record apply per parked row, entry deleted when the record
    // applied or settled as a stale LWW skip.
    Future<void> replayParked() async {
      const rank = ['assets', 'practices', 'tracks', 'clips'];
      final parked = (await store.list()).toList()
        ..sort((a, b) => rank.indexOf(a.tbl).compareTo(rank.indexOf(b.tbl)));
      for (final q in parked) {
        final res = await applier.apply([SyncRow.decode(q.payload)]);
        if (res.applied > 0 || res.skipped > 0) {
          await store.delete(q.tbl, q.id);
        }
      }
    }

    test('park-and-restore: a remote UPDATE repointing a pre-existing row at '
        'a QUARANTINED parent parks the post-update snapshot and restores '
        'the row verbatim; replay converges once the parent lands', () async {
      // The parent is locally quarantined — here by an implausibly-future
      // stamp, the same locally-parked mechanics as a newer-schema parent
      // (which a test cannot un-park, since currentRecordSchemaV is a
      // build-time const, while a drift park recovers once the clock
      // catches up). Either way the child's page ends up behind the change
      // token, the server never re-delivers it, and only the parking lot
      // can converge the pair.
      var now = 1000;
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => now),
        builder: SyncRowBuilder(db),
        quarantine: store,
      );
      await applier.apply([practice('P', 'v1', hlc(900))]);

      final parentStamp = hlc(1000 + HlcClock.defaultMaxDriftMs + 60 * 1000);
      final res = await applier.apply([
        assetRecord('A', parentStamp), // quarantined at the drift gate
        practice('P', 'v2', hlc(950), separationAssetId: 'A'),
      ]);
      expect(res.committed, isTrue);
      expect(res.quarantined, 1); // the parent
      expect(res.applied, 0, reason: 'the update applied but did not stick');
      expect(res.failed, 1); // the parked update

      // The row was restored VERBATIM to its exact pre-batch state: pointer
      // intact (null), prior content, prior HLC — a pure revert, no derived
      // stamp minted. NOT the old heal (update kept, pointer nulled, remote
      // HLC adopted).
      final p = await practiceState('P');
      expect(p['title'], 'v1');
      expect(p['separation_asset_id'], isNull);
      expect(p['hlc_packed'], hlc(900).packed);

      final parked = await store.list();
      expect(parked, hasLength(2));
      final parkedUpdate = parked.singleWhere((q) => q.tbl == 'practices');
      expect(parkedUpdate.id, QuarantineStore.sweepSlotId(_id('P')));
      expect(
        parkedUpdate.reason,
        'fk-orphan: parent assets/${_id('A')} missing',
      );
      // The parked snapshot is the POST-update state under the remote HLC:
      // strictly newer than the restored row, so replay wins by plain LWW.
      expect(parkedUpdate.remoteHlcPacked, hlc(950).packed);
      final replayable = SyncRow.decode(parkedUpdate.payload);
      expect(replayable.fields['title'], 'v2');
      expect(replayable.fields['separation_asset_id'], _id('A'));

      // The clock catches up (the drift analogue of the app upgrade that
      // un-parks a newer-schema parent) and the replay runs: the parent
      // lands first, then the parked update — BOTH land, the pointer is
      // correct, and the parking lot is empty.
      now = parentStamp.wallMs;
      await replayParked();
      final healed = await practiceState('P');
      expect(healed['title'], 'v2');
      expect(healed['separation_asset_id'], _id('A'));
      expect(healed['hlc_packed'], hlc(950).packed);
      expect(await store.list(), isEmpty);
    });

    test('a re-delivered IDENTICAL update is strictly newer than the '
        'restored row: it re-applies and re-parks instead of the old '
        'permanently-nulled pointer', () async {
      await applier.apply([practice('P', 'v1', hlc(900))]);
      final update = practice('P', 'v2', hlc(950), separationAssetId: 'A');

      final first = await applier.apply([update]);
      expect(first.applied, 0);
      expect(first.failed, 1);
      expect((await practiceState('P'))['hlc_packed'], hlc(900).packed);

      // The server re-delivers the identical snapshot (e.g. a token reset
      // replays the zone). Under the old null-the-pointer heal the row held
      // the update's OWN HLC with the pointer nulled, so the re-delivery
      // was LWW-EQUAL and skipped — the reference was lost forever unless
      // some strictly-newer update ever arrived. Against the RESTORED row
      // (prior HLC) it is strictly newer: it re-applies and is parked and
      // restored again, converging on the same recoverable state.
      final second = await applier.apply([update]);
      expect(second.skipped, 0, reason: 'not LWW-equal to the restored row');
      expect(second.applied, 0);
      expect(second.failed, 1);
      final p = await practiceState('P');
      expect(p['title'], 'v1');
      expect(p['separation_asset_id'], isNull);
      expect(p['hlc_packed'], hlc(900).packed);
      final parked = await store.list();
      expect(parked, hasLength(1)); // upserted, not piled up
      expect(parked.single.remoteHlcPacked, hlc(950).packed);

      // Once the parent exists, the replay converges to the updated state.
      final landed = await applier.apply([assetRecord('A', hlc(960))]);
      expect(landed.applied, 1);
      await replayParked();
      final healed = await practiceState('P');
      expect(healed['title'], 'v2');
      expect(healed['separation_asset_id'], _id('A'));
      expect(healed['hlc_packed'], hlc(950).packed);
      expect(await store.list(), isEmpty);
    });

    test('a dangling NOT NULL FK on an updated-this-batch row COMMITS the '
        'batch (the token seam runs) via the same park-and-restore path; '
        'replay converges after the parent lands', () async {
      await applier.apply([
        practice('P', 'p', hlc(900)),
        track('T', 'P', hlc(901)),
        assetRecord('A1', hlc(902)),
        clip('C', 'T', 'A1', hlc(903)),
      ]);

      // clips.asset_id is NOT NULL: the old sweep could only abort, and
      // with the parent locally quarantined (never re-delivered) the engine
      // retried the same page forever, until an app upgrade. The restore
      // heals every repointed column at once, so the NOT NULL case rides
      // the same path as the nullable one.
      var tokenWrites = 0;
      final res = await applier.apply(
        [clip('C', 'T', 'A2', hlc(950))], // A2 never arrived
        beforeCommit: () async => tokenWrites++,
      );
      expect(res.committed, isTrue, reason: 'no permanent page retry');
      expect(tokenWrites, 1, reason: 'the change token advances past the page');
      expect(res.applied, 0);
      expect(res.failed, 1);

      final c = await clipState('C');
      expect(c['asset_id'], _id('A1'), reason: 'restored, not aborted');
      expect(c['hlc_packed'], hlc(903).packed, reason: 'verbatim prior HLC');

      final parked = await store.list();
      expect(parked, hasLength(1));
      expect(parked.single.tbl, 'clips');
      expect(
        parked.single.reason,
        'fk-orphan: parent assets/${_id('A2')} missing',
      );

      // The missing asset lands (a later page, or an upgrade replay); the
      // parked repoint then converges by plain LWW.
      await applier.apply([assetRecord('A2', hlc(960))]);
      await replayParked();
      final healed = await clipState('C');
      expect(healed['asset_id'], _id('A2'));
      expect(healed['hlc_packed'], hlc(950).packed);
      expect(await store.list(), isEmpty);
    });

    test('a dangling pre-existing row this batch never touched keeps the '
        'old null-the-pointer heal', () async {
      await applier.apply([practice('P', 'v1', hlc(900))]);
      // No remote apply can produce this state any more (every dangling
      // pre-existing row is one the batch just updated), so manufacture the
      // residual case directly: a pointer left dangling outside the
      // applier's control.
      await db.customStatement('PRAGMA foreign_keys = OFF');
      await db.customStatement(
        'UPDATE practices SET separation_asset_id = ? WHERE id = ?',
        [_id('GONE'), _id('P')],
      );
      await db.customStatement('PRAGMA foreign_keys = ON');

      // Any batch triggers the sweep (foreign_key_check scans the whole
      // database). P is in neither the inserted nor the updated set, so
      // only its dangling column is nulled — no park, no restore, and no
      // counter moves.
      final res = await applier.apply([practice('Q', 'other', hlc(950))]);
      expect(res.committed, isTrue);
      expect(res.applied, 1);
      expect(res.failed, 0);
      final p = await practiceState('P');
      expect(p['title'], 'v1', reason: 'row kept');
      expect(p['separation_asset_id'], isNull, reason: 'pointer nulled');
      expect(await store.list(), isEmpty);
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
        builder: SyncRowBuilder(db),
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

    test('a mistyped HLC quad is parked with reason invalid-value (any '
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
      // The column gate names the offending column at the door, instead of
      // letting the mistyped value fail deep inside apply as a generic
      // 'apply-failed' with nothing to act on.
      expect(parked.single.reason, startsWith('invalid-value'));
    });

    test('a value that could not BIND to its column is parked with the '
        'column named, not left to fail during hydration', () async {
      // SQLite's dynamic typing takes a string into an INTEGER column without
      // complaint, so the failure used to surface far away — drift hydrating
      // the row and throwing inside a view model — with nothing to act on.
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('BAD', 'x', hlc(910)).columns,
            'timeline_duration_ms': 'not a number',
          },
        ),
      ]);

      expect(res.failed, 1);
      expect(await row('BAD'), isNull);
      final parked = (await store.list()).single;
      expect(parked.reason, contains('timeline_duration_ms'));
    });

    test('valid JSON of the WRONG shape is parked at the door, not left to '
        'break the practice when it is opened', () async {
      // '{}' parses, binds to a TEXT column and commits — and then the
      // hydrator's `jsonDecode(...) as List` throws inside a view model: the
      // practice disappears from the list and cannot be opened, with nothing
      // parked to act on. The door runs the real decoder, so what commits
      // hydrates.
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('BAD', 'x', hlc(910)).columns,
            'lyrics_json': '{}',
          },
        ),
      ]);

      expect(res.failed, 1);
      expect(await row('BAD'), isNull);
      final parked = (await store.list()).single;
      expect(parked.reason, startsWith('invalid-value'));
      expect(parked.reason, contains('lyrics_json'));
    });

    test('a well-formed JSON body still applies', () async {
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('GOOD', 'x', hlc(910)).columns,
            'lyrics_json': '[{"text":"line one","timeMs":120}]',
            'reverb_json':
                '{"roomSize":0.4,"damping":0.5,"wet":0.2,'
                '"width":1.0}',
          },
        ),
      ]);
      expect(res.applied, 1);
      expect(await store.list(), isEmpty);
    });

    test('a JSON array whose ELEMENTS are wrong is parked too', () async {
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('BAD2', 'x', hlc(910)).columns,
            'lyrics_json': '["just a string"]',
          },
        ),
      ]);
      expect(res.failed, 1);
      expect((await store.list()).single.reason, contains('lyrics_json'));
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

    test('a quarantine storage failure fails the WHOLE batch — the token '
        'seam never runs, nothing applies — and the same page succeeds once '
        'the store recovers', () async {
      final flaky = _FlakyQuarantineStore(db);
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        builder: SyncRowBuilder(db),
        quarantine: flaky,
      );
      var tokenWrites = 0;
      final page = [
        practice('OK', 'fine', hlc(900)),
        // Parked at the drift gate — the failing put is what must not be
        // swallowed: a committed batch would advance the token past a
        // record that was never parked, losing it permanently.
        practice('FUT', 'parked', hlc(1000 + 6 * 60 * 1000)),
      ];
      final res = await applier.apply(
        page,
        beforeCommit: () async => tokenWrites++,
      );
      expect(res.committed, isFalse);
      expect(tokenWrites, 0, reason: 'the token seam must not run');
      expect(await row('OK'), isNull, reason: 'the whole page rolled back');
      expect(await flaky.list(), isEmpty);

      // The store recovers: the engine retries the SAME page (its token
      // never moved) and it now applies cleanly, park included.
      flaky.failing = false;
      final retry = await applier.apply(
        page,
        beforeCommit: () async => tokenWrites++,
      );
      expect(retry.committed, isTrue);
      expect(tokenWrites, 1);
      expect(retry.applied, 1);
      expect(retry.quarantined, 1);
      expect((await row('OK'))!['title'], 'fine');
      final parked = await flaky.list();
      expect(parked, hasLength(1));
      expect(parked.single.id, _id('FUT'));
      expect(parked.single.reason, startsWith('hlc-drift'));
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

  group('slot-resolution revert (a park undoes the resolution it rode in '
      'on)', () {
    late QuarantineStore store;

    setUp(() {
      store = QuarantineStore(db);
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => 1000),
        builder: SyncRowBuilder(db),
        quarantine: store,
      );
    });

    // A single-slot track (accompaniment) with a routable bus pointer — the
    // FK the sweep trips over when the bus parent never lands.
    RemoteRecord accTrack(
      String id,
      String practiceId,
      Hlc h, {
      String? busId,
    }) => RemoteRecord(
      table: 'tracks',
      columns: {
        'id': _id(id),
        'practice_id': _id(practiceId),
        'kind': 'accompaniment',
        'interpretation_id': null,
        'display_name': '',
        'color_value': null,
        'muted': 0,
        'soloed': 0,
        'volume': 1.0,
        'pan': 0.0,
        'bus_id': busId == null ? null : _id(busId),
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

    RemoteRecord fxBus(String id, String practiceId, Hlc h, {int? schemaV}) =>
        RemoteRecord(
          table: 'fx_buses',
          columns: {
            'id': _id(id),
            'practice_id': _id(practiceId),
            'name': 'bus',
            'color_value': null,
            'volume': 1.0,
            'pan': 0.0,
            'muted': 0,
            'soloed': 0,
            'output_bus_id': null,
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
            'schema_v': schemaV ?? 1,
          },
        );

    Future<Map<String, Object?>?> trackState(String id) async {
      final r = await db
          .customSelect(
            'SELECT practice_id, kind, deleted_at, hlc_packed, bus_id '
            'FROM tracks WHERE id = ?',
            variables: [Variable.withString(_id(id))],
          )
          .getSingleOrNull();
      return r?.data;
    }

    Future<List<Map<String, Object?>>> oplogRows() async => [
      for (final r
          in await db
              .customSelect(
                'SELECT entity_id, op, hlc_packed FROM oplog ORDER BY seq',
              )
              .get())
        r.data,
    ];

    // The derived stamp a slot resolution writes: one counter above the
    // loser's own stamp, under the resolving device's DERIVED node (which
    // sorts below every real node, so bookkeeping loses (wall, counter)
    // ties to real edits — see HlcClock.derivedNode).
    // The derived tombstone a slot resolution writes: the loser's own stamp
    // with the node extended by this device — same (wall, counter), so a
    // parked node-space winner's replay still out-ranks it (HlcClock.derived).
    Hlc derivedAt(int wall) => HlcClock.derived(hlc(wall), _id('LOCAL'));

    test('park+delete: an inserted record that WON the slot (its bus parent '
        'quarantined, newer schema) takes the occupant tombstone and its '
        'journal entry back out with it; the replayed record kills the '
        'occupant only when it actually lands', () async {
      await applier.apply([
        practice('P', 'p', hlc(800)),
        accTrack('O', 'P', hlc(850)), // the current slot occupant
      ]);
      expect(await oplogRows(), isEmpty);

      // The bus parent is from a newer build (parked at the schema gate);
      // the incoming accompaniment routes into it AND wins the (P, acc)
      // slot, so applying it tombstones + journals the occupant BEFORE the
      // sweep discovers the record itself cannot stay.
      final incoming = accTrack('T', 'P', hlc(900), busId: 'B');
      final res = await applier.apply([
        fxBus('B', 'P', hlc(890), schemaV: currentRecordSchemaV + 1),
        incoming,
      ]);
      expect(res.committed, isTrue);
      expect(res.applied, 0);
      expect(res.failed, 2); // the parked bus and the swept track

      // The occupant is fully restored — live, prior HLC, no track killed
      // for a record that never landed...
      final occupant = (await trackState('O'))!;
      expect(occupant['deleted_at'], isNull);
      expect(occupant['hlc_packed'], hlc(850).packed);
      // ...and no oplog entry from the resolution remains.
      expect(await oplogRows(), isEmpty);
      // The incoming record itself was quarantined at the door: row gone,
      // parked under its own HLC with its original payload.
      expect(await trackState('T'), isNull);
      final parked = (await store.list()).singleWhere((q) => q.tbl == 'tracks');
      expect(parked.remoteHlcPacked, hlc(900).packed);
      expect(parked.reason, 'fk-orphan: parent fx_buses/${_id('B')} missing');
      final payload = SyncRow.decode(parked.payload);
      expect(payload.fields, equals(incoming.columns));

      // The parent lands; the replayed record applies — and only THEN does
      // the occupant lose the slot, with a journaled tombstone.
      await applier.apply([fxBus('B', 'P', hlc(905))]);
      final replay = await applier.apply([payload]);
      expect(replay.applied, 1);
      expect((await trackState('T'))!['deleted_at'], isNull);
      final lost = (await trackState('O'))!;
      expect(lost['deleted_at'], isNotNull);
      final derived = derivedAt(850);
      expect(lost['hlc_packed'], derived.packed);
      final journal = await oplogRows();
      expect(journal, hasLength(1));
      expect(journal.single['entity_id'], _id('O'));
      expect(journal.single['op'], 'tombstone');
      expect(journal.single['hlc_packed'], derived.packed);
    });

    test('park+restore: an UPDATED pre-existing track that won the slot is '
        'parked and reverted along with the occupant tombstone — the journal '
        'stays consistent with the restored rows', () async {
      await applier.apply([
        practice('P', 'p', hlc(800)),
        practice('Q', 'q', hlc(801)),
        accTrack('O', 'P', hlc(850)), // occupies (P, accompaniment)
        accTrack('T', 'Q', hlc(820)), // pre-exists in Q's slot
      ]);
      expect(await oplogRows(), isEmpty);

      // The incoming update MOVES T into P's slot (winning it, 900 > 850)
      // while also routing it into a bus from a newer build — the update
      // itself plants the dangling pointer that parks it.
      final incoming = accTrack('T', 'P', hlc(900), busId: 'B');
      final res = await applier.apply([
        fxBus('B', 'P', hlc(890), schemaV: currentRecordSchemaV + 1),
        incoming,
      ]);
      expect(res.committed, isTrue);
      expect(res.applied, 0);
      expect(res.failed, 2);

      // The updated row is restored verbatim to its pre-batch state...
      final t = (await trackState('T'))!;
      expect(t['practice_id'], _id('Q'));
      expect(t['deleted_at'], isNull);
      expect(t['bus_id'], isNull);
      expect(t['hlc_packed'], hlc(820).packed);
      // ...the occupant is restored live on its own prior stamp...
      final occupant = (await trackState('O'))!;
      expect(occupant['deleted_at'], isNull);
      expect(occupant['hlc_packed'], hlc(850).packed);
      // ...and the journal carries no stray resolution entries for either.
      expect(await oplogRows(), isEmpty);
      // The post-update snapshot is parked under the remote HLC (strictly
      // newer than both restored rows, so replay wins by plain LWW).
      final parked = (await store.list()).singleWhere((q) => q.tbl == 'tracks');
      expect(parked.remoteHlcPacked, hlc(900).packed);

      // Convergence: the parent lands and the replay takes the slot for
      // real — occupant tombstoned, resolution journaled, exactly once.
      await applier.apply([fxBus('B', 'P', hlc(905))]);
      final replay = await applier.apply([SyncRow.decode(parked.payload)]);
      expect(replay.applied, 1);
      expect((await trackState('T'))!['practice_id'], _id('P'));
      expect((await trackState('O'))!['deleted_at'], isNotNull);
      final journal = await oplogRows();
      expect(journal, hasLength(1));
      expect(journal.single['entity_id'], _id('O'));
      expect(journal.single['op'], 'tombstone');
    });

    test('a parked record that LOST the slot parks its ORIGINAL payload, '
        'not the tombstoned-at-the-door state whose journal entry the '
        'revert just deleted', () async {
      await applier.apply([
        practice('P', 'p', hlc(800)),
        accTrack('O', 'P', hlc(900)), // newer than the incoming record
      ]);

      final incoming = accTrack('T', 'P', hlc(850), busId: 'B');
      final res = await applier.apply([
        fxBus('B', 'P', hlc(890), schemaV: currentRecordSchemaV + 1),
        incoming,
      ]);
      expect(res.applied, 0);
      expect(res.failed, 2);

      // The occupant was never touched (the incoming record lost) and no
      // journal entry survives for the at-the-door tombstone.
      final occupant = (await trackState('O'))!;
      expect(occupant['deleted_at'], isNull);
      expect(occupant['hlc_packed'], hlc(900).packed);
      expect(await oplogRows(), isEmpty);
      expect(await trackState('T'), isNull);
      // Parked under the record's OWN hlc with the original live payload:
      // parking the derived tombstone state would later replay an
      // UN-journaled tombstone no peer ever learns about.
      final parked = (await store.list()).singleWhere((q) => q.tbl == 'tracks');
      expect(parked.remoteHlcPacked, hlc(850).packed);
      final payload = SyncRow.decode(parked.payload);
      expect(payload.fields, equals(incoming.columns));

      // Replay after the parent lands: the resolution re-runs from scratch,
      // the record still loses, and THIS time the tombstone is journaled.
      await applier.apply([fxBus('B', 'P', hlc(905))]);
      final replay = await applier.apply([payload]);
      expect(replay.applied, 1);
      expect((await trackState('T'))!['deleted_at'], isNotNull);
      expect((await trackState('O'))!['deleted_at'], isNull);
      final journal = await oplogRows();
      expect(journal, hasLength(1));
      expect(journal.single['entity_id'], _id('T'));
      expect(journal.single['op'], 'tombstone');
    });

    test('a record that tombstoned TWO successive occupants and is then '
        'parked reverts them as INDEPENDENT pairs: the latest occupant is '
        'restored with its journal entry deleted, the earlier one stays '
        'tombstoned WITH its journal entry — never tombstoned with a '
        'deleted journal — and the batch commits', () async {
      await applier.apply([
        practice('P', 'p', hlc(800)),
        accTrack('O1', 'P', hlc(850)),
      ]);
      expect(await oplogRows(), isEmpty);

      // One page: T@900 (bus parent never arrives) beats O1; O2@950 beats
      // T; T@980 beats O2 — T's revert now carries two occupant pairs.
      // Folding them into one (the FIRST snapshot under the LAST derived
      // stamp) used to restore NEITHER occupant while deleting BOTH
      // journal entries: two tracks tombstoned with their ledger gone.
      final incoming = accTrack('T', 'P', hlc(980), busId: 'B');
      final res = await applier.apply([
        accTrack('T', 'P', hlc(900), busId: 'B'),
        accTrack('O2', 'P', hlc(950)),
        incoming,
      ]);
      expect(res.committed, isTrue);
      expect(res.applied, 2);
      expect(res.failed, 1);

      // The LATEST occupant (O2) is restored verbatim: live, prior HLC —
      // exactly the state a peer that never saw T derives (O2 beats O1).
      final o2 = (await trackState('O2'))!;
      expect(o2['deleted_at'], isNull);
      expect(o2['hlc_packed'], hlc(950).packed);
      // The EARLIER occupant (O1) is not restorable — the restored O2 now
      // holds the slot — so its pair is kept WHOLE: still tombstoned AND
      // still journaled, an accounted tombstone peers learn.
      final o1 = (await trackState('O1'))!;
      expect(o1['deleted_at'], 900); // the winner's wall when T claimed it
      expect(o1['hlc_packed'], derivedAt(850).packed);
      expect(await trackState('T'), isNull);
      // Journal: O1's entry survives with its tombstone; O2's went with its
      // restore. The T entry was minted by O2's OWN resolution (O2 landed,
      // its ledger stands); with T's row gone it is a harmless dirty flag —
      // the pusher pushes CURRENT row state and skips missing rows.
      final journal = await oplogRows();
      expect(journal, hasLength(2));
      expect(journal[0]['entity_id'], _id('O1'));
      expect(journal[0]['hlc_packed'], derivedAt(850).packed);
      expect(journal[1]['entity_id'], _id('T'));
      expect(journal[1]['hlc_packed'], derivedAt(900).packed);
      // T parked under its own latest HLC with the ORIGINAL payload.
      final parked = (await store.list()).singleWhere((q) => q.tbl == 'tracks');
      expect(parked.remoteHlcPacked, hlc(980).packed);
      expect(SyncRow.decode(parked.payload).fields, equals(incoming.columns));

      // Convergence: the parent lands and the replay takes the slot for
      // real — O2 loses it with a journaled tombstone, exactly once.
      await applier.apply([fxBus('B', 'P', hlc(985))]);
      final replay = await applier.apply([SyncRow.decode(parked.payload)]);
      expect(replay.applied, 1);
      expect((await trackState('T'))!['deleted_at'], isNull);
      final o2After = (await trackState('O2'))!;
      expect(o2After['deleted_at'], 980);
      expect(o2After['hlc_packed'], derivedAt(950).packed);
      expect((await trackState('O1'))!['hlc_packed'], derivedAt(850).packed);
      final after = await oplogRows();
      expect(after, hasLength(3));
      expect(after[2]['entity_id'], _id('O2'));
      expect(after[2]['hlc_packed'], derivedAt(950).packed);
    });

    group('a later in-page winner holds the slot when the revert runs (a '
        'restore must not trip ux_track_slot and brick the page)', () {
      test('inserted winner parked after a later winner took the slot: the '
          'occupant pair is skipped whole and the page commits', () async {
        await applier.apply([
          practice('P', 'p', hlc(800)),
          accTrack('O', 'P', hlc(850)),
        ]);

        // T (bus parent never arrives) beats O; W then beats T. At revert
        // time W holds the slot live, so restoring O would violate
        // ux_track_slot — the old unconditional restore failed the OUTER
        // commit and retried this same page forever (token never advanced).
        final res = await applier.apply([
          accTrack('T', 'P', hlc(900), busId: 'B'),
          accTrack('W', 'P', hlc(950)),
        ]);
        expect(res.committed, isTrue);
        expect(res.applied, 1);
        expect(res.failed, 1);

        final w = (await trackState('W'))!;
        expect(w['deleted_at'], isNull);
        expect(w['hlc_packed'], hlc(950).packed);
        // O's pair was skipped WHOLE: tombstoned AND journaled (peers still
        // learn an accounted tombstone), never one without the other.
        final o = (await trackState('O'))!;
        expect(o['deleted_at'], 900);
        expect(o['hlc_packed'], derivedAt(850).packed);
        expect(await trackState('T'), isNull);
        final journal = await oplogRows();
        expect(journal, hasLength(2));
        expect(journal[0]['entity_id'], _id('O'));
        expect(journal[0]['hlc_packed'], derivedAt(850).packed);
        // T's tombstone entry belongs to W's resolution (W landed, so its
        // ledger stands); the parked snapshot below carries exactly that
        // journaled state — not an un-journaled ghost.
        expect(journal[1]['entity_id'], _id('T'));
        expect(journal[1]['hlc_packed'], derivedAt(900).packed);
        final parked = (await store.list()).singleWhere(
          (q) => q.tbl == 'tracks',
        );
        expect(parked.remoteHlcPacked, derivedAt(900).packed);
        expect(SyncRow.decode(parked.payload).fields['deleted_at'], 950);

        // Convergence: the parent lands and the replay applies the
        // tombstoned state as-is (no resolution runs for a dead record).
        await applier.apply([fxBus('B', 'P', hlc(985))]);
        final replay = await applier.apply([SyncRow.decode(parked.payload)]);
        expect(replay.applied, 1);
        expect((await trackState('T'))!['deleted_at'], 950);
        expect((await trackState('W'))!['deleted_at'], isNull);
        expect(await oplogRows(), hasLength(2));
      });

      test('updated winner parked after a later winner took the slot: the '
          'prior row restores verbatim into its own (free) slot, the '
          'occupant pair is skipped whole, and the page commits', () async {
        await applier.apply([
          practice('P', 'p', hlc(800)),
          practice('Q', 'q', hlc(801)),
          accTrack('O', 'P', hlc(850)),
          accTrack('T', 'Q', hlc(820)), // pre-exists in Q's slot
        ]);

        // The update MOVES T into P's slot (beating O) with a dangling bus;
        // W then beats T. The old code restored O under the live W — brick.
        final res = await applier.apply([
          accTrack('T', 'P', hlc(900), busId: 'B'),
          accTrack('W', 'P', hlc(950)),
        ]);
        expect(res.committed, isTrue);
        expect(res.applied, 1);
        expect(res.failed, 1);

        // T is back verbatim in Q's slot (free, so no index hazard)...
        final t = (await trackState('T'))!;
        expect(t['practice_id'], _id('Q'));
        expect(t['deleted_at'], isNull);
        expect(t['bus_id'], isNull);
        expect(t['hlc_packed'], hlc(820).packed);
        // ...W holds P's slot, and O's pair was skipped whole.
        expect((await trackState('W'))!['deleted_at'], isNull);
        final o = (await trackState('O'))!;
        expect(o['deleted_at'], 900);
        expect(o['hlc_packed'], derivedAt(850).packed);
        final journal = await oplogRows();
        expect(journal, hasLength(2));
        expect(journal[0]['entity_id'], _id('O'));
        // W's resolution entry for T outlived the verbatim restore: a
        // harmless dirty flag — the pusher pushes CURRENT row state (T@820,
        // state the fleet already holds), never the stale stamp.
        expect(journal[1]['entity_id'], _id('T'));
        expect(journal[1]['hlc_packed'], derivedAt(900).packed);
        final parked = (await store.list()).singleWhere(
          (q) => q.tbl == 'tracks',
        );
        expect(parked.remoteHlcPacked, derivedAt(900).packed);

        // Convergence: the parent lands; the parked (slot-lost, tombstoned,
        // journal-backed) state out-ranks the restored row and lands as-is.
        await applier.apply([fxBus('B', 'P', hlc(985))]);
        final replay = await applier.apply([SyncRow.decode(parked.payload)]);
        expect(replay.applied, 1);
        final after = (await trackState('T'))!;
        expect(after['practice_id'], _id('P'));
        expect(after['deleted_at'], 950);
        expect(after['hlc_packed'], derivedAt(900).packed);
        expect((await trackState('W'))!['deleted_at'], isNull);
        expect(await oplogRows(), hasLength(2));
      });

      test('a NODE-ONLY winner parked at the door still out-ranks the restore '
          'tombstone, so its replay lands (the counter-bump derivation '
          'buried it forever)', () async {
        // X's update beats the prior state by NODE alone — same
        // (wall, counter), bigger node. The restore tombstone derives from
        // the prior (850, 0, REMOTE…): under the old counter+1 scheme that
        // was (850, 1, …), OUT-RANKING the parked update — its replay then
        // settled as an LWW skip and the update was lost forever. Node-space
        // derivation stays at (850, 0, REMOTE…!LOCAL…), strictly between
        // the prior and the winner.
        await applier.apply([
          practice('P', 'p', hlc(800)),
          accTrack('X', 'P', hlc(850)), // prior: (850, 0, REMOTE…)
        ]);
        final nodeWin = Hlc(
          wallMs: 850,
          counter: 0,
          node: _id('ZREMOTE'), // > REMOTE… in node space alone
        );
        final res = await applier.apply([
          accTrack('X', 'P', nodeWin, busId: 'B'), // dangling bus -> parked
          accTrack('W', 'P', hlc(950)), // a later winner takes the slot
        ]);
        expect(res.committed, isTrue);
        expect(res.failed, 1);

        // The restored row is tombstoned under the derived stamp…
        final x = (await trackState('X'))!;
        expect(x['deleted_at'], 950);
        expect(x['hlc_packed'], derivedAt(850).packed);
        // …and the parked update STILL out-ranks it. This comparison is the
        // whole regression: counter-bump derivation reversed it.
        final parked = (await store.list()).singleWhere(
          (q) => q.tbl == 'tracks',
        );
        expect(
          parked.remoteHlcPacked.compareTo(x['hlc_packed']! as String),
          greaterThan(0),
          reason: 'bookkeeping must never out-rank the state it parked',
        );

        // The parent lands; the replay must LAND, not settle as a skip.
        await applier.apply([fxBus('B', 'P', hlc(985))]);
        final replay = await applier.apply([SyncRow.decode(parked.payload)]);
        expect(replay.applied, 1);
        expect(replay.skipped, 0);
        final after = (await trackState('X'))!;
        expect(after['hlc_packed'], parked.remoteHlcPacked);
      });

      test('prior-row restore hazard: the pre-batch snapshot of the parked '
          'update would land in a slot a later winner now holds — it is '
          'restored TOMBSTONED under a derived stamp and journaled, and the '
          'page commits', () async {
        await applier.apply([
          practice('P', 'p', hlc(800)),
          accTrack('X', 'P', hlc(850)), // the slot's original occupant
        ]);

        // X updates itself (dangling bus, no occupant to resolve against),
        // then W beats X. The sweep must put X's PRE-BATCH state back, but
        // its slot is now W's: a verbatim live restore would trip
        // ux_track_slot (brick); skipping would leave the dangling row
        // spinning the sweep's fixpoint; nulling just the pointer would
        // leave the table LWW-equal to the parked snapshot, which then
        // never replays. The pinned invariant: restore the snapshot
        // tombstoned + journaled, so the page commits, every remaining
        // state is accounted, and the parked snapshot still wins LWW.
        final res = await applier.apply([
          accTrack('X', 'P', hlc(900), busId: 'B'),
          accTrack('W', 'P', hlc(950)),
        ]);
        expect(res.committed, isTrue);
        expect(res.applied, 1);
        expect(res.failed, 1);

        // X: pre-batch content, tombstoned at the winner's wall, derived
        // stamp one counter above its own prior HLC.
        final x = (await trackState('X'))!;
        expect(x['practice_id'], _id('P'));
        expect(x['bus_id'], isNull);
        expect(x['deleted_at'], 950);
        expect(x['hlc_packed'], derivedAt(850).packed);
        expect((await trackState('W'))!['deleted_at'], isNull);
        // The ledger holds W's resolution entry (superseded WITHIN the
        // ledger — the drain collapses per row to the latest entry) and the
        // tombstone-restore entry, which matches the row exactly.
        final journal = await oplogRows();
        expect(journal, hasLength(2));
        expect(journal[0]['entity_id'], _id('X'));
        expect(journal[0]['hlc_packed'], derivedAt(900).packed);
        expect(journal[1]['entity_id'], _id('X'));
        expect(journal[1]['op'], 'tombstone');
        expect(journal[1]['hlc_packed'], derivedAt(850).packed);
        // The parked post-update snapshot out-ranks the derived stamp...
        final parked = (await store.list()).singleWhere(
          (q) => q.tbl == 'tracks',
        );
        expect(parked.remoteHlcPacked, derivedAt(900).packed);
        expect(
          parked.remoteHlcPacked.compareTo(derivedAt(850).packed),
          greaterThan(0),
        );

        // ...so the replay lands it once the parent arrives.
        await applier.apply([fxBus('B', 'P', hlc(985))]);
        final replay = await applier.apply([SyncRow.decode(parked.payload)]);
        expect(replay.applied, 1);
        final after = (await trackState('X'))!;
        expect(after['deleted_at'], 950);
        expect(after['bus_id'], _id('B'));
        expect(after['hlc_packed'], derivedAt(900).packed);
        expect((await trackState('W'))!['deleted_at'], isNull);
        expect(await oplogRows(), hasLength(2));
      });

      test('three-way chain: the parked record beat the ORIGINAL occupant, '
          'was beaten by a second, which a third then beat — the revert '
          'skips the original occupant under the live final winner and the '
          'page commits', () async {
        await applier.apply([
          practice('P', 'p', hlc(800)),
          accTrack('O', 'P', hlc(850)),
        ]);

        final res = await applier.apply([
          accTrack('T', 'P', hlc(900), busId: 'B'),
          accTrack('O2', 'P', hlc(950)),
          accTrack('W', 'P', hlc(980)),
        ]);
        expect(res.committed, isTrue);
        expect(res.applied, 2);
        expect(res.failed, 1);

        expect((await trackState('W'))!['deleted_at'], isNull);
        final o2 = (await trackState('O2'))!;
        expect(o2['deleted_at'], 980); // W's wall — O2's loss stands
        expect(o2['hlc_packed'], derivedAt(950).packed);
        final o = (await trackState('O'))!;
        expect(o['deleted_at'], 900);
        expect(o['hlc_packed'], derivedAt(850).packed);
        expect(await trackState('T'), isNull);
        // Every tombstone is journaled: O by the parked T's kept pair, T by
        // O2's resolution, O2 by W's — none reverted, none half-reverted.
        final journal = await oplogRows();
        expect(journal, hasLength(3));
        expect(journal[0]['entity_id'], _id('O'));
        expect(journal[0]['hlc_packed'], derivedAt(850).packed);
        expect(journal[1]['entity_id'], _id('T'));
        expect(journal[1]['hlc_packed'], derivedAt(900).packed);
        expect(journal[2]['entity_id'], _id('O2'));
        expect(journal[2]['hlc_packed'], derivedAt(950).packed);

        // Convergence after the parent lands.
        await applier.apply([fxBus('B', 'P', hlc(985))]);
        final parked = (await store.list()).singleWhere(
          (q) => q.tbl == 'tracks',
        );
        final replay = await applier.apply([SyncRow.decode(parked.payload)]);
        expect(replay.applied, 1);
        expect((await trackState('T'))!['deleted_at'], 950); // O2's wall
        expect((await trackState('W'))!['deleted_at'], isNull);
        expect(await oplogRows(), hasLength(3));
      });
    });
  });

  group('LWW quarantine store at the applier (a stale park never overwrites '
      'a newer one)', () {
    late QuarantineStore store;
    late int now;

    setUp(() {
      now = 1000;
      store = QuarantineStore(db);
      applier = SyncApplier(
        db,
        HlcClock(node: _id('LOCAL'), now: () => now),
        builder: SyncRowBuilder(db),
        quarantine: store,
      );
    });

    RemoteRecord assetRecord(String id, Hlc h) => RemoteRecord(
      table: 'assets',
      columns: {
        'id': _id(id),
        'content_hash': 'HASH_$id',
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
    );

    Future<Map<String, Object?>> practiceRow() async =>
        (await db
                .customSelect(
                  'SELECT title, separation_asset_id, hlc_packed FROM practices '
                  'WHERE id = ?',
                  variables: [Variable.withString(_id('P'))],
                )
                .getSingle())
            .data;

    test(
      'legal same-page ordering — P@950 applies then is FK-swept, P@1000 '
      'was parked at the drift gate — parks BOTH: P@950 lands with its '
      'parent while the clock is still behind, P@1000 once it catches up',
      () async {
        final future = hlc(1000 + HlcClock.defaultMaxDriftMs + 60 * 1000);
        final res = await applier.apply([
          // Applies first, into a missing asset parent (locally never seen).
          practice('P', 'v950', hlc(950), separationAssetId: 'A'),
          // Arrives later on the same page; parked at the drift gate @1000.
          practice('P', 'v1000', future),
        ]);
        expect(res.committed, isTrue);
        expect(res.quarantined, 1); // P@1000
        expect(res.applied, 0); // P@950 applied but was swept
        expect(res.failed, 1); // ...and parked as an fk-orphan
        expect(await row('P'), isNull);

        // The sweep's park of the CURRENT db row (P@950) ran LAST. Under an
        // unconditional upsert it overwrote the newer state; under the
        // shared slot's LWW guard it was filtered out instead, and the DELETE
        // then took P@950's only copy. Neither may happen: the door entry
        // keeps P@1000 and the row's sweep slot keeps P@950.
        final parked = await store.list();
        expect(parked, hasLength(2));
        final door = parked.singleWhere((q) => q.id == _id('P'));
        expect(door.remoteHlcPacked, future.packed);
        expect(door.reason, startsWith('hlc-drift'));
        final swept = parked.singleWhere(
          (q) => q.id == QuarantineStore.sweepSlotId(_id('P')),
        );
        expect(swept.remoteHlcPacked, hlc(950).packed);
        expect(swept.reason, startsWith('fk-orphan'));

        // The parent lands while P@1000 is still implausible: the replay
        // lands the honest P@950 NOW instead of leaving the row deleted
        // until the clock catches up with the runaway peer.
        await applier.apply([assetRecord('A', hlc(960))]);
        expect(
          (await applier.apply([SyncRow.decode(door.payload)])).quarantined,
          1,
        );
        expect(
          (await applier.apply([SyncRow.decode(swept.payload)])).applied,
          1,
        );
        expect((await practiceRow())['title'], 'v950');

        // The clock catches up: P@1000 lands over it by plain LWW.
        now = future.wallMs;
        final replay = await applier.apply([SyncRow.decode(door.payload)]);
        expect(replay.applied, 1);
        final r = await practiceRow();
        expect(r['title'], 'v1000');
        expect(r['hlc_packed'], future.packed);
      },
    );

    test('park-and-restore: a drift-parked newer state does not displace the '
        'parked UPDATE — the restored row moves to it once its parent '
        'lands', () async {
      await applier.apply([practice('P', 'v900', hlc(900))]);
      final future = hlc(1000 + HlcClock.defaultMaxDriftMs + 60 * 1000);
      final res = await applier.apply([
        // Repoints the pre-existing row at a missing asset: parked by the
        // sweep, the row restored to P@900 verbatim.
        practice('P', 'v950', hlc(950), separationAssetId: 'A'),
        practice('P', 'v1000', future), // parked at the drift gate
      ]);
      expect(res.committed, isTrue);
      expect((await practiceRow())['hlc_packed'], hlc(900).packed);
      expect(await store.list(), hasLength(2));

      // Under the shared slot only P@1000 survived: once the asset landed
      // the row stayed on the restored P@900 — the update every peer holding
      // the asset shows was gone from this device.
      await applier.apply([assetRecord('A', hlc(960))]);
      for (final q in await store.list()) {
        await applier.apply([SyncRow.decode(q.payload)]);
      }
      final healed = await practiceRow();
      expect(healed['title'], 'v950');
      expect(healed['separation_asset_id'], _id('A'));
      expect(healed['hlc_packed'], hlc(950).packed);
    });

    test('a permanently-unreplayable newer record cannot void the FK sweep '
        'park: P@950 applies, P@960 is parked at the value gate, the sweep '
        'still parks (and later replays) the honest P@950', () async {
      final honest = hlc(950);
      final garbage = hlc(960);
      final res = await applier.apply([
        // Applies first, into a missing asset parent (locally never seen).
        practice('P', 'honest', honest, separationAssetId: 'A'),
        // A newer state from a non-conforming writer: lyrics_json is valid
        // JSON of the wrong shape, parked at the value gate — a payload the
        // replay's identical gate re-rejects every cycle, forever.
        RemoteRecord(
          table: 'practices',
          columns: {
            ...practice('P', 'garbage', garbage).columns,
            'lyrics_json': '{}',
          },
        ),
      ]);
      expect(res.committed, isTrue);
      expect(res.failed, 2); // the parked garbage and the swept honest row
      expect(await row('P'), isNull, reason: 'the sweep deleted the row');

      // The sweep's park ran LAST with the OLDER stamp. Under plain
      // newest-stamp retention in one shared slot the dead P@960 held it and
      // the honest P@950 — the only surviving copy of a row the change token
      // is already past — was silently dropped with the DELETE. The sweep
      // parks into the row's own slot: the dead state stays beside it as
      // evidence and can never displace it.
      final parked = await store.list();
      expect(parked, hasLength(2));
      expect(
        parked.singleWhere((q) => q.id == _id('P')).reason,
        startsWith('invalid-value'),
      );
      final swept = parked.singleWhere(
        (q) => q.id == QuarantineStore.sweepSlotId(_id('P')),
      );
      expect(swept.remoteHlcPacked, honest.packed);
      expect(swept.reason, startsWith('fk-orphan'));
      final snapshot = SyncRow.decode(swept.payload);
      expect(snapshot.fields['title'], 'honest');

      // The parent lands: the replayed park restores the honest row.
      await applier.apply([assetRecord('A', hlc(955))]);
      final replay = await applier.apply([snapshot]);
      expect(replay.applied, 1);
      final r = await db
          .customSelect(
            'SELECT title, hlc_packed FROM practices WHERE id = ?',
            variables: [Variable.withString(_id('P'))],
          )
          .getSingle();
      expect(r.data['title'], 'honest');
      expect(r.data['hlc_packed'], honest.packed);
    });
  });

  group('derived-node stamps (bookkeeping can never revert a held-wall '
      'author edit)', () {
    RemoteRecord asset(String id, Hlc h, {int? deletedAt}) => RemoteRecord(
      table: 'assets',
      columns: {
        'id': _id(id),
        'content_hash': 'HASH_$id',
        'byte_size': 8,
        'ext': 'wav',
        'kind': 'audio',
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

    Future<Map<String, Object?>> practiceRow() async {
      final r = await db
          .customSelect(
            'SELECT title, separation_asset_id, hlc_ctr, hlc_node '
            'FROM practices WHERE id = ?',
            variables: [Variable.withString(_id('P'))],
          )
          .getSingle();
      return r.data;
    }

    test('a repair tying a concurrent real edit on (wall, counter) LOSES to '
        'it, and the heal re-derives above the edit', () async {
      // The protocol's documented lost update: an author whose clock wall is
      // HELD stamps a real edit at (base.wall, base.counter + 1) — exactly
      // the stamp a healer derives from the same base. Author node 'AAA…'
      // sorts BELOW the healer device 'LOCAL…', the precise draw where the
      // old real-node repair stamp won the tie and its full-row snapshot
      // reverted the author's title fleet-wide.
      final authorNode = _id('AAA');
      final base = Hlc(wallMs: 1000, counter: 5, node: authorNode);
      await applier.apply([asset('A', hlc(900))]);
      final seeded = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            'id': _id('P'),
            'title': 'original',
            'timeline_duration_ms': 0,
            'master_volume': 1.0,
            'active_interpretation_id': null,
            'separation_asset_id': _id('A'),
            'reverb_json': null,
            'lyrics_json': null,
            'created_at': base.wallMs,
            'updated_at': base.wallMs,
            'deleted_at': null,
            'hlc_wall': base.wallMs,
            'hlc_ctr': base.counter,
            'hlc_node': base.node,
            'hlc_packed': base.packed,
            'schema_v': 1,
          },
        ),
      ]);
      expect(seeded.applied, 1);
      // The asset is tombstoned remotely: P's pointer now dangles (the row
      // still exists, so the FK is satisfied — only the live-target
      // invariant broke, which is exactly what IntegrityChecker heals).
      await applier.apply([asset('A', hlc(1100), deletedAt: 1100)]);

      // The local heal derives (base.wall, base.counter + 1) under the
      // DERIVED node — '!' + the device suffix, sorting below every real
      // node id.
      final checker = IntegrityChecker(
        db,
        SyncWriter(db, HlcClock(node: _id('LOCAL'), now: () => 2000)),
      );
      expect(await checker.repair(), isEmpty);
      var row = await practiceRow();
      expect(row['separation_asset_id'], isNull);
      // Node-space derivation: same (wall, counter) as the base, node
      // extended — the repair replicates over the base yet loses to EVERY
      // stamp above it.
      expect(row['hlc_ctr'], base.counter);
      expect(
        (row['hlc_node']! as String).contains('!'),
        isTrue,
        reason: 'repairs must stamp in the derived node space',
      );

      // The author's held-wall edit arrives: the SAME (wall, counter) under
      // its real node, a renamed title, and the pre-heal dangling pointer.
      // It must win the tie — the old healer-node stamp beat it and silently
      // threw the rename away.
      final edit = Hlc(
        wallMs: base.wallMs,
        counter: base.counter + 1,
        node: authorNode,
      );
      final applied = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            'id': _id('P'),
            'title': 'renamed',
            'timeline_duration_ms': 0,
            'master_volume': 1.0,
            'active_interpretation_id': null,
            'separation_asset_id': _id('A'),
            'reverb_json': null,
            'lyrics_json': null,
            'created_at': base.wallMs,
            'updated_at': edit.wallMs,
            'deleted_at': null,
            'hlc_wall': edit.wallMs,
            'hlc_ctr': edit.counter,
            'hlc_node': edit.node,
            'hlc_packed': edit.packed,
            'schema_v': 1,
          },
        ),
      ]);
      expect(applied.applied, 1);
      row = await practiceRow();
      expect(
        row['title'],
        'renamed',
        reason: 'user intent must win the (wall, counter) tie',
      );

      // The heal is DERIVABLE state: the next pass re-derives it one counter
      // above the author's edit, keeping both the rename and the heal.
      expect(await checker.repair(), isEmpty);
      row = await practiceRow();
      expect(row['title'], 'renamed');
      expect(row['separation_asset_id'], isNull);
      expect(row['hlc_ctr'], edit.counter);
      expect((row['hlc_node']! as String).contains('!'), isTrue);
    });

    test('CHAINED repairs on one row still lose to a held-wall author edit '
        '(counter escalation was base+2 beating base+1 real)', () async {
      // TWO dangling pointers on one practice row make the SAME repair pass
      // derive twice: heal #1 from the base, heal #2 from heal #1's row. The
      // old counter derivation stamped base+1 then base+2 — and base+2
      // bookkeeping beat a base+1 REAL edit, silently reverting it
      // fleet-wide. Node-space derivation never leaves the base's
      // (wall, counter), so any number of chained heals stays below it.
      final authorNode = _id('AAA');
      final base = Hlc(wallMs: 1000, counter: 5, node: authorNode);
      RemoteRecord interp(
        String id,
        String practiceId,
        Hlc h, {
        int? deletedAt,
      }) => RemoteRecord(
        table: 'interpretations',
        columns: {
          'id': _id(id),
          'practice_id': _id(practiceId),
          'name': '',
          'order_index': 0,
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
      await applier.apply([asset('A', hlc(900))]);
      final seeded = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            'id': _id('P'),
            'title': 'original',
            'timeline_duration_ms': 0,
            'master_volume': 1.0,
            'active_interpretation_id': _id('I'),
            'separation_asset_id': _id('A'),
            'reverb_json': null,
            'lyrics_json': null,
            'created_at': base.wallMs,
            'updated_at': base.wallMs,
            'deleted_at': null,
            'hlc_wall': base.wallMs,
            'hlc_ctr': base.counter,
            'hlc_node': base.node,
            'hlc_packed': base.packed,
            'schema_v': 1,
          },
        ),
        interp('I', 'P', hlc(905)),
      ]);
      expect(seeded.applied, 2);
      // Both targets die: the repair pass must heal BOTH pointers — two
      // sequential derived writes on the same practices row.
      await applier.apply([asset('A', hlc(1100), deletedAt: 1100)]);
      await applier.apply([
        interp(
          'I',
          'P',
          Hlc(wallMs: 1105, counter: 0, node: hlc(0).node),
          deletedAt: 1105,
        ),
      ]);
      final checker = IntegrityChecker(
        db,
        SyncWriter(db, HlcClock(node: _id('LOCAL'), now: () => 2000)),
      );
      expect(await checker.repair(), isEmpty);
      var row = await practiceRow();
      expect(row['separation_asset_id'], isNull);
      final chained = row['hlc_node']! as String;
      expect(
        '!'.allMatches(chained).length,
        greaterThanOrEqualTo(2),
        reason: 'the second heal must derive from the first (a chain)',
      );
      expect(row['hlc_ctr'], base.counter, reason: 'never a counter bump');

      // The held-wall author edit at base+1 must beat the WHOLE chain.
      final edit = Hlc(
        wallMs: base.wallMs,
        counter: base.counter + 1,
        node: authorNode,
      );
      final applied = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            'id': _id('P'),
            'title': 'renamed',
            'timeline_duration_ms': 0,
            'master_volume': 1.0,
            'active_interpretation_id': _id('I'),
            'separation_asset_id': _id('A'),
            'reverb_json': null,
            'lyrics_json': null,
            'created_at': base.wallMs,
            'updated_at': edit.wallMs,
            'deleted_at': null,
            'hlc_wall': edit.wallMs,
            'hlc_ctr': edit.counter,
            'hlc_node': edit.node,
            'hlc_packed': edit.packed,
            'schema_v': 1,
          },
        ),
      ]);
      expect(applied.applied, 1);
      row = await practiceRow();
      expect(row['title'], 'renamed', reason: 'chained bookkeeping lost');

      // And the heals re-derive above the edit, keeping the rename.
      expect(await checker.repair(), isEmpty);
      row = await practiceRow();
      expect(row['title'], 'renamed');
      expect(row['separation_asset_id'], isNull);
      expect(row['hlc_ctr'], edit.counter);
    });
  });
}
