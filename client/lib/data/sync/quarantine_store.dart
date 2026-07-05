import 'package:drift/drift.dart';

import '../db/app_database.dart';

/// Persistence for the LOCAL-ONLY `sync_quarantine` parking lot: the LATEST
/// rejected remote state per (table, id) — see tables.dart for the schema and
/// the remote_hlc_packed naming rationale.
///
/// Like [SyncStateStore], every method is a plain drift operation so it joins
/// a caller-provided transaction: the [SyncApplier] persists a quarantine
/// inside the SAME outer transaction as the batch apply, keeping the parking
/// lot exactly consistent with what actually committed.
class QuarantineStore {
  QuarantineStore(this._db);

  final AppDatabase _db;

  /// Upserts the LATEST quarantined state for ([table], [id]): the composite
  /// primary key makes a re-quarantine of the same row replace the older one,
  /// so the parking lot holds one (the newest) rejected state per row instead
  /// of growing without bound on every failed pull.
  Future<void> put({
    required String table,
    required String id,
    required String hlcPacked,
    required int schemaV,
    required String payload,
    required String reason,
    required int quarantinedAt,
  }) => _db
      .into(_db.syncQuarantine)
      .insertOnConflictUpdate(
        SyncQuarantineCompanion.insert(
          tbl: table,
          id: id,
          remoteHlcPacked: hlcPacked,
          schemaV: schemaV,
          payload: payload,
          reason: reason,
          quarantinedAt: quarantinedAt,
        ),
      );

  /// All quarantined rows, oldest quarantine first (stable review order).
  Future<List<SyncQuarantineRow>> list() =>
      (_db.select(_db.syncQuarantine)..orderBy([
            (q) => OrderingTerm.asc(q.quarantinedAt),
            (q) => OrderingTerm.asc(q.tbl),
            (q) => OrderingTerm.asc(q.id),
          ]))
          .get();

  /// Removes one parked row (after a successful re-apply or a manual discard).
  /// A missing row is a no-op.
  Future<void> delete(String table, String id) => (_db.delete(
    _db.syncQuarantine,
  )..where((q) => q.tbl.equals(table) & q.id.equals(id))).go();
}
