import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';
import 'quarantine_store.dart';
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
/// Repairs journal through the [SyncWriter] like any edit, but are stamped
/// with a DERIVED HLC, never a fresh clock tick — see [_repairStamp]. And no
/// row is judged against a referenced row whose NEWER state is still parked
/// in the sync quarantine: that verdict is deferred until the parked state
/// settles, and neither reported nor repaired meanwhile — see [_parkedNewer].
class IntegrityChecker {
  IntegrityChecker(this.db, this.writer);

  final AppDatabase db;
  final SyncWriter writer;

  /// The stamp for one repair write, DERIVED from the repaired row's own HLC
  /// via [HlcClock.derived]: same (wall, counter), node extended by
  /// `'!' + this device's node`.
  ///
  /// A repair is derived state, not user intent. The extension is strictly
  /// ABOVE the row's current stamp (so the repair wins LWW on every peer and
  /// the fleet converges) yet strictly BELOW every stamp that out-ranks the
  /// base — a held-wall author edit at (wall, counter + 1), a node-space tie
  /// winner, and any later tick. A fresh `writer.stamp()` here — the oldest
  /// behavior — minted user-grade intent for bookkeeping and resurrected a
  /// deleted practice (simulation seed 0xab1a57); the intermediate
  /// counter + 1 derivation lost held-wall author edits to repairs, and let
  /// CHAINED repairs (base+2) beat base+1 real edits. Deriving in node space
  /// closes all three: see [HlcClock.derived].
  SyncStamp _repairStamp(String rowHlcPacked) =>
      SyncStamp(HlcClock.derived(Hlc.parse(rowHlcPacked), writer.clock.node));

  /// SQL predicate: the [table] row named by [idExpr] has a NEWER state still
  /// PARKED — a replayable `sync_quarantine` entry for it with no local row
  /// at least as new (the local row is older, or absent). Every verdict that
  /// judges a row against such a REFERENCED row (a child against its parent,
  /// a pointer against its target, a route against the bus it leaves) is
  /// DEFERRED while this holds: it is not a violation yet, only a pending
  /// state.
  ///
  /// Why: a repair judged against a state the quarantine already supersedes
  /// manufactures intent. The canonical case: a delete of track T on one
  /// device, concurrent edits of T and its clip C on another (both beat the
  /// delete, so the converged state is T live + C live). A puller that
  /// parks the T edit at the drift gate but applies the C edit sees a live
  /// clip under a tombstoned track, and repairing it tombstones C under a
  /// derived stamp ABOVE C's winning edit — journaled, pushed, adopted
  /// fleet-wide, and still standing after T's parked edit lands live. The
  /// same holds for a newer-schema park, and for an update the FK sweep
  /// parked and restored over (its prior state may be the tombstone).
  ///
  /// A deferral lasts exactly as long as the park. The replay that ends it —
  /// the parked state lands, or settles as an LWW skip because a newer row
  /// is held — gates the sync engine's end-of-run repair, so the verdict is
  /// re-judged in that same run (an entry removed any other way, e.g. a
  /// manual discard, waits for the next repair). A park that never settles
  /// (a newer-schema row on a build that is never upgraded) defers its
  /// dependents as long: the newest known state of the referenced row is
  /// simply not applicable here, and guessing at it would be the bug above.
  /// An UNREPLAYABLE entry ([QuarantineStore.replayableSql]) can never land,
  /// so it defers nothing. Only the REFERENCED row's parked state matters: a
  /// parked newer state of the repaired row ITSELF already out-ranks the
  /// derived repair stamp (see [_repairStamp]), here and on every peer, so
  /// it is never overridden by the repair.
  ///
  /// Both of the referenced row's parking-lot entries count — the door's,
  /// and the FK sweep's own slot, which holds exactly that parked-and-
  /// restored update (see [QuarantineStore.entryOfRowSql]).
  static String _parkedNewer(String table, String idExpr) =>
      "EXISTS (SELECT 1 FROM sync_quarantine q WHERE q.tbl = '$table' "
      'AND ${QuarantineStore.entryOfRowSql('q', idExpr)} '
      'AND ${QuarantineStore.replayableSql('q')} '
      'AND NOT EXISTS (SELECT 1 FROM $table x WHERE x.id = $idExpr '
      'AND x.hlc_packed >= q.remote_hlc_packed))';

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

