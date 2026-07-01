import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';

/// A row pulled from the cloud, ready to apply: the table name plus every synced
/// column (including the HLC quad, deleted_at and schema_v) as a value map.
class RemoteRecord {
  RemoteRecord({required this.table, required this.columns});

  final String table;
  final Map<String, Object?> columns;

  String get id => columns['id'] as String;
  String get hlcPacked => columns['hlc_packed'] as String;
  Hlc get hlc => Hlc(
    wallMs: columns['hlc_wall'] as int,
    counter: columns['hlc_ctr'] as int,
    node: columns['hlc_node'] as String,
  );
}

class ApplyResult {
  int applied = 0;
  int skipped = 0; // lost the LWW comparison (we already hold something newer)
  int quarantined = 0; // implausibly-future clock; not applied
  int failed = 0; // threw while applying (malformed / forward-incompatible row)

  @override
  String toString() =>
      'ApplyResult(applied: $applied, skipped: $skipped, '
      'quarantined: $quarantined, failed: $failed)';
}

/// The "remote door": applies records pulled from sync into the local store.
///
/// Distinct from local writes (which go through SyncWriter): a remote change
/// ADOPTS the remote HLC (it does not tick), wins only by entity-level
/// Last-Writer-Wins on the HLC, and is NEVER journaled — so an applied remote
/// change can't be re-pushed into an infinite ping-pong. The clock is advanced
/// via [HlcClock.receive] so subsequent LOCAL writes stay causally after
/// everything we've seen; a record whose clock is implausibly far in the future
/// is quarantined instead of merged (one bad peer can't poison the fleet).
///
/// Apply is order-tolerant and per-record resilient: defer_foreign_keys lets a
/// child arrive before its parent within the batch, and each record applies
/// inside its own SAVEPOINT so one malformed / forward-incompatible record is
/// quarantined ([ApplyResult.failed]) rather than rolling back every other record.
/// Because defer_foreign_keys pushes FK enforcement to the OUTER commit (outside
/// every savepoint), a record whose parent never arrives in the batch would fail
/// the whole commit — so before committing we sweep foreign_key_check and quarantine
/// just the still-orphaned rows, and the outer transaction is wrapped so a commit
/// failure degrades to a failed batch instead of throwing out of apply().
class SyncApplier {
  SyncApplier(this.db, this.clock);

  final AppDatabase db;
  final HlcClock clock;

  Future<ApplyResult> apply(List<RemoteRecord> records) async {
    final result = ApplyResult();
    try {
      await db.transaction(() async {
        await db.customStatement('PRAGMA defer_foreign_keys = ON');
        for (final rec in records) {
          try {
            clock.receive(rec.hlc);
          } on HlcDriftException {
            result.quarantined++;
            continue;
          }
          try {
            // Apply each record inside a nested transaction (SQLite SAVEPOINT) so a
            // single malformed / forward-incompatible record — a column from a newer
            // schema_v, a CHECK/NOT NULL violation, an unknown table — rolls back ONLY
            // itself and is quarantined, instead of throwing out of the batch and
            // discarding every other (good) record. Counters are updated OUTSIDE the
            // savepoint so a rollback can't desync them.
            final applied = await db.transaction(() async {
              final existing = await db
                  .customSelect(
                    'SELECT hlc_packed FROM ${rec.table} WHERE id = ?',
                    variables: [Variable.withString(rec.id)],
                  )
                  .getSingleOrNull();
              final wins = existing == null ||
                  rec.hlcPacked.compareTo(existing.read<String>('hlc_packed')) >
                      0;
              if (!wins) return false;
              // Clear any single-slot conflict BEFORE writing. The single-slot track
              // kinds (one live accompaniment/reference per practice) carry a partial
              // UNIQUE index; inserting a second LIVE row for an occupied slot raises a
              // UNIQUE violation. Resolving here (and the whole write) is in this
              // record's savepoint, so a failure rolls back the occupant tombstone too.
              final columns = await _resolveSlot(rec);
              if (existing == null) {
                await _insert(rec.table, columns);
              } else {
                await _update(rec.table, columns);
              }
              return true;
            });
            if (applied) {
              result.applied++;
            } else {
              result.skipped++;
            }
          } catch (_) {
            result.failed++;
          }
        }
        // defer_foreign_keys deferred every FK check to THIS outer commit, so a
        // record whose parent never arrived in the batch (truly orphaned /
        // forward-incompatible) would fail the whole commit and poison every good
        // record. Sweep for rows that STILL violate an FK — order-tolerance means a
        // child inserted before its parent is fine once the parent is present — and
        // hard-delete just those (they re-arrive on a later page). Loop because
        // deleting an orphan can strand its own children.
        while (true) {
          final violations =
              await db.customSelect('PRAGMA foreign_key_check').get();
          if (violations.isEmpty) break;
          for (final v in violations) {
            final table = v.read<String>('table');
            final rowid = v.read<int>('rowid');
            await db.customStatement(
              'DELETE FROM $table WHERE rowid = ?',
              [rowid],
            );
            if (result.applied > 0) result.applied--;
            result.failed++;
          }
        }
      });
    } catch (_) {
      // The outer commit failed despite the per-record savepoints and the FK sweep
      // (e.g. a deferred constraint we can't attribute to a single row). The whole
      // transaction rolled back, so nothing except the DB-untouched quarantines
      // stuck. Report the batch as failed rather than throwing out of apply(), which
      // would lose the ApplyResult and stall sync on an endless retry of this page.
      result.applied = 0;
      result.skipped = 0;
      result.failed = records.length - result.quarantined;
    }
    return result;
  }

