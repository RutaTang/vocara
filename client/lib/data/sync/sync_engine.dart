import 'dart:async';

import 'package:drift/drift.dart';

import '../db/app_database.dart';
import '../services/diagnostics_log.dart';
import 'integrity_checker.dart';
import 'oplog_drain.dart';
import 'sync_applier.dart';
import 'sync_row.dart';
import 'sync_state_store.dart';
import 'sync_transport.dart';

/// One "remote changes landed" notification, fired AFTER the page's
/// transaction committed (never from inside an open transaction) and after
/// the pull run's end-of-run integrity repair ran, so listeners always reload
/// an already-consistent view.
///
/// This event exists for FRESHNESS only, never for safety: three-way
/// reconcile (base hashes) already guarantees that an editor which misses (or
/// skips) a reload cannot revert or tombstone the synced-in rows on its next
/// save. View models therefore listen with tiny, heavily-guarded handlers and
/// simply skip the reload whenever the user is mid-anything. (That is also
/// why a run that aborts mid-backlog may drop the events of its committed
/// pages: the next successful run announces its own pages and repairs
/// everything; nothing is lost but a reload nudge.)
class RemoteApplied {
  RemoteApplied({
    required List<(String, String)> rows,
    required Set<String> practiceIds,
    required Set<String> techniqueIds,
  }) : rows = List.unmodifiable(rows),
       practiceIds = Set.unmodifiable(practiceIds),
       techniqueIds = Set.unmodifiable(techniqueIds);

  /// The (table, id) pairs the applied page carried, deduped, arrival order.
  final List<(String, String)> rows;

  /// Ids of every practice the page touched — directly (a practices row) or
  /// through a child row (tracks/interpretations/fx_buses carry practice_id
  /// on the wire; clips/notes/markers are resolved via their track_id with
  /// one local read AFTER the commit). A child whose track is unknown locally
  /// (quarantined / swept) resolves to nothing and is simply omitted.
  final Set<String> practiceIds;

  /// Ids of every technique the page touched (techniques rows and
  /// technique_media child rows).
  final Set<String> techniqueIds;

  bool touchesTable(String table) => rows.any((r) => r.$1 == table);
}

/// How a timer is minted — injectable so tests never arm a real [Timer].
typedef SyncTimerFactory = Timer Function(Duration delay, void Function() run);

/// Diagnostics of one [SyncEngine.pumpPush] run.
class PushRun {
  const PushRun({
    this.pushed = 0,
    this.heldBack = 0,
    this.ackedThrough = 0,
    this.failure,
  });

  /// Rows sent to (and accepted by) the transport across the whole run.
  final int pushed;

  /// Asset-gated entries left in the oplog for a later run (see the class
  /// docs on the asset gate).
  final int heldBack;

  /// The highest oplog sequence acknowledged (0 = nothing was acked).
  final int ackedThrough;

  /// Non-null when the run aborted (transport failure, unresolved conflict):
  /// every unacked oplog entry is still in the ledger and re-pushes later.
  final Object? failure;

  bool get failed => failure != null;

  @override
  String toString() =>
      'PushRun(pushed: $pushed, heldBack: $heldBack, '
      'ackedThrough: $ackedThrough, failure: $failure)';
}

/// Diagnostics of one [SyncEngine.pumpPull] run.
class PullRun {
  const PullRun({
    this.pages = 0,
    this.applied = 0,
    this.skipped = 0,
    this.quarantined = 0,
    this.failedRecords = 0,
    this.failure,
  });

  /// Pages whose transaction (records + change token) committed.
  final int pages;

  final int applied;
  final int skipped;
  final int quarantined;

  /// Per-record failures inside committed pages (quarantined to the store).
  final int failedRecords;

  /// Non-null when the run aborted before catching up (transport failure or a
  /// rolled-back page). The change token was NOT advanced past anything that
  /// failed to commit; the next run resumes exactly there.
  final Object? failure;

  bool get failed => failure != null;

  @override
  String toString() =>
      'PullRun(pages: $pages, applied: $applied, skipped: $skipped, '
      'quarantined: $quarantined, failedRecords: $failedRecords, '
      'failure: $failure)';
}