    // 1. Dangling children: a live row whose parent is missing or tombstoned
    //    — unless the parent's newer state is still parked (deferred, see
    //    [_parkedNewer]).
    for (final (child, fk, parent, inv) in _childEdges) {
      final rows = await db
          .customSelect(
            'SELECT c.id AS id, c.hlc_packed AS h FROM $child c WHERE '
            'c.deleted_at IS NULL AND NOT EXISTS '
            '(SELECT 1 FROM $parent p WHERE p.id = c.$fk AND p.deleted_at IS NULL) '
            'AND NOT ${_parkedNewer(parent, 'c.$fk')}',
          )
          .get();
      for (final r in rows) {
        final id = r.read<String>('id');
        out.add(
          IntegrityViolation(inv, child, id, '$parent missing/tombstoned'),
        );
        if (repair) await _tombstone(child, id, r.read<String>('h'));
      }
    }

    // 2. Scalar self-heal: a pointer resolving to a missing/tombstoned target
    //    is nulled (foreign_key_check passes it, so we must catch it here) —
    //    unless the target's newer state is still parked (deferred, see
    //    [_parkedNewer]; every query below carries that guard).
    out.addAll(
      await _scalar(
        repair: repair,
        invariant: 'active-interp',
        sql:
            'SELECT p.id AS id, p.hlc_packed AS h FROM practices p WHERE '
            'p.deleted_at IS NULL AND p.active_interpretation_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM '
            'interpretations i WHERE i.id = p.active_interpretation_id AND '
            'i.deleted_at IS NULL AND i.practice_id = p.id) '
            'AND NOT ${_parkedNewer('interpretations', 'p.active_interpretation_id')}',
        table: 'practices',
        column: 'active_interpretation_id',
      ),
    );
    out.addAll(
      await _scalar(
        repair: repair,
        invariant: 'separation-asset',
        sql:
            'SELECT p.id AS id, p.hlc_packed AS h FROM practices p WHERE '
            'p.deleted_at IS NULL AND p.separation_asset_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM assets '
            'a WHERE a.id = p.separation_asset_id AND a.deleted_at IS NULL) '
            'AND NOT ${_parkedNewer('assets', 'p.separation_asset_id')}',
        table: 'practices',
        column: 'separation_asset_id',
      ),
    );
    // A vocal take's interpretation must be a live interpretation of its OWN
    // practice; otherwise unassign it.
    out.addAll(
      await _scalar(
        repair: repair,
        invariant: 'vocal-interp',
        sql:
            'SELECT t.id AS id, t.hlc_packed AS h FROM tracks t WHERE '
            "t.deleted_at IS NULL AND t.kind = 'vocal' AND t.interpretation_id IS NOT NULL AND NOT EXISTS "
            '(SELECT 1 FROM interpretations i WHERE i.id = t.interpretation_id AND '
            'i.deleted_at IS NULL AND i.practice_id = t.practice_id) '
            'AND NOT ${_parkedNewer('interpretations', 't.interpretation_id')}',
        table: 'tracks',
        column: 'interpretation_id',
      ),
    );
    // A track's routing bus must be a live fx_bus of its OWN practice; otherwise
    // null it (playback would silently reroute to master on a dangling pointer).
    out.addAll(
      await _scalar(
        repair: repair,
        invariant: 'track-bus',
        sql:
            'SELECT t.id AS id, t.hlc_packed AS h FROM tracks t WHERE '
            't.deleted_at IS NULL AND t.bus_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM fx_buses b WHERE '
            'b.id = t.bus_id AND b.deleted_at IS NULL AND b.practice_id = '
            't.practice_id) '
            'AND NOT ${_parkedNewer('fx_buses', 't.bus_id')}',
        table: 'tracks',
        column: 'bus_id',
      ),
    );
    // A bus->bus output route must point at a live fx_bus of the same practice;
    // otherwise null it (falls back to master, same as an absent route).
    out.addAll(
      await _scalar(
        repair: repair,
        invariant: 'bus-output',
        sql:
            'SELECT b.id AS id, b.hlc_packed AS h FROM fx_buses b WHERE '
            'b.deleted_at IS NULL AND b.output_bus_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM fx_buses o '
            'WHERE o.id = b.output_bus_id AND o.deleted_at IS NULL AND '
            'o.practice_id = b.practice_id) '
            'AND NOT ${_parkedNewer('fx_buses', 'b.output_bus_id')}',
        table: 'fx_buses',
        column: 'output_bus_id',
      ),
    );

    // 2b. Bus routing must stay ACYCLIC. Every device refuses to create a cycle
    //     locally, but a merge can still synthesise one out of two individually
    //     legal edits: A→B on one device and B→A on another, offline, both
    //     applied. Nothing above catches it — each edge points at a live bus of
    //     the same practice, so `bus-output` is satisfied — and the native
    //     engine then REJECTS the whole graph, which means the database and the
    //     UI show the merged routing while playback keeps running the last
    //     accepted graph (or, on a fresh open, has no graph at all).
    out.addAll(await _busCycles(repair: repair));

