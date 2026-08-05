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
  // Rejected while applying: an unknown table/column, a newer schema_v, or a
  // throw inside the record's savepoint (malformed / forward-incompatible row).
  int failed = 0;

  /// Whether the batch's OUTER transaction committed. False means the whole
  /// batch rolled back (nothing was applied, quarantined-to-store, or written
  /// by the `beforeCommit` hook) and the caller must retry the page — this is
  /// how the sync engine knows its change token did NOT advance.
  bool committed = true;

  @override
  String toString() =>
      'ApplyResult(applied: $applied, skipped: $skipped, '
      'quarantined: $quarantined, failed: $failed, committed: $committed)';
}

/// The "remote door": applies records pulled from sync into the local store.
///
/// Distinct from local writes (which go through SyncWriter): a remote change
/// ADOPTS the remote HLC (it does not tick), wins only by entity-level
/// Last-Writer-Wins on the HLC, and is NEVER journaled — so an applied remote
/// change can't be re-pushed into an infinite ping-pong. (The single carve-out
/// is the single-slot RESOLUTION tombstone, which is a new derived local
/// decision — not an echo of the applied record — and must replicate to
/// converge; it is bounded to one write per losing row, see [_resolveSlot].)
/// The clock is advanced so subsequent LOCAL writes stay causally after
/// everything we've seen; a record whose clock is implausibly far in the
/// future is quarantined instead of merged (one bad peer can't poison the
/// fleet). The gate and the advance are split deliberately:
/// [HlcClock.assertPlausible] (check-only, same drift rule) runs with the
/// other door gates, and the state-changing [HlcClock.mergeVerified] calls
/// run only AFTER the batch's outer transaction has committed, for the
/// records whose savepoint settled as applied or skipped — a record that
/// fails inside its savepoint, and equally a whole batch whose outer commit
/// fails (an FK-sweep abort, a throwing beforeCommit hook), must leave NO
/// side effect behind, and an in-memory clock advanced for writes the
/// database rolled back is a side effect. The post-commit advance is
/// mergeVerified rather than [HlcClock.receive] because re-running the drift
/// check after the commit could throw for stamps that are already durable
/// (see the bottom of [apply]).
///
/// Apply is order-tolerant and per-record resilient: defer_foreign_keys lets a
/// child arrive before its parent within the batch, and each record applies
/// inside its own SAVEPOINT so one malformed / forward-incompatible record is
/// quarantined ([ApplyResult.failed]) rather than rolling back every other record.
/// Because defer_foreign_keys pushes FK enforcement to the OUTER commit (outside
/// every savepoint), a record whose parent never arrives in the batch would fail
/// the whole commit — so before committing we sweep foreign_key_check
/// ([_sweepFkOrphans]): rows INSERTED by this batch are PARKED to the
/// quarantine store and then deleted (the parent may be locally quarantined —
/// e.g. a newer-schema row the schema gate rejected — in which case the
/// server never re-delivers the child, and only the quarantine replay can
/// land it once the parent exists) while PRE-EXISTING rows are never deleted,
/// only their dangling pointer column is nulled. The outer transaction is
/// wrapped so a commit failure degrades to a failed batch instead of throwing
/// out of apply().
class SyncApplier {
  SyncApplier(this.db, this.clock, {required this.builder, this.quarantine});

  final AppDatabase db;
  final HlcClock clock;

  /// Projects a just-inserted row back into its full wire snapshot so the FK
  /// sweep can PARK an orphan instead of losing it (see [_sweepFkOrphans]).
  /// Deliberately REQUIRED (unlike [quarantine]): an absent projection would
  /// silently degrade the sweep to the old hard-delete, and a locally
  /// quarantined parent then makes that deletion a permanent row loss.
  final SyncRowBuilder builder;

  /// Optional collaborator: when present, a rejected record (HLC drift or a
  /// per-record apply failure) is PERSISTED to the quarantine parking lot
  /// inside the same outer transaction, so nothing is silently dropped. Kept
  /// optional so wiring (and tests) without persistence keep the counter-only
  /// behavior unchanged.
  final QuarantineStore? quarantine;