/// The sync orchestrator: drains the local oplog into [SyncTransport.pushRows]
/// and pulls [SyncTransport.fetchChanges] pages through the [SyncApplier].
///
/// HARD INVARIANTS (each has a test):
///  * The change token advances IFF the page it covers committed — the token
///    write runs inside the applier's outer transaction via `beforeCommit`.
///  * Applying remote changes journals nothing (the applier's contract), so
///    pull can never echo rows back into push. (The applier's one carve-out —
///    the single-slot resolution tombstone — is a new derived local decision,
///    bounded to one write per losing row, never an echo.)
///  * At-least-once tolerant: re-delivered rows/pages are LWW no-ops.
///  * Listeners are only notified AFTER transactions close ([onRemoteApplied]
///    fires post-commit; the oplog poke rides drift's table-update stream,
///    which is dispatched only once its transaction has completed — a
///    rollback's spurious poke drains to a no-op, see [start]).
///  * Data safety over progress: any failure leaves the oplog entries unacked
///    and the token unmoved; the next pump simply retries.
///  * Quarantine is a parking lot, never a black hole: the change token
///    advances past a quarantined record's page when its batch commits, and
///    the server only re-delivers seq > token — so nothing would ever deliver
///    a parked row again. Every pull run therefore REPLAYS the parking lot
///    through the applier with fresh drift/schema checks
///    ([_replayQuarantine]); a recovered clock or an upgraded schema lets the
///    rows in, and a row that lost LWW meanwhile is settled and dropped.
///
/// ASSET GATE: an oplog entry carrying [PendingOplogEntry.dependsOnAssetId]
/// whose asset bytes have no `asset_sync` upload marker is HELD BACK — the row
/// must never reach a peer before the bytes it references are fetchable. Held
/// entries stay in the oplog (the ack watermark stops just short of the first
/// held sequence), so rows after them may be re-pushed on the next run —
/// harmless under LWW. P2's byte uploader writes the marker and thereby
/// releases the gate; until then nothing in production journals a dependency,
/// so the gate is exercised only by tests.
///
/// SCHEDULING: tests drive [pumpPush]/[pumpPull] directly — no timers exist
/// unless [start] is called. [start] subscribes to oplog inserts (drift emits
/// them only after the owning transaction commits) and debounces into a push:
/// [quietWindow] of no further writes, capped at [maxWait] from the first
/// unflushed write. The timer factory is injectable so even the debounce is
/// testable without real time.
class SyncEngine {
  SyncEngine({
    required AppDatabase db,
    required SyncApplier applier,
    required OplogDrain drain,
    required SyncRowBuilder builder,
    required SyncStateStore state,
    required IntegrityChecker integrity,
    required SyncTransport transport,
    this.onRemoteApplied,
    this.quietWindow = const Duration(seconds: 2),
    this.maxWait = const Duration(seconds: 30),
    this.pushBatchLimit = 400,
    this.drainLimit = 500,
    SyncTimerFactory? timers,
    Future<void> Function(FetchPage page)? debugBeforePageCommit,
  }) : _db = db,
       _applier = applier,
       _drain = drain,
       _builder = builder,
       _state = state,
       _integrity = integrity,
       _transport = transport,
       _timers = timers ?? _realTimer,
       _debugBeforePageCommit = debugBeforePageCommit;

  static Timer _realTimer(Duration delay, void Function() run) =>
      Timer(delay, run);

  final AppDatabase _db;
  final SyncApplier _applier;
  final OplogDrain _drain;
  final SyncRowBuilder _builder;
  final SyncStateStore _state;
  final IntegrityChecker _integrity;
  final SyncTransport _transport;
  final SyncTimerFactory _timers;

  /// Test-only seam: runs INSIDE each page's transaction, after the token
  /// write. A throwing hook kills the transaction, proving atomicity.
  final Future<void> Function(FetchPage page)? _debugBeforePageCommit;

  /// Fired after each committed page that actually changed rows (post-commit,
  /// post-repair). Wired by Persistence to its broadcast stream.
  final void Function(RemoteApplied event)? onRemoteApplied;

  /// Debounce: a push fires after this long with no further local writes...
  final Duration quietWindow;

  /// ...but never later than this after the first unflushed write.
  final Duration maxWait;

  /// Transport batch cap (a conservative per-request row limit any backend
  /// can honor).
  final int pushBatchLimit;

  /// Raw oplog rows read per drain (see [OplogDrain.readPending]).
  final int drainLimit;

  /// Bounded conflict resolution: how many times one [pumpPush] run re-pushes
  /// rows whose local HLC beat the server row before giving up (leaving the
  /// oplog unacked for the next run).
  static const int maxConflictRetries = 3;

  StreamSubscription<Object?>? _oplogSub;
  Timer? _quietTimer;
  Timer? _maxTimer;
  bool _disposed = false;

  /// Serializes push and pull: the two must never interleave their awaits
  /// (a pull's integrity repair journals, which pokes a push — that push must
  /// queue behind the pull, not race it).
  Future<void> _serial = Future.value();

  Future<T> _exclusive<T>(Future<T> Function() op) {
    final run = _serial.then((_) => op());
    // The chain must survive a failed run; callers see the error via [run].
    _serial = run.then((_) {}, onError: (_) {});
    return run;
  }

  // --- Automatic mode (production wiring; optional) -------------------------

  /// Subscribes the debounced push to oplog inserts. Drift defers table-update
  /// notifications until the transaction that made them COMPLETES (commit or
  /// rollback), so a poke can never observe an open transaction. A rolled-back
  /// save still dispatches its notification, but that poke is harmless by
  /// construction: the journal entry rolled back with the data, so the pump
  /// drains nothing (tested). The ack's deletes are filtered out by the
  /// insert-only query.
  void start() {
    if (_disposed || _oplogSub != null) return;
    _oplogSub = _db
        .tableUpdates(
          TableUpdateQuery.onTable(
            _db.oplog,
            limitUpdateKind: UpdateKind.insert,
          ),
        )
        .listen((_) => poke());
  }

  /// Nudges the engine: schedules a debounced [pumpPush]. Called by the oplog
  /// subscription after every committed local write; safe to call manually.
  void poke() {
    if (_disposed) return;
    _quietTimer?.cancel();
    _quietTimer = _timers(quietWindow, _flush);
    _maxTimer ??= _timers(maxWait, _flush);
  }

  void _flush() {
    _quietTimer?.cancel();
    _quietTimer = null;
    _maxTimer?.cancel();
    _maxTimer = null;
    if (_disposed) return;
    unawaited(pumpPush());
  }

