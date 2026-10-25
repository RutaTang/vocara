import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/oplog_drain.dart';
import 'package:vocara/data/sync/sync_writer.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

void main() {
  late AppDatabase db;
  late SyncWriter writer;
  late OplogDrain drain;
  var now = 1000;

  setUp(() {
    db = AppDatabase(NativeDatabase.memory());
    writer = SyncWriter(db, HlcClock(node: _id('NODE'), now: () => now));
    drain = OplogDrain(db);
  });
  tearDown(() => db.close());

  /// Journals one entry through the production chokepoint and returns its HLC.
  Future<Hlc> journal(
    String table,
    String id,
    String op, {
    String payloadHash = 'h',
    String? dependsOnAssetId,
  }) async {
    final stamp = writer.stamp();
    await writer.journal(
      table: table,
      id: _id(id),
      op: op,
      hlc: stamp.hlc,
      payloadHash: op == 'tombstone' ? '' : payloadHash,
      dependsOnAssetId: dependsOnAssetId,
    );
    return stamp.hlc;
  }

  Future<int> oplogCount() async => (await db.select(db.oplog).get()).length;

  test(
    'an empty ledger drains to an empty batch and ack(0) is a no-op',
    () async {
      final batch = await drain.readPending();
      expect(batch.isEmpty, isTrue);
      expect(batch.maxSeq, 0);
      expect(await drain.ack(batch.maxSeq), 0);
    },
  );

  test('multiple entries per (table, id) collapse to the LATEST op; a '
      'trailing tombstone is never lost', () async {
    await journal('practices', 'P1', 'insert');
    await journal('practices', 'P1', 'update', payloadHash: 'h2');
    await journal('practices', 'P2', 'insert', payloadHash: 'p2');
    final tomb = await journal('practices', 'P1', 'tombstone');

    final batch = await drain.readPending();
    expect(batch.entries, hasLength(2));
    // Ordered by ascending (latest) seq: P2's insert (seq 3), P1's tomb (4).
    expect(batch.entries[0].id, _id('P2'));
    expect(batch.entries[0].op, 'insert');
    expect(batch.entries[1].id, _id('P1'));
    expect(batch.entries[1].op, 'tombstone'); // latest op won the collapse
    expect(batch.entries[1].hlcPacked, tomb.packed);
    expect(batch.entries[1].payloadHash, '');
    expect(batch.entries[1].seq, 4);
    expect(batch.maxSeq, 4); // covers the collapsed-away entries too
  });

  test('ack deletes exactly the drained range', () async {
    await journal('practices', 'P1', 'insert');
    await journal('practices', 'P1', 'update');
    await journal('tracks', 'T1', 'insert');

    final batch = await drain.readPending();
    expect(batch.maxSeq, 3);
    expect(await drain.ack(batch.maxSeq), 3); // all raw entries, not just 2
    expect(await oplogCount(), 0);
    expect((await drain.readPending()).isEmpty, isTrue);
  });

  test("entries journaled AFTER a read survive that read's ack", () async {
    await journal('practices', 'P1', 'insert');
    final batch = await drain.readPending();

    // A concurrent local edit lands between the push and the ack.
    final later = await journal('practices', 'P1', 'update', payloadHash: 'h9');

    await drain.ack(batch.maxSeq);
    expect(await oplogCount(), 1); // the later entry is untouched

    final next = await drain.readPending();
    expect(next.entries.single.op, 'update');
    expect(next.entries.single.hlcPacked, later.packed);
  });

  test('collapsing keeps the latest non-null dependsOnAssetId (a content '
      'update must not shed the insert asset gate)', () async {
    await journal('clips', 'C1', 'insert', dependsOnAssetId: _id('ASSET'));
    await journal('clips', 'C1', 'update', payloadHash: 'moved');

    final batch = await drain.readPending();
    final entry = batch.entries.single;
    expect(entry.op, 'update'); // latest op
    expect(entry.dependsOnAssetId, _id('ASSET')); // dependency preserved
  });

  test(
    'a newer dependsOnAssetId replaces an older one in the collapse',
    () async {
      await journal('clips', 'C1', 'insert', dependsOnAssetId: _id('OLD'));
      await journal('clips', 'C1', 'update', dependsOnAssetId: _id('NEW'));

      final batch = await drain.readPending();
      expect(batch.entries.single.dependsOnAssetId, _id('NEW'));
    },
  );

  test('limit bounds the RAW scan; entries beyond the window survive the ack '
      'and drain next round', () async {
    final v1 = await journal('practices', 'P1', 'insert');
    await journal('practices', 'P2', 'insert');
    final v2 = await journal('practices', 'P1', 'update', payloadHash: 'h2');

    final first = await drain.readPending(limit: 2);
    expect(first.entries, hasLength(2));
    expect(first.maxSeq, 2); // the window's max, NOT the table's
    // Within the window P1 collapses to its (older) insert — the newer update
    // sits beyond the limit and must not be acked away.
    expect(first.entries.first.hlcPacked, v1.packed);

    await drain.ack(first.maxSeq);

    final second = await drain.readPending(limit: 2);
    expect(second.entries.single.id, _id('P1'));
    expect(second.entries.single.op, 'update');
    expect(second.entries.single.hlcPacked, v2.packed);
  });

  test('a PARTIAL ack leaves the surviving entry still gated by the asset '
      'its deleted history carried', () async {
    // The push loop acks up to the entry before the first gated one, so a
    // row's own history can straddle that cut: the insert carries the asset
    // dependency, a later content-only update does not. Deleting the insert
    // without carrying its dependency forward left the next drain with a bare
    // update — and the pusher would send a row referencing bytes the server
    // has never received.
    // An unrelated row pushed in the same round, acked away with the cut.
    await journal('practices', 'P1', 'insert');
    await journal('clips', 'GATED', 'insert', dependsOnAssetId: _id('ASSET'));
    await journal('clips', 'GATED', 'update', payloadHash: 'moved');

    final batch = await drain.readPending();
    final gated = batch.entries.firstWhere((e) => e.id == _id('GATED'));
    expect(gated.dependsOnAssetId, _id('ASSET'));
    // Everything strictly before the gated row's surviving entry is acked.
    await drain.ack(gated.seq - 1);

    final next = await drain.readPending();
    expect(
      next.entries.single.id,
      _id('GATED'),
      reason: 'only the held row is left',
    );
    expect(
      next.entries.single.dependsOnAssetId,
      _id('ASSET'),
      reason: 'the gate must outlive the history that recorded it',
    );
  });

  test('a partial ack must not stamp an OLD dependency over a NEWER one that '
      'still lives above the cut', () async {
    // The repair carries a dependency from the deleted prefix onto surviving
    // null entries. Bounded by the ACK WATERMARK it takes the newest one from
    // the deleted side and stamps it on every survivor — including entries
    // that sit ABOVE a newer dependency, whose higher seq then wins the
    // collapse. The row resolves to OLD and gets pushed while NEW's bytes are
    // still missing: exactly what the repair exists to prevent.
    //
    // The cut has to fall between the row's two dependency entries, which
    // needs a DIFFERENT row to be the first gated one.
    await journal('clips', 'C1', 'insert', dependsOnAssetId: _id('OLD'));
    await journal('clips', 'OTHER', 'insert', dependsOnAssetId: _id('GATE'));
    await journal('clips', 'C1', 'update', dependsOnAssetId: _id('NEW'));
    await journal('clips', 'C1', 'update', payloadHash: 'renamed');

    final batch = await drain.readPending();
    final other = batch.entries.firstWhere((e) => e.id == _id('OTHER'));
    expect(
      batch.entries.firstWhere((e) => e.id == _id('C1')).dependsOnAssetId,
      _id('NEW'),
    );
    // Ack everything before the gated row, as the push loop does.
    await drain.ack(other.seq - 1);

    final next = await drain.readPending();
    expect(
      next.entries.firstWhere((e) => e.id == _id('C1')).dependsOnAssetId,
      _id('NEW'),
      reason:
          'the surviving entry inherits the newest dependency that '
          'precedes IT, not the newest one below the ack watermark',
    );
  });

  test('a tombstone inherits no asset dependency: a delete references no '
      'bytes, and gating one stalls the whole ledger', () async {
    // The asset an insert needed can be collected once the row is deleted, so
    // a tombstone that inherited it would wait on an upload that will never
    // happen — and because the push loop acks only up to the entry before the
    // first gated one, that one tombstone freezes the ack watermark for
    // everything behind it.
    await journal('clips', 'C1', 'insert', dependsOnAssetId: _id('ASSET'));
    await journal('clips', 'C1', 'tombstone');

    final batch = await drain.readPending();
    expect(batch.entries.single.op, 'tombstone');
    expect(batch.entries.single.dependsOnAssetId, isNull);
  });

  test('a partial ack does not persist an inherited dependency onto a '
      'tombstone either', () async {
    await journal('clips', 'C1', 'insert', dependsOnAssetId: _id('ASSET'));
    await journal('clips', 'OTHER', 'insert', dependsOnAssetId: _id('GATE'));
    await journal('clips', 'C1', 'tombstone');

    final batch = await drain.readPending();
    final other = batch.entries.firstWhere((e) => e.id == _id('OTHER'));
    await drain.ack(other.seq - 1);

    final next = await drain.readPending();
    expect(
      next.entries.firstWhere((e) => e.id == _id('C1')).dependsOnAssetId,
      isNull,
    );
  });

  test(
    'an explicit dependency journaled ON a tombstone is still honoured',
    () async {
      // The carve-out is about INHERITANCE. A caller that deliberately gates a
      // delete keeps that gate.
      await journal('clips', 'C1', 'insert');
      await journal('clips', 'C1', 'tombstone', dependsOnAssetId: _id('ASSET'));

      final batch = await drain.readPending();
      expect(batch.entries.single.dependsOnAssetId, _id('ASSET'));
    },
  );

  test('a fully-acked row that is edited again inherits the dependency, and '
      'the gate simply re-checks the upload', () async {
    await journal('clips', 'C1', 'insert', dependsOnAssetId: _id('ASSET'));
    final first = await drain.readPending();
    await drain.ack(first.maxSeq);
    expect(await oplogCount(), 0);

    // A later edit of the same row journals no dependency of its own.
    await journal('clips', 'C1', 'update', payloadHash: 'renamed');
    final second = await drain.readPending();
    expect(second.entries.single.dependsOnAssetId, isNull);
  });

  test('a non-positive limit reads nothing', () async {
    await journal('practices', 'P1', 'insert');
    final batch = await drain.readPending(limit: 0);
    expect(batch.isEmpty, isTrue);
    expect(batch.maxSeq, 0);
    expect(await oplogCount(), 1); // and acks nothing away
  });

  test('with no dependency anywhere, an ack rewrites no surviving entry: '
      'only the acked ones go', () async {
    // Nothing journals an asset dependency in production, so the carry has
    // nothing to carry — yet it used to rewrite every entry above the
    // watermark (to the NULL it already held) on every ack: quadratic over
    // a first push that drains a big ledger window by window.
    Future<int> totalChanges() async =>
        (await db.customSelect('SELECT total_changes() AS n').getSingle())
            .read<int>('n');
    for (var i = 0; i < 4; i++) {
      await journal('clips', 'C$i', 'insert');
    }
    await journal('clips', 'C0', 'update', payloadHash: 'h2');

    final first = (await drain.readPending(limit: 1)).maxSeq;
    final before = await totalChanges();
    expect(await drain.ack(first), 1);
    expect(await totalChanges() - before, 1, reason: 'the one acked entry');
    expect(await oplogCount(), 4);
  });

  test(
    'the ack and boot-compaction carry-forward probe each entity through '
    'ix_oplog_entity instead of scanning the whole ledger per entry',
    () async {
      for (var i = 0; i < 50; i++) {
        await journal('clips', 'C$i', 'insert');
        await journal('clips', 'C$i', 'update', payloadHash: 'h$i');
      }
      Future<List<String>> plan(
        String sql, [
        List<Object?> args = const [],
      ]) async {
        final rows = await db
            .customSelect(
              'EXPLAIN QUERY PLAN $sql',
              variables: [for (final a in args) Variable(a)],
            )
            .get();
        return [for (final r in rows) r.read<String>('detail')];
      }

      // Unindexed, the correlated subquery reads `SCAN o`: one full ledger
      // pass per surviving entry, quadratic in a ledger nothing acks today.
      for (final steps in [
        await plan(OplogDrain.ackCarrySql, [0]),
        await plan(Persistence.compactCarrySql),
      ]) {
        expect(
          steps,
          contains(startsWith('SEARCH o USING INDEX ix_oplog_entity')),
          reason: steps.join('\n'),
        );
        expect(steps, isNot(contains('SCAN o')));
      }
    },
  );
}
