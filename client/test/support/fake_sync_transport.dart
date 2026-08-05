import 'dart:async';

import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/data/sync/sync_transport.dart';

/// One accepted row in the fake cloud's ordered log.
class FakeCloudEntry {
  FakeCloudEntry(this.seq, this.row);

  /// Monotonically increasing server sequence (1-based). The change token is
  /// simply the highest seq a fetch has covered, as a string.
  final int seq;
  final SyncRow row;
}

/// The shared in-memory "cloud" N [FakeSyncTransport]s (one per simulated
/// device) push to and pull from: an append-only, seq-ordered log of accepted
/// rows plus the latest accepted record per (table, id) for conflict checks.
///
/// DETERMINISTIC by construction: no wall clock, no randomness — the only
/// state transitions are the calls the tests (and the engines under test)
/// make, in the order they make them.
class FakeCloud {
  final List<FakeCloudEntry> log = [];

  /// The server's current record per (table, id): the LAST ACCEPTED push,
  /// like a real sync server — it knows change tags, not HLCs, so it never
  /// resolves conflicts itself.
  final Map<(String, String), SyncRow> latest = {};

  int get latestSeq => log.length;

  /// Accepts one row: appends to the log and replaces the current record.
  /// Also used by tests to seed server state directly.
  FakeCloudEntry accept(SyncRow row) {
    final entry = FakeCloudEntry(log.length + 1, row);
    log.add(entry);
    latest[(row.table, row.id)] = row;
    return entry;
  }

  SyncRow? serverRowFor(String table, String id) => latest[(table, id)];
}

/// A pushRows call parked by [FakeSyncTransport.holdPushes], waiting for the
/// test to pump it.
class _HeldPush {
  _HeldPush(this.rows);

  final List<SyncRow> rows;
  final Completer<PushAck> completer = Completer<PushAck>();
}

/// In-memory [SyncTransport] over a shared [FakeCloud] — one instance per
/// simulated device. Fault-injection knobs (all OFF by default, all driven
/// explicitly by tests — nothing is time- or randomness-based):
///
///  * [partitioned] — every call throws [SyncTransportException] (device
///    offline).
///  * [holdPushes] — pushRows parks instead of completing; the test calls
///    [releaseHeldPushes] to land the rows and complete the acks (models
///    delivery delay; lets a test interleave work "during" a push).
///  * [pageSize] — fetchChanges page size (hasMore paging).
///  * [injectConflictOnce] — the next push of that (table, id) is CAS-checked
///    against the server's CURRENT row and rejected with it iff that row is
///    strictly newer by hlcPacked, exactly the one conflict a conforming
///    server may raise (docs/sync-protocol.md); against an older-or-equal
///    held row (or no row at all) the injection is consumed but the push is
///    accepted. A NON-conforming conflict (older/equal/mismatched serverRow)
///    is modeled with [mangleNextAck] instead — the engine must refuse it
///    (SyncEngine._validateConflict).
///  * [redeliverNextFetches] — the next N fetchChanges calls re-serve the
///    previously served page verbatim (same rows, same nextToken): the
///    at-least-once duplicate-delivery case.
///  * [expireNextFetches] — the next N fetchChanges calls throw
///    [SyncTokenExpiredException] (the server compacted its change log past
///    the token, HTTP 410). By default only NON-null tokens expire — a
///    from-scratch re-pull is honored, like a real server; set
///    [expireFreshTokensToo] to reject null tokens as well (exercises the
///    engine's refuse-to-spin guard).
class FakeSyncTransport implements SyncTransport {
  FakeSyncTransport(this.cloud, {this.name = 'device'});

  final FakeCloud cloud;

  /// Label for assertion messages.
  final String name;

  bool partitioned = false;
  int pageSize = 200;
  bool holdPushes = false;
  int redeliverNextFetches = 0;
  int expireNextFetches = 0;
  bool expireFreshTokensToo = false;

  /// When set, the NEXT push's ack is passed through this transform before it
  /// is returned, then the knob clears itself. Models a buggy or hostile
  /// server whose ack does not match the pushed rows (short, wrong-row,
  /// duplicated results) — the engine must refuse to ack its oplog on it.
  PushAck Function(List<SyncRow> pushed, PushAck ack)? mangleNextAck;

