import '../db/app_database.dart';

/// Typed access to the LOCAL-ONLY `sync_state` key-value table: the server
/// change token, pagination cursors, and misc sync-engine state.
///
/// Every method is a plain drift operation on the shared [AppDatabase], so it
/// COMPOSES into a caller-provided `db.transaction(...)` (drift transactions
/// are zone-scoped). That composability is load-bearing: the change token must
/// commit atomically WITH the batch of remote records it acknowledges —
/// persisting it outside that transaction could acknowledge records the crash
/// rolled back (lost updates) or re-apply a page forever.
class SyncStateStore {
  SyncStateStore(this._db);

  final AppDatabase _db;

  /// The server change token (opaque, transport-encoded string).
  static const String changeTokenKey = 'change_token';

  /// Set (durably, BEFORE the first outbound wire call ever made) once a sync
  /// transport has existed for this library. Its absence is a proof: no push
  /// was ever even attempted, so no server can possibly hold any of this
  /// device's rows — which is what lets the asset GC's pending-journal guard
  /// stand down on never-synced libraries (see AssetStore.sweepUnreferenced)
  /// instead of deferring local disk reclaim forever.
  static const String transportSeenKey = 'transport_seen';

  /// Whether a sync transport has ever been active for this library.
  Future<bool> transportSeen() async => await get(transportSeenKey) != null;

  /// Records that a transport exists. Must complete BEFORE the first wire
  /// call, so a crash mid-push can never leave a server-held row in a library
  /// that still claims it never had a transport.
  Future<void> markTransportSeen() => set(transportSeenKey, '1');

  /// Generic get: null when the key was never set.
  Future<String?> get(String key) async {
    final row = await (_db.select(
      _db.syncState,
    )..where((s) => s.key.equals(key))).getSingleOrNull();
    return row?.value;
  }

  /// Generic upsert.
  Future<void> set(String key, String value) => _db
      .into(_db.syncState)
      .insertOnConflictUpdate(
        SyncStateCompanion.insert(key: key, value: value),
      );

  /// Generic delete (a missing key is a no-op).
  Future<void> remove(String key) =>
      (_db.delete(_db.syncState)..where((s) => s.key.equals(key))).go();

  // Typed change-token accessors.
  Future<String?> changeToken() => get(changeTokenKey);
  Future<void> setChangeToken(String token) => set(changeTokenKey, token);

  /// Drops the token, forcing the next pull to start from scratch (the server
  /// signals an expired token this way).
  Future<void> clearChangeToken() => remove(changeTokenKey);
}