  /// Cancels the subscription and any pending timers. Idempotent. In-flight
  /// pumps finish on their own (they are already crash-tolerant, so there is
  /// nothing to interrupt safely — we just stop scheduling new ones).
  void dispose() {
    _disposed = true;
    _quietTimer?.cancel();
    _quietTimer = null;
    _maxTimer?.cancel();
    _maxTimer = null;
    unawaited(_oplogSub?.cancel());
    _oplogSub = null;
  }

  // --- Push ------------------------------------------------------------------

  /// Drains pending oplog entries, pushes the CURRENT state of their rows,
  /// and acks exactly what was handled. Loops while it makes full progress,
  /// so a large backlog drains in one call. Any failure aborts with the
  /// ledger intact (minus already-acked prefixes, which the server durably
  /// accepted).
  Future<PushRun> pumpPush() => _exclusive(_pushLocked);

  /// Once-per-instance durable marker that a transport exists (see
  /// [SyncStateStore.transportSeenKey]); the asset GC's pending-journal guard
  /// keys off it. Cached so steady-state pumps cost no extra query.
  bool _transportSeenMarked = false;

  Future<void> _ensureTransportSeen() async {
    if (_transportSeenMarked) return;
    await _state.markTransportSeen();
    _transportSeenMarked = true;
  }

  Future<PushRun> _pushLocked() async {
    var pushed = 0;
    var heldBack = 0;
    var ackedThrough = 0;
    try {
      // Durable BEFORE the first wire call of this library's life: once a push
      // may have reached a server, the asset GC's pending-journal guard must
      // engage — a crash mid-push must never leave a server-held row in a
      // library that still claims no transport ever existed.
      await _ensureTransportSeen();
      var lastMaxSeq = 0;
      while (true) {
        final batch = await _drain.readPending(limit: drainLimit);
        if (batch.isEmpty) break;
        // No forward progress is possible when the same window comes back
        // (its head is asset-gated); stop rather than spin.
        if (batch.maxSeq == lastMaxSeq) break;
        lastMaxSeq = batch.maxSeq;

        // Partition on the asset gate (entries are seq-ascending).
        final ready = <PendingOplogEntry>[];
        int? firstHeldSeq;
        var held = 0;
        for (final e in batch.entries) {
          final gated =
              e.dependsOnAssetId != null &&
              !await _assetBytesUploaded(e.dependsOnAssetId!);
          if (gated) {
            held++;
            firstHeldSeq ??= e.seq;
          } else {
            ready.add(e);
          }
        }
        heldBack += held;

        // Push CURRENT row state (the drain already collapsed to latest per
        // row; the builder re-reads, so a save that landed after the drain
        // simply makes the pushed state even fresher — and its own oplog
        // entry, with a higher seq, survives the ack below).
        final built = <SyncRow>[];
        for (final e in ready) {
          final row = await _builder.build(e.table, e.id);
          // A vanished row (hard-deleted before it ever synced, e.g. an
          // FK-swept draft) has nothing to push; its entry is still acked.
          if (row != null) built.add(row);
        }
        // Upload parents before children — the [FetchPage] ordering contract.
        // The drain's latest-op order is CHILD-first for a cascade delete
        // (children are journaled before their parent), and a receiver that
        // has never seen the aggregate applies those rows as fresh inserts:
        // child-first across a page boundary would strand a child whose
        // parent hasn't arrived yet, and the applier's FK sweep would drop it
        // with the change token already advanced — a permanently lost row
        // (convergence simulation, seed 0x5eed0001). A stable
        // (rank, journal-order) sort fixes the order; the ack cover below is
        // seq-based and unaffected. Rows are full LWW states, so reordering
        // within one push never changes any outcome, only arrival order.
        final ranks = built.length > 1
            ? await _builder.tableRanks()
            : const <String, int>{};
        final indexed = built.indexed.toList()
          ..sort((a, b) {
            final byRank = (ranks[a.$2.table] ?? 0).compareTo(
              ranks[b.$2.table] ?? 0,
            );
            return byRank != 0 ? byRank : a.$1.compareTo(b.$1);
          });
        final rows = [for (final (_, row) in indexed) row];
        for (var i = 0; i < rows.length; i += pushBatchLimit) {
          final chunk = rows.sublist(
            i,
            i + pushBatchLimit > rows.length ? rows.length : i + pushBatchLimit,
          );
          pushed += await _pushChunkResolvingConflicts(chunk);
        }

        // Ack exactly what this read covered — held back at the first gated
        // entry so it is never deleted un-pushed. Everything acked was either
        // accepted by the server or deliberately dropped (server newer).
        final ackTo = firstHeldSeq == null ? batch.maxSeq : firstHeldSeq - 1;
        if (ackTo > 0) {
          await _drain.ack(ackTo);
          ackedThrough = ackTo;
        }
        if (ackTo < batch.maxSeq) break; // gated head: later entries wait
      }
      return PushRun(
        pushed: pushed,
        heldBack: heldBack,
        ackedThrough: ackedThrough,
      );
    } catch (e) {
      // Transport failure / exhausted conflict retries: nothing past the last
      // durable ack is lost — it is all still journaled and re-pushes later.
      return PushRun(
        pushed: pushed,
        heldBack: heldBack,
        ackedThrough: ackedThrough,
        failure: e,
      );
    }
  }

