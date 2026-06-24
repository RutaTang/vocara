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

  @override
  String toString() =>
      'ApplyResult(applied: $applied, skipped: $skipped, quarantined: $quarantined)';
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
/// Apply is order-tolerant: defer_foreign_keys lets a child arrive before its
/// parent within the batch; the whole batch commits atomically.
class SyncApplier {
  SyncApplier(this.db, this.clock);

  final AppDatabase db;
  final HlcClock clock;

  Future<ApplyResult> apply(List<RemoteRecord> records) async {
    final result = ApplyResult();
    await db.transaction(() async {
      await db.customStatement('PRAGMA defer_foreign_keys = ON');
      for (final rec in records) {
        try {
          clock.receive(rec.hlc);
        } on HlcDriftException {
          result.quarantined++;
          continue;
        }
        final existing = await db
            .customSelect(
              'SELECT hlc_packed FROM ${rec.table} WHERE id = ?',
              variables: [Variable.withString(rec.id)],
            )
            .getSingleOrNull();
        final wins = existing == null ||
            rec.hlcPacked.compareTo(existing.read<String>('hlc_packed')) > 0;
        if (!wins) {
          result.skipped++;
          continue;
        }
        // Clear any single-slot conflict BEFORE writing. The single-slot track kinds
        // (one live accompaniment/reference per practice) carry a partial UNIQUE index;
        // inserting a second LIVE row for an already-occupied slot raises a UNIQUE
        // violation, which is NOT an HlcDriftException and would escape the per-record
        // guard and roll back the WHOLE batch (losing every other record).
        final columns = await _resolveSlot(rec);
        if (existing == null) {
          await _insert(rec.table, columns);
        } else {
          await _update(rec.table, columns);
        }
        result.applied++;
      }
    });
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
