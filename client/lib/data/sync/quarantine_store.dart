import 'package:drift/drift.dart';

import '../db/app_database.dart';

/// Persistence for the LOCAL-ONLY `sync_quarantine` parking lot: the NEWEST
/// (by HLC) rejected remote state per (table, id) — see tables.dart for the
/// schema and the remote_hlc_packed naming rationale. A row has up to TWO
/// entries: the door's, keyed by its id ([put]), and the FK sweep's, keyed by
/// [sweepSlotId] ([putSwept]) — two replay obligations that must never
/// displace each other (see [putSwept]).
///
/// Like [SyncStateStore], every method is a plain drift operation so it joins
/// a caller-provided transaction: the [SyncApplier] persists a quarantine
/// inside the SAME outer transaction as the batch apply, keeping the parking
/// lot exactly consistent with what actually committed.
class QuarantineStore {
  QuarantineStore(this._db);

  final AppDatabase _db;

  /// Upserts the quarantined state for ([table], [id]), keeping the NEWEST
  /// state per row by [hlcPacked] — the store itself is Last-Writer-Wins, not
  /// last-caller-wins. The composite primary key already bounds the lot to
  /// one entry per key; the guard decides WHICH state that entry holds:
  ///
  ///  * an EQUAL [hlcPacked] always replaces the stored entry (a refreshed
  ///    reason/payload for the SAME record state — the fk-orphan re-park flow
  ///    re-parks the identical snapshot every replay cycle and must keep
  ///    working, and a replay whose verdict changed relabels the entry);
  ///  * between two states of the same replay class (see below), the strictly
  ///    newer [hlcPacked] wins and a strictly OLDER park is a deliberate
  ///    no-op. Parks are not ordered by arrival: a page can legally deliver
  ///    P@1000 (parked at the door) before an older P@950 that parks too,
  ///    and the round-6 park+restore can put a live row BACK on an older HLC
  ///    that a later sweep then parks — so ordering alone can never protect
  ///    the newest state. Dropping the older park loses nothing: under LWW
  ///    the replay of the newer parked snapshot supersedes anything the
  ///    older state could have contributed (the older state would lose that
  ///    same LWW settle anyway). That holds between two REFUSED states —
  ///    neither has been vetted, and the newest is the one LWW finally keeps
  ///    — but not against the FK sweep's park, a state that already passed
  ///    every gate, which is why the sweep parks under a key of its own (see
  ///    [putSwept]);
  ///  * ACROSS replay classes the stamp order does not decide: a state whose
  ///    gate can never pass ([_unreplayablePrefixes]) supersedes nothing —
  ///    the replay re-rejects it every cycle, forever — so it must neither
  ///    displace nor block a replayable state, whatever the stamps say. Every
  ///    replayable park is the ONLY copy of its state (the change token is
  ///    already past the record's page, so nothing re-delivers it): letting a
  ///    newer-stamped but permanently dead payload hold the slot turned that
  ///    park into silent loss — first seen on the FK sweep's park, which
  ///    since has a slot of its own ([putSwept]). The dropped garbage is only
  ///    evidence of a non-conforming writer; the dropped state would have
  ///    been real data.
  ///
  /// The SQL-level `ON CONFLICT ... DO UPDATE ... WHERE` makes the guard
  /// race-free inside the caller's transaction (no read-modify-write gap),
  /// and a filtered-out update is silently kept as the old row, not an error.
  Future<void> put({
    required String table,
    required String id,
    required String hlcPacked,
    required int schemaV,
    required String payload,
    required String reason,
    required int quarantinedAt,
  }) {
    final row = SyncQuarantineCompanion.insert(
      tbl: table,
      id: id,
      remoteHlcPacked: hlcPacked,
      schemaV: schemaV,
      payload: payload,
      reason: reason,
      quarantinedAt: quarantinedAt,
    );
    return _db
        .into(_db.syncQuarantine)
        .insert(
          row,
          onConflict: DoUpdate(
            (old) => row,
            // The retention rule from the doc comment, expressed over the
            // stored row (packed HLCs compare lexically): within a replay
            // class the stored state must not be newer than the incoming
            // one; across classes the replayable state wins outright, and
            // an equal stamp (same record state) always refreshes.
            where: (old) {
              final equal = old.remoteHlcPacked.equals(hlcPacked);
              final notNewer = old.remoteHlcPacked.isSmallerOrEqualValue(
                hlcPacked,
              );
              final storedUnreplayable = _unreplayablePrefixes
                  .map((p) => old.reason.like('$p%'))
                  .reduce((a, b) => a | b);
              return isReplayable(reason)
                  ? notNewer | storedUnreplayable
                  : equal | (notNewer & storedUnreplayable);
            },
          ),
        );
  }