  /// Pushes one chunk, resolving conflicted rows per the transport contract:
  /// local HLC strictly greater than the server row's → re-push (bounded by
  /// [maxConflictRetries]); otherwise drop ours — the newer server state
  /// arrives via pull, and LWW makes dropping safe. Returns rows accepted.
  ///
  /// Every ack is VALIDATED against the rows just pushed before anything acts
  /// on it (see [_validateAck]), and every conflicted result's serverRow is
  /// validated before it may drive the retry-or-drop decision (see
  /// [_validateConflict]) — each retry round validates its own attempt list.
  ///
  /// Every DECISION binds to the ATTEMPT row — the row from our own push
  /// array — never to the echoed [PushRowResult.row], which is wire data a
  /// hostile server controls and is used ONLY to find which attempt a result
  /// answers. Trusted for anything more, a forged OLDER echo paired with an
  /// internally-consistent serverRow stamped between the echo and the true
  /// attempt passes every per-result check, the engine judges "server
  /// newer", drops the local edit, and acks away its oplog entry — silent
  /// data loss on nothing but a mangled ack.
  ///
  /// Handled assets rows are marked record-synced here, inside the round and
  /// therefore strictly BEFORE the caller's oplog ack (see
  /// [_markAssetRecordsSynced] for why that order matters).
  Future<int> _pushChunkResolvingConflicts(List<SyncRow> chunk) async {
    var accepted = 0;
    var attempt = chunk;
    for (var tries = 0; attempt.isNotEmpty; tries++) {
      final ack = await _transport.pushRows(attempt);
      _validateAck(attempt, ack);
      // Correlate each result back to the attempt's OWN row. Keys are unique
      // here — the drain collapses to one entry per (table, id), and the
      // retry list below inherits that — so the map can never conflate two
      // attempts; refuse loudly if that invariant ever breaks rather than
      // resolve results ambiguously.
      final attemptByKey = <(String, String), SyncRow>{};
      for (final row in attempt) {
        final prior = attemptByKey.putIfAbsent((row.table, row.id), () => row);
        if (!identical(prior, row)) {
          throw StateError(
            'push attempt carries duplicate rows for ${row.table}/${row.id}; '
            'ack results cannot be correlated unambiguously',
          );
        }
      }
      // Same discipline as _validateAck: the WHOLE round is vetted before
      // anything in it takes effect, so an invalid conflict claim can't land
      // after part of the round was already counted/marked. ([_validateAck]
      // proved the 1:1 identity cover, so the lookups here cannot miss.)
      for (final r in ack.results) {
        if (!r.accepted) {
          _validateConflict(attemptByKey[(r.row.table, r.row.id)]!, r);
        }
      }
      final retry = <SyncRow>[];
      // Assets rows the server now durably HOLDS after this round — accepted
      // ones, and ones dropped because the server's version is newer (the
      // record has crossed the wire either way).
      final syncedAssetIds = <String>[];
      for (final r in ack.results) {
        // From here on the echo is never read again: identity, HLC and asset
        // marking all come from the attempt row.
        final ours = attemptByKey[(r.row.table, r.row.id)]!;
        if (r.accepted) {
          accepted++;
          if (ours.table == 'assets') syncedAssetIds.add(ours.id);
        } else if (ours.hlcPacked.compareTo(r.serverRow!.hlcPacked) > 0) {
          // Unreachable against a validated ack ([_validateConflict] requires
          // the server row STRICTLY newer than the attempt); kept as the
          // documented resolution policy so that a deliberate future
          // relaxation of the conflict contract makes "local wins" re-push
          // instead of silently dropping.
          retry.add(ours);
        } else {
          // Server holds something newer — drop ours. For an assets row that
          // still counts as synced: the server holds the record, so another
          // device may reference the asset.
          if (ours.table == 'assets') syncedAssetIds.add(ours.id);
        }
      }
      await _markAssetRecordsSynced(syncedAssetIds);
      if (retry.isEmpty) break;
      if (tries + 1 >= maxConflictRetries) {
        // Give up this run WITHOUT acking: the rows stay journaled and the
        // next run retries from scratch (data safety over progress).
        throw SyncTransportException(
          'push conflicts unresolved after $maxConflictRetries attempts '
          '(${retry.length} rows)',
        );
      }
      attempt = retry;
    }
    return accepted;
  }

  /// Refuses a [PushAck] whose result set is not EXACTLY the rows that were
  /// pushed: same count, and a 1:1 identity match on (table, id) — order may
  /// differ, duplicates are invalid. Without this the ack was trusted blind:
  /// a buggy (or hostile) server returning an empty or wrong result list
  /// still let the caller ack — i.e. DELETE — the whole drained oplog range,
  /// silently losing every row the server never actually stored.
  ///
  /// The throw is a [SyncTransportException], so [pumpPush]'s existing catch
  /// handles it like any transport failure: the run reports failed, the
  /// oplog ack for this window never runs, and every unacked entry re-pushes
  /// on a later run — verified to hold, since the ack in [_pushLocked] sits
  /// after all chunk pushes inside the same try block this throw escapes.
  void _validateAck(List<SyncRow> pushed, PushAck ack) {
    if (ack.results.length != pushed.length) {
      throw SyncTransportException(
        'push ack has ${ack.results.length} results for ${pushed.length} '
        'pushed rows',
      );
    }
    // Multiset match: every result must consume exactly one pushed row. The
    // drain collapses to one entry per row, so pushed keys are unique in
    // practice — but counting keeps the check exact even if that changes.
    final expected = <(String, String), int>{};
    for (final row in pushed) {
      expected.update((row.table, row.id), (n) => n + 1, ifAbsent: () => 1);
    }
    for (final r in ack.results) {
      final key = (r.row.table, r.row.id);
      final n = expected[key] ?? 0;
      if (n == 0) {
        throw SyncTransportException(
          'push ack carries a result for ${key.$1}/${key.$2}, which was not '
          'in the pushed rows (or is a duplicate result)',
        );
      }
      expected[key] = n - 1;
    }
  }