  final List<_HeldPush> _heldPushes = [];
  final Map<(String, String), int> _conflictOnce = {};
  FetchPage? _lastServedPage;

  // Observability for tests.
  int pushCalls = 0;
  int fetchCalls = 0;
  final List<List<SyncRow>> pushedBatches = [];

  /// Number of pushes currently parked by [holdPushes].
  int get heldPushCount => _heldPushes.length;

  /// Arms a CAS check for the next [times] pushes of ([table], [id]): each
  /// is rejected with the server's current row iff that row is strictly
  /// newer (see the class docs — the fake stays protocol-conforming).
  void injectConflictOnce(String table, String id, {int times = 1}) {
    _conflictOnce[(table, id)] = times;
  }

  /// Lands every parked push (in arrival order) and completes its ack.
  Future<void> releaseHeldPushes() async {
    final parked = List<_HeldPush>.from(_heldPushes);
    _heldPushes.clear();
    for (final p in parked) {
      p.completer.complete(_accept(p.rows));
    }
    // Let the completions propagate before the test continues.
    await Future<void>.delayed(Duration.zero);
  }

  @override
  Future<PushAck> pushRows(List<SyncRow> rows) {
    pushCalls++;
    pushedBatches.add(List.unmodifiable(rows));
    if (partitioned) {
      throw SyncTransportException('$name is partitioned (push)');
    }
    if (holdPushes) {
      final held = _HeldPush(List.of(rows));
      _heldPushes.add(held);
      return held.completer.future;
    }
    return Future.value(_accept(rows));
  }

  PushAck _accept(List<SyncRow> rows) {
    final results = <PushRowResult>[];
    for (final row in rows) {
      final key = (row.table, row.id);
      final remaining = _conflictOnce[key] ?? 0;
      final serverRow = cloud.serverRowFor(row.table, row.id);
      if (remaining > 0 && serverRow != null) {
        _conflictOnce[key] = remaining - 1;
        // Conforming CAS: reject ONLY when the held row is strictly newer
        // (docs/sync-protocol.md). An older-or-equal held row falls through
        // to acceptance — a real server never conflicts on those, and the
        // engine now refuses such an ack as a protocol violation.
        if (serverRow.hlcPacked.compareTo(row.hlcPacked) > 0) {
          results.add(PushRowResult.conflicted(row, serverRow: serverRow));
          continue;
        }
      }
      cloud.accept(row);
      results.add(PushRowResult.accepted(row));
    }
    final ack = PushAck(results);
    final mangle = mangleNextAck;
    if (mangle != null) {
      mangleNextAck = null;
      return mangle(rows, ack);
    }
    return ack;
  }

  @override
  Future<FetchPage> fetchChanges(String? sinceToken) {
    fetchCalls++;
    if (partitioned) {
      throw SyncTransportException('$name is partitioned (fetch)');
    }
    if (expireNextFetches > 0 && (sinceToken != null || expireFreshTokensToo)) {
      expireNextFetches--;
      throw SyncTokenExpiredException(
        '$name: change token "$sinceToken" is no longer honorable',
      );
    }
    if (redeliverNextFetches > 0 && _lastServedPage != null) {
      redeliverNextFetches--;
      return Future.value(_lastServedPage!);
    }
    final from = sinceToken == null ? 0 : int.parse(sinceToken);
    final slice = <FakeCloudEntry>[];
    for (final e in cloud.log) {
      if (e.seq <= from) continue;
      slice.add(e);
      if (slice.length >= pageSize) break;
    }
    final lastSeq = slice.isEmpty ? from : slice.last.seq;
    final page = FetchPage(
      rows: [for (final e in slice) e.row],
      nextToken: '$lastSeq',
      hasMore: lastSeq < cloud.latestSeq,
    );
    _lastServedPage = page;
    return Future.value(page);
  }

  @override
  Future<void> pushAssetBytes(String contentHash, String filePath) {
    throw UnimplementedError('asset bytes sync is P2');
  }

  @override
  Future<void> fetchAssetBytes(String contentHash, String destPath) {
    throw UnimplementedError('asset bytes sync is P2');
  }
}
