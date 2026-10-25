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
import 'package:vocara/data/sync/sync_transport.dart';
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

/// A conflicted push result WITHOUT a serverRow. Unrepresentable through
/// [PushRowResult]'s own constructors ([PushRowResult.accepted] is derived
/// from serverRow's absence), so this models the seam a decoding transport
/// adds: a response that CLAIMS conflict while carrying no server state. The
/// engine must refuse it rather than null-deref or misread it as accepted.
class _ConflictWithoutServerRow implements PushRowResult {
  _ConflictWithoutServerRow(this.row);

  @override
  final SyncRow row;

  @override
  SyncRow? get serverRow => null;

  @override
  bool get accepted => false;
}

/// A NON-conforming transport for the pull loop's rows-bearing refuse-to-spin
/// guard: it serves the same rows-bearing page with hasMore set and, once a
/// token exists, never advances it — the violation sync_transport.dart's
/// token contract forbids ("a transport must advance the token whenever it
/// returns rows"). Fetches are BOUNDED so an engine that does spin fails the
/// test on the fetch count instead of hanging it.
class _StuckTokenTransport implements SyncTransport {
  _StuckTokenTransport(this.rows);

  final List<SyncRow> rows;
  int fetchCalls = 0;

  @override
  Future<FetchPage> fetchChanges(String? sinceToken) async {
    fetchCalls++;
    if (fetchCalls > 4) {
      throw SyncTransportException('spun: the engine kept refetching');
    }
    return FetchPage(rows: rows, nextToken: sinceToken ?? 'T0', hasMore: true);
  }

  @override
  Future<PushAck> pushRows(List<SyncRow> rows) async =>
      PushAck([for (final r in rows) PushRowResult.accepted(r)]);

  @override
  Future<void> pushAssetBytes(String contentHash, String filePath) =>
      throw UnimplementedError();

  @override
  Future<void> fetchAssetBytes(String contentHash, String destPath) =>
      throw UnimplementedError();
}

/// Stands in for a process that dies after a pull run's pages committed but
/// before its end-of-run repair transaction ran.
class _DyingChecker extends IntegrityChecker {
  _DyingChecker(super.db, super.writer);

  @override
  Future<List<IntegrityViolation>> repair({int maxPasses = 8}) =>
      throw StateError('process killed before the repair');
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