  /// Refuses a `conflicted` [PushRowResult] whose serverRow cannot be an
  /// honest CAS rejection of [attempt] — the row from OUR push array this
  /// result was correlated to, never the echoed `r.row` (which a hostile
  /// server controls; see [_pushChunkResolvingConflicts]). The serverRow
  /// decides retry-or-drop; trusted blind, a lazy lie could make the engine
  /// silently drop — and then ack away — a local edit the server never
  /// stored. Four checks, all cheap:
  ///
  ///  * it must EXIST — a conflict claim with no server state is
  ///    meaningless (structurally unrepresentable through
  ///    [PushRowResult]'s own constructors, but a decoding transport seam
  ///    must not be able to smuggle one through);
  ///  * it must carry the SAME (table, id) as the attempt row — checked on
  ///    the raw field map, so a mistyped id fails as a transport violation
  ///    rather than a TypeError;
  ///  * it must be internally CONSISTENT: its HLC quad must re-pack to its
  ///    hlc_packed — the exact rule [SyncRow.fromJson] enforces at the pull
  ///    door, applied via the shared [SyncRow.checkHlcConsistency] so the
  ///    two doors can never diverge;
  ///  * its hlcPacked must be STRICTLY greater than the ATTEMPT row's: a
  ///    conforming server accepts a push it holds nothing newer than
  ///    (docs/sync-protocol.md), so a CAS reject by an older-or-equal row is
  ///    nonsensical — and the canonical low-cost lie is echoing the pushed
  ///    row straight back as "the server's". Comparing against the echo
  ///    instead would let a forged OLDER echo smuggle a mid-HLC serverRow
  ///    through this very check.
  ///
  /// Any violation throws [SyncTransportException]: the run fails, nothing
  /// is acked, and every journaled entry re-pushes on a later run. This
  /// closes every low-cost lie; a server that fabricates an
  /// internally-consistent, strictly NEWER row is indistinguishable from one
  /// legitimately holding it, and is therefore outside the client's
  /// defensible model — the documented trust boundary (docs/sync-protocol.md
  /// "Trust model"), equivalent to the server withholding rows on pull.
  void _validateConflict(SyncRow attempt, PushRowResult r) {
    final server = r.serverRow;
    if (server == null) {
      throw SyncTransportException(
        'conflicted push result for ${attempt.table}/${attempt.id} carries '
        'no serverRow',
      );
    }
    if (server.table != attempt.table || server.fields['id'] != attempt.id) {
      throw SyncTransportException(
        'conflicted push result for ${attempt.table}/${attempt.id} carries a '
        'serverRow for ${server.table}/${server.fields['id']}',
      );
    }
    try {
      SyncRow.checkHlcConsistency(server.fields);
      SyncRow.checkIdShapes(server.table, server.fields);
    } on FormatException catch (e) {
      throw SyncTransportException(
        'conflicted serverRow for ${attempt.table}/${attempt.id} is '
        'internally inconsistent',
        e,
      );
    }
    // checkHlcConsistency proved hlc_packed is a String, so the getter below
    // cannot throw.
    if (server.hlcPacked.compareTo(attempt.hlcPacked) <= 0) {
      throw SyncTransportException(
        'conflicted serverRow for ${attempt.table}/${attempt.id} is not '
        'strictly newer than the pushed row '
        '("${server.hlcPacked}" <= "${attempt.hlcPacked}"); a conforming '
        'server accepts a push it holds nothing newer than',
      );
    }
  }

  /// Marks the given assets rows' record_sync_state 'synced' — per-device
  /// bookkeeping ONLY, so deliberately a direct drift update touching nothing
  /// but that one column: never through SyncWriter (marking must not journal
  /// — an oplog echo would re-push forever) and never touching updated_at or
  /// the HLC columns (bookkeeping must not look like an edit or shift LWW).
  ///
  /// Called BEFORE the batch's oplog ack so a crash between the two errs
  /// toward 'synced': the asset-GC's never-synced guard then merely refuses
  /// to sweep a row the server may not hold — conservative — instead of
  /// sweeping (and fleet-wide tombstoning) a row the server DOES hold.
  Future<void> _markAssetRecordsSynced(List<String> ids) async {
    if (ids.isEmpty) return;
    await (_db.update(_db.assets)..where((a) => a.id.isIn(ids))).write(
      const AssetsCompanion(recordSyncState: Value('synced')),
    );
  }

  /// Whether [assetId]'s BYTES are durably uploaded: its content hash has an
  /// asset_sync marker. A missing asset row counts as not uploaded (held back
  /// — the safe side).
  Future<bool> _assetBytesUploaded(String assetId) async {
    final row = await _db
        .customSelect(
          'SELECT 1 AS ok FROM asset_sync s '
          'JOIN assets a ON a.content_hash = s.hash WHERE a.id = ?',
          variables: [Variable.withString(assetId)],
        )
        .getSingleOrNull();
    return row != null;
  }

