import 'package:drift/drift.dart';

import '../db/app_database.dart';

/// Persistence for the LOCAL-ONLY `sync_quarantine` parking lot: the NEWEST
/// (by HLC) rejected remote state per (table, id) — see tables.dart for the
/// schema and the remote_hlc_packed naming rationale.
///
/// Like [SyncStateStore], every method is a plain drift operation so it joins
/// a caller-provided transaction: the [SyncApplier] persists a quarantine
/// inside the SAME outer transaction as the batch apply, keeping the parking
/// lot exactly consistent with what actually committed.
class QuarantineStore {
  QuarantineStore(this._db);

  final AppDatabase _db;

  /// Upserts the quarantined state for ([table], [id]), keeping the NEWEST
  /// state per row by [hlcPacked] — the store itself is Last-Writer-Wins, not
  /// last-caller-wins. The composite primary key already bounds the lot to
  /// one entry per row; the HLC guard decides WHICH state that entry holds:
  ///
  ///  * a strictly newer [hlcPacked] replaces the stored entry;
  ///  * an EQUAL [hlcPacked] also replaces it (a refreshed reason/payload for
  ///    the same state — the fk-orphan re-park flow re-parks the identical
  ///    snapshot every replay cycle and must keep working);
  ///  * a strictly OLDER [hlcPacked] is a deliberate no-op. Parks are not
  ///    ordered by arrival: a page can legally deliver P@1000 (parked at the
  ///    door) before the end-of-batch FK sweep parks the current db row
  ///    P@950, and the round-6 park+restore can put a live row BACK on an
  ///    older HLC — so ordering alone can never protect the newest state.
  ///    Dropping the older park loses nothing: under LWW the replay of the
  ///    newer parked snapshot supersedes anything the older state could have
  ///    contributed (the older state would lose that same LWW settle anyway).
  ///
  /// The SQL-level `ON CONFLICT ... DO UPDATE ... WHERE` makes the guard
  /// race-free inside the caller's transaction (no read-modify-write gap),
  /// and a filtered-out update is silently kept as the old row, not an error.
  Future<void> put({
    required String table,
    required String id,
    required String hlcPacked,
    required int schemaV,
    required String payload,
    required String reason,
    required int quarantinedAt,
  }) {
    final row = SyncQuarantineCompanion.insert(
      tbl: table,
      id: id,
      remoteHlcPacked: hlcPacked,
      schemaV: schemaV,
      payload: payload,
      reason: reason,
      quarantinedAt: quarantinedAt,
    );
    return _db
        .into(_db.syncQuarantine)
        .insert(
          row,
          onConflict: DoUpdate(
            (old) => row,
            // Overwrite only when the stored state is not newer than the
            // incoming one (packed HLCs compare lexically).
            where: (old) =>
                old.remoteHlcPacked.isSmallerOrEqualValue(hlcPacked),
          ),
        );
  }

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

  /// Removes one parked row ONLY while it still holds [hlcPacked] — the
  /// settle-then-delete used by the quarantine replay. The replay lists,
  /// applies, then deletes; a NEWER state for the same (table, id) can be
  /// parked between the apply and the delete (a concurrent pull page), and
  /// the unconditional delete threw that newer state away with the settled
  /// one. Conditioning on the stamp closes the TOCTOU: the newer entry
  /// simply survives for the next replay cycle.
  Future<void> deleteIfStamp(String table, String id, String hlcPacked) =>
      (_db.delete(_db.syncQuarantine)..where(
            (q) =>
                q.tbl.equals(table) &
                q.id.equals(id) &
                q.remoteHlcPacked.equals(hlcPacked),
          ))
          .go();
}
