import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'sync_writer.dart';

/// One detected integrity violation.
class IntegrityViolation {
  IntegrityViolation(this.invariant, this.table, this.id, this.detail);

  /// Short stable id of the broken invariant (for logging / tests).
  final String invariant;
  final String table;
  final String id;
  final String detail;

  @override
  String toString() => '[$invariant] $table/$id: $detail';
}

/// Checks (and optionally repairs) the cross-row invariants that the database's
/// own FK + CHECK constraints can't catch under soft-delete + cross-device
/// merges — e.g. a LIVE child whose parent was tombstoned (the row still exists,
/// so foreign_key_check passes), or a scalar pointer resolving to a tombstoned
/// row, or two live single-slot tracks after two offline devices both added one.
///
/// Invoked at three points: post-write (debug + periodic), migration-validate
/// (full DB on the migrated copy), and sync-apply (the affected closure).
/// Repairs go through the [SyncWriter] chokepoint so they journal like any edit.
class IntegrityChecker {
  IntegrityChecker(this.db, this.writer);

  final AppDatabase db;
  final SyncWriter writer;

  /// (childTable, fkColumn, parentTable, invariantId). Ordered PARENT-FIRST so a
  /// repair pass tombstones a dangling parent before its children are examined.
  static const List<(String, String, String, String)> _childEdges = [
    ('tracks', 'practice_id', 'practices', 'track-practice'),
    ('interpretations', 'practice_id', 'practices', 'interp-practice'),
    ('fx_buses', 'practice_id', 'practices', 'bus-practice'),
    ('technique_media', 'technique_id', 'techniques', 'media-technique'),
    ('clips', 'track_id', 'tracks', 'clip-track'),
    ('practice_notes', 'track_id', 'tracks', 'note-track'),
    ('technique_markers', 'track_id', 'tracks', 'marker-track'),
    ('clips', 'asset_id', 'assets', 'clip-asset'),
    ('technique_media', 'asset_id', 'assets', 'media-asset'),
  ];

  /// Detects all current violations without changing anything.
  Future<List<IntegrityViolation>> check() => _scan(repair: false);

  /// Detects AND repairs, looping to a fixpoint so a cascade (tombstoning a
  /// parent then its now-orphaned children) fully resolves. Returns whatever
  /// could not be repaired (should be empty).
  Future<List<IntegrityViolation>> repair({int maxPasses = 8}) async {
    for (var pass = 0; pass < maxPasses; pass++) {
      final found = await _scan(repair: true);
      if (found.isEmpty) return const [];
    }
    return _scan(repair: false); // remaining after the cap
  }