  // --- Pull ------------------------------------------------------------------

  /// Fetches remote pages and applies each one atomically WITH its change
  /// token (one transaction per page, via the applier's `beforeCommit` hook).
  /// The quarantine parking lot is replayed at the START of the run and
  /// again once caught up (see [_replayQuarantine] for why pull owns this).
  /// After the run has caught up: ONE integrity repair in its own
  /// transaction, then the buffered [onRemoteApplied] events. Stops at the
  /// first failure with the token exactly at the last committed page.
  ///
  /// The repair deliberately runs at the END of the run, never between
  /// pages: mid-run the local store may hold a HALF-DELIVERED aggregate (a
  /// practice row whose interpretation is on the next page), and repairing
  /// that transient state manufactures intent — e.g. the active-interp
  /// scalar heal nulls the pointer with an HLC above the row's that can
  /// later beat a legitimate tombstone and resurrect a deleted practice
  /// fleet-wide (found by the convergence simulation, seeds 0xc1c2b7 and
  /// 0x5eed0009). Once the run is caught up, the delivered prefix is
  /// causally complete (the pusher uploads parents before children and whole
  /// transactions in order) — but the APPLIED prefix need not be: the
  /// applier may have PARKED a row's newer state (drift gate, newer schema,
  /// an FK-sweep park-and-restore) while applying its children, and the
  /// table then still holds a state the quarantine already supersedes. The
  /// checker therefore defers every verdict that depends on a row with a
  /// newer replayable parked state (see IntegrityChecker._parkedNewer); the
  /// replay that later lands or settles that state opens the run's repair
  /// gate, so the deferred verdict is re-judged in that same run. What the
  /// checker repairs is thus only genuinely broken. Residual exposure: a
  /// pusher whose SINGLE transaction exceeds one push chunk (400 rows) can
  /// still be observed half-uploaded by a caught-up puller; P0 aggregates are
  /// far smaller, and the derived repair stamps cap the blast radius to
  /// bookkeeping that any later real write out-ranks.
  Future<PullRun> pumpPull() => _exclusive(_pullLocked);

  /// Set when a run that had already committed applied pages (or settled
  /// parked entries) aborted before its end-of-run repair: the next
  /// successful run repairs even if it applies nothing itself.
  ///
  /// Starts TRUE, so every engine lifetime repairs on its first successful
  /// run. The pages and their change token are durable the moment they
  /// commit, but this flag is not: a crash or quit between a page commit and
  /// the repair transaction lost the debt with the process, the next run
  /// (token already past those pages) applied nothing and skipped the
  /// repair, and the violations those pages delivered (a live child under a
  /// tombstoned parent, a dangling pointer, a bus cycle the native engine
  /// rejects) stood until some later pull happened to apply a row. Repairing
  /// once per boot settles whatever such a run left behind without
  /// persisting anything: it covers every case a durable debt marker would
  /// (and needs no write per page to keep one), and the repair is
  /// idempotent — on a consistent store it is one scan that writes nothing.
  bool _repairDebt = true;

