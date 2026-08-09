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

  /// The highest packed HLC this device has ever ACCEPTED from a peer —
  /// including stamps whose record never became a row.
  ///
  /// [Persistence.seedClock] restores boot monotonicity from the max
  /// hlc_packed held in the tables, which covers every stamp that landed as
  /// data. It does not cover a stamp the device verified and merged into its
  /// live clock but whose record was then parked: an FK-orphan's snapshot
  /// lives on in `sync_quarantine`, whose stamp column is deliberately NOT
  /// named hlc_packed (a quarantine also holds implausibly-future stamps that
  /// must never seed a clock — see tables.dart). So the one legitimate
  /// high-water mark those parks contribute survived only in memory, and a
  /// restart lost it: a local edit could then take a LOWER stamp than the
  /// parked record's, and the quarantine replay would silently overwrite the
  /// user's newer edit under LWW. Recording the mark here — from stamps that
  /// already passed the plausibility gate, and only ever upward — closes that
  /// window without letting a suspect stamp anywhere near the clock.
  static const String hlcHighWaterKey = 'hlc_high_water';

  /// Raises the durable high-water mark to [packed] if it is higher.
  ///
  /// Composes into the caller's transaction like everything else here, and
  /// the guard lives in the SQL so concurrent writers cannot lose an update
  /// through a read-modify-write gap. Packed HLCs are fixed-width and
  /// lexicographically ordered, so a string comparison IS the HLC ordering.
  Future<void> raiseHlcHighWater(String packed) => _db.customStatement(
    'INSERT INTO sync_state (key, value) VALUES (?, ?) '
    'ON CONFLICT(key) DO UPDATE SET value = excluded.value '
    'WHERE excluded.value > sync_state.value',
    [hlcHighWaterKey, packed],
  );

  /// The durable high-water mark, or null before any remote stamp was
  /// accepted (see [hlcHighWaterKey]).
  Future<String?> hlcHighWater() => get(hlcHighWaterKey);

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
