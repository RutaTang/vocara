import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';
import 'quarantine_store.dart';
import 'sync_row.dart';

/// A row pulled from the cloud, ready to apply: the table name plus every synced
/// column (including the HLC quad, deleted_at and schema_v) as a value map.
///
/// Unified with the wire format: this is now a thin [SyncRow] whose field map
/// keeps its original [columns] name, so existing construction sites (and the
/// applier tests) are untouched while the applier itself accepts any [SyncRow]
/// straight off the transport.
class RemoteRecord extends SyncRow {
  RemoteRecord({required super.table, required Map<String, Object?> columns})
    : super(fields: columns);

  Map<String, Object?> get columns => fields;
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
/// the whole commit — so before committing we sweep foreign_key_check
/// ([_sweepFkOrphans]): rows INSERTED by this batch are hard-deleted (they
/// re-arrive with their parent on a later page) while PRE-EXISTING rows are
/// never deleted, only their dangling pointer column is nulled. The outer
/// transaction is wrapped so a commit failure degrades to a failed batch
/// instead of throwing out of apply().
class SyncApplier {
  SyncApplier(this.db, this.clock, {this.quarantine});

  final AppDatabase db;
  final HlcClock clock;

  /// Optional collaborator: when present, a rejected record (HLC drift or a
  /// per-record apply failure) is PERSISTED to the quarantine parking lot
  /// inside the same outer transaction, so nothing is silently dropped. Kept
  /// optional so wiring (and tests) without persistence keep the counter-only
  /// behavior unchanged.
  final QuarantineStore? quarantine;

  Future<ApplyResult> apply(List<SyncRow> records) async {
    final result = ApplyResult();
    // (table, id) pairs INSERTED by this batch — the only rows the FK sweep is
    // allowed to hard-delete (see _sweepFkOrphans).
    final insertedThisBatch = <(String, String)>{};
    try {
      await db.transaction(() async {
        await db.customStatement('PRAGMA defer_foreign_keys = ON');
        for (final rec in records) {
          try {
            clock.receive(rec.hlc);
          } on HlcDriftException catch (e) {
            result.quarantined++;
            await _quarantine(rec, 'hlc-drift: $e');
            continue;
          }
          try {
            // Apply each record inside a nested transaction (SQLite SAVEPOINT) so a
            // single malformed / forward-incompatible record — a column from a newer
            // schema_v, a CHECK/NOT NULL violation, an unknown table — rolls back ONLY
            // itself and is quarantined, instead of throwing out of the batch and
            // discarding every other (good) record. Counters are updated OUTSIDE the
            // savepoint so a rollback can't desync them.
            var didInsert = false;
            final applied = await db.transaction(() async {
              final existing = await db
                  .customSelect(
                    'SELECT hlc_packed FROM ${rec.table} WHERE id = ?',
                    variables: [Variable.withString(rec.id)],
                  )
                  .getSingleOrNull();
              final wins =
                  existing == null ||
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
                didInsert = true;
              } else {
                await _update(rec.table, columns);
              }
              return true;
            });
            if (applied) {
              result.applied++;
              if (didInsert) insertedThisBatch.add((rec.table, rec.id));
            } else {
              result.skipped++;
            }
          } catch (e) {
            // Forward-incompat / malformed record: its savepoint rolled back.
            result.failed++;
            await _quarantine(rec, 'apply-failed: $e');
          }
        }
        await _sweepFkOrphans(result, insertedThisBatch);
      });
    } catch (_) {
      // The outer commit failed despite the per-record savepoints and the FK sweep
      // (e.g. a deferred constraint we can't attribute to a single row). The whole
      // transaction rolled back — including any persisted quarantine rows, which is
      // correct: the retry of this page re-quarantines them atomically with whatever
      // does commit. Report the batch as failed rather than throwing out of apply(),
      // which would lose the ApplyResult and stall sync on an endless retry of this
      // page.
      result.applied = 0;
      result.skipped = 0;
      result.failed = records.length - result.quarantined;
    }
    return result;
  }

  /// Persists a rejected record to the quarantine parking lot (latest state
  /// per row) INSIDE the current outer apply transaction. Strictly best-effort
  /// bookkeeping: a record too malformed to describe (bad id/hlc types) or a
  /// storage error here must never turn a per-record rejection into a batch
  /// failure — the counters remain the source of truth for the caller.
  Future<void> _quarantine(SyncRow rec, String reason) async {
    final store = quarantine;
    if (store == null) return;
    try {
      final id = rec.fields['id'];
      if (id is! String || id.isEmpty) return; // nothing addressable to park
      final hlcPacked = rec.fields['hlc_packed'];
      final schemaV = rec.fields['schema_v'];
      await store.put(
        table: rec.table,
        id: id,
        hlcPacked: hlcPacked is String ? hlcPacked : '',
        schemaV: schemaV is int ? schemaV : 0,
        payload: rec.encode(),
        reason: reason,
        quarantinedAt: DateTime.now().millisecondsSinceEpoch,
      );
    } catch (_) {
      // Swallowed by design (see doc comment).
    }
  }

