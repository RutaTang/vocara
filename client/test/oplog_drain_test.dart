import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
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

  test('a non-positive limit reads nothing', () async {
    await journal('practices', 'P1', 'insert');
    final batch = await drain.readPending(limit: 0);
    expect(batch.isEmpty, isTrue);
    expect(batch.maxSeq, 0);
    expect(await oplogCount(), 1); // and acks nothing away
  });
}
