import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/result.dart';

/// End-to-end sync tests across TWO simulated devices, each its own fully-wired
/// in-memory drift stack with a distinct HLC node id. Every flow mirrors what a
/// real sync pusher/puller does: the local repo writes through the SyncWriter
/// chokepoint, the pusher drains the oplog to learn WHICH rows changed, reads
/// those rows' full column state, ships them as [RemoteRecord]s, and the remote
/// side adopts them through the [SyncApplier] "remote door" (entity-level LWW by
/// HLC, never re-journaled).
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  late Persistence a;
  late Persistence b;
  // Each device's remote door, sharing that device's real HlcClock (the same
  // instance the local SyncWriter ticks) so quarantine + clock-poisoning
  // assertions reflect the actual device clock.
  late SyncApplier applierA;
  late SyncApplier applierB;

  setUp(() {
    a = Persistence.memory();
    b = Persistence.memory();
    applierA = SyncApplier(a.db, a.writer.clock);
    applierB = SyncApplier(b.db, b.writer.clock);
  });
  tearDown(() async {
    await a.db.close();
    await b.db.close();
  });

  T ok<T>(Result<T> r) => (r as Ok<T>).value;

  /// Reads a full row (all columns) from [db] for [table]/[id], or null.
  Future<Map<String, Object?>?> rowOf(
    Persistence dev,
    String table,
    String id,
  ) async {
    final r = await dev.db
        .customSelect(
          'SELECT * FROM $table WHERE id = ?',
          variables: [Variable.withString(id)],
        )
        .getSingleOrNull();
    return r?.data;
  }

  /// All currently-pending oplog rows on [dev], oldest first. This is exactly
  /// the change-set a pusher would drain.
  Future<List<OplogRow>> oplogOf(Persistence dev) async {
    return (dev.db.select(
      dev.db.oplog,
    )..orderBy([(o) => OrderingTerm(expression: o.seq)])).get();
  }

  /// Builds the [RemoteRecord]s a pusher would ship from [src] for the given
  /// oplog entries: for each (table,id) take the current full row state. Skips
  /// rows that no longer exist (defensive; shouldn't happen for our tables).
  Future<List<RemoteRecord>> shipFrom(
    Persistence src,
    List<OplogRow> ops,
  ) async {
    final out = <RemoteRecord>[];
    final seen = <String>{};
    for (final op in ops) {
      final key = '${op.entityTable}/${op.entityId}';
      if (!seen.add(key)) continue; // collapse to latest row state
      final cols = await rowOf(src, op.entityTable, op.entityId);
      if (cols == null) continue;
      out.add(RemoteRecord(table: op.entityTable, columns: Map.of(cols)));
    }
    return out;
  }

  /// Live (non-tombstoned) practice titles visible to a device's repository.
  Future<List<String>> liveTitles(Persistence dev) async {
    final list = ok(await dev.songRepo.listPractices());
    return [for (final p in list) p.title];
  }

  test('A creates + edits a practice; the change-set converges onto B', () async {
    // --- A: create then rename a practice through its repo (local writes). ---
    final created = ok(await a.songRepo.createPractice('Hai Kuo'));
    final renamed = ok(
      await a.songRepo.savePractice(
        SongPractice(
          id: created.id,
          title: 'Hai Kuo Tian Kong',
          createdAt: created.createdAt,
          updatedAt: created.updatedAt,
          versions: created.versions,
          activeVersionId: created.activeVersionId,
        ),
      ),
    );
    expect(renamed.title, 'Hai Kuo Tian Kong');

    // --- Pusher: drain A's oplog, ship the touched rows. ---
    final ops = await oplogOf(a);
    expect(ops, isNotEmpty, reason: 'local writes must journal');
    final records = await shipFrom(a, ops);
    expect(records, isNotEmpty);

    // --- B: apply through the remote door. ---
    final res = await applierB.apply(records);
    expect(res.quarantined, 0);
    expect(res.applied, greaterThan(0));

    // --- B converges to A's state (entity-level LWW). ---
    final onB = await b.songRepo.getPractice(created.id);
    expect(
      onB,
      isA<Ok<SongPractice>>(),
      reason: 'the practice + its interpretation should have arrived',
    );
    final pb = ok(onB);
    expect(pb.title, 'Hai Kuo Tian Kong');
    expect(pb.id, created.id);
    expect(
      pb.versions,
      isNotEmpty,
      reason: 'the seeded interpretation rode along',
    );

    // Row-level: B's practice HLC equals A's (it ADOPTED the remote stamp).
    final aRow = (await rowOf(a, 'practices', created.id))!;
    final bRow = (await rowOf(b, 'practices', created.id))!;
    expect(bRow['hlc_packed'], aRow['hlc_packed']);
    expect(bRow['title'], aRow['title']);
  });

  test(
    'concurrent conflict: the higher-HLC write wins on BOTH devices (LWW)',
    () async {
      // Same entity created independently, then edited on both with DIFFERENT
      // HLCs. We compare the two candidate row states by packed HLC: the higher
      // one must end up live on both devices regardless of apply direction.
      final base = ok(await a.songRepo.createPractice('shared'));
      final id = base.id;

      // Seed B with the same id at a baseline state so both have the row.
      await applierB.apply(await shipFrom(a, await oplogOf(a)));

      // A edits.
      ok(
        await a.songRepo.savePractice(
          SongPractice(
            id: id,
            title: 'edited-on-A',
            createdAt: base.createdAt,
            updatedAt: base.updatedAt,
            versions: base.versions,
            activeVersionId: base.activeVersionId,
          ),
        ),
      );
      // B edits the SAME row (its own local write, its own node + clock).
      final bBase = ok(await b.songRepo.getPractice(id));
      ok(
        await b.songRepo.savePractice(
          SongPractice(
            id: id,
            title: 'edited-on-B',
            createdAt: bBase.createdAt,
            updatedAt: bBase.updatedAt,
            versions: bBase.versions,
            activeVersionId: bBase.activeVersionId,
          ),
        ),
      );

      final aEdit = (await rowOf(a, 'practices', id))!;
      final bEdit = (await rowOf(b, 'practices', id))!;
      final aPacked = aEdit['hlc_packed'] as String;
      final bPacked = bEdit['hlc_packed'] as String;
      expect(
        aPacked,
        isNot(bPacked),
        reason: 'distinct nodes -> distinct HLCs',
      );

      // The deterministic winner by total HLC order.
      final aWins = aPacked.compareTo(bPacked) > 0;
      final expectedTitle = aWins ? 'edited-on-A' : 'edited-on-B';

      // Cross-apply both directions: A's edit -> B, and B's edit -> A. Only the
      // higher HLC is adopted; the lower one is skipped.
      final aRecord = RemoteRecord(table: 'practices', columns: Map.of(aEdit));
      final bRecord = RemoteRecord(table: 'practices', columns: Map.of(bEdit));

      final toB = await applierB.apply([aRecord]);
      final toA = await applierA.apply([bRecord]);

      // Exactly one side adopts, the other skips — deterministically.
      expect(toB.applied + toB.skipped, 1);
      expect(toA.applied + toA.skipped, 1);
      expect(toB.applied, aWins ? 1 : 0);
      expect(toA.applied, aWins ? 0 : 1);

      // Both devices now hold the identical winning row.
      final finalA = (await rowOf(a, 'practices', id))!;
      final finalB = (await rowOf(b, 'practices', id))!;
      expect(finalA['title'], expectedTitle);
      expect(finalB['title'], expectedTitle);
      expect(
        finalA['hlc_packed'],
        finalB['hlc_packed'],
        reason: 'converged to the same winner -> no split brain',
      );
    },
  );

  test(
    'tombstone propagation: a delete on A tombstones + hides the row on B',
    () async {
      final created = ok(await a.songRepo.createPractice('to delete'));
      final id = created.id;

      // Get the baseline onto B.
      await applierB.apply(await shipFrom(a, await oplogOf(a)));
      expect(await b.songRepo.getPractice(id), isA<Ok<SongPractice>>());

      // A deletes (soft-delete cascade -> new tombstone oplog entries).
      final beforeOps = (await oplogOf(a)).length;
      ok(await a.songRepo.deletePractice(id));
      final afterOps = await oplogOf(a);
      expect(
        afterOps.length,
        greaterThan(beforeOps),
        reason: 'a delete journals tombstone ops',
      );

      // Ship ONLY the new (post-delete) ops and apply to B.
      final deleteOps = afterOps.sublist(beforeOps);
      final res = await applierB.apply(await shipFrom(a, deleteOps));
      expect(res.applied, greaterThan(0));

      // B's practice row is now tombstoned and excluded from reads.
      final bRow = (await rowOf(b, 'practices', id))!;
      expect(bRow['deleted_at'], isNotNull);
      expect(await b.songRepo.getPractice(id), isA<Error<SongPractice>>());
      expect(await liveTitles(b), isNot(contains('to delete')));
    },
  );

  test(
    'future-clock quarantine: an absurd-future record is rejected and the '
    'local clock is not poisoned; integrity repair reaches a fixpoint',
    () async {
      final created = ok(await a.songRepo.createPractice('normal'));
      await applierB.apply(await shipFrom(a, await oplogOf(a)));

      final beforeMaxWall = b.writer.clock.maxWallEverSeen;
      final beforeLast = b.writer.clock.last;

      // Forge a record stamped absurdly far in the future (years ahead), well
      // beyond the clock's drift bound.
      final farFuture =
          DateTime.now().millisecondsSinceEpoch +
          HlcClock.defaultMaxDriftMs +
          const Duration(days: 365).inMilliseconds;
      final goodRow = (await rowOf(a, 'practices', created.id))!;
      final poisoned = Map<String, Object?>.of(goodRow)
        ..['title'] = 'from the year 3000'
        ..['hlc_wall'] = farFuture
        ..['hlc_ctr'] = 0
        ..['hlc_node'] = 'BADCLOCKBADCLOCKBADCLOCK00'
        ..['hlc_packed'] = Hlc(
          wallMs: farFuture,
          counter: 0,
          node: 'BADCLOCKBADCLOCKBADCLOCK00',
        ).packed;

      final res = await applierB.apply([
        RemoteRecord(table: 'practices', columns: poisoned),
      ]);
      expect(res.quarantined, 1, reason: 'beyond maxDrift -> quarantined');
      expect(res.applied, 0);

      // The poison did NOT take: title unchanged, and the clock did not ratchet
      // forward to the bogus future wall.
      final bRow = (await rowOf(b, 'practices', created.id))!;
      expect(bRow['title'], 'normal');
      expect(
        b.writer.clock.maxWallEverSeen,
        beforeMaxWall,
        reason: 'a quarantined remote must not advance maxWallEverSeen',
      );
      expect(
        b.writer.clock.last,
        beforeLast,
        reason: 'the local clock is not poisoned',
      );
      // The next local tick stays plausibly near wall time, not in the far future.
      final next = b.writer.clock.tick();
      expect(next.wallMs < farFuture, isTrue);
      expect(next.wallMs.isFinite, isTrue);

      // Integrity repair runs to a fixpoint with no remaining dangling refs.
      final checker = IntegrityChecker(b.db, b.writer);
      final remaining = await checker.repair();
      expect(remaining, isEmpty);
      expect(
        await checker.check(),
        isEmpty,
        reason: 'a clean store stays clean after repair',
      );
    },
  );

  test(
    'SyncApplier never re-journals applied remote ops (no ping-pong)',
    () async {
      // Build a varied change-set on A: create, rename, delete a second practice.
      final keep = ok(await a.songRepo.createPractice('keep'));
      ok(
        await a.songRepo.savePractice(
          SongPractice(
            id: keep.id,
            title: 'keep renamed',
            createdAt: keep.createdAt,
            updatedAt: keep.updatedAt,
            versions: keep.versions,
            activeVersionId: keep.activeVersionId,
          ),
        ),
      );
      final gone = ok(await a.songRepo.createPractice('gone'));
      ok(await a.songRepo.deletePractice(gone.id));

      // B starts with an empty oplog.
      expect(await oplogOf(b), isEmpty);

      final records = await shipFrom(a, await oplogOf(a));
      expect(records, isNotEmpty);

      final res = await applierB.apply(records);
      expect(res.applied, greaterThan(0));

      // Applying remote changes writes NO oplog on B — an applied remote op can
      // never be re-pushed (the two-door invariant that prevents an infinite
      // ping-pong).
      expect(await oplogOf(b), isEmpty);

      // Re-applying the SAME records is idempotent and still journals nothing:
      // every record loses LWW against what was just adopted.
      final again = await applierB.apply(records);
      expect(again.applied, 0);
      expect(again.skipped, greaterThan(0));
      expect(await oplogOf(b), isEmpty);

      // B's live state matches A's: 'keep renamed' present, 'gone' absent.
      final titles = await liveTitles(b);
      expect(titles, contains('keep renamed'));
      expect(titles, isNot(contains('gone')));
    },
  );
}
