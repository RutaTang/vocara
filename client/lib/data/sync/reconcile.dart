import 'sync_writer.dart';

/// Counts of what a reconcile actually changed (for tests / diagnostics).
class ReconcileResult {
  int inserts = 0;
  int updates = 0;
  int tombstones = 0;

  /// Rows deliberately NOT written because, under the three-way rules, the
  /// persisted (remote/newer) state wins: a remote-updated row this editor
  /// never touched, a remote-created row absent from a stale aggregate, or a
  /// remote-deleted row whose delete is accepted. Diagnostics only — a skip
  /// emits no write and no oplog entry, so it is not part of [total].
  int skips = 0;

  int get total => inserts + updates + tombstones;
}

/// Reconciles ONE child collection of a saved aggregate against what is
/// persisted, as a THREE-WAY merge, emitting the MINIMAL set of writes through
/// the [SyncWriter] chokepoint.
///
/// The three sides per row id:
///   * BASE  — [baseHashes]\[id\]: the row's content hash when this editor
///     LOADED the aggregate (null = the editor never held the row). The map
///     may be a flat superset covering the whole aggregate: ids are globally
///     unique ULIDs — plus the legacy `<trackId>-c0` clip shape, which carries
///     a '-' no ULID contains — so lookups never cross-contaminate between
///     collections. That is a constraint on how ids are MINTED, not a property
///     of this map, and it is load-bearing: a shared id would let one table's
///     base hash answer for another's row, which decides the rules below and
///     can send an UPDATE down the tombstone branch. A structural test
///     (sync_hashes_test.dart) fails if any two synced tables ever hold the
///     same id.
///   * MINE  — [hashOf](item) for id in [desired]: what the editor wants now.
///   * DB    — [currentHashes]\[id\]: the row's hash as persisted at save time
///     (LIVE rows in this collection's scope only; null = tombstoned or never
///     existed). Local edits never move a child id to a different parent
///     (cross-parent transplants always mint fresh ids), so "not live in this
///     scope" equates to "not live at all".
///
/// The rules (each has a named test in reconcile_three_way_test.dart):
///   1. in desired, DB == base, mine != base  -> UPDATE (a plain local edit —
///      the only write path for edits).
///   2. in desired, DB != base, mine == base  -> SKIP: the DB moved on since
///      load (a remote/newer write) and this editor never touched the row;
///      writing mine would REVERT the newer state under a fresh HLC.
///   3. in desired, DB != base, mine != base  -> TRUE CONFLICT: both sides
///      changed. Policy: the ACTIVE EDITOR wins — the open editor is the
///      intent-bearer, and silently discarding what the user is looking at
///      would be worse than overriding a background change. Mine is written
///      with a fresh HLC (so it also wins entity-level LWW downstream).
///   4. live in DB, NOT in base, NOT in desired -> SKIP: created remotely
///      while this aggregate was open. The editor never saw it, so its absence
///      from [desired] is staleness, not a removal — never tombstone it.
///   5. in base, in desired, NOT live in DB -> deleted remotely while held:
///      if mine == base, accept the delete (skip — reviving an untouched row
///      would resurrect a tombstone); if mine != base it is an edit-vs-delete
///      conflict and the active editor wins — the row is revived through the
///      upsert insert path (deleted_at cleared) with a fresh HLC.
///   6. in base, NOT in desired, live in DB -> the user explicitly removed a
///      row they held: TOMBSTONE, even when DB != base (an explicit delete
///      intent outweighs a concurrent remote edit; the tombstone syncs and
///      wins LWW with its fresh HLC).
///   7. in desired, NOT in base, NOT live in DB -> new local row -> INSERT
///      (an upsert: if the id exists tombstoned — a reused ULID — the row is
///      revived instead of crashing on the primary key).
///   * unchanged (mine == DB) -> NOTHING: no write, no HLC tick, no oplog.
///
/// A no-base legacy save passes baseHashes == currentHashes, which degenerates
/// exactly to the old two-way diff (rules 2/4/5 can then never fire).
///
/// This per-entity change detection is what keeps a fader nudge from dirtying
/// the whole practice and re-syncing it: only rows that truly changed get a
/// new HLC + a journal entry. Run inside the parent's save transaction.
Future<ReconcileResult> reconcileCollection<Item>({
  required SyncWriter writer,
  required String table,
  required Map<String, String> baseHashes,
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

  // Tombstone removed rows FIRST, then insert/update desired ones. Order
  // matters under a partial-UNIQUE index (e.g. one live accompaniment/
  // reference per practice): replacing such a row means the old id is removed
  // and a new id is added with the same (practice_id, kind). Inserting the new
  // before removing the old would momentarily have two live rows -> UNIQUE
  // violation -> the whole save rolls back. Removing first frees the slot.
  // This still holds under the three-way rules: a locally REPLACED slot row
  // was loaded by this editor, so it is in base and is tombstoned here before
  // the insert pass runs. The one case ordering cannot cover is a slot row
  // created REMOTELY while this aggregate was open: rule 4 deliberately keeps
  // it, so a colliding local insert fails the whole save (Result.error at the
  // repository boundary; the caller reverts/reloads). That is the safe side:
  // failing loudly over silently tombstoning a row this editor never saw.
  for (final id in currentHashes.keys) {
    if (desiredIds.contains(id)) continue;
    if (!baseHashes.containsKey(id)) {
      // Rule 4: live in DB, not in base, not desired — created remotely while
      // the aggregate was open. Not a removal; keep it.
      result.skips++;
      continue;
    }
    // Rule 6: the editor held this row and dropped it — an explicit removal.
    // Tombstone even if the DB content moved on since load (delete wins).
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
    final mine = hashOf(item);
    final db = currentHashes[id];
    final base = baseHashes[id];

    if (db == null) {
      // Not live in the DB.
      if (base != null && mine == base) {
        // Rule 5 (untouched side): the editor held the row, never changed it,
        // and it is gone from the DB — deleted remotely while the aggregate
        // was open. Accept the delete; reviving would resurrect a tombstone.
        result.skips++;
        continue;
      }
      // Rule 7 (base == null): a brand-new local row. Or rule 5 (edited
      // side, base != null && mine != base): an edit-vs-delete conflict — the
      // active editor wins and the row is revived. Both go through the same
      // upsert insert path (which clears deleted_at) with a fresh HLC.
      final stamp = writer.stamp();
      await insert(item, stamp);
      await writer.journal(
        table: table,
        id: id,
        op: 'insert',
        hlc: stamp.hlc,
        payloadHash: mine,
      );
      result.inserts++;
    } else if (mine == db) {
      // Unchanged relative to what is persisted (including the case where a
      // remote write already equals mine): emit nothing — no phantom op.
    } else if (base != null && db != base && mine == base) {
      // Rule 2: the DB moved on since load and this editor never touched the
      // row. Keep THEIRS — writing mine would revert the newer state to stale
      // values under a fresh HLC (the regression this three-way merge fixes).
      result.skips++;
    } else {
      // Rule 1 (db == base, mine != base): a plain local edit.
      // Rule 3 (db != base, mine != base): a true conflict — active editor
      // wins, mine is written with a fresh HLC.
      // base == null with a live row: no base knowledge (a no-base save over
      // an existing row) — treated as a conflict, local wins.
      final stamp = writer.stamp();
      await update(item, stamp);
      await writer.journal(
        table: table,
        id: id,
        op: 'update',
        hlc: stamp.hlc,
        payloadHash: mine,
      );
      result.updates++;
    }
  }

  return result;
}