  /// Table name -> drift [TableInfo] for exactly the SYNCED tables, derived
  /// structurally (see [syncedTableRegistry]) so a new synced table can never
  /// be silently missed by the structural gate in [apply]. The gate exists
  /// because [_insert]/[_update] (and the LWW SELECT) build SQL TEXT from the
  /// record's table and field names — an unvetted name from the wire must
  /// never reach the SQL parser.
  late final Map<String, TableInfo<Table, dynamic>> _tables =
      syncedTableRegistry(db);

  /// The column-name set of [table], read from the LIVE schema
  /// (PRAGMA table_info) rather than the compiled drift [TableInfo], and
  /// cached only per [apply] call (the caller passes [cache]). Both choices
  /// are deliberate: the live schema is the ground truth the INSERT/UPDATE
  /// below actually runs against, and re-reading it per batch keeps the
  /// quarantine-replay contract intact — a schema upgrade must un-park a
  /// previously forward-incompatible row on the NEXT replay, not after a
  /// process restart (see SyncEngine._replayQuarantine). [table] is already
  /// vetted against [_tables] before this is called.
  Future<Set<String>> _liveColumns(
    String table,
    Map<String, Set<String>> cache,
  ) async {
    final cached = cache[table];
    if (cached != null) return cached;
    final rows = await db.customSelect('PRAGMA table_info("$table")').get();
    return cache[table] = {for (final r in rows) r.read<String>('name')};
  }

