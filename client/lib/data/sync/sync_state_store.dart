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
