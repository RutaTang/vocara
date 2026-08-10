import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';
import 'quarantine_store.dart';
import 'sync_row.dart';
import 'sync_state_store.dart';
import 'sync_transport.dart';

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
/// converge; it is bounded to one write per losing row, see [_resolveSlot].
/// The carve-out is REVERTIBLE: if the FK sweep parks the very record whose
/// application minted it, the tombstone and its journal entry are undone
/// with the park, so no track dies for a record that never landed — see
/// [_revertSlotResolution].)
/// The clock is advanced so subsequent LOCAL writes stay causally after
/// everything we've seen; a record whose clock is implausibly far in the
/// future is quarantined instead of merged (one bad peer can't poison the
/// fleet). The gate and the advance are split deliberately:
/// [HlcClock.assertPlausible] (check-only, same drift rule) runs with the
/// other door gates, and the state-changing [HlcClock.mergeVerified] calls
/// run only AFTER the batch's outer transaction has committed, for the
/// records whose savepoint settled as applied or skipped AND for the
/// plausible stamps of records the door gates PARKED (see
/// [_noteParkedStamp]: a parked snapshot stays eligible to win LWW at
/// replay, so later local writes must stamp above it) — a record that
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
/// ([_sweepFkOrphans]), healing by provenance: a row INSERTED by this batch
/// is PARKED to the quarantine store and then deleted (the parent may be
/// locally quarantined — e.g. a newer-schema row the schema gate rejected —
/// in which case the server never re-delivers the child, and only the
/// quarantine replay can land it once the parent exists); a pre-existing row
/// UPDATED by this batch is parked the same way and then RESTORED verbatim
/// to its pre-batch snapshot (the update was effectively quarantined at the
/// door); a dangling pre-existing row this batch never touched only has its
/// dangling pointer column nulled. The outer transaction is wrapped so a
/// commit failure degrades to a failed batch instead of throwing out of
/// apply().
class SyncApplier {
  SyncApplier(this.db, this.clock, {required this.builder, this.quarantine})
    : _state = SyncStateStore(db);

  final AppDatabase db;
  final HlcClock clock;

  /// Where the durable HLC high-water mark is kept. Written inside the apply
  /// transaction so a restart can restore the monotonicity the in-memory
  /// clock gained from stamps whose records never became rows.
  final SyncStateStore _state;

