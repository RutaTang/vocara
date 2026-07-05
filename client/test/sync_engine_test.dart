import 'dart:async';

import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/oplog_drain.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_engine.dart';
import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/data/sync/sync_state_store.dart';
import 'package:vocara/data/sync/sync_writer.dart';

import 'support/fake_sync_transport.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

/// A manually-fired [Timer] so the debounce wiring is testable without time.
class _FakeTimer implements Timer {
  _FakeTimer(this.duration, this._callback);

  final Duration duration;
  final void Function() _callback;
  bool _cancelled = false;
  bool _fired = false;

  void fire() {
    if (_cancelled || _fired) return;
    _fired = true;
    _callback();
  }

  @override
  void cancel() => _cancelled = true;

  @override
  bool get isActive => !_cancelled && !_fired;

  @override
  int get tick => 0;
}

void main() {
  // A fixed epoch anchor: every stamp in these tests is derived from it, so
  // nothing depends on the machine's wall clock.
  final int t0 = DateTime.utc(2026, 1, 1).millisecondsSinceEpoch;
  final remoteNode = _id('REMOTE');

  late AppDatabase db;
  late HlcClock clock;
  late SyncWriter writer;
  late FakeCloud cloud;
  late FakeSyncTransport transport;
  late SyncStateStore state;
  late QuarantineStore quarantine;
  late SyncEngine engine;
  late List<RemoteApplied> events;
  late int nowMs;
  var killNextCommit = false;

  SyncEngine buildEngine({SyncTimerFactory? timers}) => SyncEngine(
    db: db,
    applier: SyncApplier(db, clock, quarantine: quarantine),
    drain: OplogDrain(db),
    builder: SyncRowBuilder(db),
    state: state,
    integrity: IntegrityChecker(db, writer),
    transport: transport,
    onRemoteApplied: events.add,
    timers: timers,
    debugBeforePageCommit: (_) async {
      if (killNextCommit) throw StateError('killed page commit');
    },
  );

  setUp(() {
    nowMs = t0;
    killNextCommit = false;
    events = <RemoteApplied>[];
    db = AppDatabase(NativeDatabase.memory());
    clock = HlcClock(node: _id('LOCAL'), now: () => nowMs);
    writer = SyncWriter(db, clock);
    cloud = FakeCloud();
    transport = FakeSyncTransport(cloud, name: 'A');
    state = SyncStateStore(db);
    quarantine = QuarantineStore(db);
    engine = buildEngine();
  });
  tearDown(() {
    engine.dispose();
    return db.close();
  });

  Hlc remoteHlc(int wall, [int ctr = 0]) =>
      Hlc(wallMs: wall, counter: ctr, node: remoteNode);

  /// A LOCAL practice write exactly as the SyncWriter chokepoint produces it:
  /// row + oplog entry in one transaction, stamped by the (test-driven) clock.
  Future<void> savePractice(
    String id,
    String title, {
    String? dependsOnAssetId,
  }) async {
    nowMs += 1000; // each save is strictly later than the previous one
    final s = writer.stamp();
    await db.transaction(() async {
      await db
          .into(db.practices)
          .insertOnConflictUpdate(
            PracticesCompanion.insert(
              id: _id(id),
              title: Value(title),
              createdAt: s.at,
              updatedAt: s.at,
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          );
      await writer.journal(
        table: 'practices',
        id: _id(id),
        op: 'insert',
        hlc: s.hlc,
        payloadHash: 'h:$title',
        dependsOnAssetId: dependsOnAssetId,
      );
    });
  }

  Map<String, Object?> syncCols(Hlc h, {int? deletedAt}) => {
    'created_at': h.wallMs,
    'updated_at': h.wallMs,
    'deleted_at': deletedAt,
    'hlc_wall': h.wallMs,
    'hlc_ctr': h.counter,
    'hlc_node': h.node,
    'hlc_packed': h.packed,
    'schema_v': 1,
  };

  SyncRow remotePractice(String id, String title, Hlc h, {int? deletedAt}) =>
      SyncRow(
        table: 'practices',
        fields: {
          'id': _id(id),
          'title': title,
          'timeline_duration_ms': 0,
          'master_volume': 1.0,
          'active_interpretation_id': null,
          'separation_asset_id': null,
          'reverb_json': null,
          'lyrics_json': null,
          ...syncCols(h, deletedAt: deletedAt),
        },
      );

  SyncRow remoteTrack(String id, String practiceId, Hlc h) => SyncRow(
    table: 'tracks',
    fields: {
      'id': _id(id),
      'practice_id': _id(practiceId),
      'kind': 'vocal',
      'display_name': 'take',
      'muted': 0,
      'soloed': 0,
      'volume': 1.0,
      'pan': 0.0,
      'order_index': 0,
      ...syncCols(h),
    },
  );

  SyncRow remoteAsset(String id, String hash, Hlc h) => SyncRow(
    table: 'assets',
    fields: {
      'id': _id(id),
      'content_hash': hash,
      'byte_size': 3,
      'ext': 'wav',
      'kind': 'audio',
      'local_rel_path': null,
      'record_sync_state': 'localOnly',
      'bytes_sync_state': 'localOnly',
      'remote_ref': null,
      ...syncCols(h),
    },
  );

  SyncRow remoteClip(String id, String trackId, String assetId, Hlc h) =>
      SyncRow(
        table: 'clips',
        fields: {
          'id': _id(id),
          'track_id': _id(trackId),
          'asset_id': _id(assetId),
          'start_ms': 0,
          'duration_ms': 100,
          'order_index': 0,
          ...syncCols(h),
        },
      );

  Future<int> oplogCount() async => (await db.select(db.oplog).get()).length;

  Future<String?> practiceTitle(String id) async {
    final r = await db
        .customSelect(
          'SELECT title FROM practices WHERE id = ?',
          variables: [Variable.withString(_id(id))],
        )
        .getSingleOrNull();
    return r?.data['title'] as String?;
  }

  group('push', () {
    test('drains and acks exactly the drained range; a save landing mid-push '
        'is not lost', () async {
      await savePractice('P1', 'one');
      transport.holdPushes = true;

      final run = engine.pumpPush();
      await pumpEventQueue();
      expect(transport.heldPushCount, 1, reason: 'first batch is in flight');

      // A save lands WHILE the push is in flight: its oplog entry has a
      // higher seq than the drained window, so the coming ack must not eat it.
      await savePractice('P2', 'two');

      await transport.releaseHeldPushes();
      await pumpEventQueue();
      // The engine keeps draining: the mid-push save is picked up as its own
      // second batch (still held), proving the first ack was exact.
      expect(transport.heldPushCount, 1);
      await transport.releaseHeldPushes();
      final result = await run;

      expect(result.failed, isFalse);
      expect(transport.pushedBatches, hasLength(2));
      expect(transport.pushedBatches[0].map((r) => r.id), [_id('P1')]);
      expect(transport.pushedBatches[1].map((r) => r.id), [_id('P2')]);
      expect(cloud.serverRowFor('practices', _id('P1')), isNotNull);
      expect(cloud.serverRowFor('practices', _id('P2')), isNotNull);
      expect(await oplogCount(), 0, reason: 'everything handled is acked');
    });

    test(
      'asset-gated rows are held back without blocking other rows',
      () async {
        // seq 1: an ordinary row. seq 2: gated on asset X (no upload marker).
        // seq 3: another ordinary row AFTER the gated one.
        await savePractice('B1', 'before');
        final s = writer.stamp();
        await db
            .into(db.assets)
            .insert(
              AssetsCompanion.insert(
                id: _id('AX'),
                contentHash: 'HASH_X',
                byteSize: 3,
                createdAt: s.at,
                updatedAt: s.at,
                hlcWall: s.hlc.wallMs,
                hlcCtr: s.hlc.counter,
                hlcNode: s.hlc.node,
                hlcPacked: s.hlc.packed,
              ),
            );
        await savePractice('G', 'gated', dependsOnAssetId: _id('AX'));
        await savePractice('B2', 'after');

        final first = await engine.pumpPush();
        expect(first.failed, isFalse);
        expect(first.heldBack, 1);
        // Other rows pushed — including the one AFTER the gated entry.
        expect(cloud.serverRowFor('practices', _id('B1')), isNotNull);
        expect(cloud.serverRowFor('practices', _id('B2')), isNotNull);
        expect(cloud.serverRowFor('practices', _id('G')), isNull);
        // The ack watermark stopped just short of the gated entry, so it (and
        // the already-pushed B2, harmlessly re-pushable under LWW) remain.
        expect(first.ackedThrough, 1);
        expect(await oplogCount(), 2);

        // P2's byte uploader would write the marker; simulate the release.
        await db
            .into(db.assetSync)
            .insert(AssetSyncCompanion.insert(hash: 'HASH_X', uploadedAt: 1));
        final second = await engine.pumpPush();
        expect(second.failed, isFalse);
        expect(second.heldBack, 0);
        expect(cloud.serverRowFor('practices', _id('G')), isNotNull);
        expect(await oplogCount(), 0);
      },
    );

    test(
      'conflict with an OLDER server row: local wins, re-push accepted',
      () async {
        // The server already holds an older version of P (another device's push).
        cloud.accept(remotePractice('P', 'server-old', remoteHlc(t0 - 5000)));
        await savePractice('P', 'local-new');
        transport.injectConflictOnce('practices', _id('P'));

        final run = await engine.pumpPush();

        expect(run.failed, isFalse);
        expect(transport.pushCalls, 2, reason: 'rejected once, then re-pushed');
        expect(
          cloud.serverRowFor('practices', _id('P'))!.fields['title'],
          'local-new',
        );
        expect(await oplogCount(), 0);
      },
    );

    test('conflict with a NEWER server row: ours is dropped and the server '
        'state arrives via pull', () async {
      await savePractice('P', 'local-stale');
      // Server holds a strictly newer version (one minute ahead — well within
      // the applier's drift gate).
      final serverRow = remotePractice(
        'P',
        'server-new',
        remoteHlc(nowMs + 60000),
      );
      cloud.accept(serverRow);
      transport.injectConflictOnce('practices', _id('P'));

      final run = await engine.pumpPush();

      expect(run.failed, isFalse);
      expect(transport.pushCalls, 1, reason: 'a losing row is not re-pushed');
      expect(
        cloud.serverRowFor('practices', _id('P'))!.fields['title'],
        'server-new',
      );
      expect(await oplogCount(), 0, reason: 'the dropped row is handled');
      // Local still shows ours until the pull brings the newer server state.
      expect(await practiceTitle('P'), 'local-stale');
      final pull = await engine.pumpPull();
      expect(pull.failed, isFalse);
      expect(await practiceTitle('P'), 'server-new');
    });

    test('transport failure leaves the oplog untouched; the next pump '
        'retries', () async {
      await savePractice('P', 'one');
      transport.partitioned = true;

      final failing = await engine.pumpPush();
      expect(failing.failed, isTrue);
      expect(await oplogCount(), 1);
      expect(cloud.log, isEmpty);

      transport.partitioned = false;
      final retry = await engine.pumpPush();
      expect(retry.failed, isFalse);
      expect(await oplogCount(), 0);
      expect(cloud.serverRowFor('practices', _id('P')), isNotNull);
    });
  });

  group('pull', () {
    test('applies the page and advances the token in ONE transaction: a '
        'killed commit moves neither', () async {
      cloud.accept(remotePractice('P', 'hello', remoteHlc(t0 - 1000)));

      killNextCommit = true;
      final failing = await engine.pumpPull();
      expect(failing.failed, isTrue);
      expect(failing.pages, 0);
      expect(await practiceTitle('P'), isNull, reason: 'rows rolled back');
      expect(await state.changeToken(), isNull, reason: 'token unmoved');

      killNextCommit = false;
      final retry = await engine.pumpPull();
      expect(retry.failed, isFalse);
      expect(retry.applied, 1);
      expect(await practiceTitle('P'), 'hello');
      expect(await state.changeToken(), '1');
      expect(await oplogCount(), 0, reason: 'remote applies journal nothing');
    });

    test('a re-delivered page is a no-op (at-least-once tolerance)', () async {
      cloud.accept(remotePractice('P', 'v1', remoteHlc(t0 - 1000)));
      final first = await engine.pumpPull();
      expect(first.applied, 1);
      expect(events, hasLength(1));

      transport.redeliverNextFetches = 1;
      final again = await engine.pumpPull();
      expect(again.failed, isFalse);
      expect(again.applied, 0);
      expect(again.skipped, 1, reason: 'LWW: we already hold this exact row');
      expect(await practiceTitle('P'), 'v1');
      expect(await state.changeToken(), '1');
      expect(await oplogCount(), 0);
      expect(events, hasLength(1), reason: 'a no-op page announces nothing');
    });

    test(
      'a quarantined record is parked AND the token advances past it',
      () async {
        cloud.accept(remotePractice('GOOD', 'kept', remoteHlc(t0 - 1000)));
        // Implausibly far in the future: beyond the 5-minute drift gate.
        cloud.accept(
          remotePractice('FUT', 'poison', remoteHlc(t0 + 6 * 60 * 1000)),
        );

        final run = await engine.pumpPull();

        expect(run.failed, isFalse);
        expect(run.applied, 1);
        expect(run.quarantined, 1);
        expect(await practiceTitle('GOOD'), 'kept');
        expect(await practiceTitle('FUT'), isNull);
        expect(await state.changeToken(), '2', reason: 'sync is not stalled');
        final parked = await quarantine.list();
        expect(parked, hasLength(1));
        expect(parked.single.id, _id('FUT'));
        // Still parked (not silently dropped) after further pulls.
        await engine.pumpPull();
        expect(await quarantine.list(), hasLength(1));
      },
    );

    test('remoteApplied carries (table, id) pairs and rolls child rows up to '
        'their practice', () async {
      final h = remoteHlc(t0 - 1000);
      cloud.accept(remotePractice('P', 'song', h));
      cloud.accept(remoteAsset('A', 'HASH_A', h));
      cloud.accept(remoteTrack('T', 'P', h));
      // The clip row carries only track_id — the engine resolves the practice
      // with a post-commit lookup.
      cloud.accept(remoteClip('C', 'T', 'A', h));

      final run = await engine.pumpPull();
      expect(run.applied, 4);
      expect(events, hasLength(1));
      final e = events.single;
      expect(
        e.rows,
        containsAll(<(String, String)>[
          ('practices', _id('P')),
          ('assets', _id('A')),
          ('tracks', _id('T')),
          ('clips', _id('C')),
        ]),
      );
      expect(e.practiceIds, {_id('P')});
      expect(e.techniqueIds, isEmpty);
    });
  });

  group('poke wiring', () {
    test('pokes fire only AFTER a transaction completes, and a rolled-back '
        'write drains to a no-op push', () async {
      final created = <_FakeTimer>[];
      engine.dispose();
      engine = buildEngine(
        timers: (d, run) {
          final t = _FakeTimer(d, run);
          created.add(t);
          return t;
        },
      );
      engine.start();

      // A rolled-back transaction: drift dispatches its table-update
      // notification only once the transaction has COMPLETED (here: rolled
      // back) — never from inside it. The resulting poke is harmless: the
      // journal entry rolled back with the data, so the pump pushes nothing.
      try {
        await db.transaction(() async {
          final s = writer.stamp();
          await writer.journal(
            table: 'practices',
            id: _id('GONE'),
            op: 'insert',
            hlc: s.hlc,
            payloadHash: 'h',
          );
          expect(created, isEmpty, reason: 'no poke inside an open txn');
          throw StateError('roll it back');
        });
      } catch (_) {}
      await pumpEventQueue();
      for (final t in created) {
        t.fire();
      }
      await pumpEventQueue();
      expect(transport.pushCalls, 0, reason: 'nothing survived the rollback');
      expect(cloud.log, isEmpty);

      // A committed save pokes: quiet + max-wait timers are armed.
      final before = created.length;
      await savePractice('P', 'one');
      await pumpEventQueue();
      expect(created.length, before + 2);
      expect(created[before].duration, const Duration(seconds: 2));
      expect(created[before + 1].duration, const Duration(seconds: 30));

      // Firing the quiet timer runs the push.
      created[before].fire();
      await pumpEventQueue();
      expect(cloud.serverRowFor('practices', _id('P')), isNotNull);
      expect(await oplogCount(), 0);
    });
  });
}