  Future<List<IntegrityViolation>> _scan({required bool repair}) async {
    final out = <IntegrityViolation>[];

    // 1. Dangling children: a live row whose parent is missing or tombstoned.
    for (final (child, fk, parent, inv) in _childEdges) {
      final rows = await db.customSelect(
        'SELECT c.id AS id FROM $child c WHERE c.deleted_at IS NULL AND NOT EXISTS '
        '(SELECT 1 FROM $parent p WHERE p.id = c.$fk AND p.deleted_at IS NULL)',
      ).get();
      for (final r in rows) {
        final id = r.read<String>('id');
        out.add(IntegrityViolation(inv, child, id, '$parent missing/tombstoned'));
        if (repair) await _tombstone(child, id);
      }
    }

    // 2. Scalar self-heal: a pointer resolving to a missing/tombstoned target
    //    is nulled (foreign_key_check passes it, so we must catch it here).
    out.addAll(await _scalar(
      repair: repair,
      invariant: 'active-interp',
      sql:
          "SELECT p.id AS id FROM practices p WHERE p.deleted_at IS NULL AND "
          "p.active_interpretation_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM "
          "interpretations i WHERE i.id = p.active_interpretation_id AND "
          "i.deleted_at IS NULL AND i.practice_id = p.id)",
      table: 'practices',
      column: 'active_interpretation_id',
    ));
    out.addAll(await _scalar(
      repair: repair,
      invariant: 'separation-asset',
      sql:
          "SELECT p.id AS id FROM practices p WHERE p.deleted_at IS NULL AND "
          "p.separation_asset_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM assets "
          "a WHERE a.id = p.separation_asset_id AND a.deleted_at IS NULL)",
      table: 'practices',
      column: 'separation_asset_id',
    ));
    // A vocal take's interpretation must be a live interpretation of its OWN
    // practice; otherwise unassign it.
    out.addAll(await _scalar(
      repair: repair,
      invariant: 'vocal-interp',
      sql:
          "SELECT t.id AS id FROM tracks t WHERE t.deleted_at IS NULL AND "
          "t.kind = 'vocal' AND t.interpretation_id IS NOT NULL AND NOT EXISTS "
          "(SELECT 1 FROM interpretations i WHERE i.id = t.interpretation_id AND "
          "i.deleted_at IS NULL AND i.practice_id = t.practice_id)",
      table: 'tracks',
      column: 'interpretation_id',
    ));
    // A track's routing bus must be a live fx_bus of its OWN practice; otherwise
    // null it (playback would silently reroute to master on a dangling pointer).
    out.addAll(await _scalar(
      repair: repair,
      invariant: 'track-bus',
      sql:
          "SELECT t.id AS id FROM tracks t WHERE t.deleted_at IS NULL AND "
          "t.bus_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM fx_buses b WHERE "
          "b.id = t.bus_id AND b.deleted_at IS NULL AND b.practice_id = "
          "t.practice_id)",
      table: 'tracks',
      column: 'bus_id',
    ));
    // A bus->bus output route must point at a live fx_bus of the same practice;
    // otherwise null it (falls back to master, same as an absent route).
    out.addAll(await _scalar(
      repair: repair,
      invariant: 'bus-output',
      sql:
          "SELECT b.id AS id FROM fx_buses b WHERE b.deleted_at IS NULL AND "
          "b.output_bus_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM fx_buses o "
          "WHERE o.id = b.output_bus_id AND o.deleted_at IS NULL AND "
          "o.practice_id = b.practice_id)",
      table: 'fx_buses',
      column: 'output_bus_id',
    ));

    // 3. Single-slot LWW: at most one live accompaniment / reference per
    //    practice. Keep the highest HLC, tombstone the losers.
    final dupes = await db.customSelect(
      "SELECT practice_id, kind FROM tracks WHERE deleted_at IS NULL AND "
      "kind IN ('accompaniment', 'reference') GROUP BY practice_id, kind "
      "HAVING COUNT(*) > 1",
    ).get();
    for (final d in dupes) {
      final rows = await db.customSelect(
        "SELECT id FROM tracks WHERE deleted_at IS NULL AND practice_id = ? AND "
        "kind = ? ORDER BY hlc_packed DESC",
        variables: [
          Variable.withString(d.read<String>('practice_id')),
          Variable.withString(d.read<String>('kind')),
        ],
      ).get();
      for (var i = 1; i < rows.length; i++) {
        final id = rows[i].read<String>('id');
        out.add(IntegrityViolation(
          'slot-dup',
          'tracks',
          id,
          'duplicate live ${d.read<String>('kind')} slot',
        ));
        if (repair) await _tombstone('tracks', id);
      }
    }

    return out;
  }

  Future<List<IntegrityViolation>> _scalar({
    required bool repair,
    required String invariant,
    required String sql,
    required String table,
    required String column,
  }) async {
    final rows = await db.customSelect(sql).get();
    final out = <IntegrityViolation>[];
    for (final r in rows) {
      final id = r.read<String>('id');
      out.add(IntegrityViolation(invariant, table, id, '$column dangling'));
      if (repair) {
        final s = writer.stamp();
        await db.customUpdate(
          'UPDATE $table SET $column = NULL, updated_at = ?, hlc_wall = ?, '
          'hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?',
          variables: [
            Variable.withInt(s.at),
            Variable.withInt(s.hlc.wallMs),
            Variable.withInt(s.hlc.counter),
            Variable.withString(s.hlc.node),
            Variable.withString(s.hlc.packed),
            Variable.withString(id),
          ],
        );
        await writer.journal(
          table: table,
          id: id,
          op: 'update',
          hlc: s.hlc,
          payloadHash: 'integrity:$column',
        );
      }
    }
    return out;
  }

  Future<void> _tombstone(String table, String id) async {
    final s = writer.stamp();
    await db.customUpdate(
      'UPDATE $table SET deleted_at = ?, updated_at = ?, hlc_wall = ?, '
      'hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?',
      variables: [
        Variable.withInt(s.at),
        Variable.withInt(s.at),
        Variable.withInt(s.hlc.wallMs),
        Variable.withInt(s.hlc.counter),
        Variable.withString(s.hlc.node),
        Variable.withString(s.hlc.packed),
        Variable.withString(id),
      ],
    );
    await writer.journal(
      table: table,
      id: id,
      op: 'tombstone',
      hlc: s.hlc,
      payloadHash: '',
    );
  }
}