  Future<PullRun> _pullLocked() async {
    var pages = 0;
    var applied = 0;
    var skipped = 0;
    var quarantined = 0;
    var failedRecords = 0;
    // Rows the quarantine replay landed this run (they count toward the
    // repair gate, not toward the page counters — they are not page rows).
    var replayed = 0;
    // Parked entries the replay SETTLED this run — landed or LWW-skipped.
    // A skip changes no row, but it ends the deferral of any verdict the
    // checker held back for that parked state, so it gates the repair too.
    var settled = 0;
    // Events buffered until the end-of-run repair has run (listeners must
    // always reload an already-consistent view).
    final announcements = <RemoteApplied>[];
    try {
      // A pull proves a transport exists just as much as a push does (see
      // _pushLocked): record it before the first wire call.
      await _ensureTransportSeen();
      // Replay the quarantine parking lot FIRST: the token is already past
      // those rows, so no fetch will ever re-deliver them — a recovered
      // clock or an upgraded schema must be able to let them in even when
      // this run fetches nothing new (the fresh-device-with-a-slow-clock
      // case parks EVERY first-pull row while the token marches to head).
      final firstReplay = await _replayQuarantine();
      replayed = firstReplay.count;
      settled = firstReplay.settled;
      // Replayed rows are announced like page rows: the primary recovery
      // scenario (a whole first-pull backlog parked, then un-parked by a
      // corrected clock) can land an entire library in a run that fetches
      // nothing — without an event, open screens would never refresh.
      if (firstReplay.rows.isNotEmpty) {
        announcements.add(await _describeRows(firstReplay.rows));
      }
      var token = await _state.changeToken();
      // One token reset per run: a transport signalling an EXPIRED change
      // token ([SyncTokenExpiredException], e.g. HTTP 410 from a server that
      // compacted its change log past it) is recovered by clearing the
      // persisted token and restarting the pull from the beginning of the
      // zone, in this same run. Safe by construction: the applier is
      // idempotent entity-level LWW, so re-applying the whole zone is a
      // stream of clean skips for rows we already hold and normal applies
      // for the rest. Bounded to ONE reset — a transport that rejects even a
      // fresh (null) token is treated as an ordinary failed run instead of
      // spinning clear-and-refetch forever.
      var tokenResetUsed = false;
      while (true) {
        final FetchPage page;
        try {
          page = await _transport.fetchChanges(token);
        } on SyncTokenExpiredException {
          if (token == null || tokenResetUsed) rethrow;
          tokenResetUsed = true;
          await _state.clearChangeToken();
          token = null;
          continue;
        }
        if (page.rows.isEmpty && page.rejects.isEmpty) {
          // Caught up. Persist a moved empty-page token (a server may advance
          // the token without changes); there is no batch to be atomic with.
          if (page.nextToken != token) {
            await _state.setChangeToken(page.nextToken);
          }
          if (!page.hasMore) break;
          if (page.nextToken == token) break; // defensive: refuse to spin
          token = page.nextToken;
          continue;
        }
        final result = await _applier.apply(
          page.rows,
          // Parked inside the page's own transaction, so a record that never
          // decoded is atomic with the token that moves past it — the server
          // only re-delivers seq > token, so anything the token passes and
          // nothing recorded is gone for good.
          rejects: page.rejects,
          beforeCommit: () async {
            await _state.setChangeToken(page.nextToken);
            final hook = _debugBeforePageCommit;
            if (hook != null) await hook(page);
          },
        );
        if (!result.committed) {
          // The page (and its token) rolled back in full: stop, retry later.
          throw StateError('pull page failed to commit; token unmoved');
        }
        pages++;
        applied += result.applied;
        skipped += result.skipped;
        quarantined += result.quarantined;
        failedRecords += result.failed;
        if (result.applied > 0) {
          announcements.add(await _describeRows(page.rows));
        }
        // An unmoved token WITHOUT hasMore is the legal at-least-once
        // redelivery (the page just re-applied as clean LWW skips, and the
        // token write was a same-value no-op): done, like any final page.
        if (!page.hasMore) break;
        // TRUST MODEL (sync_transport.dart): a page bearing rows or rejects
        // MUST advance the token. Looping on an unmoved one would refetch
        // this identical page forever — every record a clean skip, hasMore
        // true, no failure surfaced — silently wedging sync (and every push
        // queued behind _exclusive) in an invisible hot loop. The empty
        // branch above already refuses to spin; this is the rows-bearing
        // analogue, and like every other cheaply-verifiable transport
        // violation it fails the run loudly.
        if (page.nextToken == token) {
          throw StateError(
            'pull transport violation: a page with ${page.rows.length} '
            'row(s) and ${page.rejects.length} reject(s) returned an '
            'unmoved change token with hasMore set',
          );
        }
        token = page.nextToken;
      }
      // Replay once more after catching up: this run's successful receives
      // may have raised the clock's max-wall-ever-seen (or delivered a
      // missing parent), unlocking rows parked just moments ago.
      final secondReplay = await _replayQuarantine();
      replayed += secondReplay.count;
      settled += secondReplay.settled;
      if (secondReplay.rows.isNotEmpty) {
        announcements.add(await _describeRows(secondReplay.rows));
      }
      if (applied > 0 || replayed > 0 || settled > 0 || _repairDebt) {
        // Caught up: repair the invariants closure ONCE, in one transaction
        // so each repair write commits with its journal entry, then announce.
        await _db.transaction(() => _integrity.repair());
        _repairDebt = false;
      }
      for (final event in announcements) {
        _fireRemoteApplied(event);
      }
      return PullRun(
        pages: pages,
        applied: applied,
        skipped: skipped,
        quarantined: quarantined,
        failedRecords: failedRecords,
      );
    } catch (e) {
      // Aborted mid-backlog: committed pages exist without their repair (and
      // without their announcements — freshness only, see [RemoteApplied]).
      // Carry the repair forward so the next successful run settles it.
      if (applied > 0 || replayed > 0 || settled > 0) _repairDebt = true;
      return PullRun(
        pages: pages,
        applied: applied,
        skipped: skipped,
        quarantined: quarantined,
        failedRecords: failedRecords,
        failure: e,
      );
    }
  }