  /// Applies one batch of remote records in ONE outer transaction.
  ///
  /// [beforeCommit], when given, runs INSIDE that outer transaction as its
  /// very last step (after every record and the FK sweep), so whatever it
  /// writes commits atomically with the batch. This is the sync engine's
  /// change-token hook: the token it covers advances IFF the batch committed.
  /// A throwing hook rolls the WHOLE batch back ([ApplyResult.committed]
  /// false) — deliberately, since applying records whose token write failed
  /// would re-apply the page forever.
  Future<ApplyResult> apply(
    List<SyncRow> records, {
    Future<void> Function()? beforeCommit,
  }) async {
    final result = ApplyResult();
    // (table, id) pairs INSERTED by this batch — the only rows the FK sweep is
    // allowed to park-and-delete (see _sweepFkOrphans).
    final insertedThisBatch = <(String, String)>{};
    // Per-batch live-schema cache for the column gate (see [_liveColumns]).
    final liveColumns = <String, Set<String>>{};
    // Stamps of records whose savepoint settled as applied or skipped. Their
    // state-changing clock.mergeVerified runs only once the outer transaction
    // has committed (bottom of this method) — an advance for records the
    // outer commit then rolled back would be a side effect surviving the
    // rollback.
    final settledHlcs = <Hlc>[];
    try {
      await db.transaction(() async {
        await db.customStatement('PRAGMA defer_foreign_keys = ON');
        for (final rec in records) {
          // EVERYTHING a single record does is wrapped so that ANY per-record
          // exception — not just the enumerated gates below — quarantines that
          // record and lets the loop continue. Before this, only
          // HlcDriftException was caught around clock.receive; anything else
          // (a mistyped HLC field, a DB error) escaped the loop, failed the
          // whole page, and the engine retried the SAME page forever ("token
          // unmoved") — one bad record bricked sync and blocked every good
          // record behind it.
          try {
            // STRUCTURAL GATE — before any SQL is built from the record.
            // [_insert]/[_update] (and the LWW SELECT) interpolate rec.table
            // and its field names into SQL text, so both must be vetted
            // against the schema, never trusted off the wire.
            if (!_tables.containsKey(rec.table)) {
              result.failed++;
              await _quarantine(rec, 'unknown-table: "${rec.table}"');
              continue;
            }
            // LWW settle — deliberately AHEAD of the schema_v/column gates
            // (the table name above is the only thing this SELECT
            // interpolates, and it is vetted): a record that is not strictly
            // newer than the local row is a clean skip, and since no write
            // will happen its (possibly forward-incompatible) shape never
            // matters. The quarantine replay relies on this order — a parked
            // row that lost LWW meanwhile must SETTLE as a skip (its entry is
            // then removed) instead of being re-parked on its shape forever.
            final held = await db
                .customSelect(
                  'SELECT hlc_packed FROM ${rec.table} WHERE id = ?',
                  variables: [Variable.withString(rec.id)],
                )
                .getSingleOrNull();
            if (held != null &&
                rec.hlcPacked.compareTo(held.read<String>('hlc_packed')) <= 0) {
              result.skipped++;
              continue;
            }
            // Wire schema versions start at 1 — no build has ever written a
            // lower [currentRecordSchemaV] — so schema_v 0 or negative is
            // never an "older peer": it is a malformed or non-conforming
            // record, and it MUST be stopped here because the two schema
            // gates below bracket it without catching it (the newer-schema
            // gate matches only v > current, the full-row gate only
            // v == current), so a below-range record used to walk between
            // them and apply with missing columns silently accepted. This is
            // an applier gate rather than a stricter SyncRow.fromJson:
            // fromJson runs at the transport/page layer (a real transport
            // decodes the WHOLE page, so one throw there fails every record
            // on it) and on quarantine-replay decode (where a throw keeps
            // the entry parked forever instead of letting LWW settle it) —
            // only this per-record path can park the one bad record and keep
            // the one-bad-record-never-bricks-the-page invariant.
            if (rec.schemaV < 1) {
              result.failed++;
              await _quarantine(
                rec,
                'invalid-schema_v: ${rec.schemaV} is below 1, the lowest '
                'version any build ever wrote',
              );
              continue;
            }
            // A record from a NEWER record-schema version is quarantined
            // before the column gate, keeping the pre-gate behavior (and its
            // apply-failed reason) for genuinely newer peers: its shape is by
            // definition not fully understood here, and the quarantine replay
            // un-parks it after the app upgrade that is.
            if (rec.schemaV > currentRecordSchemaV) {
              result.failed++;
              await _quarantine(
                rec,
                'apply-failed: schema_v ${rec.schemaV} is newer than this '
                'build understands ($currentRecordSchemaV)',
              );
              continue;
            }
            // At an accepted schema_v an unknown column is never legitimate:
            // quarantine it at the door instead of letting the name reach the
            // SQL text below.
            final known = await _liveColumns(rec.table, liveColumns);
            final unknown = rec.fields.keys
                .where((k) => !known.contains(k))
                .toList();
            if (unknown.isNotEmpty) {
              result.failed++;
              await _quarantine(
                rec,
                'unknown-column: ${unknown.map((c) => '"$c"').join(', ')} '
                'not in "${rec.table}"',
              );
              continue;
            }
            // The mirror gate: at the CURRENT record schema version a record
            // must carry EVERY column of the table (minus the per-device set,
            // which never rides the wire) — the wire contract is full-row
            // snapshots (docs/sync-protocol.md). A partial row that won LWW
            // would advance hlc_packed while keeping STALE local content for
            // its missing columns, and a later genuine full row with an
            // intermediate HLC then loses LWW forever — the row is poisoned,
            // not just briefly wrong. Quarantined like an unknown column: the
            // sender is non-conforming, and the parking lot (not a silent
            // partial write) is where a non-conforming record belongs.
            //
            // Deliberately NOT enforced for schema_v < current (which, past
            // the below-range gate above, means valid older versions only —
            // 1 <= schema_v < current): an older peer legitimately lacks
            // columns this build added, and its full-row snapshot of the OLD
            // shape is still the safest thing to apply. A
            // per-version column registry (required set keyed by schema_v) is
            // the future-proof answer once a v2 exists; until then the live
            // schema — the same source the unknown-column gate reads — IS the
            // v-current column set.
            if (rec.schemaV == currentRecordSchemaV) {
              final perDevice = perDeviceColumns[rec.table] ?? const <String>{};
              final missing = known
                  .where(
                    (c) => !perDevice.contains(c) && !rec.fields.containsKey(c),
                  )
                  .toList();
              if (missing.isNotEmpty) {
                result.failed++;
                await _quarantine(
                  rec,
                  'missing-column: ${missing.map((c) => '"$c"').join(', ')} '
                  'absent from a schema_v $currentRecordSchemaV record for '
                  '"${rec.table}" (the wire contract is full-row snapshots)',
                );
                continue;
              }
            }
            try {
              // Check-only: the drift gate must not advance the clock, or a
              // record that then fails/quarantines inside its savepoint would
              // still have moved it. The advancing mergeVerified() runs only
              // after the outer transaction commits (bottom of this method).
              clock.assertPlausible(rec.hlc);
            } on HlcDriftException catch (e) {
              result.quarantined++;
              await _quarantine(rec, 'hlc-drift: $e');
              continue;
            }
            // Apply each record inside a nested transaction (SQLite SAVEPOINT) so a
            // single malformed record — a CHECK/NOT NULL violation, a constraint
            // the gates above cannot see — rolls back ONLY itself and is
            // quarantined, instead of throwing out of the batch and discarding
            // every other (good) record. Counters are updated OUTSIDE the
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
              final columns = _localizePerDevice(
                rec.table,
                await _resolveSlot(rec),
              );
              if (existing == null) {
                await _insert(rec.table, columns);
                didInsert = true;
              } else {
                await _update(rec.table, columns);
              }
              return true;
            });
            // The record settled (applied, or cleanly lost LWW): remember its
            // stamp for the post-commit clock advance. NOT merged here —
            // this line still runs inside the outer transaction, and an
            // FK-sweep abort, a throwing beforeCommit hook, or a failed
            // outer commit would roll the database back while the in-memory
            // clock kept the advance.
            settledHlcs.add(rec.hlc);
            if (applied) {
              result.applied++;
              if (didInsert) insertedThisBatch.add((rec.table, rec.id));
            } else {
              result.skipped++;
            }
          } catch (e) {
            // Any other per-record failure. If the record was inside its
            // savepoint, drift rolled the savepoint back BEFORE rethrowing,
            // so by the time this quarantine write runs the record's partial
            // writes are gone while the quarantine row (written in the OUTER
            // transaction) survives with the rest of the batch.
            result.failed++;
            await _quarantine(rec, 'apply-failed: $e');
          }
        }
        await _sweepFkOrphans(result, insertedThisBatch);
        if (beforeCommit != null) await beforeCommit();
      });
    } catch (_) {
      // The outer commit failed despite the per-record savepoints and the FK sweep
      // (e.g. a deferred constraint we can't attribute to a single row, or a
      // throwing beforeCommit hook). The whole transaction rolled back — including
      // any persisted quarantine rows, which is correct: the retry of this page
      // re-quarantines them atomically with whatever does commit. Report the batch
      // as failed rather than throwing out of apply(), which would lose the
      // ApplyResult and stall sync on an endless retry of this page.
      result.applied = 0;
      result.skipped = 0;
      result.failed = records.length - result.quarantined;
      result.committed = false;
    }
    if (result.committed) {
      // The batch is durably committed: only now advance the clock past every
      // settled record's stamp, so subsequent local writes stay causally
      // after everything the database actually holds. mergeVerified, NOT
      // receive: every stamp here already passed assertPlausible at its
      // gate, and re-running the drift check now could THROW after the data
      // and the change token are durable — a wall clock rolled back between
      // the gate and this line shrinks the max(now, maxWallEverSeen)
      // reference (assertPlausible never raises it), turning a committed
      // page into a reported failure. Clamping the stamp instead was
      // rejected: it would corrupt causal order. Stamps of records the
      // FK-orphan sweep later parked-and-deleted merge too, deliberately: HLC
      // semantics advance on any RECEIVED message; row survival is a
      // different layer. Deferring the advance also means the drift gate
      // judged every record of this batch against the PRE-batch clock
      // reference — that is deliberate and conservative: a record plausible
      // only relative to an earlier in-batch stamp quarantines now and is
      // un-parked by the sync engine's end-of-run quarantine replay, once
      // this commit's merges have raised the reference.
      for (final hlc in settledHlcs) {
        clock.mergeVerified(hlc);
      }
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
  ///  * A row INSERTED by this batch is PARKED, then deleted. The delete is
  ///    non-negotiable — the deferred FK still fails the outer commit while
  ///    the row exists — but deleting ALONE is not enough: "it re-arrives
  ///    with its parent on a later page" is false when the parent is locally
  ///    QUARANTINED (e.g. a newer-schema row parked by the schema gate) while
  ///    the child applied — the child's page is then already behind the
  ///    change token, the server never re-delivers it, and the row would be
  ///    lost forever. So its full wire snapshot (reconstructed via
  ///    [SyncRowBuilder.build], carrying the row's OWN HLC so a later LWW
  ///    settle still works) goes to the quarantine store first, with the
  ///    distinct `fk-orphan` reason. The pull run's quarantine replay then
  ///    converges it: a replayed orphan whose parent now exists applies
  ///    normally (and its entry settles), one whose parent is STILL missing
  ///    is re-swept here and re-parked (the store's upsert replaces the
  ///    entry) — never lost, never a hot loop. Unlike [_quarantine]'s
  ///    best-effort bookkeeping the park is NOT swallowed on error: after
  ///    the delete a failed park IS a lost row, so a storage error aborts
  ///    the batch (a retried page over silent loss). Without a quarantine
  ///    store (counter-only test wiring) the old hard-delete stands.
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
          final store = quarantine;
          if (store != null) {
            // Park BEFORE deleting (same outer transaction, so the entry and
            // the delete commit or roll back together). The snapshot is the
            // shared outbound projection — never a hand-written second one —
            // and reads the row's own adopted HLC straight from the table.
            final snapshot = await builder.build(table, id);
            if (snapshot != null) {
              final fkid = v.readNullable<int>('fkid');
              final column = fkid == null ? null : await _fkColumn(table, fkid);
              final parentId = column == null ? null : snapshot.fields[column];
              await store.put(
                table: table,
                id: id,
                hlcPacked: snapshot.hlcPacked,
                schemaV: snapshot.schemaV,
                payload: snapshot.encode(),
                reason:
                    'fk-orphan: parent ${v.read<String>('parent')}/'
                    '${parentId ?? 'unknown'} missing',
                quarantinedAt: DateTime.now().millisecondsSinceEpoch,
              );
            }
          }
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
  ///
  /// The resolution REPLICATES: the loser's tombstone is stamped one HLC
  /// counter above the loser's own stamp (under OUR node — see
  /// [_derivedHlc]) and journaled, because whether a device even observes
  /// the conflict depends on its local apply order (the loser's creator
  /// meets the winner arriving; a device that received the winner's later
  /// tombstone first never sees two live rows at once). An un-journaled,
  /// un-bumped local tombstone therefore diverged permanently: same
  /// hlc_packed, live on one device and dead on another (convergence
  /// simulation, seeds 0x5eed0009 / 0xabd782). With the bump + journal the
  /// resolution wins LWW on every peer, and two devices resolving the same
  /// pair independently produce identical rows up to the node tiebreak.
  /// The tombstone TIME is the winner's wall clock (the moment the slot was
  /// claimed) — the same value on every resolver. This is the ONE local
  /// write the applier journals; it is a new derived decision, never an echo
  /// of the applied record, so it cannot ping-pong (see the class docs).
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
          'SELECT id, hlc_packed, hlc_wall FROM tracks WHERE practice_id = ? '
          'AND kind = ? AND deleted_at IS NULL AND id <> ?',
          variables: [
            Variable.withString(practiceId),
            Variable.withString(kind as String),
            Variable.withString(rec.id),
          ],
        )
        .getSingleOrNull();
    if (occupant == null) return rec.fields;
    if (rec.hlcPacked.compareTo(occupant.read<String>('hlc_packed')) > 0) {
      // Incoming wins → free the slot by tombstoning the current occupant
      // (the loser), stamped one counter above the occupant's own HLC.
      final loserId = occupant.read<String>('id');
      final stamp = _derivedHlc(Hlc.parse(occupant.read<String>('hlc_packed')));
      final tombstoneAt = rec.fields['hlc_wall'];
      await db.customStatement(
        'UPDATE tracks SET deleted_at = ?, updated_at = ?, hlc_wall = ?, '
        'hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?',
        [
          _bind(tombstoneAt),
          _bind(tombstoneAt),
          stamp.wallMs,
          stamp.counter,
          stamp.node,
          stamp.packed,
          loserId,
        ],
      );
      await _journalSlotTombstone(loserId, stamp);
      return rec.fields;
    }
    // Incoming loses → store it tombstoned (stamped one counter above its own
    // HLC, dated to the occupant/winner's wall) so it never occupies the live
    // slot and the resolution replicates to peers still holding it live.
    final stamp = _derivedHlc(rec.hlc);
    await _journalSlotTombstone(rec.id, stamp);
    return {
      ...rec.fields,
      'deleted_at': occupant.read<int>('hlc_wall'),
      'updated_at': occupant.read<int>('hlc_wall'),
      'hlc_wall': stamp.wallMs,
      'hlc_ctr': stamp.counter,
      'hlc_node': stamp.node,
      'hlc_packed': stamp.packed,
    };
  }

  /// A DERIVED stamp for a local resolution write: one counter above [base],
  /// under this device's node. Strictly above the row's current stamp (so
  /// the write replicates via LWW) yet strictly below any later real edit
  /// (every device holding the row has advanced its clock past [base], so
  /// its next tick out-ranks the bump) — derived state must never out-rank
  /// user intent. Same derivation as IntegrityChecker's repair stamps.
  Hlc _derivedHlc(Hlc base) {
    var wall = base.wallMs;
    var counter = base.counter + 1;
    if (counter > HlcClock.maxCounter) {
      // Keep the packed encoding's fixed-width counter: spill to the next
      // millisecond, exactly like HlcClock does.
      wall += 1;
      counter = 0;
    }
    return Hlc(wallMs: wall, counter: counter, node: clock.node);
  }

  /// Journals the slot-resolution tombstone so the resolution replicates.
  /// Runs inside the record's savepoint: the row write and its ledger entry
  /// commit (or roll back) together, like every local write.
  Future<void> _journalSlotTombstone(String id, Hlc hlc) {
    return db
        .into(db.oplog)
        .insert(
          OplogCompanion.insert(
            entityTable: 'tracks',
            entityId: id,
            op: 'tombstone',
            hlcPacked: hlc.packed,
            payloadHash: '',
            createdAt: hlc.wallMs,
          ),
        );
  }

  /// Localizes an inbound record's per-device bookkeeping before writing:
  /// drops [perDeviceColumns] and then force-sets [inboundPerDeviceOverrides].
  ///
  /// The strip: the builder already omits per-device columns from the
  /// outbound projection, but a peer on an older build (or any non-conforming
  /// pusher) may still send them — applying them verbatim would overwrite
  /// THIS device's local bookkeeping (e.g. clobbering assets.local_rel_path
  /// so resolve() misses bytes that exist here). Stripped, they are ordinary
  /// missing columns: an UPDATE leaves the local values untouched and an
  /// INSERT takes the schema defaults.
  ///
  /// The overrides: some per-device state is DEDUCIBLE from the arrival
  /// itself — an assets row that came over the wire is by definition
  /// record-synced, and leaving an inserted row on the 'localOnly' schema
  /// default would hand the asset-GC's never-synced guard exactly the rows it
  /// must protect (see [inboundPerDeviceOverrides] for the full rationale).
  static Map<String, Object?> _localizePerDevice(
    String table,
    Map<String, Object?> columns,
  ) {
    final perDevice = perDeviceColumns[table];
    final overrides = inboundPerDeviceOverrides[table];
    if (perDevice == null && overrides == null) return columns;
    return {
      for (final e in columns.entries)
        if (perDevice == null || !perDevice.contains(e.key)) e.key: e.value,
      ...?overrides,
    };
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