  /// The parking-lot key of row [id]'s FK-SWEEP slot: the row id plus a
  /// suffix outside the wire id alphabet (SyncRow's id shape admits only
  /// `[0-9A-Za-z_-]`, and the sweep only ever parks rows that passed it), so
  /// no conforming record's door entry can share the key — a hostile id
  /// spelling it parks as 'invalid-id:', which [put]'s class rule never lets
  /// displace the replayable sweep park. The payload still carries the real
  /// id: the replay decodes it, never this key.
  static String sweepSlotId(String id) => '$id$_sweepSlotSuffix';

  static const String _sweepSlotSuffix = '#fk-sweep';

  /// The SQL twin of a row's keys: a predicate over the `sync_quarantine`
  /// row aliased [alias], TRUE when it is an entry OF the row whose id is
  /// the SQL expression [idExpr] — its door entry or its sweep slot
  /// ([sweepSlotId]). A reader asking "is a newer state of this row
  /// parked?" must match both: the sweep's park is often exactly that state
  /// (an update parked and restored over, see [putSwept]).
  static String entryOfRowSql(String alias, String idExpr) =>
      "$alias.id IN ($idExpr, $idExpr || '$_sweepSlotSuffix')";

  /// Parks the FK sweep's snapshot of row ([table], [id]) in that row's
  /// SWEEP slot ([sweepSlotId]) — beside the door's (table, id) entry, never
  /// in competition with it.
  ///
  /// The two hold different replay obligations, and neither can stand in
  /// for the other. A door park is a state some gate REFUSED; it replays
  /// once that gate passes (a recovered clock, an app upgrade). The sweep's
  /// park is a state that passed EVERY gate and applied, rolled back only
  /// because its parent has not arrived yet — the sole surviving copy of a
  /// row the sweep then deleted or restored to its pre-batch state, due the
  /// moment the parent lands. Sharing one slot let them displace each other
  /// by stamp: a drift-parked newer state (a peer's runaway clock) evicted
  /// the sweep's copy — or, parked first on the same page, filtered the
  /// sweep's put out — and once the parent arrived the replay found only the
  /// still-implausible state, so the row stayed deleted (or stale) for as
  /// long as the clock stayed behind, while every peer that held the parent
  /// showed the honest state. [put]'s "the newer state supersedes it" holds
  /// only once the newer state CAN replay, and a door gate is not the
  /// sweep's gate. In separate slots each entry replays and settles on its
  /// own terms, and plain LWW at replay converges the row whichever lands
  /// first.
  ///
  /// Within the slot [put]'s retention applies unchanged (every sweep park
  /// is replayable, so the newest wins and an equal stamp refreshes the
  /// re-park cycle). A door entry holding the SAME state (equal stamp) is
  /// folded away: that state is now accounted for here under its current
  /// verdict — the relabel a door park whose replay applied and was then
  /// swept used to get from the shared slot, and the path by which an
  /// fk-orphan entry parked under the bare row id by an older build moves
  /// over on its first replay.
  Future<void> putSwept({
    required String table,
    required String id,
    required String hlcPacked,
    required int schemaV,
    required String payload,
    required String reason,
    required int quarantinedAt,
  }) => _db.transaction(() async {
    await put(
      table: table,
      id: sweepSlotId(id),
      hlcPacked: hlcPacked,
      schemaV: schemaV,
      payload: payload,
      reason: reason,
      quarantinedAt: quarantinedAt,
    );
    await (_db.delete(_db.syncQuarantine)..where(
          (q) =>
              q.tbl.equals(table) &
              q.id.equals(id) &
              q.remoteHlcPacked.equals(hlcPacked),
        ))
        .go();
  });

