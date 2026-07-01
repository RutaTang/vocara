import 'sync_writer.dart';

/// Counts of what a reconcile actually changed (for tests / diagnostics).
class ReconcileResult {
  int inserts = 0;
  int updates = 0;
  int tombstones = 0;

  int get total => inserts + updates + tombstones;
}

/// Reconciles ONE child collection of a saved aggregate against what is already
/// persisted, emitting the MINIMAL set of writes through the [SyncWriter]
/// chokepoint:
///   - a new id  -> INSERT (an upsert: if the id was previously soft-deleted
///     and its ULID is reused, this revives the tombstoned row instead of
///     crashing on a primary-key collision)
///   - an existing id whose content hash changed -> UPDATE
///   - an existing id that is no longer desired  -> TOMBSTONE (never hard-delete)
///   - an unchanged id -> NOTHING (no HLC tick, no oplog)
///
/// This per-entity change detection is what keeps a fader nudge from dirtying
/// the whole practice and re-syncing it: only rows that truly changed get a new
/// HLC + a journal entry. Run inside the parent's save transaction.
///
/// [currentHashes] maps each currently-persisted LIVE row id to its content
/// hash (computed by the caller from the loaded rows with the SAME projection
/// as [hashOf]); [desired] is the wanted set.
Future<ReconcileResult> reconcileCollection<Item>({
  required SyncWriter writer,
  required String table,
  required Map<String, String> currentHashes,
  required List<Item> desired,
  required String Function(Item item) idOf,
  required String Function(Item item) hashOf,
  required Future<void> Function(Item item, SyncStamp stamp) insert,
  required Future<void> Function(Item item, SyncStamp stamp) update,
  required Future<void> Function(String id, SyncStamp stamp) tombstone,
}) async {
  final result = ReconcileResult();
  final desiredIds = {for (final item in desired) idOf(item)};

  // Tombstone removed rows FIRST, then insert/update desired ones. Order matters
  // under a partial-UNIQUE index (e.g. one live accompaniment/reference per
  // practice): replacing such a row means the old id is removed and a new id is
  // added with the same (practice_id, kind). Inserting the new before removing
  // the old would momentarily have two live rows -> UNIQUE violation -> the
  // whole save rolls back. Removing first frees the slot.
  for (final id in currentHashes.keys) {
    if (desiredIds.contains(id)) continue;
    final stamp = writer.stamp();
    await tombstone(id, stamp);
    await writer.journal(
      table: table,
      id: id,
      op: 'tombstone',
      hlc: stamp.hlc,
      payloadHash: '',
    );
    result.tombstones++;
  }

  for (final item in desired) {
    final id = idOf(item);
    final prev = currentHashes[id];
    final hash = hashOf(item);
    if (prev == null) {
      final stamp = writer.stamp();
      await insert(item, stamp);
      await writer.journal(
        table: table,
        id: id,
        op: 'insert',
        hlc: stamp.hlc,
        payloadHash: hash,
      );
      result.inserts++;
    } else if (prev != hash) {
      final stamp = writer.stamp();
      await update(item, stamp);
      await writer.journal(
        table: table,
        id: id,
        op: 'update',
        hlc: stamp.hlc,
        payloadHash: hash,
      );
      result.updates++;
    }
    // else: unchanged — emit nothing.
  }

  return result;
}