  SyncEngine buildEngine({
    SyncTimerFactory? timers,
    IntegrityChecker? integrity,
  }) => SyncEngine(
    db: db,
    applier: SyncApplier(
      db,
      clock,
      builder: SyncRowBuilder(db),
      quarantine: quarantine,
    ),
    drain: OplogDrain(db),
    builder: SyncRowBuilder(db),
    state: state,
    integrity: integrity ?? IntegrityChecker(db, writer),
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

  // Full-row snapshot (the wire contract): the applier's missing-column gate
  // quarantines a schema_v-current record that omits any table column.
  SyncRow remoteTrack(String id, String practiceId, Hlc h, {int? deletedAt}) =>
      SyncRow(
        table: 'tracks',
        fields: {
          'id': _id(id),
          'practice_id': _id(practiceId),
          'kind': 'vocal',
          'interpretation_id': null,
          'display_name': 'take',
          'color_value': null,
          'muted': 0,
          'soloed': 0,
          'volume': 1.0,
          'pan': 0.0,
          'bus_id': null,
          'fx_json': null,
          'sends_json': null,
          'order_index': 0,
          ...syncCols(h, deletedAt: deletedAt),
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

  SyncRow remoteClip(
    String id,
    String trackId,
    String assetId,
    Hlc h, {
    int? deletedAt,
    int startMs = 0,
  }) => SyncRow(
    table: 'clips',
    fields: {
      'id': _id(id),
      'track_id': _id(trackId),
      'asset_id': _id(assetId),
      'start_ms': startMs,
      'duration_ms': 100,
      'order_index': 0,
      ...syncCols(h, deletedAt: deletedAt),
    },
  );

  /// A wire ECHO of [real] forged to claim the HLC [h] — same (table, id)
  /// identity and an internally-consistent quad, so it correlates cleanly
  /// with the attempt and passes every shape check. Models a hostile ack
  /// trying to re-anchor the engine's HLC comparisons; the engine must bind
  /// every decision to its own attempt row instead.
  SyncRow forgedEcho(SyncRow real, Hlc h) => SyncRow(
    table: real.table,
    fields: {
      ...real.fields,
      'hlc_wall': h.wallMs,
      'hlc_ctr': h.counter,
      'hlc_node': h.node,
      'hlc_packed': h.packed,
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

    test('deleting a gated row does not strand the ack watermark: a tombstone '
        'references no bytes', () async {
      // A delete inheriting its insert's dependency waits on an upload that
      // may never come (the asset is collectable once nothing references it)
      // — and since the push loop acks only up to the entry before the first
      // gated one, that one tombstone freezes the watermark for the entire
      // ledger: everything behind it is re-pushed on every run, forever.
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
      // The row is deleted before its bytes were ever uploaded.
      nowMs += 1000;
      final t = writer.stamp();
      await db.transaction(() async {
        await db.customStatement(
          'UPDATE practices SET deleted_at = ? WHERE id = ?',
          [t.at, _id('G')],
        );
        await writer.journal(
          table: 'practices',
          id: _id('G'),
          op: 'tombstone',
          hlc: t.hlc,
          payloadHash: '',
        );
      });
      await savePractice('AFTER', 'behind the tombstone');

      final run = await engine.pumpPush();
      expect(run.failed, isFalse);
      expect(run.heldBack, 0, reason: 'a delete needs no bytes');
      expect(await oplogCount(), 0, reason: 'the whole ledger acked');
      expect(cloud.serverRowFor('practices', _id('AFTER')), isNotNull);
    });

    test('a gated row edited again STAYS gated across pumps: the partial ack '
        'must not delete the entry that recorded its dependency', () async {
      // The insert carries the dependency; a later content-only edit does
      // not. The drain collapses them to the newest entry (keeping the
      // dependency), so the ack cut falls BELOW the insert — which used to
      // delete the only journaled record of the gate. The next pump then saw
      // a bare update and pushed a row referencing bytes the server had
      // never received.
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
      await savePractice('B1', 'before');
      await savePractice('G', 'gated', dependsOnAssetId: _id('AX'));
      await savePractice('G', 'gated, renamed'); // no dependency of its own

      final first = await engine.pumpPush();
      expect(first.heldBack, 1);
      expect(cloud.serverRowFor('practices', _id('G')), isNull);

      final second = await engine.pumpPush();
      expect(second.heldBack, 1, reason: 'the bytes are still not uploaded');
      expect(
        cloud.serverRowFor('practices', _id('G')),
        isNull,
        reason: 'a row must never reach the server before its bytes',
      );

      // Once the uploader posts the marker the row goes, as always.
      await db
          .into(db.assetSync)
          .insert(AssetSyncCompanion.insert(hash: 'HASH_X', uploadedAt: 1));
      final third = await engine.pumpPush();
      expect(third.heldBack, 0);
      expect(cloud.serverRowFor('practices', _id('G')), isNotNull);
      expect(await oplogCount(), 0);
    });

    test('a CAS check against an OLDER server row never conflicts: the '
        'conforming server accepts the newer push outright', () async {
      // The server holds an older version of P (another device's push); a
      // conforming CAS accepts our strictly newer row — an "older-row
      // conflict" does not exist in the protocol (the engine refuses one
      // as a lie; see the conflicted-ack validation group).
      cloud.accept(remotePractice('P', 'server-old', remoteHlc(t0 - 5000)));
      await savePractice('P', 'local-new');
      transport.injectConflictOnce('practices', _id('P'));

      final run = await engine.pumpPush();

      expect(run.failed, isFalse);
      expect(transport.pushCalls, 1, reason: 'accepted on the first push');
      expect(
        cloud.serverRowFor('practices', _id('P'))!.fields['title'],
        'local-new',
      );
      expect(await oplogCount(), 0);
    });

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

  group('push ack validation', () {
    // A PushAck is never trusted blind: the result set must cover EXACTLY the
    // pushed rows, or the engine must fail the run BEFORE the oplog ack —
    // otherwise a buggy or hostile server returning a short/wrong ack lets
    // the engine delete journal entries for rows the server never stored.
    // The well-formed-ack path is covered by every other push test.

    test('an ack with FEWER results than pushed rows fails the run and '
        'leaves the oplog intact', () async {
      await savePractice('P1', 'one');
      await savePractice('P2', 'two');
      transport.mangleNextAck = (pushed, ack) =>
          PushAck(ack.results.sublist(0, 1));

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(run.ackedThrough, 0);
      expect(await oplogCount(), 2, reason: 'nothing acked on a bad ack');

      // The knob cleared itself: an honest server drains the retry (the
      // re-push of already-stored rows is a harmless LWW no-op server-side).
      final retry = await engine.pumpPush();
      expect(retry.failed, isFalse);
      expect(await oplogCount(), 0);
    });

    test('an ack whose result is for a DIFFERENT row than was pushed fails '
        'the run (same count is not enough — identity must match)', () async {
      await savePractice('P1', 'one');
      transport.mangleNextAck = (pushed, ack) => PushAck([
        PushRowResult.accepted(
          remotePractice('IMPOSTOR', 'not what we sent', remoteHlc(t0)),
        ),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 1);
    });

    test('DUPLICATE results for one row (masking a missing one) fail the '
        'run — the match is 1:1, not set membership', () async {
      await savePractice('P1', 'one');
      await savePractice('P2', 'two');
      transport.mangleNextAck = (pushed, ack) =>
          PushAck([ack.results.first, ack.results.first]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 2);
    });
  });

  group('conflicted-ack validation', () {
    // A conflicted result's serverRow decides retry-or-drop; trusted blind,
    // a lazy lie lets the server make the engine silently drop (and ack
    // away) an edit it never stored. The engine validates every conflicted
    // serverRow — existence, identity, internal HLC consistency, strict
    // newness — and fails the run on any violation, oplog intact. The
    // legitimate conflict (server strictly newer, consistent) keeps
    // resolving as a drop: see 'conflict with a NEWER server row' above.

    test('a conflicted result carrying NO serverRow fails the run', () async {
      await savePractice('P', 'one');
      transport.mangleNextAck = (pushed, ack) =>
          PushAck([_ConflictWithoutServerRow(ack.results.single.row)]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 1, reason: 'nothing acked on a bad ack');
    });

    test('a conflicted serverRow with a MISMATCHED identity fails the run '
        '(a rejection must be justified by the row it rejects)', () async {
      await savePractice('P', 'one');
      transport.mangleNextAck = (pushed, ack) => PushAck([
        PushRowResult.conflicted(
          ack.results.single.row,
          // Internally consistent and plausibly newer — but for another row.
          serverRow: remotePractice('OTHER', 'x', remoteHlc(nowMs + 60000)),
        ),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 1);
    });

    test('a conflicted serverRow whose hlc_packed disagrees with its quad '
        'fails the run (the forged-LWW-key lie, same rule as the pull '
        'door)', () async {
      await savePractice('P', 'one');
      final honest = remotePractice('P', 'x', remoteHlc(nowMs + 60000));
      transport.mangleNextAck = (pushed, ack) => PushAck([
        PushRowResult.conflicted(
          ack.results.single.row,
          // The quad says +60s while the packed LWW key claims +120s: exactly
          // the inconsistency SyncRow.fromJson refuses on pull, enforced here
          // through the same shared routine.
          serverRow: SyncRow(
            table: 'practices',
            fields: {
              ...honest.fields,
              'hlc_packed': remoteHlc(nowMs + 120000).packed,
            },
          ),
        ),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 1);
    });

    test('a conflicted serverRow OLDER than the pushed row fails the run '
        '(a conforming server accepts a push it holds nothing newer '
        'than)', () async {
      await savePractice('P', 'local-new');
      transport.mangleNextAck = (pushed, ack) => PushAck([
        PushRowResult.conflicted(
          ack.results.single.row,
          // Internally consistent, same identity — but strictly OLDER than
          // the push it claims to reject: a nonsensical CAS.
          serverRow: remotePractice('P', 'server-old', remoteHlc(t0 - 5000)),
        ),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 1, reason: 'nothing acked on a bad ack');
    });

    test('a conflicted serverRow that merely ECHOES the pushed row (equal '
        'HLC — not strictly newer) fails the run', () async {
      await savePractice('P', 'one');
      transport.mangleNextAck = (pushed, ack) => PushAck([
        for (final r in ack.results)
          // The cheapest possible lie: "I hold your row, therefore you
          // conflict". An equal HLC can never legitimately CAS-reject
          // (equal is the idempotent re-push, which a conforming server
          // accepts).
          PushRowResult.conflicted(r.row, serverRow: r.row),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 1);

      // The knob cleared itself: an honest server drains the retry.
      final retry = await engine.pumpPush();
      expect(retry.failed, isFalse);
      expect(await oplogCount(), 0);
    });

    test('a forged OLDER echo with an internally-consistent mid-HLC serverRow '
        'fails the run: every decision binds to the ATTEMPT row, never the '
        'echo', () async {
      // The counter-example this rule exists for: we push P@100. A hostile
      // ack echoes P@0 as "the pushed row" and pairs it with a consistent
      // serverRow P@50 — strictly newer than the ECHO, strictly older than
      // the true attempt. Validated against the echo, this passed every
      // check; the engine judged "server newer", dropped ours, and acked
      // away the drained oplog entry — the local edit silently lost to
      // nothing but a mangled ack.
      await savePractice('P', 'local-new');
      transport.mangleNextAck = (pushed, ack) => PushAck([
        PushRowResult.conflicted(
          forgedEcho(pushed.single, remoteHlc(t0 - 10000)),
          serverRow: remotePractice('P', 'server-mid', remoteHlc(t0 - 5000)),
        ),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTransportException>());
      expect(await oplogCount(), 1, reason: 'nothing acked, nothing lost');
      expect(await practiceTitle('P'), 'local-new');

      // The knob cleared itself: an honest server drains the retry and the
      // local edit survives to the cloud.
      final retry = await engine.pumpPush();
      expect(retry.failed, isFalse);
      expect(
        cloud.serverRowFor('practices', _id('P'))!.fields['title'],
        'local-new',
      );
      expect(await oplogCount(), 0);
    });

    test('a GENUINE conflict (serverRow strictly newer than the attempt) '
        'still resolves as a drop even when the echo is forged: the echo is '
        'the correlation key and nothing else', () async {
      await savePractice('P', 'local-stale');
      final serverNew = remotePractice(
        'P',
        'server-new',
        remoteHlc(nowMs + 60000),
      );
      cloud.accept(serverNew);
      transport.injectConflictOnce('practices', _id('P'));
      // A conforming CAS rejection, except the echo claims an ancient HLC.
      // Judged against the attempt the serverRow is still strictly newer, so
      // the conflict is genuine and resolves exactly as with an honest echo.
      transport.mangleNextAck = (pushed, ack) => PushAck([
        PushRowResult.conflicted(
          forgedEcho(pushed.single, remoteHlc(t0 - 10000)),
          serverRow: ack.results.single.serverRow!,
        ),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isFalse);
      expect(transport.pushCalls, 1, reason: 'a losing row is not re-pushed');
      expect(await oplogCount(), 0, reason: 'the dropped row is handled');
      expect(
        cloud.serverRowFor('practices', _id('P'))!.fields['title'],
        'server-new',
      );
    });
  });

  group('asset record sync-state (push side)', () {
    /// A LOCAL asset row + its journal entry, exactly as importFile commits
    /// them (row and oplog entry in one transaction, drift defaults for the
    /// per-device sync-state columns).
    Future<void> saveAsset(String id, String hash) async {
      nowMs += 1000;
      final s = writer.stamp();
      await db.transaction(() async {
        await db
            .into(db.assets)
            .insert(
              AssetsCompanion.insert(
                id: _id(id),
                contentHash: hash,
                byteSize: 3,
                createdAt: s.at,
                updatedAt: s.at,
                hlcWall: s.hlc.wallMs,
                hlcCtr: s.hlc.counter,
                hlcNode: s.hlc.node,
                hlcPacked: s.hlc.packed,
              ),
            );
        await writer.journal(
          table: 'assets',
          id: _id(id),
          op: 'insert',
          hlc: s.hlc,
          payloadHash: 'h:$hash',
        );
      });
    }

    Future<Map<String, Object?>> assetState(String id) async {
      final r = await db
          .customSelect(
            'SELECT record_sync_state, bytes_sync_state, updated_at, '
            'hlc_packed FROM assets WHERE id = ?',
            variables: [Variable.withString(_id(id))],
          )
          .getSingle();
      return r.data;
    }

    test(
      'a pushed-and-accepted asset is marked record-synced as pure '
      'bookkeeping: no journal entry, no updated_at, no HLC change',
      () async {
        await saveAsset('A', 'HASH_A');
        final before = await assetState('A');
        expect(before['record_sync_state'], 'localOnly');

        final run = await engine.pumpPush();
        expect(run.failed, isFalse);
        expect(cloud.serverRowFor('assets', _id('A')), isNotNull);

        final after = await assetState('A');
        expect(
          after['record_sync_state'],
          'synced',
          reason:
              'the record crossed the wire — the GC never-synced guard must '
              'now protect this row',
        );
        expect(
          after['bytes_sync_state'],
          'localOnly',
          reason: 'blob upload has no transport yet; its state is untouched',
        );
        expect(after['updated_at'], before['updated_at']);
        expect(after['hlc_packed'], before['hlc_packed']);
        expect(
          await oplogCount(),
          0,
          reason: 'the marking is per-device bookkeeping and journals nothing',
        );
      },
    );

    test('an asset dropped as server-newer counts as synced too (the server '
        'holds the record either way)', () async {
      await saveAsset('A', 'HASH_A');
      // The server already holds a strictly newer version of the record.
      cloud.accept(remoteAsset('A', 'HASH_A', remoteHlc(nowMs + 60000)));
      transport.injectConflictOnce('assets', _id('A'));

      final run = await engine.pumpPush();
      expect(run.failed, isFalse);
      expect(transport.pushCalls, 1, reason: 'a losing row is not re-pushed');
      expect((await assetState('A'))['record_sync_state'], 'synced');
      expect(await oplogCount(), 0);
    });

    test('asset record-sync marking binds to the attempt row too: a genuine '
        'server-newer conflict whose echo is forged still marks the asset '
        'synced', () async {
      await saveAsset('A', 'HASH_A');
      cloud.accept(remoteAsset('A', 'HASH_A', remoteHlc(nowMs + 60000)));
      transport.injectConflictOnce('assets', _id('A'));
      transport.mangleNextAck = (pushed, ack) => PushAck([
        PushRowResult.conflicted(
          forgedEcho(pushed.single, remoteHlc(t0 - 10000)),
          serverRow: ack.results.single.serverRow!,
        ),
      ]);

      final run = await engine.pumpPush();
      expect(run.failed, isFalse);
      expect(
        (await assetState('A'))['record_sync_state'],
        'synced',
        reason:
            'the drop decision and the marking both read the attempt row — '
            'the mangled echo must not divert either',
      );
      expect(await oplogCount(), 0);
    });

    test('a local asset never accepted by the server stays localOnly (a '
        'failed run must not pre-mark anything)', () async {
      transport.partitioned = true;
      await saveAsset('A', 'HASH_A');

      final run = await engine.pumpPush();
      expect(run.failed, isTrue);
      expect((await assetState('A'))['record_sync_state'], 'localOnly');
      expect(await oplogCount(), 1, reason: 'still journaled for the retry');
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

    test('a run that dies between its committed page and its repair leaves '
        'nothing broken after a RESTART: the new engine repairs on its first '
        'successful run even though that run applies nothing', () async {
      // A live track under a tombstoned practice: the page applies both
      // (the FK is satisfied — the practice row exists), and only the
      // end-of-run repair tombstones the track.
      cloud.accept(
        remotePractice(
          'P',
          'deleted',
          remoteHlc(t0 - 5000),
          deletedAt: t0 - 5000,
        ),
      );
      cloud.accept(remoteTrack('T', 'P', remoteHlc(t0 - 4000)));
      engine.dispose();
      engine = buildEngine(integrity: _DyingChecker(db, writer));
      final died = await engine.pumpPull();
      expect(died.failed, isTrue);
      expect(died.pages, 1, reason: 'the page and its token committed');
      expect(await IntegrityChecker(db, writer).check(), isNotEmpty);

      // "Restart": a fresh engine over the same durable store. The token is
      // already at head, so this run fetches and applies nothing — the
      // debt the dead process held in memory is gone with it.
      engine.dispose();
      engine = buildEngine();
      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(run.applied, 0);
      expect(
        await IntegrityChecker(db, writer).check(),
        isEmpty,
        reason: 'the first successful run after boot repairs regardless',
      );
    });

    test('a rows-bearing page with an UNMOVED token and hasMore fails the run '
        'loudly instead of spinning forever (the rows-bearing analogue of the '
        'empty branch\'s refuse-to-spin guard)', () async {
      final stuck = _StuckTokenTransport([
        remotePractice('P', 'served forever', remoteHlc(t0 - 1000)),
      ]);
      engine.dispose();
      engine = SyncEngine(
        db: db,
        applier: SyncApplier(
          db,
          clock,
          builder: SyncRowBuilder(db),
          quarantine: quarantine,
        ),
        drain: OplogDrain(db),
        builder: SyncRowBuilder(db),
        state: state,
        integrity: IntegrityChecker(db, writer),
        transport: stuck,
        onRemoteApplied: events.add,
      );

      final run = await engine.pumpPull();
      // The first delivery landed normally (null token cannot be "unmoved");
      // the second returned the identical page with the token pinned. An
      // unguarded loop refetched it forever: every record a clean LWW skip,
      // hasMore true, no failure surfaced — sync (and every push queued
      // behind the engine's serialization) silently wedged in-process.
      expect(run.failed, isTrue, reason: 'the violation must surface');
      expect('${run.failure}', contains('transport violation'));
      expect(
        stuck.fetchCalls,
        2,
        reason: 'stop at the first unmoved token — no spin',
      );
      expect(run.applied, 1);
      expect(await practiceTitle('P'), 'served forever');
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

    test('a record with a mistyped HLC quad is quarantined ALONE: the rest of '
        'the page applies and the token advances (one bad record must never '
        'brick sync)', () async {
      // Regression: the TypeError out of rec.hlc used to escape the applier's
      // per-record handling, roll back the whole page, and leave the engine
      // throwing "pull page failed to commit; token unmoved" — the same page
      // then retried forever, blocking every good record behind it.
      cloud.accept(remotePractice('GOOD', 'kept', remoteHlc(t0 - 2000)));
      final bad = remotePractice('BAD', 'poison', remoteHlc(t0 - 1000));
      cloud.accept(
        SyncRow(
          table: 'practices',
          fields: {...bad.fields, 'hlc_wall': 'not-an-int'},
        ),
      );
      cloud.accept(remotePractice('ALSO', 'behind', remoteHlc(t0 - 500)));

      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(run.applied, 2);
      expect(run.failedRecords, 1);
      expect(await practiceTitle('GOOD'), 'kept');
      expect(await practiceTitle('ALSO'), 'behind');
      expect(await practiceTitle('BAD'), isNull);
      expect(await state.changeToken(), '3', reason: 'sync is not stalled');
      final parked = await quarantine.list();
      expect(parked, hasLength(1));
      expect(parked.single.id, _id('BAD'));
      // The column gate names the offending column at the door, instead of
      // letting the mistyped value fail deep inside apply as a generic
      // 'apply-failed' with nothing to act on.
      expect(parked.single.reason, startsWith('invalid-value'));
    });

    test('a record claiming a BELOW-RANGE schema_v (0 or negative) is '
        'quarantined alone: the rest of the page applies and the token '
        'advances', () async {
      // Wire schema versions start at 1. A below-range claim used to slip
      // between the newer-schema gate (> current) and the full-row gate
      // (== current) and apply with missing columns accepted; now it parks.
      cloud.accept(remotePractice('GOOD', 'kept', remoteHlc(t0 - 3000)));
      final zero = remotePractice('Z', 'poison', remoteHlc(t0 - 2000));
      cloud.accept(
        SyncRow(table: 'practices', fields: {...zero.fields, 'schema_v': 0}),
      );
      final negative = remotePractice('N', 'poison', remoteHlc(t0 - 1000));
      cloud.accept(
        SyncRow(
          table: 'practices',
          fields: {...negative.fields, 'schema_v': -1},
        ),
      );

      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(run.applied, 1);
      expect(run.failedRecords, 2);
      expect(await practiceTitle('GOOD'), 'kept');
      expect(await practiceTitle('Z'), isNull);
      expect(await practiceTitle('N'), isNull);
      expect(await state.changeToken(), '3', reason: 'sync is not stalled');
      final parked = await quarantine.list();
      expect(parked, hasLength(2));
      for (final q in parked) {
        expect(q.reason, startsWith('invalid-schema_v'));
      }
    });

    test('an expired change token (server compacted its log) clears the '
        'token and re-pulls from scratch within the same run', () async {
      cloud.accept(remotePractice('P1', 'one', remoteHlc(t0 - 2000)));
      cloud.accept(remotePractice('P2', 'two', remoteHlc(t0 - 1000)));
      final first = await engine.pumpPull();
      expect(first.applied, 2);
      expect(await state.changeToken(), '2');

      // The server compacts its change log past our token: the next fetch
      // with it throws SyncTokenExpiredException. The engine must clear the
      // token and restart from the beginning of the zone — safe because
      // re-applied rows are LWW no-ops.
      transport.expireNextFetches = 1;
      final recovered = await engine.pumpPull();
      expect(recovered.failed, isFalse);
      expect(recovered.applied, 0);
      expect(recovered.skipped, 2, reason: 're-applied rows settle as skips');
      expect(await practiceTitle('P1'), 'one');
      expect(await practiceTitle('P2'), 'two');
      expect(await state.changeToken(), '2', reason: 'token rebuilt to head');
    });

    test('a transport that rejects even a FRESH token degrades to a normal '
        'failed run instead of spinning clear-and-refetch forever', () async {
      cloud.accept(remotePractice('P', 'one', remoteHlc(t0 - 1000)));
      await engine.pumpPull();
      expect(await state.changeToken(), '1');

      transport.expireNextFetches = 10;
      transport.expireFreshTokensToo = true;
      final before = transport.fetchCalls;
      final run = await engine.pumpPull();
      expect(run.failed, isTrue);
      expect(run.failure, isA<SyncTokenExpiredException>());
      // Exactly two fetches: the expired stored token, then the one bounded
      // retry with a fresh (null) token — never a spin.
      expect(transport.fetchCalls - before, 2);
      expect(
        await state.changeToken(),
        isNull,
        reason:
            'the stored token was already dead; cleared is the honest state',
      );
    });

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

  group('quarantine replay', () {
    test('a fresh device with a slow clock parks EVERY inbound row while the '
        'token marches to head; fixing the clock lets the next pull replay '
        'them all (quarantine is not a black hole)', () async {
      // Rows stamped at "true" time by a healthy remote device.
      cloud.accept(remotePractice('P1', 'one', remoteHlc(t0)));
      cloud.accept(remotePractice('P2', 'two', remoteHlc(t0 + 10)));
      // This device's wall clock is 10 minutes SLOW. Fresh install: empty DB
      // means maxWallEverSeen is 0, so the drift gate judges against the bad
      // local `now` alone and every correctly-stamped row looks far-future.
      nowMs = t0 - 10 * 60 * 1000;

      final bad = await engine.pumpPull();
      expect(bad.failed, isFalse);
      expect(bad.quarantined, 2);
      expect(await practiceTitle('P1'), isNull);
      expect(await quarantine.list(), hasLength(2));
      expect(
        await state.changeToken(),
        '2',
        reason:
            'the token is already PAST the parked rows: the server will '
            'never re-deliver them, only replay can let them in',
      );

      // The clock is corrected. The cloud has nothing new (token at head).
      nowMs = t0 + 1000;
      final recovered = await engine.pumpPull();
      expect(recovered.failed, isFalse);
      expect(await practiceTitle('P1'), 'one');
      expect(await practiceTitle('P2'), 'two');
      expect(await quarantine.list(), isEmpty);
      expect(await oplogCount(), 0, reason: 'replayed applies journal nothing');
    });

    test('an apply-failure record replays once the failure cause is gone '
        '(the schema gains the unknown column)', () async {
      final rec = remotePractice('P', 'future schema', remoteHlc(t0 - 1000));
      cloud.accept(
        SyncRow(
          table: 'practices',
          fields: {...rec.fields, 'brand_new_column': 7},
        ),
      );

      final first = await engine.pumpPull();
      expect(first.failed, isFalse);
      expect(first.failedRecords, 1);
      expect(await practiceTitle('P'), isNull);
      expect(await quarantine.list(), hasLength(1));

      // Simulate the app upgrade that understands the new column.
      await db.customStatement(
        'ALTER TABLE practices ADD COLUMN brand_new_column INTEGER',
      );
      final upgraded = await engine.pumpPull();
      expect(upgraded.failed, isFalse);
      expect(await practiceTitle('P'), 'future schema');
      expect(await quarantine.list(), isEmpty);
    });

    test('a parked row that lost LWW meanwhile replays as a clean no-op and '
        'its entry is removed (LWW settles it even while the original '
        'failure cause persists)', () async {
      // v1 carries a column from the future -> parked on arrival.
      final v1 = remotePractice('P', 'stale v1', remoteHlc(t0 - 2000));
      cloud.accept(
        SyncRow(
          table: 'practices',
          fields: {...v1.fields, 'brand_new_column': 1},
        ),
      );
      final first = await engine.pumpPull();
      expect(first.failedRecords, 1);
      expect(await practiceTitle('P'), isNull);
      expect(await quarantine.list(), hasLength(1));

      // A NEWER, well-formed edit of the same row arrives and applies. The
      // end-of-run replay then finds the parked v1 strictly older than the
      // stored row: the applier's LWW comparison short-circuits BEFORE any
      // write (the unknown column never matters again), the replay is a
      // clean no-op, and the obsolete entry is settled — never a regression
      // of the row, never an entry retried forever.
      cloud.accept(remotePractice('P', 'fresh v2', remoteHlc(t0 - 1000)));
      final second = await engine.pumpPull();
      expect(second.failed, isFalse);
      expect(second.applied, 1);
      expect(
        await practiceTitle('P'),
        'fresh v2',
        reason: 'the stale replay lost LWW cleanly',
      );
      expect(
        await quarantine.list(),
        isEmpty,
        reason: 'the obsolete entry is removed, not retried forever',
      );
    });

    test('parked parent and child replay parent-first (table rank order) '
        'even when the child parked first', () async {
      nowMs = t0 - 10 * 60 * 1000; // slow clock: everything inbound parks
      // The child arrives (and parks) BEFORE its parent: parked-at order is
      // child-first, so replay must re-order by the FK-derived table rank.
      cloud.accept(remoteTrack('T', 'P', remoteHlc(t0)));
      expect((await engine.pumpPull()).quarantined, 1);
      cloud.accept(remotePractice('P', 'parent', remoteHlc(t0 + 10)));
      expect((await engine.pumpPull()).quarantined, 1);
      expect(await quarantine.list(), hasLength(2));

      nowMs = t0 + 1000; // clock fixed
      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(await practiceTitle('P'), 'parent');
      final track = await db
          .customSelect(
            'SELECT id FROM tracks WHERE id = ?',
            variables: [Variable.withString(_id('T'))],
          )
          .getSingleOrNull();
      expect(track, isNotNull, reason: 'the child landed after its parent');
      expect(await quarantine.list(), isEmpty);
    });

    test('an fk-orphan child parked beside its quarantined parent recovers '
        'WHOLE after the upgrade: replay lands both, parent-first', () async {
      // The parent is forward-incompatible — an unknown column stands in for
      // a newer build's schema, the same upgrade seam the replay tests above
      // use — so the schema gate parks it. The child is a perfectly
      // current-schema row: it APPLIES, and the FK sweep must then park it as
      // an fk-orphan rather than hard-delete it. The old sweep deleted it
      // with the token already advanced — the server never re-sent it, and
      // the upgrade replayed the parent from quarantine into a library whose
      // child was permanently gone.
      final parent = remotePractice('P', 'parent', remoteHlc(t0 - 2000));
      cloud.accept(
        SyncRow(
          table: 'practices',
          fields: {...parent.fields, 'brand_new_column': 7},
        ),
      );
      cloud.accept(remoteTrack('T', 'P', remoteHlc(t0 - 1000)));

      final first = await engine.pumpPull();
      expect(first.failed, isFalse);
      expect(first.failedRecords, 2, reason: 'parked parent + swept child');
      expect(await practiceTitle('P'), isNull);
      final parked = await quarantine.list();
      expect(parked, hasLength(2));
      expect(parked.map((q) => q.reason.split(':').first).toSet(), {
        'unknown-column',
        'fk-orphan',
      });

      // Simulate the app upgrade that understands the parent's shape.
      await db.customStatement(
        'ALTER TABLE practices ADD COLUMN brand_new_column INTEGER',
      );
      final upgraded = await engine.pumpPull();
      expect(upgraded.failed, isFalse);
      expect(await practiceTitle('P'), 'parent');
      final track = await db
          .customSelect(
            'SELECT id FROM tracks WHERE id = ?',
            variables: [Variable.withString(_id('T'))],
          )
          .getSingleOrNull();
      expect(
        track,
        isNotNull,
        reason: 'the swept child came back with its parent',
      );
      expect(await quarantine.list(), isEmpty);
    });

    test('a replayed fk-orphan whose parent is STILL missing is re-parked — '
        'never lost, never an error, no growth', () async {
      cloud.accept(remoteTrack('T', 'P', remoteHlc(t0 - 1000)));
      final first = await engine.pumpPull();
      expect(first.failed, isFalse);
      expect(first.failedRecords, 1);
      // Every later pull replays the parked child (start and end of each
      // run): it applies, the sweep finds the parent still missing, and the
      // store's (table, id) upsert REPLACES the entry — no primary-key
      // error, no pile-up, no drop.
      await engine.pumpPull();
      await engine.pumpPull();
      final parked = await quarantine.list();
      expect(parked, hasLength(1));
      expect(parked.single.tbl, 'tracks');
      expect(parked.single.id, QuarantineStore.sweepSlotId(_id('T')));
      expect(parked.single.reason, startsWith('fk-orphan'));
      final track = await db
          .customSelect(
            'SELECT id FROM tracks WHERE id = ?',
            variables: [Variable.withString(_id('T'))],
          )
          .getSingleOrNull();
      expect(track, isNull, reason: 'never live while the parent is missing');
    });

    group('a drift-parked newer state never displaces the FK sweep\'s copy '
        'of the same row', () {
      // T@honest is a plausible child whose parent P has not arrived: it
      // applies and the FK sweep parks its only copy. T@runaway is a newer
      // state of the same row from a peer whose clock runs 6 minutes ahead:
      // the drift gate parks it. Both pages are behind the change token, so
      // the parking lot is the only place either state still exists.
      final honest = remoteHlc(t0 - 1000);
      final runawayStamp = remoteHlc(t0 + 6 * 60 * 1000);
      final runaway = SyncRow(
        table: 'tracks',
        fields: {
          ...remoteTrack('T', 'P', runawayStamp).fields,
          'display_name': 'runaway',
        },
      );

      Future<Map<String, Object?>?> trackRow() async =>
          (await db
                  .customSelect(
                    'SELECT display_name, hlc_packed FROM tracks WHERE id = ?',
                    variables: [Variable.withString(_id('T'))],
                  )
                  .getSingleOrNull())
              ?.data;

      // The parent lands while the runaway clock is still ahead: the honest
      // state is due NOW. Under one shared (table, id) entry the runaway
      // state had evicted it (or filtered its park out), the replay found
      // only a stamp the drift gate still refuses, and the row stayed
      // deleted on this device — while every peer holding the parent showed
      // T@honest — until the wall clock caught up with the bad peer.
      Future<void> parentLandsThenClockCatchesUp() async {
        cloud.accept(remotePractice('P', 'parent', remoteHlc(t0 - 2000)));
        final run = await engine.pumpPull();
        expect(run.failed, isFalse);
        expect(await trackRow(), {
          'display_name': 'take',
          'hlc_packed': honest.packed,
        }, reason: 'the swept honest state lands with its parent');
        final parked = await quarantine.list();
        expect(parked, hasLength(1));
        expect(parked.single.id, _id('T'));
        expect(parked.single.reason, startsWith('hlc-drift'));

        // The clock reaches the runaway stamp: its own replay lands it over
        // the honest state by plain LWW, and the parking lot drains.
        nowMs = runawayStamp.wallMs;
        expect((await engine.pumpPull()).failed, isFalse);
        expect(await trackRow(), {
          'display_name': 'runaway',
          'hlc_packed': runawayStamp.packed,
        });
        expect(await quarantine.list(), isEmpty);
      }

      test('sweep first: a later page\'s drift park leaves the sweep\'s '
          'park in place', () async {
        cloud.accept(remoteTrack('T', 'P', honest));
        expect((await engine.pumpPull()).failedRecords, 1);
        cloud.accept(runaway);
        expect((await engine.pumpPull()).quarantined, 1);
        expect(await trackRow(), isNull);
        expect(await quarantine.list(), hasLength(2));

        await parentLandsThenClockCatchesUp();
      });

      test('same page: the door parks the runaway state BEFORE the '
          'end-of-batch sweep parks the honest one', () async {
        cloud.accept(remoteTrack('T', 'P', honest));
        cloud.accept(runaway);
        final first = await engine.pumpPull();
        expect(first.quarantined, 1);
        expect(first.failedRecords, 1);
        expect(await trackRow(), isNull);
        expect(await quarantine.list(), hasLength(2));

        await parentLandsThenClockCatchesUp();
      });
    });

    test(
      'a batch-internal orphan whose parent arrives on the NEXT page '
      'still converges within the run (the sweep\'s original design case)',
      () async {
        // A peer without the parent-first push sort can upload child-first;
        // with one row per page the child lands alone on page 1.
        cloud.accept(remoteTrack('T', 'P', remoteHlc(t0 - 2000)));
        cloud.accept(remotePractice('P', 'parent', remoteHlc(t0 - 1000)));
        transport.pageSize = 1;

        final run = await engine.pumpPull();
        expect(run.failed, isFalse);
        // Page 1 parked the child (fk-orphan) and advanced the token past it;
        // page 2 delivered the parent. The server does NOT re-send the child
        // (its page is behind the token), so what lands it is the END-OF-RUN
        // quarantine replay: the parked child now finds its parent, applies,
        // and its entry settles.
        expect(await practiceTitle('P'), 'parent');
        final track = await db
            .customSelect(
              'SELECT id FROM tracks WHERE id = ?',
              variables: [Variable.withString(_id('T'))],
            )
            .getSingleOrNull();
        expect(track, isNotNull, reason: 'the run itself converged the orphan');
        expect(await quarantine.list(), isEmpty);
      },
    );

    test('replayed rows are ANNOUNCED via RemoteApplied (a recovered backlog '
        'must refresh open screens, not land invisibly)', () async {
      // Regression: the replay used to apply rows without contributing to the
      // run's announcements, so the primary recovery scenario — a whole
      // first-pull backlog parked by a slow clock, then un-parked by a
      // corrected clock in a run that fetches NOTHING new — landed an entire
      // library with zero RemoteApplied events and no UI refresh.
      cloud.accept(remotePractice('P1', 'one', remoteHlc(t0)));
      cloud.accept(remotePractice('P2', 'two', remoteHlc(t0 + 10)));
      nowMs = t0 - 10 * 60 * 1000; // slow clock: both rows park
      await engine.pumpPull();
      expect(events, isEmpty, reason: 'nothing applied, nothing announced');

      nowMs = t0 + 1000; // clock corrected; the cloud has nothing new
      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(await practiceTitle('P1'), 'one');
      expect(events, hasLength(1));
      expect(events.single.practiceIds, {_id('P1'), _id('P2')});
      expect(events.single.touchesTable('practices'), isTrue);

      // A pull with nothing parked and nothing fetched stays silent.
      events.clear();
      await engine.pumpPull();
      expect(events, isEmpty);
    });

    test('a still-rejected record keeps its entry parked (replay is a bounded '
        'retry, never a drop)', () async {
      cloud.accept(
        remotePractice('FUT', 'poison', remoteHlc(t0 + 6 * 60 * 1000)),
      );
      await engine.pumpPull();
      expect(await quarantine.list(), hasLength(1));
      // The clock has NOT recovered: every later pull replays, re-rejects,
      // and re-parks the same single entry — no growth, no loss.
      await engine.pumpPull();
      await engine.pumpPull();
      final parked = await quarantine.list();
      expect(parked, hasLength(1));
      expect(parked.single.id, _id('FUT'));
      expect(await practiceTitle('FUT'), isNull);
    });
  });

  group('end-of-run repair vs a parked parent (the checker never judges a '
      'row against a state the quarantine already supersedes)', () {
    // Device D deletes track T (cascading a tombstone onto its clip C);
    // device B never saw that delete and, concurrently, edits C and T. Both
    // of B's edits out-rank D's delete under LWW, so the converged state is
    // T live + C live. Every test below delivers B's T edit in a way this
    // device PARKS, while B's C edit applies live under the (still locally
    // tombstoned) T.
    final deleter = _id('DELETER');
    final editor = _id('EDITOR');
    Hlc byD(int wall) => Hlc(wallMs: wall, counter: 0, node: deleter);
    Hlc byB(int wall) => Hlc(wallMs: wall, counter: 0, node: editor);

    Future<Map<String, Object?>> rowOf(String table, String id) async =>
        (await db
                .customSelect(
                  'SELECT * FROM $table WHERE id = ?',
                  variables: [Variable.withString(_id(id))],
                )
                .getSingle())
            .data;

    /// Shared history, pulled: P, X, T and C created, then D's delete.
    Future<void> pullDeletedTrack() async {
      cloud.accept(remoteAsset('X', 'HASH', byD(t0 - 100000)));
      cloud.accept(remotePractice('P', 'song', byD(t0 - 100000)));
      cloud.accept(remoteTrack('T', 'P', byD(t0 - 90000)));
      cloud.accept(remoteClip('C', 'T', 'X', byD(t0 - 90000)));
      cloud.accept(
        remoteTrack('T', 'P', byD(t0 - 60000), deletedAt: t0 - 60000),
      );
      cloud.accept(
        remoteClip('C', 'T', 'X', byD(t0 - 60000), deletedAt: t0 - 60000),
      );
      expect((await engine.pumpPull()).failed, isFalse);
      expect((await rowOf('tracks', 'T'))['deleted_at'], isNotNull);
      expect((await rowOf('clips', 'C'))['deleted_at'], isNotNull);
    }

    /// B's clip edit has APPLIED live under the locally tombstoned T, and
    /// the end-of-run repair left it alone: nothing journaled for C, and a
    /// push puts no bookkeeping tombstone on the server.
    Future<void> expectClipDeferred(Hlc clipEdit) async {
      final c = await rowOf('clips', 'C');
      expect(c['hlc_packed'], clipEdit.packed);
      expect(
        c['deleted_at'],
        isNull,
        reason:
            'the parent\'s NEWER state is parked: the clip is pending, not '
            'orphaned, and must not be tombstoned on a superseded verdict',
      );
      expect(await oplogCount(), 0, reason: 'no repair was journaled');
      expect((await engine.pumpPush()).failed, isFalse);
      expect(
        cloud.serverRowFor('clips', _id('C'))!.fields['deleted_at'],
        isNull,
        reason: 'the fleet must not receive a bookkeeping tombstone for C',
      );
    }

    /// The converged state once the parked parent has landed.
    Future<void> expectBothLive() async {
      expect((await rowOf('tracks', 'T'))['deleted_at'], isNull);
      expect(
        (await rowOf('clips', 'C'))['deleted_at'],
        isNull,
        reason: 'B\'s clip edit won LWW over D\'s delete',
      );
      expect(await quarantine.list(), isEmpty);
      expect(await IntegrityChecker(db, writer).check(), isEmpty);
    }

    test('a parent edit parked at the DRIFT gate defers the child\'s repair; '
        'once the clock catches up both land live everywhere', () async {
      await pullDeletedTrack();
      // B's clock runs ~6 min ahead of this device: its LATER track edit
      // is past the 5-minute drift bound, its earlier clip edit is not.
      final clipEdit = byB(t0 - 1000);
      cloud.accept(remoteTrack('T', 'P', byB(t0 + 6 * 60 * 1000)));
      cloud.accept(remoteClip('C', 'T', 'X', clipEdit, startMs: 500));
      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(run.quarantined, 1, reason: 'T@B parked at the drift gate');
      await expectClipDeferred(clipEdit);

      nowMs += 2 * 60 * 1000; // this device's clock catches up
      expect((await engine.pumpPull()).failed, isFalse);
      await expectBothLive();
    });

    test('a parent edit parked on its SCHEMA defers the child\'s repair until '
        'the upgrade lands it', () async {
      await pullDeletedTrack();
      // An unknown column stands in for a newer build's schema (the same
      // upgrade seam the quarantine-replay tests use).
      final trackEdit = remoteTrack('T', 'P', byB(t0 - 2000));
      cloud.accept(
        SyncRow(
          table: 'tracks',
          fields: {...trackEdit.fields, 'brand_new_column': 7},
        ),
      );
      final clipEdit = byB(t0 - 1000);
      cloud.accept(remoteClip('C', 'T', 'X', clipEdit, startMs: 500));
      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(run.failedRecords, 1, reason: 'T@B parked on its schema');
      await expectClipDeferred(clipEdit);

      await db.customStatement(
        'ALTER TABLE tracks ADD COLUMN brand_new_column INTEGER',
      );
      expect((await engine.pumpPull()).failed, isFalse);
      await expectBothLive();
    });

    test(
      'a parent edit the FK sweep parks and RESTORES to its tombstoned '
      'prior state defers the child\'s repair until the parent lands',
      () async {
        await pullDeletedTrack();
        // B routed T into a new bus Y. Y is parked here on its schema, so B's
        // T edit dangles: the sweep parks it (fk-orphan) and restores T to
        // D's tombstone — the table again holds a superseded parent state.
        final bus = SyncRow(
          table: 'fx_buses',
          fields: {
            'id': _id('Y'),
            'practice_id': _id('P'),
            'name': 'verb',
            'color_value': null,
            'volume': 1.0,
            'pan': 0.0,
            'muted': 0,
            'soloed': 0,
            'output_bus_id': null,
            'fx_json': null,
            'sends_json': null,
            'order_index': 0,
            'brand_new_column': 7,
            ...syncCols(byB(t0 - 3000)),
          },
        );
        cloud.accept(bus);
        final trackEdit = remoteTrack('T', 'P', byB(t0 - 2000));
        cloud.accept(
          SyncRow(
            table: 'tracks',
            fields: {...trackEdit.fields, 'bus_id': _id('Y')},
          ),
        );
        final clipEdit = byB(t0 - 1000);
        cloud.accept(remoteClip('C', 'T', 'X', clipEdit, startMs: 500));
        final run = await engine.pumpPull();
        expect(run.failed, isFalse);
        final parked = {for (final q in await quarantine.list()) q.tbl: q};
        expect(parked['tracks']!.reason, startsWith('fk-orphan'));
        expect((await rowOf('tracks', 'T'))['deleted_at'], isNotNull);
        await expectClipDeferred(clipEdit);

        await db.customStatement(
          'ALTER TABLE fx_buses ADD COLUMN brand_new_column INTEGER',
        );
        expect((await engine.pumpPull()).failed, isFalse);
        await expectBothLive();
        expect((await rowOf('tracks', 'T'))['bus_id'], _id('Y'));
      },
    );

    test('a parked parent state that SETTLES as an LWW skip ends the '
        'deferral: that run repairs even though it applied nothing', () async {
      await pullDeletedTrack();
      final trackEdit = remoteTrack('T', 'P', byB(t0 - 2000));
      cloud.accept(
        SyncRow(
          table: 'tracks',
          fields: {...trackEdit.fields, 'brand_new_column': 7},
        ),
      );
      final clipEdit = byB(t0 - 1000);
      cloud.accept(remoteClip('C', 'T', 'X', clipEdit, startMs: 500));
      await engine.pumpPull();
      expect((await rowOf('clips', 'C'))['deleted_at'], isNull);

      // A local write supersedes the parked state while T stays tombstoned
      // (stand-in: a re-stamp of T through the writer chokepoint). The
      // door park ratcheted the clock, so the local stamp out-ranks T@B.
      final s = writer.stamp();
      expect(s.hlc > byB(t0 - 2000), isTrue);
      await db.transaction(() async {
        await db.customUpdate(
          'UPDATE tracks SET updated_at = ?, hlc_wall = ?, hlc_ctr = ?, '
          'hlc_node = ?, hlc_packed = ? WHERE id = ?',
          variables: [
            Variable.withInt(s.at),
            Variable.withInt(s.hlc.wallMs),
            Variable.withInt(s.hlc.counter),
            Variable.withString(s.hlc.node),
            Variable.withString(s.hlc.packed),
            Variable.withString(_id('T')),
          ],
        );
        await writer.journal(
          table: 'tracks',
          id: _id('T'),
          op: 'tombstone',
          hlc: s.hlc,
          payloadHash: '',
        );
      });

      // Nothing new on the server: the run's only event is the replay
      // settling T@B as a skip. The clip's verdict is no longer deferred —
      // its parent's newest state IS the tombstone — so this run repairs.
      final run = await engine.pumpPull();
      expect(run.failed, isFalse);
      expect(run.applied, 0);
      expect(await quarantine.list(), isEmpty, reason: 'T@B settled');
      expect(
        (await rowOf('clips', 'C'))['deleted_at'],
        isNotNull,
        reason: 'the deferred verdict was re-judged when its park settled',
      );
      expect(await IntegrityChecker(db, writer).check(), isEmpty);
    });
  });

  group('a pointer whose target has not arrived yet is pending, not '
      'dangling (a half-uploaded push)', () {
    // The pusher uploads its backlog in drain windows and chunks, and boot
    // compaction leaves each row only its latest op, so a row can reach the
    // server one request before the row its FK-less pointer names. A puller
    // that catches up in between must not "heal" that pointer: the derived
    // null out-ranks the author's own row and would reach every device.
    final author = _id('AUTHOR');
    Hlc byA(int wall) => Hlc(wallMs: wall, counter: 0, node: author);

    SyncRow practiceWithActive(String interpId, Hlc h) => SyncRow(
      table: 'practices',
      fields: {
        ...remotePractice('P', 'song', h).fields,
        'active_interpretation_id': _id(interpId),
      },
    );

    SyncRow interp(String id, Hlc h) => SyncRow(
      table: 'interpretations',
      fields: {
        'id': _id(id),
        'practice_id': _id('P'),
        'name': 'take 2',
        'order_index': 0,
        ...syncCols(h),
      },
    );

    SyncRow bus(String id, Hlc h, {String? outputBusId}) => SyncRow(
      table: 'fx_buses',
      fields: {
        'id': _id(id),
        'practice_id': _id('P'),
        'name': id,
        'color_value': null,
        'volume': 1.0,
        'pan': 0.0,
        'muted': 0,
        'soloed': 0,
        'output_bus_id': outputBusId == null ? null : _id(outputBusId),
        'fx_json': null,
        'sends_json': null,
        'order_index': 0,
        ...syncCols(h),
      },
    );

    Future<Object?> column(String table, String id, String column) async =>
        (await db
                .customSelect(
                  'SELECT $column AS v FROM $table WHERE id = ?',
                  variables: [Variable.withString(_id(id))],
                )
                .getSingle())
            .data['v'];

    test(
      'an active interpretation that lands on a later upload keeps the '
      'practice pointing at it — nothing journaled, nothing to redo',
      () async {
        cloud.accept(practiceWithActive('I', byA(t0 - 2000)));
        expect((await engine.pumpPull()).failed, isFalse);
        expect(
          await column('practices', 'P', 'active_interpretation_id'),
          _id('I'),
        );
        expect(await oplogCount(), 0, reason: 'no heal may be journaled');

        cloud.accept(interp('I', byA(t0 - 1000)));
        expect((await engine.pumpPull()).failed, isFalse);
        expect(
          await column('practices', 'P', 'active_interpretation_id'),
          _id('I'),
        );
        expect(await oplogCount(), 0);
        expect(await IntegrityChecker(db, writer).check(), isEmpty);
      },
    );

    test(
      'a bus route to a bus that lands on a later upload survives',
      () async {
        cloud.accept(remotePractice('P', 'song', byA(t0 - 3000)));
        cloud.accept(bus('X', byA(t0 - 2000), outputBusId: 'Y'));
        expect((await engine.pumpPull()).failed, isFalse);
        expect(await column('fx_buses', 'X', 'output_bus_id'), _id('Y'));
        expect(await oplogCount(), 0, reason: 'no heal may be journaled');

        cloud.accept(bus('Y', byA(t0 - 1000)));
        expect((await engine.pumpPull()).failed, isFalse);
        expect(await column('fx_buses', 'X', 'output_bus_id'), _id('Y'));
        expect(await oplogCount(), 0);
        expect(await IntegrityChecker(db, writer).check(), isEmpty);
      },
    );
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
      //
      // The in-transaction count is RECORDED and asserted outside: an expect
      // inside the body throws its failure through the transaction, where the
      // rollback's own catch used to swallow it — the assertion could never
      // fail.
      int? pokesInside;
      await expectLater(
        db.transaction(() async {
          final s = writer.stamp();
          await writer.journal(
            table: 'practices',
            id: _id('GONE'),
            op: 'insert',
            hlc: s.hlc,
            payloadHash: 'h',
          );
          // Give a would-be in-transaction notification its chance to land.
          await pumpEventQueue();
          pokesInside = created.length;
          throw StateError('roll it back');
        }),
        throwsStateError,
      );
      expect(pokesInside, 0, reason: 'no poke inside an open txn');
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

  group('undecodable records', () {
    test('a record that never DECODED is parked as a dead letter and the '
        'page still applies and advances', () async {
      // Such a record has no id, table or stamp to route by, so the applier's
      // per-record quarantine cannot reach it: a transport was left choosing
      // between failing the whole page (one bad record bricks sync) and
      // dropping it silently while the token advanced past it.
      cloud.accept(remotePractice('GOOD', 'kept', remoteHlc(t0)));
      transport.pendingRejects.add(
        const RejectedRecord(payload: '{"not":"a row"}', reason: 'missing id'),
      );

      final run = await engine.pumpPull();

      expect(run.applied, 1, reason: 'the good record still lands');
      expect(run.failedRecords, 1);
      expect(await state.changeToken(), isNotNull, reason: 'not stalled');
      final parked = (await quarantine.list()).single;
      expect(parked.reason, contains('undecodable'));
      expect(parked.payload, contains('not'));
      // Never re-applied: the payload is deliberately not a SyncRow envelope.
      expect(() => SyncRow.decode(parked.payload), throwsFormatException);
    });
  });
}