  /// Projects a row back into its full wire snapshot so the FK sweep can
  /// PARK an orphan instead of losing it, and so [apply] can capture the
  /// pre-update restore point of every row it updates (see
  /// [_sweepFkOrphans]).
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
  /// [rejects] are records of the same page that never DECODED into a row
  /// (see [RejectedRecord]). They are parked as dead letters inside this
  /// batch's transaction, which is what makes them atomic with the caller's
  /// change-token write: parking them afterwards would lose them to a crash
  /// in between, and the server never re-delivers a record the token passed.
  Future<ApplyResult> apply(
    List<SyncRow> records, {
    Future<void> Function()? beforeCommit,
    List<RejectedRecord> rejects = const [],
  }) async {
    final result = ApplyResult();
    // (table, id) pairs INSERTED by this batch — the only rows the FK sweep is
    // allowed to park-and-delete (see _sweepFkOrphans).
    final insertedThisBatch = <(String, String)>{};
    // (table, id) -> the PRE-BATCH wire snapshot of every pre-existing row
    // this batch updated, captured just before the first update overwrote it.
    // The FK sweep restores from these when the update itself planted a
    // dangling pointer (see _sweepFkOrphans). Costs one extra row read per
    // applied update — the price of never nulling a reference that a parked
    // parent could still satisfy.
    final updatedThisBatch = <(String, String), SyncRow>{};
    // (table, id) -> the slot-resolution side effects the record(s) for that
    // row triggered while applying (occupant tombstone + journal entry, or
    // the record's own at-the-door tombstone + journal entry). The FK sweep
    // REVERTS these when it parks the row: a record that is quarantined at
    // the door must not leave behind a tombstone (and its oplog entry)
    // minted on its behalf (see _revertSlotResolution).
    final slotResolutions = <(String, String), _SlotResolution>{};
    // Per-batch live-schema cache for the column gate (see [_liveColumns]).
    final liveColumns = <String, Set<String>>{};
    // Stamps this batch ACCEPTED: records whose savepoint settled as applied
    // or skipped, plus door-parked records whose plausible stamp was noted by
    // [_noteParkedStamp]. Their state-changing clock.mergeVerified runs only
    // once the outer transaction has committed (bottom of this method) — an
    // advance for records the outer commit then rolled back would be a side
    // effect surviving the rollback.
    final acceptedHlcs = <Hlc>[];
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
              _noteParkedStamp(rec, acceptedHlcs);
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
              _noteParkedStamp(rec, acceptedHlcs);
              continue;
            }
            // Entity ids become filesystem path components once applied
            // (`audio/<practiceId>/`, `technique_media/<techniqueId>/`, and
            // the recursive deletes of both), so a hostile id carrying `../`
            // or a separator is a write/delete primitive OUTSIDE the media
            // tree. Parked per record, like every other malformed shape —
            // and like them, deliberately here rather than in fromJson,
            // which fails whole pages.
            try {
              SyncRow.checkIdShapes(rec.table, rec.fields);
            } on FormatException catch (e) {
              result.failed++;
              await _quarantine(rec, 'invalid-id: ${e.message}');
              _noteParkedStamp(rec, acceptedHlcs);
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
              _noteParkedStamp(rec, acceptedHlcs);
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
              _noteParkedStamp(rec, acceptedHlcs);
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
                _noteParkedStamp(rec, acceptedHlcs);
                continue;
              }
            }
            // Every value must BIND to its column. SQLite's dynamic typing
            // would take a string into an INTEGER column without complaint,
            // and the failure then surfaced far away — drift hydrating the row
            // and throwing inside a view model — instead of here, where the
            // record can still be parked with its evidence. Outside the
            // schema_v == current branch on purpose: an older peer's columns
            // must bind too.
            try {
              checkColumnTypes(_tables[rec.table]!, rec.fields);
            } on FormatException catch (e) {
              result.failed++;
              await _quarantine(rec, 'invalid-value: ${e.message}');
              _noteParkedStamp(rec, acceptedHlcs);
              continue;
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
            SyncRow? priorSnapshot;
            _SlotResolution? slotEffects;
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
              final resolved = await _resolveSlot(rec);
              slotEffects = resolved.effects;
              final columns = _localizePerDevice(rec.table, resolved.columns);
              if (existing == null) {
                await _insert(rec.table, columns);
                didInsert = true;
              } else {
                // Capture the row's pre-update wire snapshot BEFORE the
                // write, so the FK sweep can restore the exact prior state
                // if this very update turns out to have planted a dangling
                // pointer (see _sweepFkOrphans and [updatedThisBatch] for
                // the one-extra-read cost note).
                priorSnapshot = await builder.build(rec.table, rec.id);
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
            acceptedHlcs.add(rec.hlc);
            if (applied) {
              result.applied++;
              // Record any slot-resolution side effects this record's
              // savepoint committed (a rolled-back savepoint discards
              // slotEffects with everything else). Accumulated per row:
              // several records for the same id can each trigger a
              // resolution, and the sweep must be able to walk every one of
              // their revert pairs when the row is parked.
              final effects = slotEffects;
              if (effects != null) {
                final acc = slotResolutions.putIfAbsent((
                  rec.table,
                  rec.id,
                ), () => effects);
                if (!identical(acc, effects)) acc.absorb(effects);
              }
              if (didInsert) {
                insertedThisBatch.add((rec.table, rec.id));
              } else {
                final prior = priorSnapshot;
                // A row inserted EARLIER IN THIS BATCH and updated now is
                // already covered by the park-and-delete path; its mid-batch
                // snapshot is not a restore point. putIfAbsent: with two
                // updates to the same pre-existing row in one batch, the
                // restore point must stay the PRE-BATCH state, not the state
                // between the two updates.
                if (prior != null &&
                    !insertedThisBatch.contains((rec.table, rec.id))) {
                  updatedThisBatch.putIfAbsent((
                    rec.table,
                    rec.id,
                  ), () => prior);
                }
              }
            } else {
              result.skipped++;
            }
          } on _QuarantinePutException {
            // A record failed to PARK: the loop's usual answer to a
            // per-record throw — quarantine it and move on — is exactly the
            // thing that just failed. Rethrow so the outer transaction rolls
            // back (token unmoved, page retried) instead of committing a
            // batch that silently lost a rejected record (see [_quarantine]).
            rethrow;
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
        for (final reject in rejects) {
          result.failed++;
          await _parkUndecodable(reject);
        }
        await _sweepFkOrphans(
          result,
          insertedThisBatch,
          updatedThisBatch,
          slotResolutions,
        );
        // Durably record what the in-memory clock is about to merge, inside
        // this transaction so the mark can never claim stamps a rollback threw
        // away. The advance itself still happens only post-commit; this is the
        // part that has to survive a restart, because a stamp whose record the
        // FK sweep parked leaves no hlc_packed anywhere for the boot seed to
        // find (see SyncStateStore.hlcHighWaterKey).
        if (acceptedHlcs.isNotEmpty) {
          await _state.raiseHlcHighWater(
            acceptedHlcs
                .map((h) => h.packed)
                .reduce((a, b) => a.compareTo(b) >= 0 ? a : b),
          );
        }
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
      // accepted record's stamp, so subsequent local writes stay causally
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
      // different layer — and door-parked records join for the same reason
      // (see [_noteParkedStamp]). Deferring the advance also means the drift
      // gate judged every record of this batch against the PRE-batch clock
      // reference — that is deliberate and conservative: a record plausible
      // only relative to an earlier in-batch stamp quarantines now and is
      // un-parked by the sync engine's end-of-run quarantine replay, once
      // this commit's merges have raised the reference.
      for (final hlc in acceptedHlcs) {
        clock.mergeVerified(hlc);
      }
    }
    return result;
  }

  /// Records a DOOR-PARKED record's stamp for the post-commit clock advance
  /// and the durable high-water mark, exactly like a settled record's.
  ///
  /// A door park is not a rollback: the parked snapshot survives the batch
  /// and stays eligible to win LWW at replay — the newer-schema park is the
  /// DESIGNED honest path for a mixed-version fleet ("the quarantine replay
  /// un-parks it after the app upgrade"). Skipping its stamp let a later
  /// local edit stamp BELOW the parked record, and the replay then silently
  /// reverted the user's newer edit under LWW — the exact failure
  /// [SyncStateStore.hlcHighWaterKey] documents. The clock's invariant is to
  /// advance past everything the device has SEEN, row survival being a
  /// different layer (same rationale as the FK-orphan merges, see the bottom
  /// of [apply]).
  ///
  /// Two stamps are still excluded, deliberately: a malformed one (no
  /// well-formed stamp exists to advance past — the park itself keys on
  /// whatever hlc_packed the record carried) and an implausible one (the
  /// drift discipline holds at EVERY gate: a suspect stamp must never
  /// ratchet the clock, which is why the drift gate's own handler never
  /// calls this). And without a quarantine store (counter-only test wiring)
  /// nothing is parked at all — no surviving snapshot can out-rank a later
  /// local edit, and the mark must not claim stamps nothing holds.
  void _noteParkedStamp(SyncRow rec, List<Hlc> acceptedHlcs) {
    if (quarantine == null) return;
    final Hlc hlc;
    try {
      hlc = rec.hlc;
    } catch (_) {
      return;
    }
    try {
      clock.assertPlausible(hlc);
    } on HlcDriftException {
      return;
    }
    acceptedHlcs.add(hlc);
  }

  /// Persists a rejected record to the quarantine parking lot (latest state
  /// per row) INSIDE the current outer apply transaction.
  ///
  /// A STORAGE failure here fails the WHOLE batch, aligned with the FK
  /// sweep's park: the put is the only thing standing between a rejected
  /// record and silent permanent loss — the change token advances past this
  /// page when the batch commits, so a record whose park was dropped would
  /// be forever behind the token (while its dependents may have parked
  /// fine), unreachable by any replay. The failure is wrapped in
  /// [_QuarantinePutException] so the per-record handler in [apply] rethrows
  /// it (re-parking through the same failing store cannot succeed) and the
  /// outer transaction rolls back: token unmoved, a retried page over a
  /// silent loss.
  ///
  /// The two early returns below are the ONE remaining silent drop, and they
  /// are unavoidable: a record with no usable id cannot be keyed into the
  /// parking lot (or retried into any table) at all, and a payload that
  /// cannot even be serialized can never be replayed — in both cases there
  /// is genuinely nothing addressable to park, and the counters still record
  /// the rejection.
  ///
  /// The [reason] prefixes written here are part of the store's retention
  /// contract: [QuarantineStore.put] classifies parks as replayable or
  /// permanently dead by them, so a renamed gate reason must be reflected
  /// there.
  Future<void> _quarantine(SyncRow rec, String reason) async {
    final store = quarantine;
    if (store == null) return;
    final id = rec.fields['id'];
    String parkId;
    String payload;
    var parkReason = reason;
    if (id is String && id.isNotEmpty) {
      parkId = id;
      try {
        payload = rec.encode();
      } catch (_) {
        // Un-encodable (hand-built non-scalar values — wire rows are
        // scalar-only by SyncRow.fromJson): park a DEAD LETTER instead of
        // dropping silently. It can never replay, but the raw evidence
        // survives for review — silent loss is exactly what quarantine
        // exists to prevent.
        (parkId, payload) = _deadLetter(rec);
        parkReason = 'dead-letter($reason)';
      }
    } else {
      // No addressable id: same dead-letter treatment under a synthesized
      // content-hash key.
      (parkId, payload) = _deadLetter(rec);
      parkReason = 'dead-letter($reason)';
    }
    final hlcPacked = rec.fields['hlc_packed'];
    final schemaV = rec.fields['schema_v'];
    try {
      await store.put(
        table: rec.table,
        id: parkId,
        hlcPacked: hlcPacked is String ? hlcPacked : '',
        schemaV: schemaV is int ? schemaV : 0,
        payload: payload,
        reason: parkReason,
        quarantinedAt: DateTime.now().millisecondsSinceEpoch,
      );
    } catch (e) {
      throw _QuarantinePutException(e);
    }
  }

  /// The sentinel table undecodable payloads park under. Not a real synced
  /// table on purpose: the record never told us which table it belonged to,
  /// and inventing one would put a review artifact in a live table's space.
  static const String _deadLetterTable = '!undecoded';

  /// Parks a record that never decoded into a row. It has no table, id or
  /// stamp to route by, so it goes to the same dead-letter space as an
  /// unparkable record: content-keyed, deliberately not a valid [SyncRow]
  /// envelope, so the replay's decode refuses it and it stays for review.
  Future<void> _parkUndecodable(RejectedRecord reject) async {
    final store = quarantine;
    if (store == null) return;
    final payload = jsonEncode({
      'deadLetter': true,
      'undecodable': true,
      'raw': reject.payload,
    });
    final digest = sha256.convert(utf8.encode(payload)).toString();
    try {
      await store.put(
        table: _deadLetterTable,
        id: 'deadletter${digest.substring(0, 32)}',
        hlcPacked: '',
        schemaV: 0,
        payload: payload,
        reason: 'dead-letter(undecodable: ${reject.reason})',
        quarantinedAt: DateTime.now().millisecondsSinceEpoch,
      );
    } catch (e) {
      throw _QuarantinePutException(e);
    }
  }

  /// A best-effort, always-encodable snapshot of an otherwise unparkable
  /// record, keyed by a content hash so distinct dead letters get distinct
  /// rows. The payload is deliberately NOT a valid [SyncRow] envelope: the
  /// quarantine replay's decode fails and keeps it parked — a dead letter is
  /// for human review, never re-application.
  (String, String) _deadLetter(SyncRow rec) {
    final safe = <String, Object?>{};
    rec.fields.forEach((k, v) {
      safe[k] = (v == null || v is num || v is String || v is bool)
          ? v
          : v.toString();
    });
    final payload = jsonEncode({
      'deadLetter': true,
      'table': rec.table,
      'fields': safe,
    });
    // A content digest of the payload: deterministic across runs
    // (String.hashCode is not), so the same dead letter re-parks onto the same
    // row. Wide on purpose — the previous 32-bit FNV collided in a space small
    // enough to reach, and a collision is SILENT DATA LOSS here: two distinct
    // dead letters share a (table, id), and the quarantine upsert compares
    // hlc_packed, which is usually '' for a dead letter, so the second is
    // dropped rather than stored.
    final digest = sha256.convert(utf8.encode(payload)).toString();
    return ('deadletter${digest.substring(0, 32)}', payload);
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
  ///    the batch (a retried page over silent loss). An LWW-FILTERED park
  ///    (not an error) is safe by the store's replay-class retention: a
  ///    sweep park is only ever filtered by a strictly newer REPLAYABLE
  ///    state, which supersedes the row once its own replay lands — a
  ///    permanently-unreplayable entry can never hold the slot against it
  ///    (see [QuarantineStore.put]). Without a quarantine
  ///    store (counter-only test wiring) the old hard-delete stands.
  ///  * A PRE-EXISTING row UPDATED by this batch is PARKED, then RESTORED —
  ///    never deleted. It satisfied every FK when the batch began (SQLite
  ///    enforced them at every earlier commit), so the dangling pointer was
  ///    planted by this batch's own update, and the record is treated
  ///    exactly like the inserted case: it was effectively quarantined at
  ///    the door. Its CURRENT (post-update, remote-HLC) snapshot goes to the
  ///    quarantine store under the same fk-orphan reason, and the row is
  ///    then reverted VERBATIM to the pre-batch snapshot [apply] captured
  ///    just before the update — the exact prior state including its HLC,
  ///    deliberately NOT a derived stamp (a derived stamp mints new state
  ///    that can out-rank a peer's legitimate write — the resurrection class
  ///    of bug docs/sync-protocol.md warns about — while a verbatim revert
  ///    re-asserts state that already existed, so nothing new enters the
  ///    fleet). One carve-out: a live single-slot snapshot whose slot a
  ///    LATER in-page winner now holds is restored TOMBSTONED under a
  ///    derived stamp and journaled instead — a verbatim live restore would
  ///    trip ux_track_slot and brick the page (see [_restorePriorSnapshot]
  ///    for the full invariant). Convergence: the parked snapshot is
  ///    strictly newer than the
  ///    restored row, so once the missing parent lands the quarantine replay
  ///    applies it by plain LWW; and a re-delivered IDENTICAL update is
  ///    likewise strictly newer than the restored row, so it re-applies (and
  ///    is re-parked here while the parent is still missing) — under the old
  ///    null-the-pointer heal that re-delivery was LWW-equal to the healed
  ///    row and skipped, leaving the reference nulled FOREVER unless some
  ///    strictly-newer update happened to arrive. Because the restore heals
  ///    every repointed column at once, this path covers dangling NOT NULL
  ///    columns too: the abort below is unreachable for updated-this-batch
  ///    rows (with a store wired), turning the old retry-forever page into a
  ///    committed batch plus a parked, replayable update.
  ///  * A dangling PRE-EXISTING row this batch NEVER touched (and, without a
  ///    quarantine store — counter-only test wiring — any dangling
  ///    pre-existing row, since parking is impossible) keeps the old heal:
  ///    only the dangling column is nulled (the IntegrityChecker-style
  ///    scalar self-heal), deliberately un-journaled like every remote
  ///    apply. No remote apply can CREATE this case any more — every
  ///    dangling pre-existing row is one this batch just updated — so it
  ///    guards states that arose outside the applier's control. Should a
  ///    NOT NULL column dangle on such a row, we still abort so the WHOLE
  ///    batch rolls back — a retried page over silently deleting
  ///    pre-existing data (deleting the row — and then its subtree via the
  ///    fixpoint loop — would destroy local data with no tombstone and no
  ///    oplog).
  ///
  /// Loops to a fixpoint because deleting an orphan can strand its own
  /// batch-inserted children. Counters: a deleted batch-insert and a
  /// restored batch-update each move from applied to failed exactly once
  /// (duplicate violations for a multi-FK row are skipped once the row is
  /// deleted or restored); a nulled untouched row changes no counter.
  ///
  /// Slot-resolution revert: parking a row is "quarantined at the door", so
  /// on BOTH park paths any slot-resolution side effects the parked record
  /// triggered ([slotResolutions]) are reverted with it — per occupant, as
  /// atomic pairs: exactly that resolution's oplog entries are deleted and
  /// that occupant restored to its pre-resolution snapshot, or (when the
  /// stamp was superseded or a later winner holds the slot) the WHOLE pair
  /// is kept — tombstone and journal together, never a ghost (see
  /// [_revertSlotResolution]). The revert runs AFTER the parked row itself
  /// is deleted/restored, deliberately: a restored occupant comes back LIVE,
  /// and restoring it while the parked record still held the slot live
  /// would trip the ux_track_slot unique index.
  /// A parked record that LOST the slot was stored tombstoned at the door
  /// under a derived stamp; parking that derived state would replay an
  /// UN-journaled tombstone (the revert just deleted its journal entry), so
  /// the parked snapshot is substituted with the ORIGINAL incoming record —
  /// the replay then re-runs the resolution from scratch, re-deriving and
  /// re-journaling whatever it still decides. Counter-only wiring (no store)
  /// keeps today's behavior on every path, revert included.
  Future<void> _sweepFkOrphans(
    ApplyResult result,
    Set<(String, String)> insertedThisBatch,
    Map<(String, String), SyncRow> updatedThisBatch,
    Map<(String, String), _SlotResolution> slotResolutions,
  ) async {
    // Rows already park-and-restored by THIS sweep. A restored row can never
    // be flagged by a LATER fixpoint pass (its pre-batch state satisfied
    // every FK when the batch began, and the sweep only ever deletes rows
    // this batch inserted — which a pre-batch snapshot cannot reference),
    // but WITHIN the pass that restored it, foreign_key_check may have
    // reported it once per violated constraint and the later entries are
    // stale.
    final restored = <(String, String)>{};
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
        final store = quarantine;
        if (insertedThisBatch.contains((table, id))) {
          if (store != null) {
            // Park BEFORE deleting (same outer transaction, so the entry and
            // the delete commit or roll back together). The snapshot is the
            // shared outbound projection — never a hand-written second one —
            // and reads the row's own adopted HLC straight from the table
            // (or the ORIGINAL incoming record when the row holds a
            // slot-resolution tombstone the revert below is about to
            // disown — see [_parkableSnapshot]).
            final snapshot = _parkableSnapshot(
              await builder.build(table, id),
              slotResolutions[(table, id)],
            );
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
          if (store != null) {
            // The record is quarantined at the door: side effects minted on
            // its behalf must not outlive it. After the delete, so a
            // restored occupant can never transiently share the live slot
            // with the parked row.
            await _revertSlotResolution(slotResolutions.remove((table, id)));
          }
          // The insert was counted as applied above; it did not stick.
          if (result.applied > 0) result.applied--;
          result.failed++;
          continue;
        }
        // Pre-existing row UPDATED by this batch: the update itself planted
        // the dangling pointer, so park the post-update snapshot and restore
        // the pre-batch state verbatim (see the doc comment). Without a
        // store the old null-the-pointer heal below stands, like the
        // inserted case's hard-delete fallback.
        final priorSnapshot = updatedThisBatch[(table, id)];
        if (priorSnapshot != null && store != null) {
          if (!restored.add((table, id))) {
            // A later violation entry of the SAME pass for a row already
            // restored (several FK columns repointed at once): the verbatim
            // restore healed every column, so the entry is stale.
            continue;
          }
          // Park the CURRENT (post-update, remote-HLC) snapshot BEFORE the
          // restore overwrites it — the shared outbound projection and the
          // same reason format as the inserted case (with the same
          // original-record substitution when the row holds a
          // slot-resolution tombstone, see [_parkableSnapshot]). Strictly
          // newer than the restored row, so the quarantine replay lands it
          // by plain LWW once the parent exists. Like the inserted case's
          // park, a storage failure here aborts the batch rather than
          // losing the update.
          final snapshot = _parkableSnapshot(
            await builder.build(table, id),
            slotResolutions[(table, id)],
          );
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
            // Restore the prior snapshot — verbatim in the normal case,
            // tombstoned under a derived stamp when a later in-page winner
            // now holds its single-track slot live (see
            // [_restorePriorSnapshot] for the invariant). After this the
            // row has no dangling pointer, so the nullable and NOT NULL
            // cases converge on this same path.
            await _restorePriorSnapshot(priorSnapshot);
            // Quarantined at the door, like the inserted case: revert any
            // slot resolution the parked record triggered — after the
            // restore above put the row back on its (pre-batch, slot-free
            // or slot-consistent) state, so the returning occupant cannot
            // transiently double-occupy the live slot.
            await _revertSlotResolution(slotResolutions.remove((table, id)));
            // The update was counted as applied above; it did not stick.
            if (result.applied > 0) result.applied--;
            result.failed++;
            continue;
          }
          // The violation proves the row exists, so a null snapshot cannot
          // happen; if it ever does, fall through to the scalar heal below
          // rather than crash the sweep.
          restored.remove((table, id));
        }
        // Untouched pre-existing row (or counter-only wiring): null just the
        // dangling column. Which column comes from the violation's fkid
        // resolved against foreign_key_list.
        final fkid = v.readNullable<int>('fkid');
        final column = fkid == null ? null : await _fkColumn(table, fkid);
        if (column == null || !await _isNullable(table, column)) {
          // Un-heal-able (see doc comment): fail the batch, keep the data.
          // Residual by construction — an updated-this-batch row (with a
          // store wired) took the park-and-restore path above instead.
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

  /// Restores an updated-this-batch row the sweep just parked to its
  /// pre-batch [snapshot] — verbatim in the normal case: a pure revert to
  /// the exact prior state including its HLC, never a derived stamp (a
  /// derived stamp mints new state that can out-rank a peer's legitimate
  /// write — the resurrection class of bug docs/sync-protocol.md warns
  /// about — while a verbatim revert re-asserts state that already
  /// existed).
  ///
  /// The one exception is a live single-slot track whose slot a LATER
  /// in-page winner now holds: restoring the snapshot live would trip the
  /// ux_track_slot partial unique index OUTSIDE any savepoint, fail the
  /// outer commit and retry the same page forever (token unmoved). Skipping
  /// the restore is no answer either — the post-update row still dangles,
  /// so the sweep's fixpoint would spin — and nulling just the pointer
  /// leaves the table LWW-equal to the parked snapshot, which then never
  /// replays (the permanent-null regression the park-and-restore path
  /// exists to fix). So the collision is resolved the way the applier
  /// resolves every slot conflict: the snapshot is restored TOMBSTONED
  /// under a derived stamp (one counter above its own prior HLC, dated to
  /// the winner's wall like every slot tombstone) and the tombstone is
  /// JOURNALED.
  ///
  /// INVARIANT (pinned by test): the page stays committable — the
  /// tombstoned row escapes the partial index — and every state left behind
  /// is accounted. The journaled tombstone matches the row the table
  /// actually holds and replicates to peers, out-ranking the pre-batch
  /// state they still hold — exactly the resolution they would have derived
  /// had they applied the winner themselves. And the PARKED post-update
  /// snapshot still out-ranks the derived stamp (it beat the prior state by
  /// LWW, and a derived stamp stays below any later real edit — see
  /// [_derivedHlc]), so the quarantine replay lands it once the missing
  /// parent arrives and re-runs the slot resolution from scratch.
  Future<void> _restorePriorSnapshot(SyncRow snapshot) async {
    final winnerWall = await _slotWinnerWall(snapshot);
    if (winnerWall == null) {
      await _update(snapshot.table, snapshot.fields);
      return;
    }
    final stamp = _derivedHlc(snapshot.hlc);
    await _update(snapshot.table, {
      ...snapshot.fields,
      'deleted_at': winnerWall,
      'updated_at': winnerWall,
      'hlc_wall': stamp.wallMs,
      'hlc_ctr': stamp.counter,
      'hlc_node': stamp.node,
      'hlc_packed': stamp.packed,
    });
    await _journalSlotTombstone(snapshot.id, stamp);
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
  /// Alongside the columns it returns the resolution's side effects (a
  /// per-occupant revert pair — pre-tombstone snapshot, derived stamp,
  /// journal seq — or the record's own at-the-door stamp and seq) so [apply]
  /// can remember them per row and the FK sweep can REVERT them if the very
  /// record that triggered the resolution is later parked — without the
  /// revert, a record "quarantined at the door" would leave a tombstoned
  /// track and a journaled resolution behind for a write that never landed
  /// (see [_revertSlotResolution]).
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
  Future<({Map<String, Object?> columns, _SlotResolution? effects})>
  _resolveSlot(SyncRow rec) async {
    if (rec.table != 'tracks' || rec.fields['deleted_at'] != null) {
      return (columns: rec.fields, effects: null);
    }
    final kind = rec.fields['kind'];
    if (kind != 'accompaniment' && kind != 'reference') {
      return (columns: rec.fields, effects: null);
    }
    final practiceId = rec.fields['practice_id'];
    if (practiceId is! String) return (columns: rec.fields, effects: null);
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
    if (occupant == null) return (columns: rec.fields, effects: null);
    final effects = _SlotResolution(rec);
    if (rec.hlcPacked.compareTo(occupant.read<String>('hlc_packed')) > 0) {
      // Incoming wins → free the slot by tombstoning the current occupant
      // (the loser), stamped one counter above the occupant's own HLC. The
      // occupant's pre-tombstone snapshot is captured FIRST — the revert's
      // restore point if this record is later parked.
      final loserId = occupant.read<String>('id');
      final snapshot = await builder.build('tracks', loserId);
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
      final seq = await _journalSlotTombstone(loserId, stamp);
      // The occupant was selected just above, so build() returning null is a
      // never-expected race; without a restore point the pair is simply not
      // recorded, and on a revert the tombstone then stays WITH its journal
      // row — still an accounted state, never a ghost.
      if (snapshot != null) {
        effects.occupants.add(
          _OccupantRevert(
            snapshot: snapshot,
            stampPacked: stamp.packed,
            oplogSeqs: [seq],
          ),
        );
      }
      return (columns: rec.fields, effects: effects);
    }
    // Incoming loses → store it tombstoned (stamped one counter above its own
    // HLC, dated to the occupant/winner's wall) so it never occupies the live
    // slot and the resolution replicates to peers still holding it live.
    final stamp = _derivedHlc(rec.hlc);
    effects.selfStampPacked = stamp.packed;
    effects.selfOplogSeqs.add(await _journalSlotTombstone(rec.id, stamp));
    return (
      columns: {
        ...rec.fields,
        'deleted_at': occupant.read<int>('hlc_wall'),
        'updated_at': occupant.read<int>('hlc_wall'),
        'hlc_wall': stamp.wallMs,
        'hlc_ctr': stamp.counter,
        'hlc_node': stamp.node,
        'hlc_packed': stamp.packed,
      },
      effects: effects,
    );
  }

  /// The snapshot the FK sweep should PARK for a row it is about to
  /// quarantine at the door: normally the row's own [snapshot] as built from
  /// the table — except when the row currently holds the tombstoned-at-the-
  /// door state a slot [resolution] minted for it (the incoming record LOST
  /// the slot and was stored under a derived stamp). The revert is about to
  /// delete that resolution's journal entry, so parking the derived state
  /// would later replay an UN-journaled tombstone that no peer ever learns
  /// about; the ORIGINAL incoming record is parked instead, and the replay
  /// re-runs the slot resolution from scratch (re-deriving and re-journaling
  /// if it still loses). Identified by the row's stamp: any later record for
  /// the same id moved hlc_packed off the derived stamp, and its genuine
  /// remote state must then be parked as-is.
  SyncRow? _parkableSnapshot(SyncRow? snapshot, _SlotResolution? resolution) {
    if (snapshot == null || resolution == null) return snapshot;
    if (resolution.selfStampPacked != snapshot.hlcPacked) return snapshot;
    return resolution.record;
  }

  /// Reverts the side effects of a slot [resolution] whose triggering record
  /// the FK sweep just parked: the record was "quarantined at the door", so
  /// a track must not stay tombstoned — nor a resolution journaled — on
  /// behalf of a write that never landed. Rides the same outer transaction
  /// as the park, like every sweep write. A null [resolution] (no resolution
  /// recorded for the parked row) is a no-op.
  ///
  /// Occupants are reverted per entry as atomic PAIRS ([_OccupantRevert]):
  /// exactly this resolution's journal rows are deleted and the occupant is
  /// restored VERBATIM to its pre-resolution snapshot — its own prior HLC,
  /// never a derived stamp — together, or the WHOLE pair is skipped. A pair
  /// is restorable only while
  ///
  ///  * the occupant row still carries this resolution's derived tombstone
  ///    stamp — if a later record legitimately overwrote the tombstone by
  ///    LWW (or the occupant row was itself swept away), that state must
  ///    stand, reverting over it would resurrect old state — AND
  ///  * no OTHER live row currently holds the occupant's single-track slot,
  ///    checked with the same predicate the ux_track_slot partial unique
  ///    index enforces ([_slotWinnerWall]): a later in-page winner may have
  ///    claimed the slot after the parked record did, and restoring the
  ///    occupant live under it would trip the index OUTSIDE any savepoint,
  ///    fail the outer commit and retry the same page forever.
  ///
  /// A skipped pair leaves the occupant tombstoned WITH its journal rows —
  /// an accounted tombstone peers still learn — never the ghost of a kept
  /// tombstone whose ledger entry was deleted, and never a double-occupied
  /// live slot.
  ///
  /// Entries are walked LATEST-FIRST, deliberately: successive occupants of
  /// one slot were each tombstoned so the next could hold it, and restoring
  /// the latest restorable one (the earlier entries then skip on its
  /// occupancy) reproduces the state a peer that never saw the parked
  /// record derives from its own resolutions — the latest occupant live,
  /// every earlier one tombstoned under the same derived stamps. Walking
  /// earliest-first would instead restore the OLDEST occupant live, an
  /// un-journaled state no peer can converge to.
  Future<void> _revertSlotResolution(_SlotResolution? resolution) async {
    if (resolution == null) return;
    // The record's own at-the-door tombstone entries go with the park
    // unconditionally: the sweep just deleted/restored the row itself, and
    // the parked payload substitutes the ORIGINAL record for the derived
    // state (see [_parkableSnapshot]).
    if (resolution.selfOplogSeqs.isNotEmpty) {
      await (db.delete(
        db.oplog,
      )..where((o) => o.seq.isIn(resolution.selfOplogSeqs))).go();
    }
    for (final entry in resolution.occupants.reversed) {
      final snapshot = entry.snapshot;
      final current = await db
          .customSelect(
            'SELECT hlc_packed FROM ${snapshot.table} WHERE id = ?',
            variables: [Variable.withString(snapshot.id)],
          )
          .getSingleOrNull();
      if (current == null ||
          current.read<String>('hlc_packed') != entry.stampPacked) {
        continue; // superseded by a later LWW winner: skip the whole pair
      }
      if (await _slotWinnerWall(snapshot) != null) {
        continue; // the slot is held by a live later winner: skip the pair
      }
      await (db.delete(
        db.oplog,
      )..where((o) => o.seq.isIn(entry.oplogSeqs))).go();
      await _update(snapshot.table, snapshot.fields);
    }
  }

  /// The hlc_wall of the LIVE row — other than [snapshot]'s own id —
  /// currently holding [snapshot]'s single-track slot, or null when the slot
  /// is free or [snapshot] would not enter a slot at all (not a live
  /// accompaniment/reference track). The occupancy predicate mirrors the
  /// ux_track_slot partial unique index EXACTLY: this is the pre-flight
  /// check for every sweep-time restore, whose UPDATE runs outside any
  /// savepoint and would otherwise trip the index and fail the outer
  /// commit. The returned wall doubles as the tombstone time when the
  /// caller resolves the collision by restoring tombstoned
  /// ([_restorePriorSnapshot]), matching [_resolveSlot]'s convention that a
  /// slot tombstone is dated to the winner's wall clock.
  Future<int?> _slotWinnerWall(SyncRow snapshot) async {
    if (snapshot.table != 'tracks') return null;
    if (snapshot.fields['deleted_at'] != null) return null;
    final kind = snapshot.fields['kind'];
    if (kind is! String || (kind != 'accompaniment' && kind != 'reference')) {
      return null;
    }
    final practiceId = snapshot.fields['practice_id'];
    if (practiceId is! String) return null;
    final holder = await db
        .customSelect(
          'SELECT hlc_wall FROM tracks WHERE practice_id = ? AND kind = ? '
          'AND deleted_at IS NULL AND id <> ?',
          variables: [
            Variable.withString(practiceId),
            Variable.withString(kind),
            Variable.withString(snapshot.id),
          ],
        )
        .getSingleOrNull();
    return holder?.read<int>('hlc_wall');
  }

  /// A DERIVED stamp for a local resolution write, via [HlcClock.derived]:
  /// same (wall, counter) as [base], node extended by `'!' + device`. The
  /// extension out-ranks [base] (the write replicates) yet sorts below EVERY
  /// stamp above base — including the parked update that beat base by node
  /// alone, whose quarantine replay must land over this tombstone once its
  /// parent arrives. Deriving in node space (never counter space) is what
  /// guarantees that; see [HlcClock.derived]. Same derivation as
  /// IntegrityChecker's repair stamps.
  Hlc _derivedHlc(Hlc base) => HlcClock.derived(base, clock.node);

  /// Journals the slot-resolution tombstone so the resolution replicates,
  /// and returns the inserted entry's seq (drift's insert returns the new
  /// rowid, and for oplog's autoincrementing INTEGER PRIMARY KEY the rowid
  /// IS seq — no extra query, race-free inside the enclosing transaction).
  /// The seq lets the FK sweep DELETE exactly this entry if the record that
  /// triggered the resolution is later parked (see [_revertSlotResolution]).
  /// Runs inside the record's savepoint: the row write and its ledger entry
  /// commit (or roll back) together, like every local write.
  Future<int> _journalSlotTombstone(String id, Hlc hlc) {
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

/// The side effects ONE row's slot resolution(s) wrote during a batch,
/// remembered by [SyncApplier.apply] per (table, id) so the FK sweep can
/// revert them if the record that triggered them is later parked — the
/// resolution ran BEFORE the record's own application, so "quarantined at
/// the door" must undo it too (delete its oplog entries, restore the
/// tombstoned occupant). Several records for the same id can each trigger a
/// resolution within one batch; their effects are merged via [absorb],
/// which APPENDS — occupant entries are never folded into one another,
/// because each is an independent revert PAIR the sweep must decide on its
/// own. (Folding used to keep the FIRST occupant's snapshot under the LAST
/// occupant's derived stamp, so a two-occupant revert restored NEITHER
/// occupant while deleting BOTH journal entries — two tracks left
/// tombstoned with their ledger gone, a permanent unaccounted loss.)
class _SlotResolution {
  _SlotResolution(this.record);

  /// The (latest) incoming record that triggered a resolution for this row —
  /// the parked payload when the row itself holds the resolution's
  /// tombstoned-at-the-door state (see [SyncApplier._parkableSnapshot]).
  SyncRow record;

  /// The derived stamp the (latest) resolution wrote into the INCOMING
  /// record's own row (incoming-loses case): identifies the
  /// tombstoned-at-the-door state so the sweep can park the original record
  /// instead.
  String? selfStampPacked;

  /// Oplog seqs of the at-the-door tombstone entries the incoming-loses
  /// resolution(s) journaled for the record's OWN row. Deleted
  /// unconditionally with the park: the sweep just deleted/restored the row
  /// itself and parks the ORIGINAL record in place of the derived state, so
  /// these entries would journal a decision minted for a write that never
  /// landed.
  final List<int> selfOplogSeqs = [];

  /// One revert pair per occupant this row's resolution(s) tombstoned, in
  /// resolution order (earliest first). See [_OccupantRevert].
  final List<_OccupantRevert> occupants = [];

  /// Merges a later resolution's effects for the same row into this one.
  void absorb(_SlotResolution other) {
    record = other.record;
    selfStampPacked = other.selfStampPacked ?? selfStampPacked;
    selfOplogSeqs.addAll(other.selfOplogSeqs);
    occupants.addAll(other.occupants);
  }
}

/// One occupant a slot resolution tombstoned, held as an atomic revert
/// PAIR: the oplog rows exactly THIS resolution journaled, and the
/// occupant's verbatim restore point. [SyncApplier._revertSlotResolution]
/// either applies both halves (journal deleted + snapshot restored) or
/// neither (the occupant stays tombstoned WITH its journal rows — an
/// accounted tombstone peers still learn) — never the ghost in between, a
/// tombstone whose ledger entry was deleted and that therefore replicates
/// nowhere.
class _OccupantRevert {
  _OccupantRevert({
    required this.snapshot,
    required this.stampPacked,
    required this.oplogSeqs,
  });

  /// The occupant's full wire snapshot captured BEFORE the resolution
  /// tombstoned it: the verbatim restore point, its own prior HLC included.
  final SyncRow snapshot;

  /// The derived stamp the resolution wrote INTO the occupant's row; the
  /// pair is restorable only while the row still carries it.
  final String stampPacked;

  /// Oplog seqs of exactly this resolution's journal rows.
  final List<int> oplogSeqs;
}

/// Marker for a quarantine-store STORAGE failure inside [SyncApplier.apply]:
/// unlike every other per-record throw (which quarantines that record and
/// continues), this one must fail the WHOLE batch — re-parking through the
/// same failing store cannot succeed, and committing would advance the change
/// token past a record that was never parked (see [SyncApplier._quarantine]).
class _QuarantinePutException implements Exception {
  _QuarantinePutException(this.cause);

  final Object cause;

  @override
  String toString() => 'quarantine put failed: $cause';
}