  /// Re-attempts every quarantined record through the NORMAL applier path
  /// and returns how many actually applied. Bounded and idempotent — safe to
  /// run every cycle: one single-record apply per parked row, nothing spins.
  ///
  /// Each replay gets a FRESH gate pass inside [SyncApplier.apply]: the HLC
  /// drift check runs against the clock's CURRENT state (a corrected local
  /// clock, or honest stamps received since parking raising
  /// max-wall-ever-seen, un-park drift rejects) and the write re-runs against
  /// the CURRENT schema (an app upgrade un-parks forward-incompatible rows).
  ///
  /// Ordering: parent tables before child tables (the same parent-first
  /// contract as push, via [SyncRowBuilder.tableRanks], so a parked child
  /// lands after its parked parent within one pass), original parked order
  /// within a rank (the quarantine store lists parked-at ascending).
  ///
  /// Interaction with rows applied since parking is safe by construction —
  /// row-level LWW makes a stale replay lose cleanly: the applier compares
  /// the parked hlc_packed against the CURRENT row and skips the write when
  /// the parked state is older. Outcomes per row:
  ///  * applied — the record landed; its parking-lot entry is deleted.
  ///  * skipped — lost LWW to something newer applied since parking; the
  ///    parked state is obsolete, so its entry is deleted too. (The delete
  ///    runs after the apply's commit; a crash in between just re-replays it
  ///    as another clean skip next run.)
  ///  * quarantined / failed — still rejected; the applier itself re-parked
  ///    the latest state, so the entry stays for a later cycle.
  ///  * settled but unremovable — the apply committed and only the bookkeeping
  ///    delete failed; the entry stays parked and the run continues, because
  ///    the replay is idempotent and next cycle settles it as a clean skip.
  /// An undecodable payload is kept parked for manual review (it can never
  /// re-apply, but dropping it silently is exactly what quarantine exists to
  /// prevent).
  ///
  /// Returns how many rows landed ([count], with the rows themselves for the
  /// announcement) and how many entries SETTLED — landed or skipped — which
  /// the caller's repair gate needs: either outcome ends a deferral the
  /// IntegrityChecker held for that parked state.
  Future<({int count, int settled, List<SyncRow> rows})>
  _replayQuarantine() async {
    const nothing = (count: 0, settled: 0, rows: <SyncRow>[]);
    final store = _applier.quarantine;
    if (store == null) return nothing;
    final parked = await store.list();
    if (parked.isEmpty) return nothing;
    final ranks = parked.length > 1
        ? await _builder.tableRanks()
        : const <String, int>{};
    final indexed = parked.indexed.toList()
      ..sort((a, b) {
        final byRank = (ranks[a.$2.tbl] ?? 0).compareTo(ranks[b.$2.tbl] ?? 0);
        return byRank != 0 ? byRank : a.$1.compareTo(b.$1);
      });
    var applied = 0;
    var settled = 0;
    // Rows that actually landed, for the caller's RemoteApplied announcement
    // (skips changed nothing locally, so they are not announced).
    final landed = <SyncRow>[];
    for (final (_, q) in indexed) {
      SyncRow row;
      try {
        row = SyncRow.decode(q.payload);
      } on FormatException {
        continue; // undecodable: keep parked (see doc comment)
      }
      final result = await _applier.apply([row]);
      if (!result.committed) break; // storage trouble — retry next run
      if (result.applied > 0 || result.skipped > 0) {
        // Conditional on the exact state we REPLAYED: a different state for
        // this (table, id) can be parked while the apply above ran, and an
        // unconditional delete would drop it unreplayed (TOCTOU).
        //
        // A failure here is deliberately swallowed. The apply has already
        // committed, so the settle is pure bookkeeping: leaving the entry
        // parked costs one more clean LWW skip next cycle. Letting it escape
        // would abort the pull run BEFORE the change token is even fetched —
        // this replay runs first — so a single undeletable row would stall
        // every future pull, and the rows that DID land would lose their
        // announcement with it. The opposite of the applier's own park
        // failure, which must fail its batch: there a dropped write LOSES a
        // record, here it only delays one.
        try {
          await store.deleteIfUnchanged(
            q.tbl,
            q.id,
            hlcPacked: q.remoteHlcPacked,
            payload: q.payload,
          );
        } catch (e) {
          DiagnosticsLog.instance.log(
            'sync',
            'quarantine settle failed (${e.runtimeType}); '
                'the entry replays next cycle',
          );
        }
        applied += result.applied;
        settled++;
        if (result.applied > 0) landed.add(row);
      }
    }
    return (count: applied, settled: settled, rows: landed);
  }

  void _fireRemoteApplied(RemoteApplied event) {
    final sink = onRemoteApplied;
    if (sink == null || _disposed) return;
    sink(event);
  }

  /// Builds the [RemoteApplied] event for committed rows (a fetched page or
  /// a batch of replayed quarantine rows): the (table, id) pairs plus the
  /// practice/technique ids they roll up to. Runs AFTER the commit; the one
  /// extra read (track_id -> practice_id) is over ids only.
  Future<RemoteApplied> _describeRows(List<SyncRow> rows) async {
    final pairs = <(String, String)>[];
    final seen = <(String, String)>{};
    final practiceIds = <String>{};
    final techniqueIds = <String>{};
    final trackIds = <String>{};
    for (final row in rows) {
      final pair = (row.table, row.id);
      if (seen.add(pair)) pairs.add(pair);
      switch (row.table) {
        case 'practices':
          practiceIds.add(row.id);
        case 'tracks' || 'interpretations' || 'fx_buses':
          final pid = row.fields['practice_id'];
          if (pid is String) practiceIds.add(pid);
        case 'clips' || 'practice_notes' || 'technique_markers':
          final tid = row.fields['track_id'];
          if (tid is String) trackIds.add(tid);
        case 'techniques':
          techniqueIds.add(row.id);
        case 'technique_media':
          final tid = row.fields['technique_id'];
          if (tid is String) techniqueIds.add(tid);
      }
    }
    if (trackIds.isNotEmpty) {
      final placeholders = List.filled(trackIds.length, '?').join(', ');
      final trackRows = await _db
          .customSelect(
            'SELECT practice_id FROM tracks WHERE id IN ($placeholders)',
            variables: [for (final id in trackIds) Variable.withString(id)],
          )
          .get();
      for (final r in trackRows) {
        practiceIds.add(r.read<String>('practice_id'));
      }
    }
    return RemoteApplied(
      rows: pairs,
      practiceIds: practiceIds,
      techniqueIds: techniqueIds,
    );
  }
}