  /// Reason prefixes of parks whose payload can NEVER re-apply: their gate's
  /// verdict is a pure function of the payload alone, so no app upgrade,
  /// clock recovery, or later delivery ever changes it — the replay re-parks
  /// them every cycle, forever. Deliberately NOT listed: 'unknown-table',
  /// 'unknown-column' and 'missing-column' (their gates re-run against the
  /// LIVE schema, which an upgrade changes), 'hlc-drift' (a recovered clock
  /// un-parks it), 'fk-orphan' (a delivered parent un-parks it), and
  /// 'apply-failed' (a savepoint failure may be transient, and the
  /// newer-schema gate parks under this prefix — an upgrade un-parks it;
  /// the applier keeps the class honest by parking DETERMINISTIC savepoint
  /// failures — CHECK / NOT NULL violations, pure functions of the
  /// payload — under 'invalid-value:' instead, so a forever-failing entry
  /// can never sit in a replayable class and displace a replayable park).
  /// The prefixes are [SyncApplier]'s reason vocabulary; the applier's
  /// _quarantine doc points back here.
  static const List<String> _unreplayablePrefixes = [
    'invalid-schema_v:',
    'invalid-id:',
    'invalid-value:',
    'dead-letter(',
  ];

  /// Whether a park under [reason] can ever re-apply — the replay class
  /// this store's retention rule keys on (see [_unreplayablePrefixes]).
  /// Public so the applier classifies its own parks by the SAME rule (a
  /// replayable park's stamp must ratchet the clock, see
  /// SyncApplier._noteParkedStamp).
  static bool isReplayable(String reason) =>
      !_unreplayablePrefixes.any(reason.startsWith);

  /// The SQL twin of [isReplayable], for queries that read the parking lot
  /// directly (the [IntegrityChecker]'s parked-newer-state guard): a
  /// predicate over the `sync_quarantine` row aliased [alias], TRUE when its
  /// stored reason is in a replayable class. GLOB rather than LIKE so it is
  /// the same exact, case-sensitive prefix test as [isReplayable] (LIKE folds
  /// case, and its `_` wildcard matches inside 'invalid-schema_v:'); the
  /// prefixes carry no GLOB metacharacter or quote, so they inline safely.
  static String replayableSql(String alias) => _unreplayablePrefixes
      .map((p) => "$alias.reason NOT GLOB '$p*'")
      .join(' AND ');

  /// All quarantined rows, oldest quarantine first (stable review order).
  Future<List<SyncQuarantineRow>> list() =>
      (_db.select(_db.syncQuarantine)..orderBy([
            (q) => OrderingTerm.asc(q.quarantinedAt),
            (q) => OrderingTerm.asc(q.tbl),
            (q) => OrderingTerm.asc(q.id),
          ]))
          .get();

  /// Removes one parked row (after a successful re-apply or a manual discard).
  /// A missing row is a no-op.
  Future<void> delete(String table, String id) => (_db.delete(
    _db.syncQuarantine,
  )..where((q) => q.tbl.equals(table) & q.id.equals(id))).go();

  /// Removes one parked row ONLY while it still holds EXACTLY the state that
  /// was replayed — the settle-then-delete used by the quarantine replay,
  /// which lists, applies, then deletes. A different state for the same
  /// (table, id) can be parked between the apply and the delete (a concurrent
  /// pull page), and an unconditional delete threw that state away unreplayed.
  ///
  /// The condition is the stamp AND the payload, never the stamp alone: [put]
  /// deliberately lets an EQUAL stamp replace the stored entry with a
  /// refreshed reason and payload (the FK-orphan re-park cycle depends on
  /// that), so a stamp match does not prove the entry is still the one that
  /// settled. Both together make this a true compare-and-swap — anything
  /// parked in the window survives for the next replay cycle, where it
  /// settles on its own terms.
  Future<void> deleteIfUnchanged(
    String table,
    String id, {
    required String hlcPacked,
    required String payload,
  }) =>
      (_db.delete(_db.syncQuarantine)..where(
            (q) =>
                q.tbl.equals(table) &
                q.id.equals(id) &
                q.remoteHlcPacked.equals(hlcPacked) &
                q.payload.equals(payload),
          ))
          .go();
}