    // 3. Single-slot LWW: at most one live accompaniment / reference per
    //    practice. Keep the highest HLC, tombstone the losers.
    final dupes = await db
        .customSelect(
          'SELECT practice_id, kind FROM tracks WHERE deleted_at IS NULL AND '
          "kind IN ('accompaniment', 'reference') GROUP BY practice_id, kind "
          'HAVING COUNT(*) > 1',
        )
        .get();
    for (final d in dupes) {
      final rows = await db
          .customSelect(
            'SELECT id, hlc_packed FROM tracks WHERE deleted_at IS NULL AND '
            'practice_id = ? AND kind = ? ORDER BY hlc_packed DESC',
            variables: [
              Variable.withString(d.read<String>('practice_id')),
              Variable.withString(d.read<String>('kind')),
            ],
          )
          .get();
      for (var i = 1; i < rows.length; i++) {
        final id = rows[i].read<String>('id');
        out.add(
          IntegrityViolation(
            'slot-dup',
            'tracks',
            id,
            'duplicate live ${d.read<String>('kind')} slot',
          ),
        );
        if (repair) {
          await _tombstone('tracks', id, rows[i].read<String>('hlc_packed'));
        }
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
        await _nullColumn(table, column, id, r.read<String>('h'));
      }
    }
    return out;
  }

  /// Buses whose `output_bus_id` chain reaches themselves. `UNION` (not `UNION
  /// ALL`) dedupes the frontier, so the walk terminates ON a cycle instead of
  /// spinning in it.
  ///
  /// Repair breaks each cycle at ONE edge — the member with the lowest
  /// (hlc, id) drops its route — rather than clearing every route on the ring:
  /// the loser is the older of the two edits that collided, which matches the
  /// LWW rule the rest of the merge uses, and the newer edit survives. The
  /// outer [repair] fixpoint loop comes back for any further cycle. Every
  /// replica sees the same rows here, so every replica picks the same victim.
  ///
  /// A ring through a bus whose newer state is still parked ([_parkedNewer])
  /// is broken AT that bus instead. Its route is pending and the parked state
  /// may well not close the ring, so cutting another member would null a
  /// peer's route fleet-wide for a cycle that never existed; but the ring
  /// cannot be left standing either — the engine refuses a cyclic graph, so
  /// the practice would play nothing (or never take another structural edit)
  /// for as long as the park lasts. The parked bus's own parked state already
  /// out-ranks the derived repair stamp, so the cut lasts exactly until that
  /// state lands, and a ring it really closes is judged again then.
  Future<List<IntegrityViolation>> _busCycles({required bool repair}) async {
    final rows = await db
        .customSelect(
          'WITH RECURSIVE edge(src, dst) AS ('
          '  SELECT b.id, b.output_bus_id FROM fx_buses b '
          '  WHERE b.deleted_at IS NULL AND b.output_bus_id IS NOT NULL '
          '    AND EXISTS (SELECT 1 FROM fx_buses o WHERE o.id = b.output_bus_id '
          '      AND o.deleted_at IS NULL AND o.practice_id = b.practice_id)'
          '), reach(start, node) AS ('
          '  SELECT src, dst FROM edge '
          '  UNION '
          '  SELECT r.start, e.dst FROM reach r JOIN edge e ON e.src = r.node'
          ') '
          'SELECT b.id AS id, b.hlc_packed AS h, '
          '  ${_parkedNewer('fx_buses', 'b.id')} AS parked '
          'FROM fx_buses b '
          'WHERE EXISTS (SELECT 1 FROM reach r WHERE r.start = b.id AND r.node = b.id) '
          'ORDER BY b.hlc_packed ASC, b.id ASC',
        )
        .get();
    if (rows.isEmpty) return const [];
    final out = [
      for (final r in rows)
        IntegrityViolation(
          'bus-cycle',
          'fx_buses',
          r.read<String>('id'),
          'output_bus_id chain returns to itself',
        ),
    ];
    if (repair) {
      final victim = rows.firstWhere(
        (r) => r.read<int>('parked') != 0,
        orElse: () => rows.first,
      );
      await _nullColumn(
        'fx_buses',
        'output_bus_id',
        victim.read<String>('id'),
        victim.read<String>('h'),
      );
    }
    return out;
  }

  /// Nulls one column on one row as a repair: derived stamp, journalled like
  /// any other edit so peers converge on the same fix.
  Future<void> _nullColumn(
    String table,
    String column,
    String id,
    String rowHlcPacked,
  ) async {
    final s = _repairStamp(rowHlcPacked);
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

  Future<void> _tombstone(String table, String id, String rowHlcPacked) async {
    final s = _repairStamp(rowHlcPacked);
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