  /// Pre-commit sweep for rows that STILL violate a foreign key once the whole
  /// batch is in (defer_foreign_keys postponed enforcement to the outer
  /// commit, so order-tolerance already absorbed child-before-parent within
  /// the batch).
  ///
  /// foreign_key_check scans the WHOLE database, so a flagged row is not
  /// necessarily one this batch created: a remote UPDATE can plant a dangling
  /// pointer into a row that existed long before. Healing is by provenance:
  ///
  ///  * A row INSERTED by this batch is hard-deleted. Its parent truly never
  ///    arrived (a later page carries both), and deleting it destroys nothing
  ///    that existed before this apply.
  ///  * A PRE-EXISTING row is NEVER deleted (deleting it — and then its whole
  ///    subtree via the fixpoint loop — would silently destroy local data with
  ///    no tombstone and no oplog). It satisfied every FK when the batch began
  ///    (SQLite enforced them at every earlier commit), so it can only be
  ///    flagged because THIS batch repointed one of its FK columns — either at
  ///    an id that never arrived, or at a batch-inserted parent this sweep
  ///    just deleted. Only that column is nulled (the IntegrityChecker-style
  ///    scalar self-heal), deliberately un-journaled like every remote apply;
  ///    the pointer is restored when the missing parent and a fresher update
  ///    arrive. Every FK column a remote update realistically repoints is
  ///    nullable (separation_asset_id / interpretation_id / bus_id — the
  ///    NOT NULL parent links practice_id / track_id / technique_id are never
  ///    rewritten after insert); should a NOT NULL column ever dangle anyway
  ///    (e.g. clips.asset_id repointed at a not-yet-arrived asset), we abort
  ///    so the WHOLE batch rolls back — a retried page over silently deleting
  ///    pre-existing data.
  ///
  /// Loops to a fixpoint because deleting an orphan can strand its own
  /// batch-inserted children. Counters: a deleted batch-insert moves from
  /// applied to failed exactly once (duplicate violations for a multi-FK row
  /// are skipped once the row is gone); a nulled pre-existing row changes no
  /// counter (its update, if this batch carried one, genuinely applied).
  Future<void> _sweepFkOrphans(
    ApplyResult result,
    Set<(String, String)> insertedThisBatch,
  ) async {
    while (true) {
      final violations = await db
          .customSelect('PRAGMA foreign_key_check')
          .get();
      if (violations.isEmpty) break;
      for (final v in violations) {
        final table = v.read<String>('table');
        final rowid = v.read<int>('rowid');
        // A row with several dangling FKs is reported once per violated
        // constraint; after the DELETE below the id lookup misses, so the
        // duplicate entries are skipped (no double accounting).
        final idRow = await db
            .customSelect(
              'SELECT id FROM $table WHERE rowid = ?',
              variables: [Variable.withInt(rowid)],
            )
            .getSingleOrNull();
        if (idRow == null) continue;
        final id = idRow.read<String>('id');
        if (insertedThisBatch.contains((table, id))) {
          await db.customStatement('DELETE FROM $table WHERE rowid = ?', [
            rowid,
          ]);
          // The insert was counted as applied above; it did not stick.
          if (result.applied > 0) result.applied--;
          result.failed++;
          continue;
        }
        // Pre-existing row: null just the dangling column. Which column comes
        // from the violation's fkid resolved against foreign_key_list.
        final fkid = v.readNullable<int>('fkid');
        final column = fkid == null ? null : await _fkColumn(table, fkid);
        if (column == null || !await _isNullable(table, column)) {
          // Un-heal-able (see doc comment): fail the batch, keep the data.
          throw StateError(
            'FK sweep: pre-existing $table/$id has a dangling NOT NULL '
            'foreign key (fkid: $fkid); failing the batch to preserve it',
          );
        }
        await db.customStatement(
          'UPDATE $table SET $column = NULL WHERE rowid = ?',
          [rowid],
        );
      }
    }
  }

  /// The child column of foreign key [fkid] on [table] (our FKs are all
  /// single-column, so the first component is the whole key).
  Future<String?> _fkColumn(String table, int fkid) async {
    final fks = await db.customSelect('PRAGMA foreign_key_list($table)').get();
    for (final fk in fks) {
      if (fk.read<int>('id') == fkid) return fk.read<String>('from');
    }
    return null;
  }

  Future<bool> _isNullable(String table, String column) async {
    final cols = await db.customSelect('PRAGMA table_info($table)').get();
    for (final c in cols) {
      if (c.read<String>('name') == column) return c.read<int>('notnull') == 0;
    }
    return false;
  }

  /// Returns the column map to write for [rec] after resolving any single-slot conflict by
  /// entity-level LWW: if [rec] is a LIVE accompaniment/reference whose (practice, kind)
  /// slot is held by a DIFFERENT live row, the higher hlc_packed keeps the slot live and
  /// the loser is tombstoned. When [rec] is the loser, the returned map is tombstoned so
  /// the row is still stored (a later sync can update it) without occupying the live slot.
  Future<Map<String, Object?>> _resolveSlot(SyncRow rec) async {
    if (rec.table != 'tracks' || rec.fields['deleted_at'] != null) {
      return rec.fields;
    }
    final kind = rec.fields['kind'];
    if (kind != 'accompaniment' && kind != 'reference') return rec.fields;
    final practiceId = rec.fields['practice_id'];
    if (practiceId is! String) return rec.fields;
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
    if (occupant == null) return rec.fields;
    // A plausible tombstone time: the incoming record's wall clock.
    final tombstoneAt = rec.fields['hlc_wall'];
    if (rec.hlcPacked.compareTo(occupant.read<String>('hlc_packed')) > 0) {
      // Incoming wins → free the slot by tombstoning the current occupant.
      await db.customStatement(
        'UPDATE tracks SET deleted_at = ? WHERE id = ?',
        [_bind(tombstoneAt), occupant.read<String>('id')],
      );
      return rec.fields;
    }
    // Incoming loses → store it tombstoned so it never occupies the live slot.
    return {...rec.fields, 'deleted_at': tombstoneAt};
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
    return db.customStatement('UPDATE $table SET $setClause WHERE id = ?', [
      for (final c in cols) _bind(columns[c]),
      columns['id'],
    ]);
  }

  /// SQLite bind values are int / double / String / null / blob; map bool to its
  /// integer form.
  static Object? _bind(Object? v) => v is bool ? (v ? 1 : 0) : v;
}