  /// Returns the column map to write for [rec] after resolving any single-slot conflict by
  /// entity-level LWW: if [rec] is a LIVE accompaniment/reference whose (practice, kind)
  /// slot is held by a DIFFERENT live row, the higher hlc_packed keeps the slot live and
  /// the loser is tombstoned. When [rec] is the loser, the returned map is tombstoned so
  /// the row is still stored (a later sync can update it) without occupying the live slot.
  Future<Map<String, Object?>> _resolveSlot(RemoteRecord rec) async {
    if (rec.table != 'tracks' || rec.columns['deleted_at'] != null) {
      return rec.columns;
    }
    final kind = rec.columns['kind'];
    if (kind != 'accompaniment' && kind != 'reference') return rec.columns;
    final practiceId = rec.columns['practice_id'];
    if (practiceId is! String) return rec.columns;
    final occupant = await db
        .customSelect(
          'SELECT id, hlc_packed FROM tracks WHERE practice_id = ? AND kind = ? '
          'AND deleted_at IS NULL AND id <> ?',
          variables: [
            Variable.withString(practiceId),
            Variable.withString(kind as String),
            Variable.withString(rec.id),
          ],
        )
        .getSingleOrNull();
    if (occupant == null) return rec.columns;
    // A plausible tombstone time: the incoming record's wall clock.
    final tombstoneAt = rec.columns['hlc_wall'];
    if (rec.hlcPacked.compareTo(occupant.read<String>('hlc_packed')) > 0) {
      // Incoming wins → free the slot by tombstoning the current occupant.
      await db.customStatement(
        'UPDATE tracks SET deleted_at = ? WHERE id = ?',
        [_bind(tombstoneAt), occupant.read<String>('id')],
      );
      return rec.columns;
    }
    // Incoming loses → store it tombstoned so it never occupies the live slot.
    return {...rec.columns, 'deleted_at': tombstoneAt};
  }

  Future<void> _insert(String table, Map<String, Object?> columns) {
    final cols = columns.keys.toList();
    final placeholders = List.filled(cols.length, '?').join(', ');
    return db.customStatement(
      'INSERT INTO $table (${cols.join(', ')}) VALUES ($placeholders)',
      [for (final c in cols) _bind(columns[c])],
    );
  }

  Future<void> _update(String table, Map<String, Object?> columns) {
    final cols = columns.keys.where((c) => c != 'id').toList();
    final setClause = cols.map((c) => '$c = ?').join(', ');
    return db.customStatement(
      'UPDATE $table SET $setClause WHERE id = ?',
      [for (final c in cols) _bind(columns[c]), columns['id']],
    );
  }

  /// SQLite bind values are int / double / String / null / blob; map bool to its
  /// integer form.
  static Object? _bind(Object? v) => v is bool ? (v ? 1 : 0) : v;
}
