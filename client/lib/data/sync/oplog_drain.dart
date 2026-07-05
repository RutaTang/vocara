import 'package:drift/drift.dart';

import '../db/app_database.dart';

/// One pending push item: the LATEST journaled op for a (table, id) within the
/// drained window, with the bookkeeping the pusher needs.
class PendingOplogEntry {
  const PendingOplogEntry({
    required this.seq,
    required this.table,
    required this.id,
    required this.op,
    required this.hlcPacked,
    required this.payloadHash,
    required this.dependsOnAssetId,
  });

  /// The LATEST raw oplog sequence collapsed into this entry.
  final int seq;

  final String table;
  final String id;

  /// insert | update | tombstone — the latest op wins the collapse, so a
  /// tombstone that is followed by nothing is never lost.
  final String op;

  /// The packed HLC of the latest journaled mutation (the LWW key the pushed
  /// row state will carry).
  final String hlcPacked;

  /// The latest entry's content hash (empty for tombstones, by convention).
  final String payloadHash;

  /// Asset gate: the pusher must hold this row back until the asset's bytes
  /// are uploaded. Collapsing keeps the latest NON-null value seen for the row
  /// (a later content-only update must not shed the insert's dependency and
  /// let the row race ahead of its bytes).
  final String? dependsOnAssetId;
}

/// One [OplogDrain.readPending] result: the collapsed entries plus the ack
/// cover for everything the read consumed.
class OplogDrainBatch {
  const OplogDrainBatch({required this.entries, required this.maxSeq});

  static const OplogDrainBatch empty = OplogDrainBatch(entries: [], maxSeq: 0);

  /// Collapsed to the latest per (table, id), ordered by ascending [PendingOplogEntry.seq].
  final List<PendingOplogEntry> entries;

  /// The highest RAW sequence this read consumed — including entries collapsed
  /// away as superseded — so `ack(maxSeq)` acknowledges exactly this read and
  /// can never touch entries journaled AFTER it (they get higher sequences).
  /// 0 when the batch is empty (ack(0) deletes nothing).
  final int maxSeq;

  bool get isEmpty => entries.isEmpty;
}

/// Reads and acknowledges the durability ledger for the sync pusher.
///
/// The oplog is append-only under the SyncWriter invariant (an entry exists
/// iff its row mutation committed); this drain is its ONLY consumer. The
/// read/ack split is deliberately two-phase: the pusher reads a batch, pushes
/// it, and only acks after the server durably accepted it — a crash between
/// the two re-pushes the same rows (idempotent under LWW) instead of losing
/// them.
class OplogDrain {
  OplogDrain(this._db);

  final AppDatabase _db;

  /// Reads up to [limit] RAW oplog entries (oldest first) and collapses them
  /// to the latest per (table, id). The pusher pushes CURRENT row state, so
  /// intermediate ops are redundant; the latest op wins, which preserves a
  /// trailing tombstone. [limit] bounds the raw scan (not the collapsed
  /// count), so [OplogDrainBatch.maxSeq] is always a contiguous ack cover: a
  /// row's newer entries beyond the window survive the ack and are drained by
  /// the next read.
  Future<OplogDrainBatch> readPending({int limit = 500}) async {
    if (limit <= 0) return OplogDrainBatch.empty;
    // Filter on synced = 0 to ride the ix_oplog_unsynced index. Nothing sets
    // synced today (ack DELETES), so this reads the whole ledger in seq order.
    final rows =
        await (_db.select(_db.oplog)
              ..where((o) => o.synced.equals(false))
              ..orderBy([(o) => OrderingTerm.asc(o.seq)])
              ..limit(limit))
            .get();
    if (rows.isEmpty) return OplogDrainBatch.empty;

    var maxSeq = 0;
    final latest = <(String, String), PendingOplogEntry>{};
    for (final row in rows) {
      if (row.seq > maxSeq) maxSeq = row.seq;
      final key = (row.entityTable, row.entityId);
      final prev = latest[key];
      latest[key] = PendingOplogEntry(
        seq: row.seq,
        table: row.entityTable,
        id: row.entityId,
        op: row.op,
        hlcPacked: row.hlcPacked,
        payloadHash: row.payloadHash,
        // Keep the latest non-null dependency (see PendingOplogEntry docs).
        dependsOnAssetId: row.dependsOnAssetId ?? prev?.dependsOnAssetId,
      );
    }
    final entries = latest.values.toList()
      ..sort((a, b) => a.seq.compareTo(b.seq));
    return OplogDrainBatch(entries: entries, maxSeq: maxSeq);
  }

  /// Deletes every oplog entry with seq <= [uptoSeq], in one transaction.
  /// Call ONLY with a [OplogDrainBatch.maxSeq] whose entries were durably
  /// pushed: sequences are monotonic (AUTOINCREMENT), so entries journaled
  /// after the read carry higher sequences and are untouched. Returns the
  /// number of entries deleted.
  Future<int> ack(int uptoSeq) {
    return _db.transaction(
      () => (_db.delete(
        _db.oplog,
      )..where((o) => o.seq.isSmallerOrEqualValue(uptoSeq))).go(),
    );
  }
}
